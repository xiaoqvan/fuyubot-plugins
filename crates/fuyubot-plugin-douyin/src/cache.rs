//! 同步 SQL 缓存（**软依赖** `fuyubot.sqlite`）。
//!
//! 这里没有编译期的 SQLite 依赖：表能不能用完全取决于宿主里装没装 SQLite 插件。
//! 没有它、或者建表失败时，[`Cache`] 安静地退回「永远未命中 / 写入直接丢掉」，
//! **绝不 panic、绝不影响插件启动**——缓存只是加速，不是正确性的一部分。
//!
//! [`Cache`] 的每个方法都是**同步阻塞**的：SQLite 服务每个库一把锁，慢查询要等
//! 几毫秒到几秒。所以在 tokio 任务里调用时要么包一层
//! `tokio::task::spawn_blocking`，要么放在插件自己的工作线程上。
//!
//! 两条协议上的硬规矩：
//!
//! - 值永远走 `params` 绑定，**不拼进 SQL 文本**；
//! - SQL 文本里的表名只能用 `migrate` 回复里 `applied[0].table` 给的那个
//!   （`shared` 模式下服务会加 `<plugin>_` 前缀，硬编码 `media_cache` 会找不到表）。

use std::sync::{Arc, Mutex, MutexGuard};

use fuyubot_abi::HostApi;
use serde_json::{Value, json};

/// SQLite 服务的名字（与插件 id 相同）。
pub const SERVICE: &str = "fuyubot.sqlite";

/// 调用方插件名：服务用它选库（`per_plugin`）或做表前缀（`shared`）。
///
/// ABI v6 起调用者身份由宿主判定（服务方用 `current_plugin()` 取）。这里仍然
/// **显式声明自己是谁**：sqlite 服务会拿它和真实调用者比对（冒用别人的名字返回
/// `caller_mismatch`），宿主自己调的时候也靠这个字段选库。v5 时代它是唯一来源。
pub const PLUGIN: &str = "fuyubot.douyin";

/// `migrate` 要用的模板：`document` 就是 `key` PK / `doc` TEXT / `updated_at`。
const TEMPLATE: &str = "document";

/// 希望的默认表名。`per_plugin` 模式原样使用，`shared` 模式服务会加前缀，
/// 所以真实表名一律以 `migrate` 回复为准。
const DEFAULT_TABLE: &str = "media_cache";

// ── 缓存本体 ────────────────────────────────────────────────────────

/// 宿主里有没有这个名字的服务（ABI v6：用 `list_services` 判可用性）。
///
/// v6 把服务调用收到宿主那条 `call_service` 桥上，插件不必、也不该再
/// `get_service` 拿别人的裸 vtable——直接调函数指针会绕过宿主，提供方就
/// 看不到调用者身份。查不到清单 / 回复不是 JSON 时一律按「没有」处理：
/// 缓存只是加速，不能因为探测失败影响插件启动。
fn service_available(host: &HostApi, name: &str) -> bool {
    let Ok(items) = serde_json::from_str::<Value>(&host.list_services()) else {
        return false;
    };
    items.as_array().is_some_and(|items| {
        items
            .iter()
            .any(|item| item.get("name").and_then(Value::as_str) == Some(name))
    })
}

/// 同步的 SQL 缓存。
///
/// [`Clone`] 很廉价：内部只有宿主句柄（`Copy`）和一份 `Arc` 共享的表名状态。
/// 克隆出来的副本共享「是否可用」——一个副本 `init()` 成功，其它副本立刻可用。
#[derive(Clone)]
pub struct Cache {
    host: HostApi,
    /// `Some(实际表名)` = 已就绪；`None` = 还没 `init()` 或 `init()` 失败。
    table: Arc<Mutex<Option<String>>>,
}

impl Cache {
    pub fn new(host: HostApi) -> Self {
        Self {
            host,
            table: Arc::new(Mutex::new(None)),
        }
    }

    /// 探测服务 + 幂等建表。返回是否可用（并置位内部状态）。
    ///
    /// 服务缺失只记 Debug（没装插件是正常配置，不该用 Warn 打扰用户）；
    /// 建表被拒 / 调用失败记 Warn 并返回 `false`。重复调用安全：已可用直接 `true`。
    pub fn init(&self) -> bool {
        if self.available() {
            return true;
        }
        if !service_available(&self.host, SERVICE) {
            crate::log::debug(&format!("宿主里没有 {SERVICE}，SQL 缓存禁用"));
            return false;
        }
        let reply = match self
            .host
            .call_service(SERVICE, "migrate", &migrate_request())
        {
            Ok(reply) => reply,
            Err(error) => {
                crate::log::warn(&format!("SQL 缓存建表失败（{SERVICE}/migrate）：{error}"));
                return false;
            }
        };
        let Some(table) = parse_table(&reply) else {
            // `shared` 模式的表名只有服务知道；拿不到就干脆别用。
            let reason =
                failure(&reply).unwrap_or_else(|| "回复里没有 applied[0].table".to_owned());
            crate::log::warn(&format!("SQL 缓存建表被拒：{reason}"));
            return false;
        };
        *self.table_name() = Some(table.clone());
        crate::log::debug(&format!("SQL 缓存表就绪：{table}"));
        true
    }

