//! 插件级配置：`[plugin.fuyubot.help]` 那一段。
//!
//! 所有字段都可省略，省略就用默认值；写错的字段不会让插件起不来，只会退回默认并
//! 打一条 warn（配置是给人改的，不值得为它拒绝加载）。
//!
//! ```toml
//! [plugin.fuyubot.help]
//! title = "可用命令"
//! description = "FuyuBot · 一张图看完所有命令"
//! tips = "发送 /help 随时刷新"
//! table_desc = "共 {count} 条命令"
//! image = "https://files.catbox.moe/py80s6.jpg"  # 置空则不画头图
//! width = 760
//! height = 600
//! scale = 2.0
//! format = "jpg"                # jpg / png
//! max_commands = 120
//! send_timeout_ms = 30000
//! fallback_text = true
//! ```

use fuyubot_abi::HostApi;
use serde::Deserialize;

use crate::log;

/// 插件名（`define_plugin!` 里那个），也是配置段名。
pub const PLUGIN_NAME: &str = "fuyubot.help";

/// 图片生成服务（由 `fuyubot.html2img` 插件提供，硬依赖）。
pub const HTML2IMG_SERVICE: &str = "fuyubot.html2img";

/// 默认头图：与 help.vue 里那张一致。
pub const DEFAULT_IMAGE: &str = "https://files.catbox.moe/py80s6.jpg";

/// 默认标题。
pub const DEFAULT_TITLE: &str = "可用命令";

/// 默认副标题。
pub const DEFAULT_DESCRIPTION: &str = "FuyuBot · 一张图看完所有命令";

/// 默认底部提示。
pub const DEFAULT_TIPS: &str = "发送 /help 随时刷新";

/// 默认分组说明（每组 = 一个登记命令的插件），支持 `{count}` / `{plugins}` / `{version}`。
pub const DEFAULT_TABLE_DESC: &str = "共 {count} 条命令";

/// 默认渲染宽度（CSS px）。760 与 help.vue 的排版最接近。
pub const DEFAULT_WIDTH: u32 = 760;

/// 默认渲染高度下限（`auto_height = true` 时内容更高就自动撑开）。
pub const DEFAULT_HEIGHT: u32 = 600;

/// 默认输出倍率（2.0 ≈ 高清屏）。
pub const DEFAULT_SCALE: f32 = 2.0;

/// 默认输出格式。
pub const DEFAULT_FORMAT: &str = "jpg";

/// 默认最多画多少条命令（超出截断并提示）。
pub const DEFAULT_MAX_COMMANDS: usize = 120;

/// 默认发送超时（毫秒）。
pub const DEFAULT_SEND_TIMEOUT_MS: u64 = 30_000;

/// 插件版本（编译期来自 Cargo.toml）。
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// 展示用版本号：`v0.1.0`。
pub fn version_label() -> String {
    format!("v{}", version())
}

/// 解析后的配置。
#[derive(Debug, Clone)]
pub struct Config {
    pub title: String,
    pub description: String,
    pub tips: String,
    /// 每组（一个插件）的说明模板，支持 `{count}` / `{plugins}` / `{version}`。
    pub table_desc: String,
    /// 头图 URL（支持 `http(s)` 与 `data:`）；空串表示不画头图。
    pub image: String,
    /// 渲染宽度（CSS px）。
    pub width: u32,
    /// 渲染高度下限（CSS px）。
    pub height: u32,
    /// 输出倍率。
    pub scale: f32,
    /// 输出格式：`jpg` / `png`。
    pub format: String,
    /// 单张图最多画多少条命令。
    pub max_commands: usize,
    /// 发消息的等待上限。
    pub send_timeout_ms: u64,
    /// 图片生成 / 发送失败时，是否补发一条文字版命令表。
    pub fallback_text: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            title: DEFAULT_TITLE.to_owned(),
            description: DEFAULT_DESCRIPTION.to_owned(),
            tips: DEFAULT_TIPS.to_owned(),
            table_desc: DEFAULT_TABLE_DESC.to_owned(),
            image: DEFAULT_IMAGE.to_owned(),
            width: DEFAULT_WIDTH,
            height: DEFAULT_HEIGHT,
            scale: DEFAULT_SCALE,
            format: DEFAULT_FORMAT.to_owned(),
            max_commands: DEFAULT_MAX_COMMANDS,
            send_timeout_ms: DEFAULT_SEND_TIMEOUT_MS,
            fallback_text: true,
        }
    }
}

