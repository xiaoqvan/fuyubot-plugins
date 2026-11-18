//! 本地渲染一张 `/help` 预览图（不连宿主、不发消息）。
//!
//! ```bash
//! cargo run -p fuyubot-plugin-help --example preview
//! # 换个文件名
//! cargo run -p fuyubot-plugin-help --example preview -- preview-owner.jpg
//! ```
//!
//! 数据是**手搭的**（模拟一张混合了 user / chat_admin / admin / owner 命令的宿主表），
//! 但走的是真实链路：`build_render_request` → `TEMPLATE` → `vue-template-html` →
//! `hyper-render`，所以样式、图标、描边都是最终效果。

use std::collections::BTreeMap;

use fuyubot_plugin_help::commands::build_render_request;
use fuyubot_plugin_help::config::Config;
use serde_json::{Value, json};
use vue_template_html::Template;

fn main() -> Result<(), String> {
    let output = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "preview.jpg".to_owned());

    let config = Config {
        // 预览不联网：头图关掉。
        image: String::new(),
        format: "jpg".to_owned(),
        ..Config::default()
    };
    let names = BTreeMap::from([
        ("xiaoqvan.adblock".to_owned(), "广告拦截".to_owned()),
        ("xiaoqvan.help".to_owned(), "Help (Image)".to_owned()),
        ("xiaoqvan.douyin".to_owned(), "抖音解析".to_owned()),
    ]);

    let (request, _stats) = build_render_request(&config, &demo_commands(), &names);

    let template = Template::compile(request["html"].as_str().unwrap_or_default())
        .map_err(|error| format!("模板编译失败：{error}"))?;
    let html = template
        .render(&request["data"])
        .map_err(|error| format!("模板渲染失败：{error}"))?;
    let width = request["width"].as_u64().unwrap_or(760) as u32;

    let png = hyper_render::render_to_png(
        &document(&html, width),
        hyper_render::Config::new()
            .width(width)
            .height(request["height"].as_u64().unwrap_or(600) as u32)
            .scale(2.0)
            .auto_height(true),
    )
    .map_err(|error| format!("渲染失败：{error}"))?;

    let bytes = to_jpeg(&png, 88)?;
    std::fs::write(&output, &bytes).map_err(|error| format!("写文件失败：{error}"))?;
    println!("已写出 {output}（{} 字节）", bytes.len());
    Ok(())
}

/// 补成完整文档，**照抄** html2img 的基础样式（`html,body` 固定成 CSS 宽）。
///
/// 这一条不是可选的：hyper-render 的 viewport 把 `Config::width` 当物理像素
/// （CSS 宽 = width / scale），不写死 `html/body` 宽度时布局只有一半宽。
fn document(fragment: &str, width: u32) -> String {
    format!(
        "<!DOCTYPE html><html><head><meta charset=\"utf-8\"><style>\
         *,*::before,*::after{{box-sizing:border-box}}\
         html,body{{margin:0;width:{width}px}}\
         body{{padding:0;background:#fff;color:#111;font-family:sans-serif;font-size:16px;line-height:1.5}}\
         img,svg,video{{max-width:100%;height:auto}}\
         </style></head><body>{fragment}</body></html>"
    )
}

/// PNG → JPEG（透明区域压白底），与 html2img 插件的转码逻辑一致。
fn to_jpeg(png: &[u8], quality: u8) -> Result<Vec<u8>, String> {
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

/// 一张混合了四种角色、三种会话范围的命令表。
fn demo_commands() -> Vec<Value> {
    vec![
        json!({
            "name": "start", "description": "Bot 启动指令（配置 AI 接口）",
            "plugin": "xiaoqvan.adblock", "role": "user", "scopes": ["private", "group"],
        }),
        json!({
            "name": "adaiadd", "description": "AI 判断广告文本，关键词 > 3 则入黑名单并返回 ID（仅 Bot 主人）",
            "plugin": "xiaoqvan.adblock", "role": "owner", "scopes": ["private"],
        }),
        json!({
            "name": "adaiset", "description": "管理内置 AI 授权群（仅 Bot 主人）",
            "plugin": "xiaoqvan.adblock", "role": "owner", "scopes": ["private"],
        }),
        json!({
            "name": "adconfig", "description": "配置广告屏蔽功能（仅本群管理）",
            "plugin": "xiaoqvan.adblock", "role": "chat_admin", "scopes": ["group"],
        }),
        json!({
            "name": "setlogchannel", "description": "设置日志频道（仅本群管理）",
            "plugin": "xiaoqvan.adblock", "role": "chat_admin", "scopes": ["group"],
        }),
        json!({
            "name": "addkw", "description": "向 ads / bio 黑名单添加关键词",
            "plugin": "xiaoqvan.adblock", "role": "chat_admin", "scopes": ["group"],
        }),
        json!({
            "name": "qadid", "description": "查询广告 ID",
            "plugin": "xiaoqvan.adblock", "role": "user", "scopes": ["private", "group", "channel"],
        }),
        json!({
            "name": "spam", "description": "AI 检测垃圾信息（Bot 需群管权限）",
            "plugin": "xiaoqvan.adblock", "role": "user", "scopes": ["group", "channel"],
        }),
        json!({
            "name": "plugins", "description": "看看装了哪些插件",
            "role": "user", "scopes": ["private"],
        }),
        json!({
            "name": "help", "description": "这条帮助（/help text 出文字版）",
            "plugin": "xiaoqvan.help", "role": "user", "scopes": ["private", "group", "channel"],
        }),
        json!({
            "name": "douyin", "description": "解析抖音分享链接",
            "plugin": "xiaoqvan.douyin", "role": "user", "scopes": ["private", "group", "channel"],
        }),
        json!({
            "name": "dyadmin", "description": "抖音解析管理（管理员）",
            "plugin": "xiaoqvan.douyin", "role": "admin", "scopes": ["private"],
        }),
    ]
}
