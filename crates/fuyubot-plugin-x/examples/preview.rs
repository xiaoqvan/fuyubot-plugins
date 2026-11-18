//! 本地预览卡片：不连宿主、不发消息，只用同一份渲染代码生成 HTML。
//!
//! ```bash
//! cargo run -p fuyubot-plugin-x --example preview                 # 打印到 stdout
//! cargo run -p fuyubot-plugin-x --example preview -- card.html     # 写文件
//! ```
//!
//! 默认读 tests/fixtures/status_article.json（一条真实的 X 文章响应）。
//! 想换成别的帖子：`--example preview -- --json other.json card.html`。
//!
//! 输出的 HTML 用浏览器打开即可看到卡片长什么样；插件在宿主里渲染用的是
//! xiaoqvan.html2img（Blitz + Stylo + Vello），浏览器预览只能保证版式接近。

use std::path::PathBuf;

fn main() {
    let mut args = std::env::args().skip(1);
    let mut json_path = PathBuf::from("tests/fixtures/status_article.json");
    let mut out: Option<PathBuf> = None;

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--json" | "-j" => {
                if let Some(path) = args.next() {
                    json_path = PathBuf::from(path);
                }
            }
            other => out = Some(PathBuf::from(other)),
        }
    }

    let Ok(json) = std::fs::read_to_string(&json_path) else {
        eprintln!(
            "读不到 {}（在 crates/fuyubot-plugin-x 下运行）",
            json_path.display()
        );
        std::process::exit(2);
    };

    match fuyubot_plugin_x::preview_card_html(&json) {
        Ok(html) => match out {
            Some(path) => {
                if let Err(error) = std::fs::write(&path, html) {
                    eprintln!("写 {} 失败：{error}", path.display());
                    std::process::exit(1);
                }
                println!("已写出 {}", path.display());
            }
            None => println!("{html}"),
        },
        Err(error) => {
            eprintln!("渲染失败：{error}");
            std::process::exit(1);
        }
    }
}
