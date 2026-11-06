//! 插件级配置：`[plugin.fuyubot.adblock]` 那一段。
//!
//! TS 版把这些写死在代码里（Bot 主人的判断靠框架的 permission，总日志频道 / AI Key
//! 是硬编码常量）。这里全部改成配置项，理由有三个：
//!
//! 1. **密钥不进源码**——TS 版里那两个 `sk-…` 已经提交进仓库了，重构时不再复制；
//! 2. **总日志频道是可选的**——原代码把日志发到某个固定频道的固定话题，
//!    换个人部署就是往别人群里灌消息；
//! 3. **FuyuBot 没有「Bot 主人」概念**——`owner_ids` 是这张名单的唯一来源。
//!
//! ```toml
//! [plugin.fuyubot.adblock]                  # 只有这一个段，全部平铺
//! owner_ids = [123456789]                   # /qadid /delad /addkw /adkew /adaiset … 的授权人
//! admin_ids = [987654321]                   # TS 里 permission=admin 那一档
//! db = "fuyubot"                            # 覆盖 storage.mongo 的默认库
//! log_channel = -1001234567890              # 可选：全局封禁审计频道
//! log_topic = 1622                          # 可选：论坛群的话题 id
//! ai_api_key = "sk-…"                       # 可选：内置默认 AI 接口
//! ai_base_url = "https://api.openai.com/v1"
//! ai_model = "gpt-4o-mini"
//! ai_log_channel = -1001234567890           # 可选：AI 审查总日志
//! ai_log_topic = 11740
//! ai_db_channel = -1001234567890            # 可选：/spam 判定后写库的通知
//! ai_db_topic = 3034
//! ```

use std::collections::HashSet;

use fuyubot_abi::HostApi;
use serde_json::Value;

use crate::log;

/// 插件名（`define_plugin!` 里那个），也是配置段名。
pub const PLUGIN_NAME: &str = "fuyubot.adblock";
pub const CONFIG_SECTION: &str = PLUGIN_NAME;

/// 默认 AI 接口（OpenAI 兼容）。`api_key` 为空 = 没有默认接口。
#[derive(Debug, Clone)]
pub struct AiEndpoint {
    pub api_key: String,
    pub base_url: String,
    pub model: String,
}

impl AiEndpoint {
    pub fn is_usable(&self) -> bool {
        !self.api_key.is_empty() && !self.base_url.is_empty()
    }
}

/// 插件启动时读一次的配置。
#[derive(Debug, Clone, Default)]
pub struct PluginConfig {
    /// 能执行「仅 Bot 主人」命令的 TGID 名单。
    pub owner_ids: HashSet<i64>,
    /// 「Bot 管理员」名单：对应 TS 框架 `permission:"admin"` 的那一档
    /// （`super_admin` 或 `admin[]`）。主人天然也算管理员。
    ///
    /// 影响 `/qadid` `/delad` `/addkw` `/editkw` `/addfwdbl` `/rmfwdbl`。
    pub admin_ids: HashSet<i64>,
    /// 覆盖 `storage.mongo` 的默认库（`None` = 用 mongo 插件的 `db`）。
    ///
    /// 典型用法：把 adblock 的数据放进跟 TS 版同一个库，老数据直接接上。
    pub db: Option<String>,
    /// 全局封禁审计频道（对应 TS 版写死的 `-1003040986800`）。
    pub log_channel: Option<i64>,
    pub log_topic: Option<i32>,
    /// 内置默认 AI 接口（群没有自定义配置时用它）。
    pub ai: Option<AiEndpoint>,
    /// AI 审查总日志（对应 TS 版的 topic 11740）。
    pub ai_log_channel: Option<i64>,
    pub ai_log_topic: Option<i32>,
    /// `/spam` 判定后写库的通知（对应 TS 版的 topic 3034）。
    pub ai_db_channel: Option<i64>,
    pub ai_db_topic: Option<i32>,
}

impl PluginConfig {
    /// 从宿主配置树读。
    ///
    /// 读不到任何一项不是错误：插件照常加载，只是「仅主人」命令与 AI 默认接口不可用；
    /// 这种情况下命令会自己给出可操作的提示（见 `commands::require_owner`）。
    pub fn load(host: &HostApi) -> Self {
        let mut config = Self::default();

        if let Some(value) = get(host, "owner_ids") {
            config.owner_ids = match value {
                Value::Array(items) => items.iter().filter_map(as_i64).collect(),
                other => as_i64(&other).into_iter().collect(),
            };
        }
        // 单数写法也认：`owner_id = 123`
        if let Some(value) = get(host, "owner_id")
            && let Some(id) = as_i64(&value)
        {
            config.owner_ids.insert(id);
        }

        // `/help` 里 `permission:"admin"` 那一档（TS 的 `admin[]`）。
        if let Some(value) = get(host, "admin_ids") {
            config.admin_ids = match value {
                Value::Array(items) => items.iter().filter_map(as_i64).collect(),
                other => as_i64(&other).into_iter().collect(),
            };
        }
        if let Some(value) = get(host, "admin_id")
            && let Some(id) = as_i64(&value)
        {
            config.admin_ids.insert(id);
        }

        config.log_channel = channel_of(get(host, "log_channel"));
        config.db = get(host, "db")
            .as_ref()
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|db| !db.is_empty())
            .map(str::to_owned);
        config.log_topic = topic_of(get(host, "log_topic"));

