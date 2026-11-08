//! 插件级配置：`[plugin.fuyubot.html2img]` 那一段。
//!
//! 所有字段都可省略，省略就是这里的默认值；写错的字段不会让插件起不来，
//! 只会退回默认并打一条 warn（配置是给人改的，不值得为它拒绝加载）。
//!
//! ```toml
//! [plugin.fuyubot.html2img]
//! width = 1024              # 视口宽度（CSS px）
//! height = 768              # 视口高度；auto_height = true 时只作下限参考
//! scale = 1.0               # 输出倍率（2.0 ≈ 高清屏）
//! auto_height = true        # 高度按内容撑开（长页面不会被截断）
//! dark = false              # 深色底 / prefers-color-scheme: dark
//! transparent = false       # 透明底（只影响没自己画背景的 HTML）
//! padding = 20              # 片段模式下 body 的内边距
//! font_family = "system-ui, ..."  # 片段模式的默认字体栈
//! format = "jpg"            # 默认输出格式：jpg / png
//! jpeg_quality = 85         # JPEG 质量（1–100）
//! max_width = 1920          # 以下都是防滥用的上限
//! max_height = 6000
//! max_scale = 3.0
//! max_html_bytes = 131072
//! max_pixels = 16000000
//! ```

use std::path::PathBuf;

use fuyubot_abi::HostApi;
use serde::Deserialize;

use crate::log;

/// 插件名（`define_plugin!` 里那个），也是配置段名。
pub const PLUGIN_NAME: &str = "fuyubot.html2img";

/// 片段模式的默认字体栈：先吃系统 UI 字体，再按平台补中文 / 日文 / 韩文字体。
///
/// hyper-render 只用**系统字体**（不支持 `@font-face`），所以这里列的是
/// Windows / macOS / Linux / Android 上真实存在的字体名；一个都没有时会退回
/// fontdb 的默认 sans-serif。
pub const DEFAULT_FONT_FAMILY: &str = "system-ui, -apple-system, \"Segoe UI\", Roboto, \
     \"Noto Sans CJK SC\", \"Source Han Sans SC\", \"Microsoft YaHei\", \"PingFang SC\", \
     \"Hiragino Sans GB\", sans-serif";

/// 一张图片最终要用渲染尺寸算的上限（宽 × 高 × scale²）。
pub const DEFAULT_MAX_PIXELS: u64 = 16_000_000;

/// 输出图片格式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ImageFormat {
    /// 无损 PNG，保留 alpha 通道。
    Png,
    /// 有损 JPEG，体积小得多；透明区域压到白底。
    #[default]
    Jpg,
}

impl ImageFormat {
    /// 从配置值 / 请求参数解析：`png` / `jpg` / `jpeg`，不分大小写。
    pub fn parse(text: &str) -> Option<Self> {
        match text.trim().to_ascii_lowercase().as_str() {
            "png" => Some(Self::Png),
            "jpg" | "jpeg" => Some(Self::Jpg),
            _ => None,
        }
    }

    /// 规范名（`png` / `jpg`），同时用作文件扩展名。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Png => "png",
            Self::Jpg => "jpg",
        }
    }
}

/// 解析后的配置。
#[derive(Debug, Clone)]
pub struct Config {
    pub width: u32,
    pub height: u32,
    pub scale: f32,
    pub auto_height: bool,
    pub dark: bool,
    pub transparent: bool,
    pub padding: u32,
    pub font_family: String,
    /// 默认输出格式（`render` 入参没给 `format` 时用它）。
    pub format: ImageFormat,
    /// JPEG 质量（1–100），只在输出 JPEG 时生效。
    pub jpeg_quality: u8,
    /// 渲染产物的落盘目录；`None` = 系统临时目录下的 `fuyubot-html2img/`。
    pub temp_dir: Option<PathBuf>,
    pub max_width: u32,
    pub max_height: u32,
    pub max_scale: f32,
    pub max_html_bytes: usize,
    pub max_pixels: u64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            width: 1024,
            height: 768,
            scale: 1.0,
            auto_height: true,
            dark: false,
            transparent: false,
            padding: 20,
            font_family: DEFAULT_FONT_FAMILY.to_owned(),
            format: ImageFormat::Jpg,
            jpeg_quality: 85,
            temp_dir: None,
            max_width: 1920,
            max_height: 6000,
            max_scale: 3.0,
            max_html_bytes: 128 * 1024,
            max_pixels: DEFAULT_MAX_PIXELS,
        }
    }
}

/// 配置文件的原始形状：全部 `Option`，缺省与写错都不影响启动。
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct Raw {
    width: Option<u32>,
    height: Option<u32>,
    scale: Option<f64>,
    auto_height: Option<bool>,
    dark: Option<bool>,
    transparent: Option<bool>,
    padding: Option<u32>,
    font_family: Option<String>,
    format: Option<String>,
    jpeg_quality: Option<u64>,
    temp_dir: Option<String>,
    max_width: Option<u32>,
    max_height: Option<u32>,
    max_scale: Option<f64>,
    max_html_bytes: Option<u64>,
    max_pixels: Option<u64>,
}

impl Config {
    /// 从宿主的 `[plugin.fuyubot.html2img]` 段读；读不到或坏掉就用默认值。
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

