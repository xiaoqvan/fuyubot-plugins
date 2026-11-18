//! 本地把卡片渲染成 PNG —— 和宿主里 xiaoqvan.html2img 走**同一个渲染器**。
//!
//! ```bash
//! cargo run -p fuyubot-plugin-x --example render_card                       # 默认 fixture → target/card.png
//! cargo run -p fuyubot-plugin-x --example render_card -- other.json out.png
//! ```
//!
//! 这里刻意复刻了 html2img 的 document() 包装：它会往 <head> 里插一条
//! html,body{width:<width>px}。少了这条，hyper-render 会把 Config::width 当**物理**
//! 像素、CSS 宽度只有 width/scale，600px 的卡片会被裁掉右半边。

use std::path::PathBuf;

/// 与插件配置默认值一致。
const WIDTH: u32 = 512;
const SCALE: f32 = 2.0;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let json_path = PathBuf::from(
        args.first()
            .cloned()
            .unwrap_or_else(|| "tests/fixtures/status_article.json".to_owned()),
    );
    let out = PathBuf::from(
        args.get(1)
            .cloned()
            .unwrap_or_else(|| "target/card.png".to_owned()),
    );

    let json = std::fs::read_to_string(&json_path).unwrap_or_else(|error| {
        panic!(
            "读不到 {}：{error}（在 crates/fuyubot-plugin-x 下运行）",
            json_path.display()
        )
    });
    let translation = args.get(2).map(String::as_str);
    let footer = args.get(3).map(String::as_str);
    let avatar = args.get(4).map(String::as_str);
    let html = fuyubot_plugin_x::preview_card_html_with_footer(&json, translation, footer, avatar)
        .expect("组装卡片 HTML 失败");
    let html = inject_base_width(&html, WIDTH);

    let config = hyper_render::Config::new()
        .width(WIDTH)
        .height(600)
        .scale(SCALE)
        .auto_height(true)
        .color_scheme(hyper_render::ColorScheme::Light)
        // 与 html2img 一致：嵌入场景走 Sequential，避免 Stylo 的全局 rayon 池串味。
        .style_threading(hyper_render::StyleThreading::Sequential);

    let png = hyper_render::render_to_png(&html, config).expect("渲染 PNG 失败");
    if let Some(parent) = out.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    std::fs::write(&out, &png).expect("写 PNG 失败");
    println!("已写出 {}（{} 字节）", out.display(), png.len());
}

/// 复刻 html2img 的“把宽度钉死”那一步。
fn inject_base_width(html: &str, width: u32) -> String {
    let style =
        format!("<style data-fuyubot=\"base\">html,body{{margin:0;width:{width}px}}</style>");
    match html.find("<head>") {
        Some(index) => {
            let at = index + "<head>".len();
            format!("{}{}{}", &html[..at], style, &html[at..])
        }
        None => format!("{style}{html}"),
    }
}