        // 内置默认 AI 接口：三项平铺（不再有 `.ai` 子表）。
        let api_key = string_of(host, "ai_api_key").unwrap_or_default();
        let base_url = string_of(host, "ai_base_url").unwrap_or_default();
        let model = string_of(host, "ai_model").unwrap_or_else(|| "gpt-4o-mini".to_owned());
        if !api_key.is_empty() && !base_url.is_empty() {
            config.ai = Some(AiEndpoint {
                api_key,
                base_url: base_url.trim_end_matches('/').to_owned(),
                model,
            });
        }

        config.ai_log_channel = channel_of(get(host, "ai_log_channel"));
        config.ai_log_topic = topic_of(get(host, "ai_log_topic"));
        config.ai_db_channel = channel_of(get(host, "ai_db_channel"));
        config.ai_db_topic = topic_of(get(host, "ai_db_topic"));

        config
    }

    pub fn is_owner(&self, user_id: i64) -> bool {
        self.owner_ids.contains(&user_id)
    }

    /// TS 框架 `permission:"admin"` 的等价判断：主人 ∪ `admin_ids`。
    pub fn is_bot_admin(&self, user_id: i64) -> bool {
        self.is_owner(user_id) || self.admin_ids.contains(&user_id)
    }

    /// 启动时打一行，把「哪些能力没开」说清楚——比事后猜为什么 AI 不工作省事。
    pub fn summary(&self) -> String {
        let ai = match &self.ai {
            Some(endpoint) => format!("默认 AI 接口 {} / {}", endpoint.base_url, endpoint.model),
            None => "无默认 AI 接口（只用各群自定义配置）".to_owned(),
        };
        let audit = match self.log_channel {
            Some(channel) => format!("全局审计频道 {channel}"),
            None => "无全局审计频道".to_owned(),
        };
        let db = match &self.db {
            Some(db) => format!("库 {db}"),
            None => "用 storage.mongo 的默认库".to_owned(),
        };
        format!(
            "主人 {} 人 / 管理员 {} 人；{db}；{ai}；{audit}",
            self.owner_ids.len(),
            self.admin_ids.len()
        )
    }
}

fn get(host: &HostApi, key: &str) -> Option<Value> {
    let raw = host.get_config(CONFIG_SECTION, key)?;
    match serde_json::from_str::<Value>(&raw) {
        Ok(value) => Some(value),
        Err(error) => {
            log::warnf(
                host,
                format_args!("配置项 {key} 不是合法 JSON（{error}），已忽略"),
            );
            None
        }
    }
}

fn as_i64(value: &Value) -> Option<i64> {
    match value {
        Value::Number(number) => number.as_i64(),
        Value::String(text) => text.parse().ok(),
        _ => None,
    }
}

/// 取一个字符串项（去首尾空白；空串当没配）。
fn string_of(host: &HostApi, key: &str) -> Option<String> {
    get(host, key)
        .as_ref()
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_owned)
}

/// 频道 id（数字或数字字符串都认）。
fn channel_of(value: Option<Value>) -> Option<i64> {
    value.as_ref().and_then(as_i64)
}

/// 论坛话题 id（必须落在 i32 里）。
fn topic_of(value: Option<Value>) -> Option<i32> {
    channel_of(value).and_then(|topic| i32::try_from(topic).ok())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn owner_list_parsing() {
        let list = json!([1, "2", 3]);
        let ids: HashSet<i64> = list
            .as_array()
            .expect("数组")
            .iter()
            .filter_map(as_i64)
            .collect();
        assert_eq!(ids, HashSet::from([1, 2, 3]));
    }

    #[test]
    fn channel_and_topic() {
        assert_eq!(channel_of(Some(json!(-100))), Some(-100));
        assert_eq!(channel_of(Some(json!("-100"))), Some(-100));
        assert_eq!(channel_of(None), None);
        assert_eq!(topic_of(Some(json!(42))), Some(42));
        // 超出 i32 的话题 id 当没配（TDLib 的 topic id 是 i32）。
        assert_eq!(topic_of(Some(json!(9_999_999_999_i64))), None);
    }
}