    /// `init()` 成功过才 `true`。
    pub fn available(&self) -> bool {
        self.table_name().is_some()
    }

    /// 读一个键。未命中 / 服务不可用 / 任何失败都返回 `None`（失败只 warn / debug）。
    pub fn get(&self, key: &str) -> Option<Value> {
        let table = self.table_name().clone()?;
        let reply = match self
            .host
            .call_service(SERVICE, "query", &query_request(&table, key))
        {
            Ok(reply) => reply,
            Err(error) => {
                crate::log::warn(&format!("SQL 缓存读取失败（{SERVICE}/query）：{error}"));
                return None;
            }
        };
        if let Some(reason) = failure(&reply) {
            crate::log::warn(&format!("SQL 缓存读取被拒：{reason}"));
            return None;
        }
        let doc = parse_doc(&reply);
        if doc.is_none() && parse_row_text(&reply).is_some() {
            // 行里有内容但反序列化不了：当未命中处理，别把坏数据当缓存喂给业务。
            crate::log::debug(&format!("缓存键 {key} 的文档不是合法 JSON，按未命中处理"));
        }
        doc
    }

    /// upsert 一个键。服务不可用 / 失败只 warn，调用方照常往下走。
    pub fn put(&self, key: &str, doc: &Value) {
        let Some(table) = self.table_name().clone() else {
            return;
        };
        match self
            .host
            .call_service(SERVICE, "execute", &upsert_request(&table, key, doc))
        {
            Ok(reply) => {
                if let Some(reason) = failure(&reply) {
                    crate::log::warn(&format!("SQL 缓存写入被拒：{reason}"));
                }
            }
            Err(error) => {
                crate::log::warn(&format!("SQL 缓存写入失败（{SERVICE}/execute）：{error}"));
            }
        }
    }

    /// 删除一个键（缓存条目失效时用）。失败只 warn。
    pub fn remove(&self, key: &str) {
        let Some(table) = self.table_name().clone() else {
            return;
        };
        match self
            .host
            .call_service(SERVICE, "execute", &delete_request(&table, key))
        {
            Ok(reply) => {
                if let Some(reason) = failure(&reply) {
                    crate::log::warn(&format!("SQL 缓存删除被拒：{reason}"));
                }
            }
            Err(error) => {
                crate::log::warn(&format!("SQL 缓存删除失败（{SERVICE}/execute）：{error}"));
            }
        }
    }

    /// 拿表名锁。中毒也继续用：缓存不是正确性关键，别因为一次 panic 永久禁用。
    fn table_name(&self) -> MutexGuard<'_, Option<String>> {
        self.table
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }
}

// ── 缓存键 ──────────────────────────────────────────────────────────

/// 把片段拼成缓存键：片段之间用 `:`，片段里除 `[A-Za-z0-9_.-]` 的字符一律换成 `_`。
///
/// 规则是固定的（不是启发式）：一个非法字符换一个 `_`、不合并；空片段保留原位。
/// 因为 `:` 自己一定会被替换掉，所以键的切分永远无歧义。
///
/// ```
/// # use fuyubot_plugin_douyin::cache::key;
/// assert_eq!(key(&["netease", "186016", "lossless"]), "netease:186016:lossless");
/// assert_eq!(key(&["网易云", "186016"]), "___:186016");
/// ```
pub fn key(parts: &[&str]) -> String {
    let mut out = String::new();
    for (index, part) in parts.iter().enumerate() {
        if index > 0 {
            out.push(':');
        }
        for ch in part.chars() {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
                out.push(ch);
            } else {
                out.push('_');
            }
        }
    }
    out
}

// ── 请求构造（纯函数，单测直接断言形状）─────────────────────────────

/// `migrate` 请求：建 `document` 模板表。表名只是**建议**，实际名字看回复。
fn migrate_request() -> String {
    json!({
        "plugin": PLUGIN,
        "tables": [{"template": TEMPLATE, "table": DEFAULT_TABLE}],
    })
    .to_string()
}

