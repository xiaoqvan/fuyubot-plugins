//! HTML → PNG：把用户给的 HTML 补成一份完整文档，再交给 hyper-render。
//!
//! hyper-render 只做「解析 → 样式 → 布局 → 光栅化」，它不认识 Telegram，
//! 也不管输出文件；这一层只负责把 HTML 变成 PNG 字节，外加两件必须做的事：
//!
//! 1. **补默认样式**：用户多半只发一个片段（`<div>…</div>`），没有 `body` 背景、
//!    没有字体栈；直接用会得到黑底 / 衬线字 / 零内边距。所以片段会包进一份
//!    带基础 CSS 的完整文档；就算用户发的是完整文档，也把这份基础 CSS 插到
//!    `<head>` 最前面——用户自己的样式在后面，天然覆盖我们的默认值。
//! 2. **量尺寸**：`auto_height` 时输出高度由内容决定，真实像素尺寸只能从
//!    PNG 的 IHDR 里读回来（服务返回的 `width` / `height` 要用它）。

use hyper_render::{ColorScheme, Config as RenderConfig};

use crate::config::Config;

/// 一次渲染请求（服务入参的单次覆盖已经夹过范围）。
#[derive(Debug, Clone)]
pub struct Request {
    pub html: String,
    pub width: u32,
    pub height: u32,
    pub scale: f32,
    pub auto_height: bool,
    pub dark: bool,
    pub transparent: bool,
    pub padding: u32,
    pub font_family: String,
}

impl Request {
    /// 按插件配置起一份默认请求。
    pub fn new(html: impl Into<String>, config: &Config) -> Self {
        Self {
            html: html.into(),
            width: config.width,
            height: config.height,
            scale: config.scale,
            auto_height: config.auto_height,
            dark: config.dark,
            transparent: config.transparent,
            padding: config.padding,
            font_family: config.font_family.clone(),
        }
    }
}

/// 渲染结果：PNG 字节 + 实际像素尺寸。
#[derive(Debug, Clone)]
pub struct Rendered {
    pub png: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

/// 把 HTML 渲染成 PNG。
pub fn render(request: &Request) -> Result<Rendered, String> {
    let html = document(request);
    let config = RenderConfig::new()
        .width(request.width)
        .height(request.height)
        .scale(request.scale)
        .auto_height(request.auto_height)
        .color_scheme(if request.dark {
            ColorScheme::Dark
        } else {
            ColorScheme::Light
        })
        // 服务方法可能被多个插件从不同线程同时调用。
        // Stylo 的全局 rayon 池在并发 Parallel 遍历时会撞 thread-local 缓存，
        // 所以嵌入场景统一走 Sequential（PR #1 新增的开关）。
        .style_threading(hyper_render::StyleThreading::Sequential);

    let png = hyper_render::render_to_png(&html, config).map_err(|error| error.to_string())?;
    let (width, height) =
        png_dimensions(&png).ok_or_else(|| "渲染结果不是合法 PNG（读不到 IHDR）".to_owned())?;
    Ok(Rendered { png, width, height })
}

/// 把渲染出来的 PNG 转成 JPEG。
///
/// JPEG 没有 alpha 通道，半透明区域按白底合成（网页常见做法）；`quality` 会被夹到 1–100。
pub fn to_jpeg(png: &[u8], quality: u8) -> Result<Vec<u8>, String> {
    use image::{ExtendedColorType, ImageFormat, Rgb, RgbImage};

    let image = image::load_from_memory_with_format(png, ImageFormat::Png)
        .map_err(|error| format!("解码渲染结果失败：{error}"))?;
    let rgba = image.to_rgba8();
    let mut rgb = RgbImage::new(rgba.width(), rgba.height());
    for (x, y, pixel) in rgba.enumerate_pixels() {
        let alpha = u16::from(pixel[3]);
        let blend = |channel: u8| -> u8 {
            ((u16::from(channel) * alpha + 255 * (255 - alpha)) / 255) as u8
        };
        rgb.put_pixel(
            x,
            y,
            Rgb([blend(pixel[0]), blend(pixel[1]), blend(pixel[2])]),
        );
    }

    let mut out = Vec::new();
    let mut encoder =
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, quality.clamp(1, 100));
    encoder
        .encode(
            rgb.as_raw(),
            rgb.width(),
            rgb.height(),
            ExtendedColorType::Rgb8,
        )
        .map_err(|error| format!("编码 JPEG 失败：{error}"))?;
    Ok(out)
}

