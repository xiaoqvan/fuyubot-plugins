//! 数据访问层：把 TS 版的 Mongo 集合原样搬到 `storage.mongo` 服务上。
//!
//! ## 为什么是「服务」而不是连接池
//!
//! FuyuBot 的插件是动态库，宿主进程里不该躺着第二份 Mongo 驱动/连接池。
//! 数据库能力由 **`storage.mongo` 插件**提供（它注册的是进程级共享服务），本插件
//! 只通过 JSON 调它的 `find` / `insert` / `update` / `delete` / `aggregate`。
//!
//! 这是一条**硬依赖**：`inject()` 里声明了 `storage.mongo`，宿主会等它先激活；
//! `start()` 再 `ping` 一次，连不上就直接失败——「没有库还硬跑」只会把广告当没看见。
//!
//! ## 与 TS 版的兼容
//!
//! 集合名、字段名、索引全部照旧（`adb_config` / `ads_blacklist` / `flieId_blacklist`
//! ——最后这个拼写错误也保留了，它已经在库里了）。日期统一存 BSON `Date`（用
//! Extended JSON 的 `{"$date":{"$numberLong":"…"}}` 写），读回来折成毫秒整数。
//!
//! ## 线程
//!
//! 所有方法都是**同步阻塞**的（服务内部在 tokio 上 `block_on`）。只能在插件自己的
//! 工作线程里调，绝不能在宿主的更新回调里调——见 `runtime` 的线程模型。

use fuyubot_abi::HostApi;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::log;
use crate::util::{new_id, now_ms};

/// 数据库插件注册的服务名。**跨仓契约**：mongo 插件拆出去之后这个名字不变。
pub const SERVICE: &str = "storage.mongo";

/// 与 TS 版一致的集合名。留成常量，改错一个字符会当场不认识数据。
pub mod collections {
    pub const CONFIG: &str = "adb_config";
    pub const BAN_RECORDS: &str = "ban_records";
    pub const MESSAGE_RECORDS: &str = "message_records";
    pub const MUTE_REVIEWS: &str = "mute_review_records";
    pub const WATCHES: &str = "new_member_watch_records";
    pub const ADS: &str = "ads_blacklist";
    pub const BIO: &str = "bio_blacklist";
    pub const NAME: &str = "name_blacklist";
    pub const USERNAME: &str = "username_blacklist";
    pub const FORWARD: &str = "forward_blacklist";
    /// ⚠️ 原版就是 `flieId_blacklist`（拼错了）。库里已经有这个集合，改名等于丢数据。
    pub const FILE_IDS: &str = "flieId_blacklist";
    pub const STICKERS: &str = "sticker_blacklist";
    pub const STICKER_SETS: &str = "sticker_set_blacklist";
    pub const AI_FULLTEXT: &str = "ai_fulltext_blacklist";
    pub const AI_DEFAULT_CHATS: &str = "ai_default_chats";
    /// 插件自己的设置（全局 AI 触发关键词）。TS 版放的是本地 lowdb 文件。
    pub const SETTINGS: &str = "adblock_settings";
    /// `/spam` 的举报流水（TS 版就有，保留以免审计断档）。
    pub const SPAM_LOG: &str = "SPAM_Log";
}

/// 消息记录保留 48 小时。
pub const MESSAGE_RECORD_TTL_SECONDS: u64 = 48 * 60 * 60;
/// 入群监视窗口：24 小时。
pub const NEW_MEMBER_WATCH_MS: i64 = 24 * 60 * 60 * 1000;
/// 单条回复的安全长度（TDLib 文本上限约 4096）。
pub const MAX_TEXT: usize = 4000;

// ── 模型 ──────────────────────────────────────────────────────────

/// 广告处置模式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum LimitMode {
    Ban,
    #[default]
    Mute,
}

impl LimitMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ban => "ban",
            Self::Mute => "mute",
        }
    }
}

/// 一个群的配置（对应 TS 版 `GroupConfig`）。
///
/// 字段名与库里的键一一对应；`Whitelist` 大写、`spam_fileId` 驼峰都是**库里的原样**。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct GroupConfig {
    #[serde(rename = "type")]
    pub kind: String,
    pub log_channel: Option<i64>,
    #[serde(rename = "spam_fileId")]
    pub spam_file_id: Vec<String>,
    pub spam_sticker: Vec<String>,
    pub limit_mode: LimitMode,
    #[serde(rename = "Whitelist")]
    pub whitelist: Vec<i64>,
    /// 群里发不发「已处置」提示。默认 **true**（`/adconfig` 的展示也是这个默认）。
    pub spam_tips: bool,
    /// 提示多少秒后自删；`0` = 不删。默认 30。
    pub spam_tips_meg_delay: u32,
    pub ai_review_enabled: bool,
    pub check_new_member: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ai_api_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ai_base_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ai_model: Option<String>,
}

impl Default for GroupConfig {
    fn default() -> Self {
        Self {
            kind: "group".to_owned(),
            log_channel: None,
            spam_file_id: Vec::new(),
            spam_sticker: Vec::new(),
            limit_mode: LimitMode::Mute,
            whitelist: Vec::new(),
            spam_tips: true,
            spam_tips_meg_delay: 30,
            ai_review_enabled: false,
            check_new_member: false,
            ai_api_key: None,
            ai_base_url: None,
            ai_model: None,
        }
    }
}

impl GroupConfig {
    pub fn has_custom_ai(&self) -> bool {
        self.ai_api_key
            .as_deref()
            .is_some_and(|key| !key.is_empty())
            && self
                .ai_base_url
                .as_deref()
                .is_some_and(|url| !url.is_empty())
    }
}

/// 从文档里取字符串数组（`$` 字段缺失 / 类型不对时给空表）。
fn strings_of(doc: &Value, key: &str) -> Vec<String> {
    doc.get(key)
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

fn i64s_of(doc: &Value, key: &str) -> Vec<i64> {
    doc.get(key)
        .and_then(Value::as_array)
        .map(|items| items.iter().filter_map(Value::as_i64).collect())
        .unwrap_or_default()
}

fn bool_of(doc: &Value, key: &str) -> Option<bool> {
    doc.get(key).and_then(Value::as_bool)
}

fn u32_of(doc: &Value, key: &str) -> Option<u32> {
    doc.get(key)
        .and_then(Value::as_u64)
        .map(|value| value.min(u32::MAX as u64) as u32)
}

fn i64_of(doc: &Value, key: &str) -> Option<i64> {
    doc.get(key).and_then(Value::as_i64)
}

impl GroupConfig {
    /// 文档 → 配置。缺字段时用默认值（与 TS 版 `cfg?.x ?? 默认` 的分支等价）。
    pub fn from_doc(doc: &Value) -> Self {
        let defaults = Self::default();
        Self {
            kind: doc
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or("group")
                .to_owned(),
            log_channel: i64_of(doc, "log_channel"),
            spam_file_id: strings_of(doc, "spam_fileId"),
            spam_sticker: strings_of(doc, "spam_sticker"),
            limit_mode: match doc.get("limit_mode").and_then(Value::as_str) {
                Some("ban") => LimitMode::Ban,
                Some("mute") => LimitMode::Mute,
                _ => defaults.limit_mode,
            },
            whitelist: i64s_of(doc, "Whitelist"),
            spam_tips: bool_of(doc, "spam_tips").unwrap_or(defaults.spam_tips),
            spam_tips_meg_delay: u32_of(doc, "spam_tips_meg_delay")
                .unwrap_or(defaults.spam_tips_meg_delay),
            ai_review_enabled: bool_of(doc, "ai_review_enabled").unwrap_or(false),
            check_new_member: bool_of(doc, "check_new_member").unwrap_or(false),
            ai_api_key: doc
                .get("ai_api_key")
                .and_then(Value::as_str)
                .filter(|text| !text.is_empty())
                .map(str::to_owned),
            ai_base_url: doc
                .get("ai_base_url")
                .and_then(Value::as_str)
                .filter(|text| !text.is_empty())
                .map(str::to_owned),
            ai_model: doc
                .get("ai_model")
                .and_then(Value::as_str)
                .filter(|text| !text.is_empty())
                .map(str::to_owned),
        }
    }
}

/// 封禁 / 禁言审计（`ban_records`）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BanRecord {
    pub id: String,
    pub chat_id: i64,
    pub target_tgid: i64,
    pub target_type: String,
    pub action: String,
    pub spam_type: String,
    pub spam_detail: String,
    pub trigger_db_id: Option<String>,
    pub created_at: i64,
}

