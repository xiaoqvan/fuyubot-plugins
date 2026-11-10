//! `/help` 图片版的 Vue 模板。
//!
//! 模板与样式放在 `assets/help.html`，编译期 `include_str!` 进动态库，运行时不需要
//! 额外的资源文件。交给 `fuyubot.html2img` 时带 `vue: true`，由它用
//! `vue-template-html` 编译、用本插件拼好的命令数据渲染。
//!
//! 样式是手写的（不是 Tailwind）。`hyper-render` 不跑 JS、也没有 Tailwind 运行时，
//! 所以只保留模板里用到的那一组布局 / 颜色规则，并把 `body` 的内边距压成 0，
//! 让渐变背景铺满整张图。

/// 原样交给 html2img 的模板（含 `<style>` 和根节点）。
pub const TEMPLATE: &str = include_str!("../assets/help.html");

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn template_compiles_and_renders_command_table() {
        let template = vue_template_html::Template::compile(TEMPLATE).expect("模板应能编译");
        let html = template
            .render(&json!({
                "title": "可用命令",
                "description": "FuyuBot 命令总览",
                "tips": "发送 /help 随时刷新",
                "version": "v0.1.0",
                "imgSrc": "",
                "data": [{
                    "name": "全部命令",
                    "desc": "共 2 条命令",
                    "commands": [
                        { "name": "help", "desc": "这条帮助" },
                        { "name": "start", "desc": "开始使用" },
                    ],
                }],
            }))
            .expect("模板应能渲染");

        assert!(html.contains("help-root"), "{html}");
        assert!(html.contains("可用命令"), "{html}");
        assert!(html.contains("/help"), "{html}");
        assert!(html.contains("/start"), "{html}");
        assert!(!html.contains("{{"), "不应残留插值：{html}");
    }

    #[test]
    fn empty_command_list_shows_placeholder() {
        let template = vue_template_html::Template::compile(TEMPLATE).expect("模板应能编译");
        let html = template
            .render(&json!({
                "title": "可用命令",
                "description": "",
                "tips": "",
                "version": "v0.1.0",
                "imgSrc": "",
                "data": [{ "name": "全部命令", "desc": "", "commands": [] }],
            }))
            .expect("空命令表也应能渲染");
        assert!(html.contains("暂无命令"), "{html}");
    }
}