/// `query` 请求：按主键取一行，键走 `?1` 绑定。
fn query_request(table: &str, key: &str) -> String {
    json!({
        "plugin": PLUGIN,
        "sql": format!("SELECT doc FROM {table} WHERE key = ?1"),
        "params": [key],
        "limit": 1,
    })
    .to_string()
}

/// `execute` 请求：upsert。文档序列化成 JSON 文本走 `?2`，绝不拼进 SQL。
fn upsert_request(table: &str, key: &str, doc: &Value) -> String {
    json!({
        "plugin": PLUGIN,
        "sql": format!(
            "INSERT INTO {table} (key, doc, updated_at) VALUES (?1, ?2, strftime('%s','now')) \
             ON CONFLICT(key) DO UPDATE SET doc = excluded.doc, updated_at = excluded.updated_at"
        ),
        "params": [key, doc.to_string()],
    })
    .to_string()
}

/// `execute` 请求：按主键删除。
fn delete_request(table: &str, key: &str) -> String {
    json!({
        "plugin": PLUGIN,
        "sql": format!("DELETE FROM {table} WHERE key = ?1"),
        "params": [key],
    })
    .to_string()
}

// ── 回复解析（纯函数）──────────────────────────────────────────────

/// 从 `migrate` 回复里取**实际**表名（`shared` 模式带前缀，不能硬编码）。
fn parse_table(reply: &str) -> Option<String> {
    let value: Value = serde_json::from_str(reply).ok()?;
    if !value.get("ok")?.as_bool()? {
        return None;
    }
    let table = value
        .get("applied")?
        .as_array()?
        .first()?
        .get("table")?
        .as_str()?;
    if table.is_empty() {
        None
    } else {
        Some(table.to_owned())
    }
}

/// `query` 回复里第一行第一列（`rows[0][0]`）的文本；未命中 / 不是字符串 → `None`。
fn parse_row_text(reply: &str) -> Option<String> {
    let value: Value = serde_json::from_str(reply).ok()?;
    if !value.get("ok")?.as_bool()? {
        return None;
    }
    let cell = value
        .get("rows")?
        .as_array()?
        .first()?
        .as_array()?
        .first()?;
    cell.as_str().map(str::to_owned)
}

/// `query` 回复 → 文档。没有这一行、或列里的文本不是合法 JSON，都返回 `None`。
fn parse_doc(reply: &str) -> Option<Value> {
    serde_json::from_str(&parse_row_text(reply)?).ok()
}

/// 回复是失败时返回「code: message」（连 JSON 都不是就说明原因），成功返回 `None`。
fn failure(reply: &str) -> Option<String> {
    let Ok(value) = serde_json::from_str::<Value>(reply) else {
        return Some(format!(
            "回复不是 JSON：{}",
            crate::log::shorten(reply, 200)
        ));
    };
    if value.get("ok").and_then(Value::as_bool) == Some(true) {
        return None;
    }
    let error = value.get("error");
    let code = error
        .and_then(|error| error.get("code"))
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let message = error
        .and_then(|error| error.get("message"))
        .and_then(Value::as_str)
        .unwrap_or("（没有 message）");
    Some(format!("{code}: {message}"))
}