/// 禁言待复审（`mute_review_records`）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MuteReview {
    pub chat_id: i64,
    pub user_id: i64,
    pub message_id: Option<i64>,
    pub reason: String,
    pub confidence: f64,
    pub muted_at: i64,
    pub deadline: i64,
    /// `pending` / `approved` / `banned`
    pub status: String,
    pub updated_at: i64,
}

/// 入群 24 小时监视（`new_member_watch_records`）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewMemberWatch {
    pub chat_id: i64,
    pub user_id: i64,
    pub join_at: i64,
    pub expires_at: i64,
    pub join_name: String,
    pub join_bio: String,
    pub profile_changed: bool,
    pub profile_rechecked: bool,
    pub last_recheck_at: Option<i64>,
    pub recheck_count: u32,
    pub review_approved: bool,
    pub review_approved_at: Option<i64>,
    pub updated_at: i64,
}

impl NewMemberWatch {
    fn from_doc(doc: &Value) -> Self {
        Self {
            chat_id: i64_of(doc, "chat_id").unwrap_or(0),
            user_id: i64_of(doc, "user_id").unwrap_or(0),
            join_at: date_ms(doc.get("join_at").unwrap_or(&Value::Null)).unwrap_or(0),
            expires_at: date_ms(doc.get("expires_at").unwrap_or(&Value::Null)).unwrap_or(0),
            join_name: string_of(doc, "join_name"),
            join_bio: string_of(doc, "join_bio"),
            profile_changed: bool_of(doc, "profile_changed").unwrap_or(false),
            profile_rechecked: bool_of(doc, "profile_rechecked").unwrap_or(false),
            last_recheck_at: doc.get("last_recheck_at").and_then(date_ms),
            recheck_count: u32_of(doc, "recheck_count").unwrap_or(0),
            review_approved: bool_of(doc, "review_approved").unwrap_or(false),
            review_approved_at: doc.get("review_approved_at").and_then(date_ms),
            updated_at: date_ms(doc.get("updated_at").unwrap_or(&Value::Null)).unwrap_or(0),
        }
    }
}

fn string_of(doc: &Value, key: &str) -> String {
    doc.get(key)
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned()
}

/// 黑名单集合（`/qadid`、`/delad` 的「集合」名）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Collection {
    Ads,
    Bio,
    Forward,
    Name,
    Username,
    AiFullText,
    File,
    StickerSet,
    Sticker,
}

impl Collection {
    pub fn name(self) -> &'static str {
        match self {
            Self::Ads => collections::ADS,
            Self::Bio => collections::BIO,
            Self::Forward => collections::FORWARD,
            Self::Name => collections::NAME,
            Self::Username => collections::USERNAME,
            Self::AiFullText => collections::AI_FULLTEXT,
            Self::File => collections::FILE_IDS,
            Self::StickerSet => collections::STICKER_SETS,
            Self::Sticker => collections::STICKERS,
        }
    }
}

/// 一条查到的黑名单记录（`/qadid` 用）。
#[derive(Debug, Clone)]
pub struct BlacklistEntry {
    pub collection: Collection,
    pub record: Value,
}

// ── 日期 ──────────────────────────────────────────────────────────

/// 毫秒时间戳 → BSON `Date` 的 Extended JSON 写法。
///
/// 库里的老数据是 TS 的 `new Date()`（BSON Date），这里必须写同一种类型：
/// TTL 索引只认 Date，混着整数会让「48 小时自动过期」静默失效。
pub fn date(ms: i64) -> Value {
    json!({ "$date": { "$numberLong": ms.to_string() } })
}

/// BSON `Date`（任意 Extended JSON 形态）→ 毫秒时间戳。
pub fn date_ms(value: &Value) -> Option<i64> {
    match value {
        Value::String(text) => parse_rfc3339_ms(text),
        Value::Number(number) => number.as_i64(),
        Value::Object(object) => {
            let inner = object.get("$date")?;
            match inner {
                Value::String(text) => parse_rfc3339_ms(text),
                Value::Number(number) => number.as_i64(),
                Value::Object(inner) => inner.get("$numberLong").and_then(|value| match value {
                    Value::String(text) => text.parse::<i64>().ok(),
                    Value::Number(number) => number.as_i64(),
                    _ => None,
                }),
                _ => None,
            }
        }
        _ => None,
    }
}

/// `2026-11-01T06:00:00Z` / `…+08:00` → 毫秒。只认本插件自己写得出的形状。
fn parse_rfc3339_ms(text: &str) -> Option<i64> {
    let text = text.trim();
    let (date, rest) = text.split_once('T')?;
    let mut date_parts = date.split('-');
    let year: i64 = date_parts.next()?.parse().ok()?;
    let month: i64 = date_parts.next()?.parse().ok()?;
    let day: i64 = date_parts.next()?.parse().ok()?;
    let (time, offset_seconds) = match rest.find(['Z', 'z', '+']) {
        Some(index)
            if rest.as_bytes()[index] == b'+'
                || rest.as_bytes()[index] == b'Z'
                || rest.as_bytes()[index] == b'z' =>
        {
            let zulu = rest.as_bytes()[index] == b'Z' || rest.as_bytes()[index] == b'z';
            let time = &rest[..index];
            let offset = if zulu {
                0
            } else {
                let offset_text = &rest[index + 1..];
                let (hours, minutes) = offset_text.split_once(':')?;
                let hours: i64 = hours.parse().ok()?;
                let minutes: i64 = minutes.parse().ok()?;
                hours * 3600 + minutes * 60
            };
            (time, offset)
        }
        _ => (rest, 0),
    };
    let mut time_parts = time.split(':');
    let hour: i64 = time_parts.next()?.parse().ok()?;
    let minute: i64 = time_parts.next()?.parse().ok()?;
    let second_text = time_parts.next()?;
    let (second, millis) = match second_text.split_once('.') {
        Some((second, fraction)) => {
            let second: i64 = second.parse().ok()?;
            let fraction = format!("{fraction:0<3}");
            let millis: i64 = fraction[..3].parse().ok()?;
            (second, millis)
        }
        None => (second_text.parse().ok()?, 0),
    };
    let days = days_from_civil(year, month, day);
    let seconds = days * 86_400 + hour * 3600 + minute * 60 + second - offset_seconds;
    Some(seconds * 1000 + millis)
}

