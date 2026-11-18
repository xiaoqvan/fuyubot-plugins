//! 插件级配置：`[plugin.xiaoqvan.x]` 那一段。
//!
//! 所有字段都可省略，省略就用默认值；写错的字段不会让插件起不来，只会退回默认并
//! 打一条 warn（配置是给人改的，不值得为它拒绝加载）。
//!
//! ```toml
//! [plugin.xiaoqvan.x]
//! api_base = "https://api.fxtwitter.com"   # 解析用的公开接口，可换成自建实例
//! auto_parse = true                        # 聊天里贴链接就自动解析
//! album_limit = 10                         # 相册每组个数（Telegram 硬上限 10）
//! max_media = 100                          # 一条文章最多发多少个媒体
//! send_text = true                         # 文章正文是否再发一条文字消息
//! text_chunk = 3500                        # 单条文字消息的上限（Telegram 4096）
//! send_timeout_ms = 120000
//! image_card = true                        # 是否出 X 风格卡片图（图片最多两张，视频只画封面）
//! fallback_text = true                     # 出图失败就发纯文本
//! card_width = 512
//! card_scale = 2.0
//! card_format = "jpg"
//! card_dark = false
//! card_max_chars = 6000                    # 卡片里最多画多少字
//! ```

use fuyubot_abi::HostApi;
use serde::Deserialize;

use crate::log;

/// 插件名（`define_plugin!` 里那个），也是配置段名。
pub const PLUGIN_NAME: &str = "xiaoqvan.x";

/// 图片生成服务（由 `fuyubot.html2img` 提供，**软依赖**）。
///
/// 没装 / 没启用时插件照常工作，纯文本文章退化成文字消息。
pub const HTML2IMG_SERVICE: &str = "fuyubot.html2img";

/// 默认解析接口。FxTwitter 是公开的第三方只读接口，不需要 token。
pub const DEFAULT_API_BASE: &str = "https://api.fxtwitter.com";

/// 相册每组的项数上限。Telegram 的 `sendMessageAlbum` 只接受 2..=10 项，
/// 所以这个值会被夹到 `2..=10`；条目更多时由插件切成多条相册消息。
pub const ALBUM_MAX: usize = 10;
pub const ALBUM_MIN: usize = 2;

/// 一条文章最多发多少个媒体（防止一条链接刷屏）。
pub const DEFAULT_MAX_MEDIA: usize = 100;

/// 单条文字消息的字符上限。Telegram 是 4096，留出换行与 HTML 标签的余量。
pub const DEFAULT_TEXT_CHUNK: usize = 3500;

/// 发一组相册 / 一个视频的等待预算：TDLib 要先把媒体拿到手再发。
pub const DEFAULT_SEND_TIMEOUT_MS: u64 = 120_000;

pub const DEFAULT_CARD_WIDTH: u32 = 512;
pub const DEFAULT_CARD_SCALE: f32 = 2.0;
pub const DEFAULT_CARD_FORMAT: &str = "jpg";
pub const DEFAULT_CARD_MAX_CHARS: usize = 6000;

/// 解析后的配置。
#[derive(Debug, Clone)]
pub struct Config {
    /// 解析接口的根地址（不带结尾的 `/`）。
    pub api_base: String,
    /// 聊天里贴了 X 链接就自动解析。
    pub auto_parse: bool,
    /// 相册每组的项数（已夹到 2..=10）。
    pub album_limit: usize,
    /// 一条文章最多发多少个媒体。
    pub max_media: usize,
    /// 有媒体时，是否再单独发一条正文文字消息。
    pub send_text: bool,
    /// 单条文字消息的字符上限。
    pub text_chunk: usize,
    /// 发送的等待预算。
    pub send_timeout_ms: u64,
    /// 是否把帖子渲染成 X 风格卡片图（图片最多两张，视频只画封面）。
    pub image_card: bool,
    /// 出图失败 / 没有 html2img 时是否补发纯文本。
    pub fallback_text: bool,
    pub card_width: u32,
    pub card_scale: f32,
    /// `jpg` / `png`。
    pub card_format: String,
    /// 深色卡片（与 X 的深色主题一致）。
    pub card_dark: bool,
    /// 卡片里最多画多少字符（超长文章截断并提示看原链接）。
    pub card_max_chars: usize,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            api_base: DEFAULT_API_BASE.to_owned(),
            auto_parse: true,
            album_limit: ALBUM_MAX,
            max_media: DEFAULT_MAX_MEDIA,
            send_text: true,
            text_chunk: DEFAULT_TEXT_CHUNK,
            send_timeout_ms: DEFAULT_SEND_TIMEOUT_MS,
            image_card: true,
            fallback_text: true,
            card_width: DEFAULT_CARD_WIDTH,
            card_scale: DEFAULT_CARD_SCALE,
            card_format: DEFAULT_CARD_FORMAT.to_owned(),
            card_dark: false,
            card_max_chars: DEFAULT_CARD_MAX_CHARS,
        }
    }
}

