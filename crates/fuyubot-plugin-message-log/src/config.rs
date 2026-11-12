//! 插件配置：`[plugin.fuyubot.message-log]` 那一段。
//!
//! 读的是**自己那段**（`get_config` 传空 section，由宿主换算成
//! `plugin.<本插件名>`），所以插件 id 里带点、带连字符都不用自己拼路径。
//!
//! 所有字段都可省略；写错类型 / 坏 TOML 不会让插件起不来，只会退回默认值并打一条
//! warn——配置是给人改的，不值得为它拒绝加载。
//!
//! ```toml
//! [plugin.fuyubot.message-log]
//! enabled = true          # false = 不订阅、什么都不打
//! ignore_outgoing = false # true = 不记录本账号自己发出的消息
//! max_text_chars = 4096   # 正文最多保留多少个字符；0 = 不截断
//! ```

use fuyubot_abi::HostApi;
use serde::Deserialize;

use crate::log;

/// 插件名（`define_plugin!` 里那个，也是 `[plugin.<名>]` 的段名）。
pub const PLUGIN_NAME: &str = "fuyubot.message-log";

/// 正文默认上限：Telegram 单条文本消息的上限是 4096 个 UTF-16 码元，
/// 正常消息不会被截；只有富消息 / 超长 caption 才可能触发。
pub const DEFAULT_MAX_TEXT_CHARS: usize = 4096;

/// 解析后的配置。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    /// 总开关。`false` 时插件照常加载，但不订阅 `updateNewMessage`。
    pub enabled: bool,
    /// 是否跳过 `is_outgoing = true` 的消息（本账号自己发的）。
    ///
    /// 默认 `false`：与 TS 版 `plugins/message_log.ts` 一致，发出去的也记。
    pub ignore_outgoing: bool,
    /// 正文最多保留多少个字符；`0` = 不截断。
    pub max_text_chars: usize,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            enabled: true,
            ignore_outgoing: false,
            max_text_chars: DEFAULT_MAX_TEXT_CHARS,
        }
    }
}

/// 配置文件的原始形状：全部 `Option`，缺省与写错都不影响启动。
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct Raw {
    enabled: Option<bool>,
    ignore_outgoing: Option<bool>,
    max_text_chars: Option<u64>,
}

impl Config {
    /// 从宿主读本插件的配置段；没有配置就用默认值。
    pub fn from_host(host: &HostApi) -> Self {
        // 空 section = 读自己那段（ABI 文档约定的默认值）。
        let raw: Raw = match host.get_config("", "") {
            Some(text) if !text.trim().is_empty() => match serde_json::from_str(&text) {
                Ok(raw) => raw,
                Err(error) => {
                    log::warn(
                        host,
                        format!("[plugin.{PLUGIN_NAME}] 配置解析失败（{error}），改用默认值"),
                    );
                    Raw::default()
                }
            },
            _ => Raw::default(),
        };
        Self::from_raw(raw)
    }

    fn from_raw(raw: Raw) -> Self {
        let mut config = Self::default();
        if let Some(value) = raw.enabled {
            config.enabled = value;
        }
        if let Some(value) = raw.ignore_outgoing {
            config.ignore_outgoing = value;
        }
        if let Some(value) = raw.max_text_chars {
            // 32 位平台（Android）上 u64 可能装不进 usize；钳到平台上限即可。
            config.max_text_chars = value.min(usize::MAX as u64) as usize;
        }
        config
    }

    /// 启动日志里的一行摘要。
    pub fn summary(&self) -> String {
        let limit = if self.max_text_chars == 0 {
            "不截断".to_owned()
        } else {
            format!("{} 字符", self.max_text_chars)
        };
        format!(
            "记录自己发的消息={}，正文上限={limit}",
            !self.ignore_outgoing
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_the_documented_values() {
        let config = Config::default();
        assert!(config.enabled, "默认开启");
        assert!(!config.ignore_outgoing, "默认与 TS 版一致：发出去的也记");
        assert_eq!(config.max_text_chars, DEFAULT_MAX_TEXT_CHARS);
    }

    #[test]
    fn partial_config_keeps_other_defaults() {
        let raw: Raw = serde_json::from_str(r#"{"ignore_outgoing": true}"#).unwrap();
        let config = Config::from_raw(raw);
        assert!(config.enabled);
        assert!(config.ignore_outgoing);
        assert_eq!(config.max_text_chars, DEFAULT_MAX_TEXT_CHARS);
    }

    #[test]
    fn zero_means_unlimited_and_is_reported_as_such() {
        let raw: Raw = serde_json::from_str(r#"{"max_text_chars": 0}"#).unwrap();
        let config = Config::from_raw(raw);
        assert_eq!(config.max_text_chars, 0);
        assert!(config.summary().contains("不截断"));
    }

    #[test]
    fn summary_reports_the_real_switches() {
        let config = Config {
            enabled: true,
            ignore_outgoing: true,
            max_text_chars: 100,
        };
        let summary = config.summary();
        assert!(summary.contains("记录自己发的消息=false"), "{summary}");
        assert!(summary.contains("正文上限=100 字符"), "{summary}");
    }
}