/// 与 [`crate::util::fmt_utc`] 的 `civil_from_days` 互逆。
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = if month > 2 { month - 3 } else { month + 9 };
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

// ── 服务客户端 ────────────────────────────────────────────────────

/// `storage.mongo` 的薄客户端。
///
/// 只做三件事：拼 `{"collection":…, …}`、检查回复里的 `ok`、把错误统一成 `String`。
#[derive(Debug, Clone)]
pub struct Mongo {
    host: HostApi,
    /// 覆盖服务端的默认库（`None` = 用 mongo 插件的 `db`）。
    db: Option<String>,
}

impl Mongo {
    pub fn new(host: HostApi, db: Option<String>) -> Self {
        Self { host, db }
    }

    fn call(&self, method: &str, mut input: Value) -> Result<Value, String> {
        if let Some(db) = &self.db
            && let Some(object) = input.as_object_mut()
            && !object.contains_key("db")
        {
            object.insert("db".to_owned(), Value::String(db.clone()));
        }
        let reply = self
            .host
            .call_service(SERVICE, method, &input.to_string())
            .map_err(|error| format!("{SERVICE}.{method} 调用失败：{error}"))?;
        let value: Value = serde_json::from_str(&reply)
            .map_err(|error| format!("{SERVICE}.{method} 的响应不是 JSON：{error}"))?;
        if value.get("ok").and_then(Value::as_bool) != Some(true) {
            let message = value
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or("未知错误");
            return Err(format!("{SERVICE}.{method}：{message}"));
        }
        Ok(value)
    }

    pub fn ping(&self) -> Result<Value, String> {
        self.call("ping", json!({}))
    }

    pub fn find(
        &self,
        collection: &str,
        filter: Value,
        sort: Option<Value>,
        limit: Option<i64>,
    ) -> Result<Vec<Value>, String> {
        let mut input = json!({ "collection": collection, "filter": filter });
        if let Some(sort) = sort {
            input["sort"] = sort;
        }
        if let Some(limit) = limit {
            input["limit"] = json!(limit);
        }
        let reply = self.call("find", input)?;
        Ok(reply
            .get("docs")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default())
    }

    pub fn find_one(&self, collection: &str, filter: Value) -> Result<Option<Value>, String> {
        let reply = self.call(
            "find_one",
            json!({ "collection": collection, "filter": filter }),
        )?;
        Ok(reply.get("doc").filter(|doc| !doc.is_null()).cloned())
    }

    pub fn insert(&self, collection: &str, doc: Value) -> Result<Value, String> {
        let reply = self.call("insert", json!({ "collection": collection, "docs": doc }))?;
        Ok(reply
            .get("ids")
            .and_then(Value::as_array)
            .and_then(|ids| ids.first())
            .cloned()
            .unwrap_or(Value::Null))
    }

    pub fn update(
        &self,
        collection: &str,
        filter: Value,
        update: Value,
        upsert: bool,
        many: bool,
    ) -> Result<Value, String> {
        self.call(
            "update",
            json!({
                "collection": collection,
                "filter": filter,
                "update": update,
                "upsert": upsert,
                "many": many,
            }),
        )
    }

    pub fn delete(&self, collection: &str, filter: Value, many: bool) -> Result<u64, String> {
        let reply = self.call(
            "delete",
            json!({ "collection": collection, "filter": filter, "many": many }),
        )?;
        Ok(reply.get("deleted").and_then(Value::as_u64).unwrap_or(0))
    }

    pub fn count(&self, collection: &str, filter: Value) -> Result<u64, String> {
        let reply = self.call(
            "count",
            json!({ "collection": collection, "filter": filter }),
        )?;
        Ok(reply.get("count").and_then(Value::as_u64).unwrap_or(0))
    }

    pub fn aggregate(&self, collection: &str, pipeline: Value) -> Result<Vec<Value>, String> {
        let reply = self.call(
            "aggregate",
            json!({ "collection": collection, "pipeline": pipeline }),
        )?;
        Ok(reply
            .get("docs")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default())
    }

    pub fn ensure_index(
        &self,
        collection: &str,
        keys: Value,
        unique: bool,
        ttl_seconds: Option<u64>,
    ) -> Result<(), String> {
        let mut input = json!({
            "collection": collection,
            "keys": keys,
            "unique": unique,
        });
        if let Some(ttl) = ttl_seconds {
            input["ttl_seconds"] = json!(ttl);
        }
        self.call("ensure_index", input).map(|_| ())
    }
}

// ── 业务数据层 ────────────────────────────────────────────────────

/// 本插件用到的全部数据访问。`Clone` 共享同一个宿主句柄（`HostApi` 是 `Copy`）。
#[derive(Debug, Clone)]
pub struct Db {
    mongo: Mongo,
    host: HostApi,
}

impl Db {
    pub fn new(host: HostApi, db: Option<String>) -> Self {
        Self {
            mongo: Mongo::new(host, db),
            host,
        }
    }

    pub fn ping(&self) -> Result<Value, String> {
        self.mongo.ping()
    }

    /// 建索引（幂等：已存在 / 冲突都只记 debug）。
    ///
    /// 与 TS 版 `Database/index.ts` 一一对应：消息记录 48h TTL、封禁按 TGID、
    /// 复审按状态与截止时间、入群监视按 `expires_at` 自动过期。
    pub fn ensure_indexes(&self) -> (usize, usize) {
        use collections as c;
        let specs: &[(&str, Value, bool, Option<u64>)] = &[
            (
                c::MESSAGE_RECORDS,
                json!({"createdAt": 1}),
                false,
                Some(MESSAGE_RECORD_TTL_SECONDS),
            ),
            (
                c::MESSAGE_RECORDS,
                json!({"userId": 1, "chatId": 1}),
                false,
                None,
            ),
            (c::MESSAGE_RECORDS, json!({"messageId": 1}), false, None),
            (
                c::BAN_RECORDS,
                json!({"target_tgid": 1, "createdAt": -1}),
                false,
                None,
            ),
            (
                c::MUTE_REVIEWS,
                json!({"chat_id": 1, "user_id": 1}),
                false,
                None,
            ),
            (
                c::MUTE_REVIEWS,
                json!({"status": 1, "deadline": 1}),
                false,
                None,
            ),
            (c::WATCHES, json!({"chat_id": 1, "user_id": 1}), true, None),
            (
                c::WATCHES,
                json!({"user_id": 1, "expires_at": 1}),
                false,
                None,
            ),
            (c::WATCHES, json!({"expires_at": 1}), false, Some(0)),
            (c::CONFIG, json!({"chat_id": 1}), true, None),
            (c::ADS, json!({"keywords": 1}), false, None),
            (c::AI_FULLTEXT, json!({"text": 1}), false, None),
            (c::FORWARD, json!({"chat_id": 1}), false, None),
        ];
        let mut created = 0;
        let mut failed = 0;
        for (collection, keys, unique, ttl) in specs {
            match self
                .mongo
                .ensure_index(collection, keys.clone(), *unique, *ttl)
            {
                Ok(()) => created += 1,
                Err(error) => {
                    // 冲突（85/86）说明等价索引已经在库里：这是正常情况，不是错误。
                    log::debug(
                        &self.host,
                        format!("索引 {collection} {keys} 跳过：{error}"),
                    );
                    failed += 1;
                }
            }
        }
        (created, failed)
    }

