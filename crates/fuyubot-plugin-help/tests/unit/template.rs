use serde_json::json;

use super::*;

#[test]
fn template_compiles_and_renders_command_table() {
    let template = vue_template_html::Template::compile(TEMPLATE).expect("模板应能编译");
    let html = template
        .render(&json!({
            "title": "可用命令",
            "description": "FuyuBot 命令总览",
            "tips": "可使用 /help t 获取文本版本",
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