/// 补成完整文档。
fn document(request: &Request) -> String {
    let style = base_style(request);
    if looks_like_document(&request.html) {
        insert_style(&request.html, &style)
    } else {
        format!(
            "<!DOCTYPE html><html><head><meta charset=\"utf-8\">\
             <style data-fuyubot=\"base\">{style}</style></head><body>{}</body></html>",
            request.html
        )
    }
}

/// 基础 CSS。深色 / 透明只影响「用户自己没指定」的那部分。
///
/// ⚠️ **`html,body` 的固定宽度是刻意的**：hyper-render 的 viewport 把 `Config::width`
/// 当**物理像素**（CSS 宽度 = width / scale），但输出缓冲又是 `width * scale`——
/// 也就是说 `scale = 2` 时布局只有一半宽、右边三分之一是空白。给 `html/body` 显式
/// 写 `width: <CSS 宽>px` 之后，布局宽度不再取决于那个被除过的视口，输出正好是
/// `width × scale` 像素；同一份 HTML 在 `-s 1/2/3` 下换行位置一致（只是清晰度不同）。
fn base_style(request: &Request) -> String {
    let background = if request.transparent {
        "transparent".to_owned()
    } else if request.dark {
        "#0f1115".to_owned()
    } else {
        "#ffffff".to_owned()
    };
    let foreground = if request.dark { "#e6e8eb" } else { "#111111" };
    let padding = if looks_like_document(&request.html) {
        0
    } else {
        request.padding
    };

    format!(
        "*,*::before,*::after{{box-sizing:border-box}}\
         html,body{{margin:0;width:{}px}}\
         body{{padding:{padding}px;background:{background};color:{foreground};\
         font-family:{};font-size:16px;line-height:1.5}}\
         img,svg,video{{max-width:100%;height:auto}}\
         pre,code{{white-space:pre-wrap;word-break:break-word}}\
         table{{border-collapse:collapse}}",
        request.width, request.font_family
    )
}

/// 用户发的是不是一份完整文档（有 `<html` / `<!doctype html`）。
fn looks_like_document(html: &str) -> bool {
    let lower = html.to_ascii_lowercase();
    lower.contains("<html") || lower.contains("<!doctype html")
}

/// 把基础样式插到用户样式**前面**：优先 `<head>` 之后，其次 `<body>` 之前。
fn insert_style(html: &str, style: &str) -> String {
    let tag = format!("<style data-fuyubot=\"base\">{style}</style>");
    if let Some(index) = find_tag(html, "head")
        && let Some(offset) = html[index..].find('>')
    {
        return insert_at(html, index + offset + 1, &tag);
    }
    if let Some(index) = find_tag(html, "body") {
        return insert_at(html, index, &tag);
    }
    if let Some(index) = find_tag(html, "html")
        && let Some(offset) = html[index..].find('>')
    {
        return insert_at(html, index + offset + 1, &tag);
    }
    format!("{tag}{html}")
}

/// 找一个**开标签** `<tag`（大小写不敏感），且标签名后面必须紧跟 `>` 或空白。
///
/// 不能直接 `find("<head")`：那会把 `<header>` 当成 `<head>`，样式就插进页头里了。
fn find_tag(haystack: &str, tag: &str) -> Option<usize> {
    let lower = haystack.to_ascii_lowercase();
    let needle = format!("<{tag}");
    let mut from = 0;
    while let Some(index) = lower[from..].find(&needle) {
        let at = from + index;
        let next = lower[at + needle.len()..].chars().next();
        if matches!(next, Some(ch) if ch == '>' || ch.is_whitespace()) {
            return Some(at);
        }
        from = at + needle.len();
    }
    None
}

fn insert_at(text: &str, index: usize, addition: &str) -> String {
    let mut out = String::with_capacity(text.len() + addition.len());
    out.push_str(&text[..index]);
    out.push_str(addition);
    out.push_str(&text[index..]);
    out
}

/// 从 PNG 的 IHDR 读尺寸（签名后第一个块固定是 IHDR）。
pub fn png_dimensions(png: &[u8]) -> Option<(u32, u32)> {
    const SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    if png.len() < 24 || png[..8] != SIGNATURE || &png[12..16] != b"IHDR" {
        return None;
    }
    let width = u32::from_be_bytes([png[16], png[17], png[18], png[19]]);
    let height = u32::from_be_bytes([png[20], png[21], png[22], png[23]]);
    Some((width, height))
}

#[cfg(test)]
#[path = "../tests/unit/render.rs"]
mod tests;