    // ── 群配置 ────────────────────────────────────────────────────

    pub fn config(&self, chat_id: i64) -> Option<GroupConfig> {
        self.mongo
            .find_one(collections::CONFIG, json!({ "chat_id": chat_id }))
            .ok()
            .flatten()
            .map(|doc| GroupConfig::from_doc(&doc))
    }

    pub fn config_or_default(&self, chat_id: i64) -> GroupConfig {
        self.config(chat_id).unwrap_or_default()
    }

    /// 读 → 改 → 整段写回（`$set` + upsert）。
    ///
    /// 用「整段写回」而不是逐个 `$set`，是为了让新增配置项自动带上默认值：
    /// 少写一个字段，读侧也会用默认值兜住。
    pub fn update_config(&self, chat_id: i64, f: impl FnOnce(&mut GroupConfig)) -> GroupConfig {
        let mut config = self.config_or_default(chat_id);
        config.kind = "group".to_owned();
        f(&mut config);
        if let Err(error) = self.save_config(chat_id, &config) {
            log::warn(
                &self.host,
                format!("写 adb_config（{chat_id}）失败：{error}"),
            );
        }
        config
    }

    fn save_config(&self, chat_id: i64, config: &GroupConfig) -> Result<(), String> {
        let mut set = serde_json::to_value(config).map_err(|error| error.to_string())?;
        // `chat_id` 只放过滤条件里；写进 `$set` 反而会和 `$setOnInsert` 冲突。
        if let Some(object) = set.as_object_mut() {
            object.remove("chat_id");
            object.remove("_id");
        }
        self.mongo.update(
            collections::CONFIG,
            json!({ "chat_id": chat_id }),
            json!({ "$set": set, "$setOnInsert": { "chat_id": chat_id } }),
            true,
            false,
        )?;
        Ok(())
    }

    pub fn set_whitelisted(&self, chat_id: i64, id: i64, add: bool) {
        let update = if add {
            json!({ "$addToSet": { "Whitelist": id }, "$set": { "type": "group" } })
        } else {
            json!({ "$pull": { "Whitelist": id } })
        };
        if let Err(error) = self.mongo.update(
            collections::CONFIG,
            json!({ "chat_id": chat_id }),
            update,
            add,
            false,
        ) {
            log::warn(&self.host, format!("更新白名单失败：{error}"));
        }
    }

    pub fn is_whitelisted(&self, chat_id: i64, id: i64) -> bool {
        self.config(chat_id)
            .is_some_and(|config| config.whitelist.contains(&id))
    }

    pub fn set_spam_sticker(&self, chat_id: i64, set_id: &str, add: bool) {
        let update = if add {
            json!({ "$addToSet": { "spam_sticker": set_id }, "$set": { "type": "group" } })
        } else {
            json!({ "$pull": { "spam_sticker": set_id } })
        };
        let _ = self.mongo.update(
            collections::CONFIG,
            json!({ "chat_id": chat_id }),
            update,
            add,
            false,
        );
    }

    pub fn set_spam_file(&self, chat_id: i64, file_id: &str, add: bool) {
        let update = if add {
            json!({ "$addToSet": { "spam_fileId": file_id }, "$set": { "type": "group" } })
        } else {
            json!({ "$pull": { "spam_fileId": file_id } })
        };
        let _ = self.mongo.update(
            collections::CONFIG,
            json!({ "chat_id": chat_id }),
            update,
            add,
            false,
        );
    }

    // ── 封禁记录 ──────────────────────────────────────────────────

    pub fn add_ban_record(
        &self,
        chat_id: i64,
        target_tgid: i64,
        target_type: &str,
        action: &str,
        spam_type: &str,
        spam_detail: &str,
        trigger_db_id: Option<&str>,
    ) {
        let doc = json!({
            "chat_id": chat_id,
            "target_tgid": target_tgid,
            "target_type": target_type,
            "action": action,
            "spam_type": spam_type,
            "spam_detail": spam_detail,
            "trigger_db_id": trigger_db_id,
            "createdAt": date(now_ms()),
        });
        if let Err(error) = self.mongo.insert(collections::BAN_RECORDS, doc) {
            log::warn(&self.host, format!("写 ban_records 失败：{error}"));
        }
    }

    pub fn ban_records_by_tgid(&self, tgid: i64, limit: usize) -> Vec<BanRecord> {
        let limit = limit.clamp(1, 50) as i64;
        let docs = self
            .mongo
            .find(
                collections::BAN_RECORDS,
                json!({ "target_tgid": tgid }),
                Some(json!({ "createdAt": -1 })),
                Some(limit),
            )
            .unwrap_or_default();
        docs.iter()
            .map(|doc| BanRecord {
                id: doc_id(doc),
                chat_id: i64_of(doc, "chat_id").unwrap_or(0),
                target_tgid: i64_of(doc, "target_tgid").unwrap_or(0),
                target_type: string_of(doc, "target_type"),
                action: string_of(doc, "action"),
                spam_type: string_of(doc, "spam_type"),
                spam_detail: string_of(doc, "spam_detail"),
                trigger_db_id: doc
                    .get("trigger_db_id")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                created_at: doc.get("createdAt").and_then(date_ms).unwrap_or(0),
            })
            .collect()
    }

    // ── 消息记录 ──────────────────────────────────────────────────

    pub fn add_message(&self, chat_id: i64, user_id: i64, message_id: i64) {
        let doc = json!({
            "messageId": message_id,
            "userId": user_id,
            "chatId": chat_id,
            "createdAt": date(now_ms()),
        });
        if let Err(error) = self.mongo.insert(collections::MESSAGE_RECORDS, doc) {
            log::debug(&self.host, format!("写 message_records 失败：{error}"));
        }
    }

    pub fn user_message_ids(&self, chat_id: i64, user_id: i64) -> Vec<i64> {
        let docs = self
            .mongo
            .find(
                collections::MESSAGE_RECORDS,
                json!({ "userId": user_id, "chatId": chat_id }),
                None,
                None,
            )
            .unwrap_or_default();
        docs.iter()
            .filter_map(|doc| i64_of(doc, "messageId"))
            .collect()
    }

    pub fn remove_message_ids(&self, chat_id: i64, message_ids: &[i64]) -> u64 {
        if message_ids.is_empty() {
            return 0;
        }
        self.mongo
            .delete(
                collections::MESSAGE_RECORDS,
                json!({ "chatId": chat_id, "messageId": { "$in": message_ids } }),
                true,
            )
            .unwrap_or(0)
    }