        // 上限先落定，后面的具体值才好夹。
        config.max_width = raw.max_width.unwrap_or(config.max_width).clamp(64, 4096);
        config.max_height = raw
            .max_height
            .unwrap_or(config.max_height)
            .clamp(64, 16_384);
        config.max_scale = raw
            .max_scale
            .map_or(config.max_scale, |value| value as f32)
            .clamp(0.1, 4.0);
        config.max_html_bytes = raw
            .max_html_bytes
            .map_or(config.max_html_bytes, |value| value as usize)
            .clamp(1024, 2 * 1024 * 1024);
        config.max_pixels = raw
            .max_pixels
            .unwrap_or(config.max_pixels)
            .clamp(100_000, 64_000_000);

        config.width = raw
            .width
            .unwrap_or(config.width)
            .clamp(64, config.max_width);
        config.height = raw
            .height
            .unwrap_or(config.height)
            .clamp(64, config.max_height);
        config.scale = raw
            .scale
            .map_or(config.scale, |value| value as f32)
            .clamp(0.1, config.max_scale);
        config.auto_height = raw.auto_height.unwrap_or(config.auto_height);
        config.dark = raw.dark.unwrap_or(config.dark);
        config.transparent = raw.transparent.unwrap_or(config.transparent);
        config.padding = raw.padding.unwrap_or(config.padding).min(200);
        config.font_family = raw
            .font_family
            .filter(|family| safe_font_family(family))
            .unwrap_or_else(|| DEFAULT_FONT_FAMILY.to_owned());
        if let Some(text) = raw.format.as_deref() {
            match ImageFormat::parse(text) {
                Some(format) => config.format = format,
                None => log::warn(
                    host,
                    format!("配置 format = {text:?} 不认识（可用 png / jpg），按默认 jpg 处理"),
                ),
            }
        }
        config.jpeg_quality = raw
            .jpeg_quality
            .unwrap_or(u64::from(config.jpeg_quality))
            .clamp(1, 100) as u8;
        config.temp_dir = raw
            .temp_dir
            .map(|dir| dir.trim().to_owned())
            .filter(|dir| !dir.is_empty())
            .map(PathBuf::from);

        config.max_html_bytes = config.max_html_bytes.max(1024);
        config
    }

    /// 单次请求的覆盖值夹到合法范围。
    pub fn clamp_width(&self, width: u32) -> u32 {
        width.clamp(64, self.max_width)
    }

    pub fn clamp_height(&self, height: u32) -> u32 {
        height.clamp(64, self.max_height)
    }

    pub fn clamp_scale(&self, scale: f32) -> f32 {
        if scale.is_finite() {
            scale.clamp(0.1, self.max_scale)
        } else {
            self.scale
        }
    }

    /// 渲染尺寸的像素预算（宽度 × 高度 × scale²），超过就拒绝。
    pub fn within_budget(&self, width: u32, height: u32, scale: f32) -> bool {
        let (width, height) = (width as u64, height as u64);
        let scaled = |value: u64| ((value as f64) * (scale as f64)).round() as u64;
        scaled(width).saturating_mul(scaled(height)) <= self.max_pixels
    }
}

/// 字体栈只允许 CSS 值里合法的字符，避免配置里的 `}` / `;` 提前结束这条规则。
fn safe_font_family(family: &str) -> bool {
    !family.trim().is_empty()
        && family
            .chars()
            .all(|ch| ch.is_alphanumeric() || matches!(ch, ' ' | ',' | '-' | '_' | '"' | '\''))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_format_parses_aliases() {
        assert_eq!(ImageFormat::parse("PNG"), Some(ImageFormat::Png));
        assert_eq!(ImageFormat::parse(" jpeg "), Some(ImageFormat::Jpg));
        assert_eq!(ImageFormat::parse("webp"), None);
        assert_eq!(ImageFormat::default(), ImageFormat::Jpg);
        assert_eq!(ImageFormat::Jpg.as_str(), "jpg");
    }

    #[test]
    fn defaults_are_valid() {
        let config = Config::default();
        assert_eq!(config.width, 1024);
        assert!(config.auto_height);
        assert!(config.within_budget(config.width, config.height, config.scale));
    }

    #[test]
    fn clamp_keeps_values_in_range() {
        let config = Config::default();
        assert_eq!(config.clamp_width(5), 64);
        assert_eq!(config.clamp_width(99_999), config.max_width);
        assert_eq!(config.clamp_height(1), 64);
        assert_eq!(config.clamp_scale(f32::NAN), config.scale);
        assert_eq!(config.clamp_scale(99.0), config.max_scale);
        assert_eq!(config.clamp_scale(-1.0), 0.1);
    }

    #[test]
    fn budget_rejects_oversized_images() {
        let config = Config::default();
        // 1920 × 6000 × 4 > 1600 万像素。
        assert!(!config.within_budget(1920, 6000, 2.0));
        assert!(config.within_budget(800, 600, 1.0));
    }

    #[test]
    fn font_family_rejects_css_injection() {
        assert!(safe_font_family(DEFAULT_FONT_FAMILY));
        assert!(!safe_font_family("sans-serif; } body { background: red"));
        assert!(!safe_font_family("   "));
    }
}
