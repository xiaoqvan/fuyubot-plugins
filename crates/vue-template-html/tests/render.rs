//! 端到端集成测试：覆盖 README / 任务清单里的全部 P0 + P1 能力。
//!
//! 这些测试只走公开 API（`Template::compile` / `render` / `RenderOptions`），
//! 不碰内部模块——它们就是「别人怎么用这个 crate」的样例。

use serde_json::{Value, json};
use vue_template_html::{
    CompileOptions, Error, ErrorKind, RenderOptions, Sanitizer, Template, UndefinedBehavior,
};

fn render(source: &str, data: Value) -> String {
    Template::compile(source)
        .unwrap_or_else(|error| panic!("编译失败：{error}"))
        .render(&data)
        .unwrap_or_else(|error| panic!("渲染失败：{error}"))
}

fn render_with(source: &str, data: Value, options: &RenderOptions<'_>) -> String {
    Template::compile(source)
        .unwrap_or_else(|error| panic!("编译失败：{error}"))
        .render_with_options(&data, options)
        .unwrap_or_else(|error| panic!("渲染失败：{error}"))
}

fn compile_error(source: &str) -> vue_template_html::Error {
    match Template::compile(source) {
        Ok(_) => panic!("应当编译失败：{source}"),
        Err(error) => error,
    }
}

// ── 基础 HTML 与插值 ─────────────────────────────────────────────