    // ── 黑名单：写 ────────────────────────────────────────────────

    /// 写一组广告关键词；返回新记录的 `_id`。
    pub fn add_ad_keywords(&self, keywords: &[String], is_r18: bool) -> Result<String, String> {
        let valid: Vec<String> = keywords
            .iter()
            .map(|keyword| keyword.trim().to_owned())
            .filter(|keyword| !keyword.is_empty())
            .collect();
        if valid.is_empty() {
            return Err("无有效关键词".to_owned());
        }
        // 与 TS 版同样的护栏：太少且太短的关键词组命中面太宽，等于误封正常聊天。
        if valid.len() <= 2 && valid.iter().all(|keyword| keyword.chars().count() <= 4) {
            return Err("关键词过短，未添加".to_owned());
        }
        let doc = json!({
            "keywords": valid,
            "isR18": is_r18,
            "createdAt": date(now_ms()),
        });
        let id = self.mongo.insert(collections::ADS, doc)?;
        Ok(id_string(&id))
    }

    pub fn add_bio(&self, keywords: &[String]) -> Result<String, String> {
        let valid: Vec<String> = keywords
            .iter()
            .map(|keyword| keyword.trim().to_owned())
            .filter(|keyword| !keyword.is_empty())
            .collect();
        if valid.is_empty() {
            return Err("无有效关键词".to_owned());
        }
        let mut doc = json!({
            "keywords": valid,
            "createdAt": date(now_ms()),
        });
        // 单条完整 bio 同时写 `text`，全文精确匹配走它（与 TS 版一致）。
        if valid.len() == 1 {
            doc["text"] = Value::String(valid[0].clone());
        }
        let id = self.mongo.insert(collections::BIO, doc)?;
        Ok(id_string(&id))
    }

    pub fn add_name(&self, name: &str) -> Result<String, String> {
        let clean = name.trim();
        if clean.is_empty() {
            return Err("名称不能为空".to_owned());
        }
        let doc = json!({ "name": clean, "createdAt": date(now_ms()) });
        let id = self.mongo.insert(collections::NAME, doc)?;
        Ok(id_string(&id))
    }

    /// 写用户名黑名单；已存在返回 `(旧 id, true)`。
    pub fn add_username(&self, username: &str) -> Result<(String, bool), String> {
        let clean = username.trim().trim_start_matches('@').to_lowercase();
        if clean.is_empty() {
            return Err("用户名不能为空".to_owned());
        }
        if let Ok(Some(existing)) = self
            .mongo
            .find_one(collections::USERNAME, json!({ "username": clean }))
        {
            return Ok((doc_id(&existing), true));
        }
        let doc = json!({ "username": clean, "createdAt": date(now_ms()) });
        let id = self.mongo.insert(collections::USERNAME, doc)?;
        Ok((id_string(&id), false))
    }

    pub fn add_forward(&self, chat_id: i64) -> Result<String, String> {
        if let Ok(Some(existing)) = self
            .mongo
            .find_one(collections::FORWARD, json!({ "chat_id": chat_id }))
        {
            return Ok(doc_id(&existing));
        }
        let doc = json!({ "chat_id": chat_id, "createdAt": date(now_ms()) });
        let id = self.mongo.insert(collections::FORWARD, doc)?;
        Ok(id_string(&id))
    }

    pub fn remove_forward(&self, chat_id: i64) -> bool {
        self.mongo
            .delete(collections::FORWARD, json!({ "chat_id": chat_id }), false)
            .map(|deleted| deleted > 0)
            .unwrap_or(false)
    }

    pub fn add_file(&self, unique_id: &str) -> Result<String, String> {
        let clean = unique_id.trim();
        if clean.is_empty() {
            return Err("文件 unique_id 不能为空".to_owned());
        }
        self.mongo.update(
            collections::FILE_IDS,
            json!({ "unique_id": clean }),
            json!({ "$setOnInsert": { "unique_id": clean, "createdAt": date(now_ms()) } }),
            true,
            false,
        )?;
        Ok(clean.to_owned())
    }

    pub fn add_sticker(&self, unique_id: &str) -> Result<String, String> {
        let clean = unique_id.trim();
        if clean.is_empty() {
            return Err("贴纸 unique_id 不能为空".to_owned());
        }
        let doc = json!({ "unique_id": clean, "createdAt": date(now_ms()) });
        let id = self.mongo.insert(collections::STICKERS, doc)?;
        Ok(id_string(&id))
    }

    pub fn add_sticker_set(&self, set_id: &str, title: &str) -> Result<String, String> {
        let clean = set_id.trim();
        if clean.is_empty() {
            return Err("贴纸集 set_id 不能为空".to_owned());
        }
        let doc = json!({ "set_id": clean, "title": title, "createdAt": date(now_ms()) });
        let id = self.mongo.insert(collections::STICKER_SETS, doc)?;
        Ok(id_string(&id))
    }

    pub fn add_ai_fulltext(&self, text: &str) -> Result<(String, bool), String> {
        let clean = text.trim();
        if clean.is_empty() {
            return Err("文本不能为空".to_owned());
        }
        if let Ok(Some(existing)) = self
            .mongo
            .find_one(collections::AI_FULLTEXT, json!({ "text": clean }))
        {
            return Ok((doc_id(&existing), true));
        }
        let doc = json!({ "text": clean, "createdAt": date(now_ms()) });
        let id = self.mongo.insert(collections::AI_FULLTEXT, doc)?;
        Ok((id_string(&id), false))
    }

    // ── 黑名单：查 ────────────────────────────────────────────────

    /// `ads_blacklist` 的关键词组 AND 匹配（服务端聚合，照搬 TS 的管道）。
    pub fn match_ad_keywords(&self, text: &str) -> Option<Value> {
        if text.trim().is_empty() {
            return None;
        }
        let lower = text.to_lowercase();
        let pipeline = json!([
            { "$match": {
                "keywords": {
                    "$exists": true,
                    "$ne": [],
                    "$not": { "$elemMatch": { "$in": [null, ""] } }
                }
            }},
            { "$addFields": {
                "allMatched": {
                    "$allElementsTrue": {
                        "$map": {
                            "input": "$keywords",
                            "as": "keyword",
                            "in": {
                                "$and": [
                                    { "$ne": ["$$keyword", null] },
                                    { "$ne": ["$$keyword", ""] },
                                    { "$gt": [{ "$indexOfCP": [lower, { "$toLower": "$$keyword" }] }, -1] }
                                ]
                            }
                        }
                    }
                }
            }},
            { "$match": { "allMatched": true } },
            { "$limit": 1 }
        ]);
        match self.mongo.aggregate(collections::ADS, pipeline) {
            Ok(docs) => docs.into_iter().next(),
            Err(error) => {
                log::debug(&self.host, format!("查询 ads_blacklist 失败：{error}"));
                None
            }
        }
    }