// ── 单测：只测纯函数，不构造假 HostApi ──────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// 把请求 JSON 解析回 `Value`，后续按字段断言（不依赖键的序列化顺序）。
    fn request_value(request: &str) -> Value {
        serde_json::from_str(request).expect("请求必须是 JSON")
    }

    #[test]
    fn key_joins_parts_with_colon() {
        assert_eq!(
            key(&["netease", "186016", "lossless"]),
            "netease:186016:lossless"
        );
        assert_eq!(key(&["douyin", "7304"]), "douyin:7304");
        assert_eq!(key(&[]), "");
    }

    #[test]
    fn key_sanitizes_separator_whitespace_and_non_ascii() {
        // `:` 是分隔符，片段里出现必须替换，否则键的切分会歧义。
        assert_eq!(key(&["a:b", "c"]), "a_b:c");
        // 空白与控制字符。
        assert_eq!(key(&["a b", "c"]), "a_b:c");
        assert_eq!(key(&["a\tb"]), "a_b");
        // 非 ASCII：一个字符换一个 `_`。
        assert_eq!(key(&["周杰伦"]), "___");
        // 允许的字符原样保留。
        assert_eq!(key(&["a-b_c.d"]), "a-b_c.d");
        // 空片段保留原位，不合并分隔符。
        assert_eq!(key(&["", "x"]), ":x");
    }

    #[test]
    fn migrate_request_declares_document_table() {
        let request = request_value(&migrate_request());
        assert_eq!(request["plugin"], PLUGIN);
        assert_eq!(request["tables"][0]["template"], "document");
        assert_eq!(request["tables"][0]["table"], "media_cache");
    }

    #[test]
    fn parse_table_takes_actual_name_from_reply() {
        // per_plugin：服务原样返回。
        assert_eq!(
            parse_table(
                r#"{"ok":true,"applied":[{"template":"document","table":"media_cache","created":true,"statements":1}]}"#
            ),
            Some("media_cache".to_owned())
        );
        // shared：服务加了前缀，必须用返回值，不能硬编码。
        assert_eq!(
            parse_table(
                r#"{"ok":true,"applied":[{"template":"document","table":"fuyubot_music_media_cache","created":true,"statements":1}]}"#
            ),
            Some("fuyubot_music_media_cache".to_owned())
        );
    }

    #[test]
    fn parse_table_rejects_failure_and_malformed_replies() {
        let denied = r#"{"ok":false,"error":{"code":"denied_sql","message":"只读库"}}"#;
        assert_eq!(parse_table(denied), None);
        assert_eq!(parse_table("not json"), None);
        assert_eq!(parse_table(r#"{"ok":true,"applied":[]}"#), None);
        assert_eq!(parse_table(r#"{"ok":true,"applied":[{"table":""}]}"#), None);

        // 失败回复要能还原出 code / message，日志里才有可读的原因。
        let reason = failure(denied).expect("失败回复必须能解析出原因");
        assert!(reason.contains("denied_sql"), "{reason}");
        assert!(reason.contains("只读库"), "{reason}");
        assert_eq!(failure(r#"{"ok":true,"applied":[]}"#), None);
        assert!(failure("not json").is_some());
    }

    #[test]
    fn query_request_binds_key_as_param() {
        let request = request_value(&query_request("media_cache", "netease:186016:lossless"));
        assert_eq!(request["plugin"], PLUGIN);
        assert_eq!(request["sql"], "SELECT doc FROM media_cache WHERE key = ?1");
        assert_eq!(request["params"], json!(["netease:186016:lossless"]));
        assert_eq!(request["limit"], 1);
    }

    #[test]
    fn parse_doc_reads_first_cell() {
        let reply = r#"{"ok":true,"columns":["doc"],"rows":[["{\"title\":\"x\"}"]]}"#;
        let doc = parse_doc(reply).expect("命中");
        assert_eq!(doc["title"], "x");
    }

    #[test]
    fn parse_doc_treats_miss_and_broken_json_as_none() {
        assert_eq!(
            parse_doc(r#"{"ok":true,"columns":["doc"],"rows":[]}"#),
            None
        );
        assert_eq!(
            parse_doc(r#"{"ok":true,"columns":["doc"],"rows":[["不是 JSON"]]}"#),
            None
        );
        assert_eq!(
            parse_doc(r#"{"ok":false,"error":{"code":"denied_sql","message":"x"}}"#),
            None
        );
        assert_eq!(parse_doc("not json"), None);
    }

    #[test]
    fn upsert_request_uses_conflict_clause_and_params() {
        let doc = json!({"title": "晴天", "quality": "lossless"});
        let request = request_value(&upsert_request(
            "media_cache",
            "netease:186016:lossless",
            &doc,
        ));
        assert_eq!(request["plugin"], PLUGIN);

        let sql = request["sql"].as_str().expect("sql 是字符串");
        assert!(
            sql.starts_with(
                "INSERT INTO media_cache (key, doc, updated_at) VALUES (?1, ?2, strftime('%s','now'))"
            ),
            "{sql}"
        );
        assert!(
            sql.contains("ON CONFLICT(key) DO UPDATE SET doc = excluded.doc"),
            "{sql}"
        );
        assert!(sql.contains("updated_at = excluded.updated_at"), "{sql}");

        assert_eq!(request["params"][0], "netease:186016:lossless");
        // 第二个参数是文档的 JSON **文本**——值永远不拼进 SQL。
        let stored: Value =
            serde_json::from_str(request["params"][1].as_str().expect("doc 参数是字符串"))
                .expect("doc 参数是 JSON 文本");
        assert_eq!(stored, doc);
    }

    #[test]
    fn delete_request_binds_key_as_param() {
        let request = request_value(&delete_request("media_cache", "douyin:7304"));
        assert_eq!(request["plugin"], PLUGIN);
        assert_eq!(request["sql"], "DELETE FROM media_cache WHERE key = ?1");
        assert_eq!(request["params"], json!(["douyin:7304"]));
        assert!(request.get("limit").is_none());
    }
}