/// 配置文件的原始形状：全部 `Option`，缺省与写错都不影响启动。
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct Raw {
    api_base: Option<String>,
    auto_parse: Option<bool>,
    album_limit: Option<u64>,
    max_media: Option<u64>,
    send_text: Option<bool>,
    text_chunk: Option<u64>,
    send_timeout_ms: Option<u64>,
    image_card: Option<bool>,
    fallback_text: Option<bool>,
    card_width: Option<u32>,
    card_scale: Option<f64>,
    card_format: Option<String>,
    card_dark: Option<bool>,
    card_max_chars: Option<u64>,
}

impl Config {
    /// 从宿主的 `[plugin.xiaoqvan.x]` 段读；读不到就用默认值。
    pub fn from_host(host: &HostApi) -> Self {
        match host.get_config(PLUGIN_NAME, "") {
            Some(json) => Self::from_json(&json),
            None => Self::default(),
        }
    }

    /// 从一段 JSON 解析（单测直接喂字符串，不用造宿主）。
    ///
    /// 坏掉的字段退回默认值并打 warn：配置是给人改的，不值得为它拒绝加载。
    pub fn from_json(json: &str) -> Self {
        let raw: Raw = match serde_json::from_str(json) {
            Ok(raw) => raw,
            Err(error) => {
                log::warn(&format!(
                    "配置段 [{PLUGIN_NAME}] 解析失败，全部用默认值：{error}"
                ));
                Raw::default()
            }
        };
        let mut config = Self::default();

        if let Some(base) = raw.api_base {
            let base = base.trim().trim_end_matches('/').to_owned();
            if base.starts_with("http://") || base.starts_with("https://") {
                config.api_base = base;
            } else if !base.is_empty() {
                log::warn(&format!("api_base = {base:?} 不是 http(s) 地址，用默认值"));
            }
        }
        config.auto_parse = raw.auto_parse.unwrap_or(config.auto_parse);
        config.album_limit = raw
            .album_limit
            .map_or(config.album_limit, |value| value as usize)
            .clamp(ALBUM_MIN, ALBUM_MAX);
        config.max_media = raw
            .max_media
            .map_or(config.max_media, |value| value as usize)
            .clamp(1, 500);
        config.send_text = raw.send_text.unwrap_or(config.send_text);
        config.text_chunk = raw
            .text_chunk
            .map_or(config.text_chunk, |value| value as usize)
            .clamp(200, 4000);
        config.send_timeout_ms = raw
            .send_timeout_ms
            .unwrap_or(config.send_timeout_ms)
            .clamp(5_000, 600_000);
        config.image_card = raw.image_card.unwrap_or(config.image_card);
        config.fallback_text = raw.fallback_text.unwrap_or(config.fallback_text);
        config.card_width = raw.card_width.unwrap_or(config.card_width).clamp(240, 1600);
        config.card_scale = raw
            .card_scale
            .map_or(config.card_scale, |value| value as f32)
            .clamp(0.5, 3.0);
        if let Some(format) = raw.card_format {
            match format.trim().to_ascii_lowercase().as_str() {
                "jpg" | "jpeg" => config.card_format = "jpg".to_owned(),
                "png" => config.card_format = "png".to_owned(),
                other => log::warn(&format!(
                    "card_format = {other:?} 不认识（可用 jpg / png），按 jpg 处理"
                )),
            }
        }
        config.card_dark = raw.card_dark.unwrap_or(config.card_dark);
        config.card_max_chars = raw
            .card_max_chars
            .map_or(config.card_max_chars, |value| value as usize)
            .clamp(200, 20_000);
        config
    }
}

#[cfg(test)]
#[path = "../tests/unit/config.rs"]
mod tests;