    /// `username_blacklist`：文本里出现用户名（或 `@用户名`）。
    pub fn match_username(&self, text: &str) -> Option<Value> {
        if text.trim().is_empty() {
            return None;
        }
        let lower = text.to_lowercase();
        let pipeline = json!([
            { "$match": { "username": { "$exists": true, "$type": "string", "$nin": [null, ""] } } },
            { "$addFields": {
                "isMatched": {
                    "$or": [
                        { "$gt": [{ "$indexOfCP": [lower, { "$toLower": "$username" }] }, -1] },
                        { "$gt": [{ "$indexOfCP": [lower, { "$concat": ["@", { "$toLower": "$username" }] }] }, -1] }
                    ]
                }
            }},
            { "$match": { "isMatched": true } },
            { "$limit": 1 }
        ]);
        match self.mongo.aggregate(collections::USERNAME, pipeline) {
            Ok(docs) => docs.into_iter().next(),
            Err(error) => {
                log::debug(&self.host, format!("查询 username_blacklist 失败：{error}"));
                None
            }
        }
    }

    pub fn match_name(&self, name: &str) -> Option<Value> {
        let clean = name.trim();
        if clean.is_empty() {
            return None;
        }
        self.mongo
            .find_one(collections::NAME, json!({ "name": clean }))
            .ok()
            .flatten()
    }

    pub fn match_bio(&self, text: &str) -> Option<Value> {
        let clean = text.trim();
        if clean.is_empty() {
            return None;
        }
        self.mongo
            .find_one(
                collections::BIO,
                json!({ "$or": [{ "text": clean }, { "keywords": { "$in": [clean] } }] }),
            )
            .ok()
            .flatten()
    }

    pub fn match_ai_fulltext(&self, text: &str) -> Option<Value> {
        let clean = text.trim();
        if clean.is_empty() {
            return None;
        }
        self.mongo
            .find_one(collections::AI_FULLTEXT, json!({ "text": clean }))
            .ok()
            .flatten()
    }

    pub fn match_forward(&self, chat_id: i64) -> Option<Value> {
        self.mongo
            .find_one(collections::FORWARD, json!({ "chat_id": chat_id }))
            .ok()
            .flatten()
    }

    pub fn match_file_ids(&self, unique_ids: &[String]) -> Option<Value> {
        if unique_ids.is_empty() {
            return None;
        }
        self.mongo
            .find_one(
                collections::FILE_IDS,
                json!({ "unique_id": { "$in": unique_ids } }),
            )
            .ok()
            .flatten()
    }

    pub fn match_sticker_set(&self, set_id: &str) -> Option<Value> {
        self.mongo
            .find_one(collections::STICKER_SETS, json!({ "set_id": set_id }))
            .ok()
            .flatten()
    }

    pub fn match_sticker(&self, unique_id: &str) -> Option<Value> {
        self.mongo
            .find_one(collections::STICKERS, json!({ "unique_id": unique_id }))
            .ok()
            .flatten()
    }

    // ── 黑名单：改 / 删 / 查 ─────────────────────────────────────

    /// 按 `_id` 更新关键词（先 `ads_blacklist`、再 `bio_blacklist`）。
    pub fn update_keywords(
        &self,
        id: &str,
        keywords: &[String],
    ) -> Result<Option<Collection>, String> {
        if !crate::util::is_object_id(id) {
            return Err("无效的 _id 格式，需为 24 位十六进制字符串".to_owned());
        }
        let valid: Vec<String> = keywords
            .iter()
            .map(|keyword| keyword.trim().to_owned())
            .filter(|keyword| !keyword.is_empty())
            .collect();
        if valid.is_empty() {
            return Err("无有效关键词".to_owned());
        }
        let object_id = json!({ "$oid": id });
        for collection in [collections::ADS, collections::BIO] {
            let mut set = json!({ "keywords": valid });
            if collection == collections::BIO {
                set["text"] = if valid.len() == 1 {
                    Value::String(valid[0].clone())
                } else {
                    Value::Null
                };
            }
            let reply = self.mongo.update(
                collection,
                json!({ "_id": object_id }),
                json!({ "$set": set }),
                false,
                false,
            )?;
            if reply.get("matched").and_then(Value::as_u64).unwrap_or(0) > 0 {
                return Ok(Some(if collection == collections::ADS {
                    Collection::Ads
                } else {
                    Collection::Bio
                }));
            }
        }
        Ok(None)
    }

    /// 按 `_id`（或旧数据里的 chat_id / name）查一条黑名单记录。
    pub fn find_blacklist(&self, id: &str) -> Option<BlacklistEntry> {
        if id.is_empty() {
            return None;
        }
        // 24 位十六进制按 ObjectId 查（TS 写入的 `_id` 就是 ObjectId）；
        // 其余（老的手写 `_id`）按字符串查。
        let by_id = if crate::util::is_object_id(id) {
            json!({ "_id": { "$oid": id } })
        } else {
            json!({ "_id": id })
        };
        let collections = [
            (Collection::Ads, collections::ADS),
            (Collection::Bio, collections::BIO),
            (Collection::Forward, collections::FORWARD),
            (Collection::Name, collections::NAME),
            (Collection::Username, collections::USERNAME),
            (Collection::AiFullText, collections::AI_FULLTEXT),
            (Collection::File, collections::FILE_IDS),
            (Collection::Sticker, collections::STICKERS),
            (Collection::StickerSet, collections::STICKER_SETS),
        ];
        for (collection, name) in collections {
            if let Ok(Some(doc)) = self.mongo.find_one(name, by_id.clone()) {
                return Some(BlacklistEntry {
                    collection,
                    record: doc,
                });
            }
        }
        // 兜底：forward 按 chat_id、name 按 name（TS 版也是这么查的）。
        if let Ok(chat_id) = id.parse::<i64>()
            && let Ok(Some(doc)) = self
                .mongo
                .find_one(collections::FORWARD, json!({ "chat_id": chat_id }))
        {
            return Some(BlacklistEntry {
                collection: Collection::Forward,
                record: doc,
            });
        }
        if let Ok(Some(doc)) = self
            .mongo
            .find_one(collections::NAME, json!({ "name": id }))
        {
            return Some(BlacklistEntry {
                collection: Collection::Name,
                record: doc,
            });
        }
        None
    }

    pub fn delete_blacklist(&self, id: &str) -> Option<Collection> {
        let entry = self.find_blacklist(id)?;
        let filter = entry
            .record
            .get("_id")
            .cloned()
            .map(|id| json!({ "_id": id }))
            .or_else(|| {
                entry
                    .record
                    .get("chat_id")
                    .cloned()
                    .map(|id| json!({ "chat_id": id }))
            })?;
        match self.mongo.delete(entry.collection.name(), filter, false) {
            Ok(deleted) if deleted > 0 => Some(entry.collection),
            _ => None,
        }
    }

    // ── AI 授权群 / 全局触发词 ───────────────────────────────────

    pub fn add_ai_default_chat(&self, chat_id: i64, title: &str) -> bool {
        if let Ok(Some(_)) = self
            .mongo
            .find_one(collections::AI_DEFAULT_CHATS, json!({ "chat_id": chat_id }))
        {
            return false;
        }
        let doc = json!({ "chat_id": chat_id, "title": title, "createdAt": date(now_ms()) });
        self.mongo
            .insert(collections::AI_DEFAULT_CHATS, doc)
            .is_ok()
    }