/// 配置文件的原始形状：全部 `Option`，缺省与写错都不影响启动。
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct Raw {
    title: Option<String>,
    description: Option<String>,
    tips: Option<String>,
    table_desc: Option<String>,
    image: Option<String>,
    width: Option<u32>,
    height: Option<u32>,
    scale: Option<f64>,
    format: Option<String>,
    max_commands: Option<u64>,
    send_timeout_ms: Option<u64>,
    fallback_text: Option<bool>,
}

impl Config {
    /// 从宿主的 `[plugin.fuyubot.help]` 段读；读不到或坏掉就用默认值。
    pub fn from_host(host: &HostApi) -> Self {
        let raw: Raw = match host.get_config(PLUGIN_NAME, "") {
            Some(json) => match serde_json::from_str(&json) {
                Ok(raw) => raw,
                Err(error) => {
                    log::warn(
                        host,
                        format!("配置段 [{PLUGIN_NAME}] 解析失败，全部用默认值：{error}"),
                    );
                    Raw::default()
                }
            },
            None => Raw::default(),
        };

        let mut config = Self::default();

        if let Some(value) = raw.title {
            config.title = value.trim().to_owned();
        }
        if let Some(value) = raw.description {
            config.description = value.trim().to_owned();
        }
        if let Some(value) = raw.tips {
            config.tips = value.trim().to_owned();
        }
        if let Some(value) = raw.table_desc {
            config.table_desc = value.trim().to_owned();
        }
        if let Some(value) = raw.image {
            // 空串是有意义的：明确表示「不要头图」。
            config.image = value.trim().to_owned();
        }

        config.width = raw.width.unwrap_or(config.width).clamp(320, 1920);
        config.height = raw.height.unwrap_or(config.height).clamp(200, 6000);
        config.scale = raw
            .scale
            .map_or(config.scale, |value| value as f32)
            .clamp(1.0, 3.0);
        if let Some(text) = raw.format.as_deref() {
            match normalize_format(text) {
                Some(format) => config.format = format,
                None => log::warn(
                    host,
                    format!("配置 format = {text:?} 不认识（可用 jpg / png），按 jpg 处理"),
                ),
            }
        }
        config.max_commands = raw
            .max_commands
            .map_or(config.max_commands, |value| value as usize)
            .clamp(1, 500);
        config.send_timeout_ms = raw
            .send_timeout_ms
            .unwrap_or(config.send_timeout_ms)
            .clamp(1_000, 120_000);
        config.fallback_text = raw.fallback_text.unwrap_or(config.fallback_text);

        config
    }

    /// 展开 `{count}` / `{plugins}` / `{version}`。
    ///
    /// 不用通用模板引擎：就三个占位符，`str::replace` 足够，也不会把用户文本里的
    /// 其它花括号当成语法。
    pub fn expand(&self, text: &str, count: usize, plugins: usize) -> String {
        text.replace("{count}", &count.to_string())
            .replace("{plugins}", &plugins.to_string())
            .replace("{version}", &version_label())
    }
}

/// 规范化输出格式：`jpg` / `png`（`jpeg` 当 `jpg`）。
fn normalize_format(text: &str) -> Option<String> {
    match text.trim().to_ascii_lowercase().as_str() {
        "jpg" | "jpeg" => Some("jpg".to_owned()),
        "png" => Some("png".to_owned()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_sane() {
        let config = Config::default();
        assert_eq!(config.width, 760);
        assert_eq!(config.scale, 2.0);
        assert_eq!(config.format, "jpg");
        assert!(config.fallback_text);
        assert!(!config.image.is_empty());
    }

    #[test]
    fn placeholders_expand() {
        let config = Config::default();
        let text = config.expand("{count} 条 / {plugins} 个插件 / {version}", 7, 3);
        assert_eq!(text, format!("7 条 / 3 个插件 / {}", version_label()));
    }

    #[test]
    fn format_aliases() {
        assert_eq!(normalize_format("JPEG").as_deref(), Some("jpg"));
        assert_eq!(normalize_format(" png ").as_deref(), Some("png"));
        assert_eq!(normalize_format("webp"), None);
    }
}