#[test]
fn plain_html_is_preserved() {
    let html = render(
        r#"<div class="card"><span>hello</span><br></div>"#,
        json!({}),
    );
    assert_eq!(html, r#"<div class="card"><span>hello</span><br></div>"#);
}

#[test]
fn interpolation_with_surrounding_text() {
    assert_eq!(
        render("Hello, {{ name }}!", json!({ "name": "Alice" })),
        "Hello, Alice!"
    );
}

#[test]
fn multiple_interpolations() {
    assert_eq!(
        render(
            "<p>{{ first }} + {{ second }} = {{ first + second }}</p>",
            json!({ "first": 1, "second": 2 })
        ),
        "<p>1 + 2 = 3</p>"
    );
}

#[test]
fn interpolation_is_html_escaped() {
    assert_eq!(
        render("<p>{{ value }}</p>", json!({ "value": "<b>&\"'</b>" })),
        "<p>&lt;b&gt;&amp;&quot;&#39;&lt;/b&gt;</p>"
    );
}

#[test]
fn object_property_access() {
    assert_eq!(
        render("{{ user.name }}", json!({ "user": { "name": "Alice" } })),
        "Alice"
    );
}

#[test]
fn array_index_access() {
    assert_eq!(
        render(
            "{{ items[0].name }} / {{ items[1] }}",
            json!({ "items": [{ "name": "A" }, "B"] })
        ),
        "A / B"
    );
}

#[test]
fn deeply_nested_access() {
    assert_eq!(
        render("{{ a.b[0].c }}", json!({ "a": { "b": [{ "c": "deep" }] } })),
        "deep"
    );
}

// ── v-if 系列 ────────────────────────────────────────────────────

#[test]
fn v_if_true_renders_and_false_omits() {
    assert_eq!(
        render(r#"<p v-if="ok">yes</p>"#, json!({ "ok": true })),
        "<p>yes</p>"
    );
    assert_eq!(
        render(r#"<p v-if="ok">yes</p>"#, json!({ "ok": false })),
        ""
    );
}

#[test]
fn v_else_if_chain() {
    let source = r#"<p v-if="n === 1">one</p><p v-else-if="n === 2">two</p><p v-else>many</p>"#;
    assert_eq!(render(source, json!({ "n": 1 })), "<p>one</p>");
    assert_eq!(render(source, json!({ "n": 2 })), "<p>two</p>");
    assert_eq!(render(source, json!({ "n": 3 })), "<p>many</p>");
}

#[test]
fn v_else_branch() {
    assert_eq!(
        render(
            r#"<div v-if="logged"><b>hi</b></div><div v-else><i>guest</i></div>"#,
            json!({ "logged": false })
        ),
        "<div><i>guest</i></div>"
    );
}

#[test]
fn whitespace_and_comments_between_branches_are_allowed() {
    let source = "<p v-if=\"ok\">a</p>\n  <!-- c -->\n  <p v-else>b</p>";
    assert_eq!(render(source, json!({ "ok": false })), "<p>b</p>");
}

#[test]
fn nested_conditions() {
    let source =
        r#"<div v-if="outer"><span v-if="inner">both</span><span v-else>outer only</span></div>"#;
    assert_eq!(
        render(source, json!({ "outer": true, "inner": true })),
        "<div><span>both</span></div>"
    );
    assert_eq!(
        render(source, json!({ "outer": true, "inner": false })),
        "<div><span>outer only</span></div>"
    );
}

// ── v-for ───────────────────────────────────────────────────────

#[test]
fn v_for_array() {
    assert_eq!(
        render(
            r#"<li v-for="item in items">{{ item }}</li>"#,
            json!({ "items": ["a", "b"] })
        ),
        "<li>a</li><li>b</li>"
    );
}

#[test]
fn v_for_array_with_index() {
    assert_eq!(
        render(
            r#"<li v-for="(item, i) in items">{{ i }}:{{ item }}</li>"#,
            json!({ "items": ["a", "b"] })
        ),
        "<li>0:a</li><li>1:b</li>"
    );
}

#[test]
fn v_for_object_value_and_key() {
    // 遍历顺序跟着 serde_json 的 Map 走：默认按键排序，开了 `preserve_order`
    // 就是声明顺序（整个 workspace 一起编时，别的 crate 可能把它打开）。
    let html = render(
        r#"<li v-for="(value, key) in obj">{{ key }}={{ value }}</li>"#,
        json!({ "obj": { "b": 2, "a": 1 } }),
    );
    let mut items: Vec<&str> = html
        .split("</li>")
        .filter(|part| !part.is_empty())
        .map(|part| part.trim_start_matches("<li>"))
        .collect();
    items.sort_unstable();
    assert_eq!(items, ["a=1", "b=2"]);
}

#[test]
fn v_for_object_with_index() {
    assert_eq!(
        render(
            r#"<li v-for="(value, key, i) in obj">{{ i }}{{ key }}{{ value }}</li>"#,
            json!({ "obj": { "x": 1 } })
        ),
        "<li>0x1</li>"
    );
}

#[test]
fn v_for_number_range() {
    assert_eq!(
        render(r#"<i v-for="i in 3">{{ i }}</i>"#, json!({})),
        "<i>1</i><i>2</i><i>3</i>"
    );
    assert_eq!(render(r#"<i v-for="i in 0">{{ i }}</i>"#, json!({})), "");
}

#[test]
fn v_for_negative_or_null_renders_nothing() {
    assert_eq!(
        render(r#"<i v-for="i in n">{{ i }}</i>"#, json!({ "n": -2 })),
        ""
    );
    assert_eq!(
        render(r#"<i v-for="i in n">{{ i }}</i>"#, json!({ "n": null })),
        ""
    );
}

#[test]
fn nested_loops() {
    let source = r#"<div v-for="row in rows"><span v-for="cell in row">{{ cell }}</span></div>"#;
    assert_eq!(
        render(source, json!({ "rows": [[1, 2], [3]] })),
        "<div><span>1</span><span>2</span></div><div><span>3</span></div>"
    );
}

#[test]
fn loop_variables_shadow_outer_and_restore() {
    let source = r#"{{ item }}|<b v-for="item in items">{{ item }}</b>|{{ item }}"#;
    assert_eq!(
        render(source, json!({ "item": "outer", "items": ["a"] })),
        "outer|<b>a</b>|outer"
    );
}

#[test]
fn v_for_key_is_not_rendered() {
    assert_eq!(
        render(
            r#"<li v-for="item in items" :key="item.id">{{ item.name }}</li>"#,
            json!({ "items": [{ "id": 7, "name": "A" }] })
        ),
        "<li>A</li>"
    );
}

#[test]
fn v_if_and_v_for_on_same_element_if_wins() {
    let source = r#"<i v-if="show" v-for="item in items">{{ item }}</i>"#;
    assert_eq!(
        render(source, json!({ "show": false, "items": [1, 2] })),
        ""
    );
    assert_eq!(
        render(source, json!({ "show": true, "items": [1, 2] })),
        "<i>1</i><i>2</i>"
    );
}

// ── v-html / v-text ─────────────────────────────────────────────

#[test]
fn v_html_outputs_raw_html() {
    assert_eq!(
        render(
            r#"<div v-html="content"></div>"#,
            json!({ "content": "<b>bold</b>" })
        ),
        "<div><b>bold</b></div>"
    );
}

#[test]
fn v_html_overrides_children() {
    assert_eq!(
        render(
            r#"<div v-html="content"><span>old</span></div>"#,
            json!({ "content": "<i>new</i>" })
        ),
        "<div><i>new</i></div>"
    );
}

#[test]
fn v_html_can_be_escaped() {
    let options = RenderOptions {
        trusted_html: false,
        ..RenderOptions::default()
    };
    assert_eq!(
        render_with(
            r#"<div v-html="content"></div>"#,
            json!({ "content": "<b>x</b>" }),
            &options
        ),
        "<div>&lt;b&gt;x&lt;/b&gt;</div>"
    );
}

struct Uppercase;

impl Sanitizer for Uppercase {
    fn sanitize(&self, html: &str) -> Result<String, vue_template_html::Error> {
        Ok(html.replace('b', "B"))
    }
}

#[test]
fn v_html_uses_sanitizer() {
    let sanitizer = Uppercase;
    let options = RenderOptions::default().sanitizer(&sanitizer);
    assert_eq!(
        render_with(
            r#"<div v-html="content"></div>"#,
            json!({ "content": "<b>bb</b>" }),
            &options
        ),
        "<div><B>BB</B></div>"
    );
}

#[test]
fn v_text_escapes_and_overrides_children() {
    assert_eq!(
        render(
            r#"<div v-text="content"><span>old</span></div>"#,
            json!({ "content": "<b>x</b>" })
        ),
        "<div>&lt;b&gt;x&lt;/b&gt;</div>"
    );
}

// ── v-bind / class / style ──────────────────────────────────────

#[test]
fn dynamic_attributes() {
    // 属性顺序不影响 HTML 语义：静态属性先输出，动态绑定随后。
    let html = render(
        r#"<img :src="image.url" :width="image.width" alt="cover">"#,
        json!({ "image": { "url": "/a.png", "width": 320 } }),
    );
    assert!(html.contains(r#"src="/a.png""#), "{html}");
    assert!(html.contains(r#"width="320""#), "{html}");
    assert!(html.contains(r#"alt="cover""#), "{html}");
}

#[test]
fn full_v_bind_syntax() {
    assert_eq!(
        render(
            r#"<div v-bind:title="title"></div>"#,
            json!({ "title": "hello" })
        ),
        r#"<div title="hello"></div>"#
    );
}

#[test]
fn null_undefined_and_false_booleans_are_omitted() {
    assert_eq!(
        render(
            r#"<div :title="missing" :hidden="false" :data-x="null"></div>"#,
            json!({})
        ),
        "<div></div>"
    );
}

#[test]
fn boolean_attributes() {
    assert_eq!(
        render(
            r#"<input :disabled="locked" :required="true" :readonly="false">"#,
            json!({ "locked": true })
        ),
        "<input disabled required>"
    );
    assert_eq!(
        render(r#"<input disabled readonly>"#, json!({})),
        "<input disabled readonly>"
    );
}

#[test]
fn attribute_values_are_escaped() {
    assert_eq!(
        render(
            r#"<div :title="quote"></div>"#,
            json!({ "quote": "a \" b & c" })
        ),
        r#"<div title="a &quot; b &amp; c"></div>"#
    );
}

#[test]
fn dynamic_class_string_array_and_object() {
    assert_eq!(
        render(
            r#"<div class="card" :class="extra"></div>"#,
            json!({ "extra": "wide" })
        ),
        r#"<div class="card wide"></div>"#
    );
    assert_eq!(
        render(
            r#"<div :class="['card', extra]"></div>"#,
            json!({ "extra": "wide" })
        ),
        r#"<div class="card wide"></div>"#
    );
    assert_eq!(
        render(
            r#"<div :class="{ active: on, hidden: off }"></div>"#,
            json!({ "on": true, "off": false })
        ),
        r#"<div class="active"></div>"#
    );
}

#[test]
fn dynamic_class_is_not_deduped() {
    // Vue 只做 join，不去重：静态 class 原样保留，动态 class 依次追加。
    assert_eq!(
        render(r#"<div class="a a b" :class="'a c'"></div>"#, json!({})),
        r#"<div class="a a b a c"></div>"#
    );
}

#[test]
fn dynamic_style_object_with_px() {
    assert_eq!(
        render(
            r#"<div :style="{ color: color, fontSize: size + 'px', opacity: 0.5 }"></div>"#,
            json!({ "color": "red", "size": 14 })
        ),
        r#"<div style="color: red; font-size: 14px; opacity: 0.5"></div>"#
    );
}

#[test]
fn dynamic_style_numbers_are_not_auto_px() {
    // Vue 3 不会给数字补 px：需要单位就自己拼 `12 + 'px'`。
    // 属性顺序跟着 serde_json 的 Map（默认按键排序，`preserve_order` 下是声明顺序），
    // 这里只比“有哪些属性”，不锁顺序。
    let html = render(
        r#"<div :style="{ padding: 12, lineHeight: 1.5, zIndex: 3 }"></div>"#,
        json!({}),
    );
    let start = html.find("style=\"").expect("应有 style 属性") + "style=\"".len();
    let end = start + html[start..].find('"').expect("style 应以引号结束");
    let mut props: Vec<&str> = html[start..end].split("; ").collect();
    props.sort_unstable();
    assert_eq!(props, ["line-height: 1.5", "padding: 12", "z-index: 3"]);
}

#[test]
fn dynamic_style_merges_with_static_and_overrides() {
    assert_eq!(
        render(
            r#"<div style="color: red; margin: 0" :style="{ color: 'blue' }"></div>"#,
            json!({})
        ),
        r#"<div style="color: blue; margin: 0"></div>"#
    );
}

#[test]
fn dynamic_style_array_and_string() {
    assert_eq!(
        render(
            r#"<div :style="[{ color: 'red' }, 'font-weight: bold']"></div>"#,
            json!({})
        ),
        r#"<div style="color: red; font-weight: bold"></div>"#
    );
}

// ── v-show ──────────────────────────────────────────────────────

#[test]
fn v_show_false_sets_display_none() {
    assert_eq!(
        render(
            r#"<div v-show="visible">x</div>"#,
            json!({ "visible": false })
        ),
        r#"<div style="display: none">x</div>"#
    );
}

#[test]
fn v_show_true_keeps_element_untouched() {
    assert_eq!(
        render(
            r#"<div v-show="visible">x</div>"#,
            json!({ "visible": true })
        ),
        "<div>x</div>"
    );
}

#[test]
fn v_show_merges_with_existing_style() {
    assert_eq!(
        render(
            r#"<div style="color: red" v-show="false">x</div>"#,
            json!({ "visible": false })
        ),
        r#"<div style="color: red; display: none">x</div>"#
    );
}

// ── template / v-pre / SVG ──────────────────────────────────────

#[test]
fn template_conditional_does_not_emit_tag() {
    let source = r#"<template v-if="visible"><div>a</div><div>b</div></template>"#;
    assert_eq!(
        render(source, json!({ "visible": true })),
        "<div>a</div><div>b</div>"
    );
    assert_eq!(render(source, json!({ "visible": false })), "");
}

#[test]
fn template_loop_does_not_emit_tag() {
    assert_eq!(
        render(
            r#"<template v-for="item in items"><b>{{ item }}</b></template>"#,
            json!({ "items": [1, 2] })
        ),
        "<b>1</b><b>2</b>"
    );
}

#[test]
fn v_pre_keeps_interpolation_and_directives_literal() {
    let html = render(
        r#"<div v-pre>{{ msg }}<span :class="x">a</span></div>"#,
        json!({ "msg": "hi", "x": "y" }),
    );
    assert_eq!(html, r#"<div>{{ msg }}<span :class="x">a</span></div>"#);
}

#[test]
fn svg_tags_and_attributes_keep_case() {
    let source = r#"<svg viewBox="0 0 24 24" preserveAspectRatio="xMidYMid"><linearGradient id="g"><stop offset="0"/></linearGradient><path d="M0 0"/></svg>"#;
    let html = render(source, json!({}));
    assert!(html.contains(r#"viewBox="0 0 24 24""#), "{html}");
    assert!(html.contains(r#"preserveAspectRatio="xMidYMid""#), "{html}");
    assert!(html.contains("<linearGradient"), "{html}");
    assert!(html.contains(r#"<stop offset="0"></stop>"#), "{html}");
    assert!(html.contains(r#"<path d="M0 0"></path>"#), "{html}");
}

#[test]
fn svg_namespaced_attribute() {
    let html = render(r##"<svg><use xlink:href="#icon"/></svg>"##, json!({}));
    assert!(html.contains(r##"xlink:href="#icon""##), "{html}");
}

// ── 表达式 / 错误 / 限额 ────────────────────────────────────────

#[test]
fn ternary_and_logical_expressions() {
    assert_eq!(
        render(
            "{{ ok ? 'yes' : 'no' }}|{{ name || 'anon' }}|{{ a && b }}",
            json!({ "ok": true, "name": "", "a": 1, "b": 2 })
        ),
        "yes|anon|2"
    );
}

#[test]
fn invalid_expression_is_a_compile_error() {
    let error = compile_error("<p>{{ a + }}</p>");
    assert_eq!(error.kind, ErrorKind::Expression, "{error}");
    assert_eq!(error.line, 1);
}

#[test]
fn unsupported_function_call_is_rejected() {
    let error = compile_error("<p>{{ foo() }}</p>");
    assert_eq!(error.kind, ErrorKind::Expression);
    assert!(error.message.contains("函数"), "{error}");
}

#[test]
fn undefined_variable_is_empty_by_default() {
    assert_eq!(render("<p>{{ missing }}</p>", json!({})), "<p></p>");
}

#[test]
fn undefined_variable_can_be_an_error() {
    let template = Template::compile("<p>{{ missing }}</p>").unwrap();
    let options = RenderOptions::default().strict_undefined();
    let error = template
        .render_with_options(&json!({}), &options)
        .expect_err("应当报错");
    assert_eq!(error.kind, ErrorKind::Render);
    assert!(error.message.contains("missing"), "{error}");
}

#[test]
fn max_iterations_is_enforced() {
    let template = Template::compile(r#"<i v-for="i in n">{{ i }}</i>"#).unwrap();
    let options = RenderOptions::default().max_iterations(3);
    let error = template
        .render_with_options(&json!({ "n": 10 }), &options)
        .expect_err("应当超限");
    assert_eq!(error.kind, ErrorKind::Limit);
}

#[test]
fn v_for_type_error() {
    let template = Template::compile(r#"<i v-for="i in n">{{ i }}</i>"#).unwrap();
    let error = template
        .render(&json!({ "n": "not-a-list" }))
        .expect_err("字符串不能 v-for");
    assert_eq!(error.kind, ErrorKind::Render);
}

// ── 其它 HTML 正确性 ────────────────────────────────────────────

#[test]
fn comments_are_dropped() {
    assert_eq!(render("a<!-- hidden -->b", json!({})), "ab");
}

#[test]
fn entities_round_trip() {
    assert_eq!(render("a &amp; b &lt; c", json!({})), "a &amp; b &lt; c");
}

#[test]
fn script_and_style_content_is_not_escaped() {
    let html = render(
        r#"<style>.a > .b { color: red }</style><script>if (a < b) { x() }</script>"#,
        json!({}),
    );
    assert_eq!(
        html,
        r#"<style>.a > .b { color: red }</style><script>if (a < b) { x() }</script>"#
    );
}

#[test]
fn void_elements_have_no_closing_tag() {
    assert_eq!(
        render(r#"<div><img src="a"><br><input></div>"#, json!({})),
        r#"<div><img src="a"><br><input></div>"#
    );
}

#[test]
fn self_closing_elements_get_explicit_close_tag() {
    // 模板里可以写 `<div/>`，但 HTML 输出必须是 `<div></div>`。
    assert_eq!(render("<div/>", json!({})), "<div></div>");
}

#[test]
fn compiled_template_can_render_multiple_data_sets() {
    let template = Template::compile("<p>{{ name }}</p>").unwrap();
    assert_eq!(
        template.render(&json!({ "name": "a" })).unwrap(),
        "<p>a</p>"
    );
    assert_eq!(
        template.render(&json!({ "name": "b" })).unwrap(),
        "<p>b</p>"
    );
}

#[test]
fn numbers_use_javascript_formatting() {
    assert_eq!(
        render("{{ 2.0 }}|{{ 1.5 }}|{{ 1 / 2 }}", json!({})),
        "2|1.5|0.5"
    );
}

#[test]
fn multiple_root_nodes_are_all_rendered() {
    assert_eq!(render("<b>a</b><i>b</i>", json!({})), "<b>a</b><i>b</i>");
}

#[test]
fn undefined_behavior_enum_is_public() {
    // 确保 API 名字是稳定的。
    let _ = UndefinedBehavior::Lenient;
    let _ = UndefinedBehavior::Error;
}

#[test]
fn textarea_supports_interpolation() {
    // textarea 是 RCDATA：不解析标签，但插值照常工作（Vue 的常见写法）。
    assert_eq!(
        render(
            "<textarea>{{ value }}</textarea>",
            json!({ "value": "a < b & c" })
        ),
        "<textarea>a &lt; b &amp; c</textarea>"
    );
}

#[test]
fn template_can_be_else_branch() {
    assert_eq!(
        render(
            r#"<div v-if="ok">A</div><template v-else><b>B</b><i>C</i></template>"#,
            json!({ "ok": false })
        ),
        "<b>B</b><i>C</i>"
    );
}

#[test]
fn v_pre_skips_directives_too() {
    let html = render(
        r#"<div v-pre><span v-for="x in xs">{{ x }}</span></div>"#,
        json!({ "xs": [1, 2] }),
    );
    assert_eq!(html, r#"<div><span v-for="x in xs">{{ x }}</span></div>"#);
}

#[test]
fn strict_mode_rejects_mustache_in_attributes() {
    let error = Template::compile_with_options(
        r#"<div title="{{ x }}"></div>"#,
        CompileOptions { strict: true },
    )
    .expect_err("属性 mustache 在严格模式应报错");
    assert_eq!(error.kind, ErrorKind::Unsupported, "{error}");
}

#[test]
fn strict_mode_rejects_unknown_directives() {
    let error = Template::compile_with_options(
        r#"<div v-model="x"></div>"#,
        CompileOptions { strict: true },
    )
    .expect_err("v-model 应报错");
    assert_eq!(error.kind, ErrorKind::Unsupported, "{error}");

    // 非严格模式：静默丢弃，不影响其余渲染。
    assert_eq!(
        render(r#"<div v-model="x">kept</div>"#, json!({})),
        "<div>kept</div>"
    );
}

#[test]
fn missing_object_property_is_empty() {
    assert_eq!(render("{{ user.missing.deep }}", json!({ "user": {} })), "");
}

struct Reject;

impl Sanitizer for Reject {
    fn sanitize(&self, _html: &str) -> Result<String, Error> {
        Err(Error::custom(ErrorKind::Sanitize, "不允许 HTML"))
    }
}

#[test]
fn sanitizer_error_becomes_sanitize_error() {
    let sanitizer = Reject;
    let template = Template::compile(r#"<div v-html="x"></div>"#).unwrap();
    let options = RenderOptions::default().sanitizer(&sanitizer);
    let error = template
        .render_with_options(&json!({ "x": "<b>x</b>" }), &options)
        .expect_err("sanitizer 拒绝应终止渲染");
    assert_eq!(error.kind, ErrorKind::Sanitize);
    assert!(error.message.contains("不允许"), "{error}");
}

#[test]
fn empty_template_renders_empty_string() {
    assert_eq!(render("", json!({})), "");
}

#[test]
fn template_is_send_and_sync() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<Template>();
    assert_send_sync::<Error>();
}

#[test]
fn empty_interpolation_renders_empty() {
    assert_eq!(render("a{{}}b{{  }}c", json!({})), "abc");
}

#[test]
fn object_literal_inside_interpolation() {
    assert_eq!(render("{{ { a: 1 }.a }}", json!({})), "1");
}

#[test]
fn stray_end_tag_start_is_text() {
    // `<` 只有在后跟 `/` + 字母时才是结束标签。
    assert_eq!(render("a </ b", json!({})), "a &lt;/ b");
}

#[test]
fn bare_template_is_also_a_logical_container() {
    // 任务要求：`<template>` 本身不出现在最终 HTML 里（哪怕没有指令）。
    assert_eq!(
        render(
            r#"<div><template class="x"><b>a</b></template></div>"#,
            json!({})
        ),
        "<div><b>a</b></div>"
    );
}