    pub fn remove_ai_default_chat(&self, chat_id: i64) -> bool {
        self.mongo
            .delete(
                collections::AI_DEFAULT_CHATS,
                json!({ "chat_id": chat_id }),
                false,
            )
            .map(|deleted| deleted > 0)
            .unwrap_or(false)
    }

    pub fn is_ai_default_chat(&self, chat_id: i64) -> bool {
        self.mongo
            .find_one(collections::AI_DEFAULT_CHATS, json!({ "chat_id": chat_id }))
            .ok()
            .flatten()
            .is_some()
    }

    pub fn list_ai_default_chats(&self) -> Vec<(i64, String)> {
        self.mongo
            .find(
                collections::AI_DEFAULT_CHATS,
                json!({}),
                Some(json!({ "createdAt": 1 })),
                None,
            )
            .unwrap_or_default()
            .iter()
            .map(|doc| (i64_of(doc, "chat_id").unwrap_or(0), string_of(doc, "title")))
            .collect()
    }

    pub fn global_ai_keywords(&self) -> Vec<String> {
        self.mongo
            .find_one(
                collections::SETTINGS,
                json!({ "_id": "global_ai_keywords" }),
            )
            .ok()
            .flatten()
            .map(|doc| strings_of(&doc, "values"))
            .unwrap_or_default()
    }

    pub fn set_global_ai_keywords(&self, keywords: Vec<String>) {
        let cleaned: Vec<String> = keywords
            .into_iter()
            .map(|keyword| keyword.trim().to_owned())
            .filter(|keyword| !keyword.is_empty())
            .collect();
        let update = json!({ "$set": { "values": cleaned, "updatedAt": date(now_ms()) } });
        if let Err(error) = self.mongo.update(
            collections::SETTINGS,
            json!({ "_id": "global_ai_keywords" }),
            update,
            true,
            false,
        ) {
            log::warn(&self.host, format!("写全局 AI 关键词失败：{error}"));
        }
    }

    // ── 禁言复审 ──────────────────────────────────────────────────

    pub fn upsert_mute_review(
        &self,
        chat_id: i64,
        user_id: i64,
        message_id: Option<i64>,
        reason: &str,
        confidence: f64,
        deadline: i64,
    ) {
        let now = now_ms();
        let update = json!({
            "$set": {
                "message_id": message_id,
                "reason": reason,
                "confidence": confidence,
                "muted_at": date(now),
                "deadline": date(deadline),
                "status": "pending",
                "updated_at": date(now),
            },
            "$setOnInsert": { "chat_id": chat_id, "user_id": user_id },
        });
        if let Err(error) = self.mongo.update(
            collections::MUTE_REVIEWS,
            json!({ "chat_id": chat_id, "user_id": user_id }),
            update,
            true,
            false,
        ) {
            log::warn(&self.host, format!("写禁言复审记录失败：{error}"));
        }
    }

    pub fn mark_mute_review_approved(&self, chat_id: i64, user_id: i64) {
        let _ = self.mongo.update(
            collections::MUTE_REVIEWS,
            json!({ "chat_id": chat_id, "user_id": user_id, "status": "pending" }),
            json!({ "$set": { "status": "approved", "updated_at": date(now_ms()) } }),
            false,
            false,
        );
    }

    /// `pending → banned` 的原子转移；只有真的转移了才返回 `true`。
    pub fn mark_mute_review_banned(&self, chat_id: i64, user_id: i64) -> bool {
        self.mongo
            .update(
                collections::MUTE_REVIEWS,
                json!({ "chat_id": chat_id, "user_id": user_id, "status": "pending" }),
                json!({ "$set": { "status": "banned", "updated_at": date(now_ms()) } }),
                false,
                false,
            )
            .map(|reply| reply.get("modified").and_then(Value::as_u64).unwrap_or(0) > 0)
            .unwrap_or(false)
    }

    pub fn mute_review(&self, chat_id: i64, user_id: i64) -> Option<MuteReview> {
        let doc = self
            .mongo
            .find_one(
                collections::MUTE_REVIEWS,
                json!({ "chat_id": chat_id, "user_id": user_id }),
            )
            .ok()
            .flatten()?;
        Some(MuteReview {
            chat_id: i64_of(&doc, "chat_id").unwrap_or(0),
            user_id: i64_of(&doc, "user_id").unwrap_or(0),
            message_id: i64_of(&doc, "message_id"),
            reason: string_of(&doc, "reason"),
            confidence: doc.get("confidence").and_then(Value::as_f64).unwrap_or(0.0),
            muted_at: doc.get("muted_at").and_then(date_ms).unwrap_or(0),
            deadline: doc.get("deadline").and_then(date_ms).unwrap_or(0),
            status: string_of(&doc, "status"),
            updated_at: doc.get("updated_at").and_then(date_ms).unwrap_or(0),
        })
    }

    pub fn expired_mute_reviews(&self, now: i64) -> Vec<MuteReview> {
        let docs = self
            .mongo
            .find(
                collections::MUTE_REVIEWS,
                json!({ "status": "pending", "deadline": { "$lte": date(now) } }),
                None,
                None,
            )
            .unwrap_or_default();
        docs.iter()
            .map(|doc| MuteReview {
                chat_id: i64_of(doc, "chat_id").unwrap_or(0),
                user_id: i64_of(doc, "user_id").unwrap_or(0),
                message_id: i64_of(doc, "message_id"),
                reason: string_of(doc, "reason"),
                confidence: doc.get("confidence").and_then(Value::as_f64).unwrap_or(0.0),
                muted_at: doc.get("muted_at").and_then(date_ms).unwrap_or(0),
                deadline: doc.get("deadline").and_then(date_ms).unwrap_or(0),
                status: string_of(doc, "status"),
                updated_at: doc.get("updated_at").and_then(date_ms).unwrap_or(0),
            })
            .collect()
    }

    // ── 入群监视 ──────────────────────────────────────────────────

    pub fn upsert_watch(&self, chat_id: i64, user_id: i64, name: &str, bio: &str) {
        let now = now_ms();
        let update = json!({
            "$set": {
                "join_at": date(now),
                "expires_at": date(now + NEW_MEMBER_WATCH_MS),
                "join_name": name,
                "join_bio": bio,
                "profile_changed": false,
                "profile_rechecked": false,
                "last_recheck_at": null,
                "recheck_count": 0,
                "review_approved": false,
                "review_approved_at": null,
                "updated_at": date(now),
            },
            "$setOnInsert": { "chat_id": chat_id, "user_id": user_id, "created_at": date(now) },
        });
        let _ = self.mongo.update(
            collections::WATCHES,
            json!({ "chat_id": chat_id, "user_id": user_id }),
            update,
            true,
            false,
        );
    }

    pub fn active_watch(&self, chat_id: i64, user_id: i64) -> Option<NewMemberWatch> {
        self.mongo
            .find_one(
                collections::WATCHES,
                json!({
                    "chat_id": chat_id,
                    "user_id": user_id,
                    "expires_at": { "$gt": date(now_ms()) },
                }),
            )
            .ok()
            .flatten()
            .map(|doc| NewMemberWatch::from_doc(&doc))
    }

    pub fn has_any_active_watch(&self, user_id: i64) -> bool {
        self.mongo
            .find_one(
                collections::WATCHES,
                json!({ "user_id": user_id, "expires_at": { "$gt": date(now_ms()) } }),
            )
            .ok()
            .flatten()
            .is_some()
    }

    pub fn watches_by_user(&self, user_id: i64) -> Vec<NewMemberWatch> {
        self.mongo
            .find(
                collections::WATCHES,
                json!({ "user_id": user_id, "expires_at": { "$gt": date(now_ms()) } }),
                None,
                None,
            )
            .unwrap_or_default()
            .iter()
            .map(NewMemberWatch::from_doc)
            .collect()
    }

    pub fn mark_profile_changed(&self, chat_id: i64, user_id: i64, name: &str, bio: &str) {
        let _ = self.mongo.update(
            collections::WATCHES,
            json!({
                "chat_id": chat_id,
                "user_id": user_id,
                "expires_at": { "$gt": date(now_ms()) },
            }),
            json!({ "$set": {
                "profile_changed": true,
                "join_name": name,
                "join_bio": bio,
                "updated_at": date(now_ms()),
            }}),
            false,
            false,
        );
    }

    pub fn mark_profile_rechecked(&self, chat_id: i64, user_id: i64) {
        let now = now_ms();
        let _ = self.mongo.update(
            collections::WATCHES,
            json!({ "chat_id": chat_id, "user_id": user_id }),
            json!({
                "$set": {
                    "profile_rechecked": true,
                    "last_recheck_at": date(now),
                    "updated_at": date(now),
                },
                "$inc": { "recheck_count": 1 },
            }),
            false,
            false,
        );
    }

    pub fn mark_watch_review_approved(&self, chat_id: i64, user_id: i64) {
        let now = now_ms();
        let _ = self.mongo.update(
            collections::WATCHES,
            json!({ "chat_id": chat_id, "user_id": user_id }),
            json!({ "$set": {
                "review_approved": true,
                "review_approved_at": date(now),
                "profile_changed": false,
                "updated_at": date(now),
            }}),
            false,
            false,
        );
    }

    pub fn is_profile_changed_watch_user(&self, chat_id: i64, user_id: i64) -> bool {
        self.active_watch(chat_id, user_id)
            .is_some_and(|watch| watch.profile_changed)
    }

    // ── /spam 举报流水 ───────────────────────────────────────────

    pub fn log_spam_report(&self, chat_id: i64, reporter: i64, message: &Value) {
        let doc = json!({
            "chat_id": chat_id,
            "reporter_id": reporter,
            "message": message,
            "createdAt": date(now_ms()),
        });
        if let Err(error) = self.mongo.insert(collections::SPAM_LOG, doc) {
            log::debug(&self.host, format!("写 SPAM_Log 失败：{error}"));
        }
    }

    // ── 统计（启动日志） ─────────────────────────────────────────

    pub fn counts(&self) -> Vec<(&'static str, u64)> {
        let specs: &[(&str, &str)] = &[
            (collections::CONFIG, "adb_config"),
            (collections::BAN_RECORDS, "ban_records"),
            (collections::MESSAGE_RECORDS, "message_records"),
            (collections::ADS, "ads_blacklist"),
            (collections::AI_FULLTEXT, "ai_fulltext_blacklist"),
            (collections::WATCHES, "new_member_watch_records"),
            (collections::MUTE_REVIEWS, "mute_review_records"),
        ];
        specs
            .iter()
            .filter_map(|(collection, label)| {
                self.mongo
                    .count(collection, json!({}))
                    .ok()
                    .map(|count| (*label, count))
            })
            .collect()
    }
}

/// 文档的 `_id` → 字符串（ObjectId 的 `{"$oid":…}` / 普通字符串都认）。
pub fn doc_id(doc: &Value) -> String {
    match doc.get("_id") {
        Some(Value::String(text)) => text.clone(),
        Some(value) => id_string(value),
        None => String::new(),
    }
}

/// `insert` 返回的 id（`{"$oid": …}` 或字符串）→ 裸字符串。
fn id_string(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Object(object) => object
            .get("$oid")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        _ => String::new(),
    }
}

/// 给一个「没有 `_id`」的新文档用（`SPAM_Log` 之类由 Mongo 自己生成 `_id`）。
#[allow(dead_code)]
fn fresh_id() -> String {
    new_id()
}

// ── 测试 ──────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn date_round_trip() {
        let ms = 1_767_225_600_000;
        let encoded = date(ms);
        assert_eq!(date_ms(&encoded), Some(ms));
        // TS 写入的 relaxed 形态
        assert_eq!(date_ms(&json!({"$date": "2026-01-01T00:00:00Z"})), Some(ms));
        // Extended JSON v1
        assert_eq!(date_ms(&json!({"$date": ms})), Some(ms));
    }

    #[test]
    fn config_parses_legacy_fields() {
        let doc = json!({
            "chat_id": -100,
            "type": "group",
            "limit_mode": "ban",
            "Whitelist": [1, 2],
            "spam_fileId": ["a"],
            "spam_sticker": ["b"],
            "spam_tips": false,
            "spam_tips_meg_delay": 5,
            "ai_review_enabled": true,
            "check_new_member": true,
            "ai_api_key": "k",
            "ai_base_url": "https://x",
            "ai_model": "m"
        });
        let config = GroupConfig::from_doc(&doc);
        assert_eq!(config.kind, "group");
        assert_eq!(config.limit_mode, LimitMode::Ban);
        assert_eq!(config.whitelist, vec![1, 2]);
        assert_eq!(config.spam_file_id, vec!["a"]);
        assert!(!config.spam_tips);
        assert_eq!(config.spam_tips_meg_delay, 5);
        assert!(config.ai_review_enabled);
        assert!(config.check_new_member);
        assert!(config.has_custom_ai());
    }

    #[test]
    fn config_defaults_when_fields_missing() {
        let config = GroupConfig::from_doc(&json!({"chat_id": 1}));
        assert!(config.spam_tips);
        assert_eq!(config.spam_tips_meg_delay, 30);
        assert_eq!(config.limit_mode, LimitMode::Mute);
        assert!(!config.ai_review_enabled);
    }

    #[test]
    fn doc_id_reads_object_id() {
        assert_eq!(
            doc_id(&json!({"_id": {"$oid": "507f1f77bcf86cd799439011"}})),
            "507f1f77bcf86cd799439011"
        );
        assert_eq!(doc_id(&json!({"_id": "abc"})), "abc");
    }

    #[test]
    fn watch_doc_parses() {
        let doc = json!({
            "chat_id": -100,
            "user_id": 7,
            "join_at": {"$date": "2026-01-01T00:00:00Z"},
            "expires_at": {"$date": "2026-01-02T00:00:00Z"},
            "join_name": "n",
            "join_bio": "b",
            "profile_changed": true,
            "recheck_count": 2
        });
        let watch = NewMemberWatch::from_doc(&doc);
        assert_eq!(watch.user_id, 7);
        assert!(watch.profile_changed);
        assert_eq!(watch.recheck_count, 2);
        assert!(watch.expires_at > watch.join_at);
    }
}
