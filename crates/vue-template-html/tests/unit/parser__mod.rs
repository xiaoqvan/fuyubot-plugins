//! 解析器的单元测试：覆盖节点切分、属性、元素规则、`v-pre`、实体与错误位置。

use super::parse;
use crate::error::{Error, ErrorKind};
use crate::syntax::{Element, Interpolation, Node, Root, Span, Text};

fn parse_ok(source: &str) -> Root {
    parse(source).expect("模板应当解析成功")
}

fn parse_err(source: &str) -> Error {
    parse(source).expect_err("模板应当解析失败")
}

fn element(node: &Node) -> &Element {
    match node {
        Node::Element(element) => element,
        other => panic!("期望元素节点，实际是 {other:?}"),
    }
}

fn text(node: &Node) -> &Text {
    match node {
        Node::Text(text) => text,
        other => panic!("期望文本节点，实际是 {other:?}"),
    }
}

fn interpolation(node: &Node) -> &Interpolation {
    match node {
        Node::Interpolation(interpolation) => interpolation,
        other => panic!("期望插值节点，实际是 {other:?}"),
    }
}

/// 递归检查子树里是否存在插值节点。
fn has_interpolation(nodes: &[Node]) -> bool {
    nodes.iter().any(|node| match node {
        Node::Interpolation(_) => true,
        Node::Element(element) => has_interpolation(&element.children),
        Node::Text(_) | Node::Comment(_) => false,
    })
}

#[test]
fn empty_source() {
    let root = parse_ok("");
    assert!(root.children.is_empty());
    assert_eq!(root.span, Span::new(0, 0));
}

#[test]
fn pure_text() {
    let source = "hello 世界";
    let root = parse_ok(source);
    assert_eq!(root.children.len(), 1);
    let text = text(&root.children[0]);
    assert_eq!(text.content, source);
    assert_eq!(text.span, Span::new(0, source.len()));
    assert_eq!(root.span, Span::new(0, source.len()));
}

#[test]
fn mixed_text_and_interpolation() {
    let source = "Hello, {{ name }}!";
    let root = parse_ok(source);
    assert_eq!(root.children.len(), 3);

    let first = text(&root.children[0]);
    assert_eq!(first.content, "Hello, ");
    assert_eq!(first.span, Span::new(0, 7));

    let middle = interpolation(&root.children[1]);
    assert_eq!(middle.raw, " name ");
    assert_eq!(middle.raw_start, 9);
    assert_eq!(middle.span, Span::new(7, 17));

    let last = text(&root.children[2]);
    assert_eq!(last.content, "!");
    assert_eq!(last.span, Span::new(17, 18));
}

#[test]
fn multiple_interpolations() {
    let root = parse_ok("{{a}}-{{ b }}-{{c}}");
    assert_eq!(root.children.len(), 5);
    assert_eq!(interpolation(&root.children[0]).raw, "a");
    assert_eq!(text(&root.children[1]).content, "-");
    assert_eq!(interpolation(&root.children[2]).raw, " b ");
    assert_eq!(text(&root.children[3]).content, "-");
    assert_eq!(interpolation(&root.children[4]).raw, "c");
}

#[test]
fn interpolation_with_object_literal() {
    let source = "{{ {a: 1} }}";
    let root = parse_ok(source);
    assert_eq!(root.children.len(), 1);
    let node = interpolation(&root.children[0]);
    assert_eq!(node.raw, " {a: 1} ");
    assert_eq!(node.raw_start, 2);
    assert_eq!(node.span, Span::new(0, source.len()));
}

#[test]
fn interpolation_with_object_literal_in_call() {
    let source = "{{ fn({a: 1}) }}";
    let root = parse_ok(source);
    let node = interpolation(&root.children[0]);
    assert_eq!(node.raw, " fn({a: 1}) ");
    assert_eq!(node.span, Span::new(0, source.len()));
}

#[test]
fn interpolation_skips_strings_with_braces() {
    let source = r#"{{ "}}" }}"#;
    let root = parse_ok(source);
    let node = interpolation(&root.children[0]);
    assert_eq!(node.raw, r#" "}}" "#);
    assert_eq!(node.span, Span::new(0, source.len()));

    let root = parse_ok("{{ 'a' + '}}' }}");
    let node = interpolation(&root.children[0]);
    assert_eq!(node.raw, " 'a' + '}}' ");
}

#[test]
fn nested_elements_and_spans() {
    let source = "<div><span>x</span></div>";
    let root = parse_ok(source);
    assert_eq!(root.children.len(), 1);

    let outer = element(&root.children[0]);
    assert_eq!(outer.tag, "div");
    assert!(!outer.self_closing);
    assert_eq!(outer.span, Span::new(0, source.len()));
    assert_eq!(outer.children.len(), 1);

    let inner = element(&outer.children[0]);
    assert_eq!(inner.tag, "span");
    assert_eq!(inner.span, Span::new(5, 19));
    assert_eq!(text(&inner.children[0]).content, "x");
}

#[test]
fn template_is_a_normal_element() {
    let root = parse_ok("<template><p>x</p></template>");
    let template = element(&root.children[0]);
    assert_eq!(template.tag, "template");
    assert_eq!(template.children.len(), 1);
    assert_eq!(element(&template.children[0]).tag, "p");
}

#[test]
fn attribute_quotes_and_spans() {
    let root = parse_ok(r#"<div class="a" title='b' data-x=c></div>"#);
    let div = element(&root.children[0]);
    assert_eq!(div.attrs.len(), 3);

    assert_eq!(div.attrs[0].name, "class");
    assert_eq!(div.attrs[0].value.as_deref(), Some("a"));
    assert_eq!(div.attrs[0].span, Span::new(5, 14));

    assert_eq!(div.attrs[1].name, "title");
    assert_eq!(div.attrs[1].value.as_deref(), Some("b"));
    assert_eq!(div.attrs[1].span, Span::new(15, 24));

    assert_eq!(div.attrs[2].name, "data-x");
    assert_eq!(div.attrs[2].value.as_deref(), Some("c"));
    assert_eq!(div.attrs[2].span, Span::new(25, 33));
}

#[test]
fn quoted_value_may_contain_angle_brackets() {
    let source = r#"<div title="a > b">x</div>"#;
    let root = parse_ok(source);
    let div = element(&root.children[0]);
    assert_eq!(div.attrs[0].value.as_deref(), Some("a > b"));
    assert_eq!(text(&div.children[0]).content, "x");
    assert_eq!(div.span, Span::new(0, source.len()));
}

#[test]
fn boolean_attributes() {
    let source = "<input disabled required>";
    let root = parse_ok(source);
    let input = element(&root.children[0]);
    assert_eq!(input.attrs.len(), 2);
    assert_eq!(input.attrs[0].name, "disabled");
    assert!(input.attrs[0].value.is_none());
    assert_eq!(input.attrs[1].name, "required");
    assert!(input.attrs[1].value.is_none());
    assert!(input.self_closing);
    assert_eq!(input.span, Span::new(0, source.len()));
}

#[test]
fn directive_attributes_keep_their_names() {
    let source = concat!(
        r#"<button :class="cls" @click.stop="go" #slot v-bind:src="url" "#,
        r#"[dynamic]="d" v-if="ok">x</button>"#
    );
    let root = parse_ok(source);
    let button = element(&root.children[0]);
    let names: Vec<&str> = button.attrs.iter().map(|attr| attr.name.as_str()).collect();
    assert_eq!(
        names,
        [
            ":class",
            "@click.stop",
            "#slot",
            "v-bind:src",
            "[dynamic]",
            "v-if"
        ]
    );
    assert_eq!(button.attrs[0].value.as_deref(), Some("cls"));
    assert_eq!(button.attrs[1].value.as_deref(), Some("go"));
    assert!(button.attrs[2].value.is_none());
    assert_eq!(button.attrs[3].value.as_deref(), Some("url"));
    assert_eq!(button.attrs[4].value.as_deref(), Some("d"));
    // v-if 在这一层只是一个普通属性，语义留给 transform。
    assert_eq!(button.attrs[5].value.as_deref(), Some("ok"));
}

#[test]
fn void_elements_have_no_children_and_no_end_tag() {
    let source = r#"<div><br><img src="x"><hr></div>"#;
    let root = parse_ok(source);
    let div = element(&root.children[0]);
    assert_eq!(div.children.len(), 3);
    let tags: Vec<&str> = div
        .children
        .iter()
        .map(|node| element(node).tag.as_str())
        .collect();
    assert_eq!(tags, ["br", "img", "hr"]);
    for child in &div.children {
        let element = element(child);
        assert!(element.self_closing);
        assert!(element.children.is_empty());
    }
    assert_eq!(
        element(&div.children[1]).attrs[0].value.as_deref(),
        Some("x")
    );
}

#[test]
fn explicit_self_closing() {
    let source = "<div/><span />";
    let root = parse_ok(source);
    assert_eq!(root.children.len(), 2);
    let div = element(&root.children[0]);
    assert_eq!(div.tag, "div");
    assert!(div.self_closing);
    assert_eq!(div.span, Span::new(0, 6));
    let span = element(&root.children[1]);
    assert_eq!(span.tag, "span");
    assert!(span.self_closing);
    assert_eq!(span.span, Span::new(6, source.len()));
}

#[test]
fn whitespace_inside_tags() {
    let source = "<div\n  class = \"a\"\n>x</div>";
    let root = parse_ok(source);
    let div = element(&root.children[0]);
    assert_eq!(div.tag, "div");
    assert_eq!(div.attrs.len(), 1);
    assert_eq!(div.attrs[0].name, "class");
    assert_eq!(div.attrs[0].value.as_deref(), Some("a"));
}

#[test]
fn comments_are_preserved() {
    let source = "a<!-- hi -->b";
    let root = parse_ok(source);
    assert_eq!(root.children.len(), 3);
    assert_eq!(text(&root.children[0]).content, "a");
    match &root.children[1] {
        Node::Comment(comment) => {
            assert_eq!(comment.content, " hi ");
            assert_eq!(comment.span, Span::new(1, 12));
        }
        other => panic!("期望注释节点，实际是 {other:?}"),
    }
    assert_eq!(text(&root.children[2]).content, "b");
}

#[test]
fn comment_content_is_not_parsed() {
    let root = parse_ok("<!-- {{ x }} <div> -->");
    assert_eq!(root.children.len(), 1);
    assert!(!has_interpolation(&root.children));
}

#[test]
fn doctype_is_skipped() {
    let source = "<!DOCTYPE html><div>x</div>";
    let root = parse_ok(source);
    assert_eq!(root.children.len(), 1);
    let div = element(&root.children[0]);
    assert_eq!(div.tag, "div");
    assert_eq!(div.span, Span::new(15, source.len()));
}

#[test]
fn cdata_content_becomes_text() {
    let source = "<![CDATA[a < b & c]]>";
    let root = parse_ok(source);
    assert_eq!(root.children.len(), 1);
    let text = text(&root.children[0]);
    assert_eq!(text.content, "a < b & c");
    assert_eq!(text.span, Span::new(9, 18));
}

#[test]
fn script_content_is_raw_text() {
    let source = "<script>if (a < b) { {{x}} }</script>";
    let root = parse_ok(source);
    let script = element(&root.children[0]);
    assert_eq!(script.tag, "script");
    assert_eq!(script.span, Span::new(0, source.len()));
    assert_eq!(script.children.len(), 1);
    assert_eq!(text(&script.children[0]).content, "if (a < b) { {{x}} }");
    assert!(!has_interpolation(&root.children));
}

#[test]
fn style_is_raw_text_and_textarea_is_rcdata() {
    let style_source = r#"<style>.a::before { content: "<b>"; }</style>"#;
    let style_root = parse_ok(style_source);
    let style = element(&style_root.children[0]);
    assert_eq!(style.tag, "style");
    assert_eq!(
        text(&style.children[0]).content,
        r#".a::before { content: "<b>"; }"#
    );

    // textarea 是 RCDATA：`<b>` 是文本，但 `{{ x }}` 仍然是插值。
    let textarea_source = "<textarea><b>{{ x }}</b></textarea>";
    let textarea_root = parse_ok(textarea_source);
    let textarea = element(&textarea_root.children[0]);
    assert_eq!(textarea.tag, "textarea");
    assert_eq!(textarea.children.len(), 3);
    assert_eq!(text(&textarea.children[0]).content, "<b>");
    assert!(has_interpolation(&textarea.children));
    assert_eq!(text(&textarea.children[2]).content, "</b>");
}

#[test]
fn v_pre_keeps_interpolation_as_text() {
    let root = parse_ok("<div v-pre>{{ raw }}<span>{{ inner }}</span></div>");
    let div = element(&root.children[0]);
    assert_eq!(div.attrs[0].name, "v-pre");
    assert!(div.attrs[0].value.is_none());
    assert!(!has_interpolation(&root.children));
    assert_eq!(div.children.len(), 2);
    assert_eq!(text(&div.children[0]).content, "{{ raw }}");

    let span = element(&div.children[1]);
    assert_eq!(text(&span.children[0]).content, "{{ inner }}");
}

#[test]
fn v_pre_is_inherited_by_descendants() {
    let root = parse_ok("<section><div v-pre><p><b>{{ x }}</b></p></div><p>{{ y }}</p></section>");
    let section = element(&root.children[0]);
    assert_eq!(section.children.len(), 2);

    let div = element(&section.children[0]);
    assert!(!has_interpolation(&div.children));
    let p = element(&div.children[0]);
    let b = element(&p.children[0]);
    assert_eq!(text(&b.children[0]).content, "{{ x }}");

    let sibling = element(&section.children[1]);
    let node = sibling
        .children
        .iter()
        .find_map(|node| match node {
            Node::Interpolation(interpolation) => Some(interpolation),
            Node::Element(_) | Node::Text(_) | Node::Comment(_) => None,
        })
        .expect("v-pre 只影响自己的子树");
    assert_eq!(node.raw, " y ");
}

#[test]
fn v_pre_only_affects_its_own_subtree() {
    let root = parse_ok("<div v-pre>{{a}}</div><div>{{b}}</div>");
    let first = element(&root.children[0]);
    assert_eq!(text(&first.children[0]).content, "{{a}}");
    assert!(!has_interpolation(&first.children));

    let second = element(&root.children[1]);
    assert!(has_interpolation(&second.children));
    assert_eq!(interpolation(&second.children[0]).raw, "b");
}

#[test]
fn tag_case_is_preserved_but_end_tag_match_is_case_insensitive() {
    let source = "<MyComp></mycomp>";
    let root = parse_ok(source);
    let element = element(&root.children[0]);
    assert_eq!(element.tag, "MyComp");
    assert_eq!(element.span, Span::new(0, source.len()));
}

#[test]
fn ordinary_text_is_entity_decoded_but_raw_text_is_not() {
    let source = "a &amp; b &lt; c &#65;";
    let root = parse_ok(source);
    let node = text(&root.children[0]);
    assert_eq!(node.content, "a & b < c A");
    assert_eq!(node.span, Span::new(0, source.len()));

    // 原始文本元素保持字节原样：序列化时才统一转义。
    let source = "<script>a &amp; b &lt; c</script>";
    let root = parse_ok(source);
    let script = element(&root.children[0]);
    assert_eq!(text(&script.children[0]).content, "a &amp; b &lt; c");
}

#[test]
fn named_entities_are_decoded() {
    let source = concat!(
        r#"<div title="a &amp; b &lt; c &gt; d &quot;e&quot; &#39;f&#39; "#,
        r#"&nbsp; &copy; &hellip; &mdash; &ndash; &times; &divide; &laquo; &raquo;">x</div>"#
    );
    let root = parse_ok(source);
    let div = element(&root.children[0]);
    assert_eq!(
        div.attrs[0].value.as_deref(),
        Some("a & b < c > d \"e\" 'f' \u{a0} © … — – × ÷ « »")
    );
}

#[test]
fn numeric_entities_are_decoded() {
    let root = parse_ok(r#"<div title="&#65;&#x42;&#X43;&#x1f600;&#20320;"></div>"#);
    let div = element(&root.children[0]);
    assert_eq!(div.attrs[0].value.as_deref(), Some("ABC😀你"));
}

#[test]
fn unknown_and_invalid_entities_are_preserved() {
    let source = r#"<p title="&unknown; &amp;lt; &bogus &#xZZ; &#;"></p>"#;
    let root = parse_ok(source);
    let p = element(&root.children[0]);
    assert_eq!(
        p.attrs[0].value.as_deref(),
        Some("&unknown; &lt; &bogus &#xZZ; &#;")
    );
}

#[test]
fn unquoted_attribute_values() {
    let root = parse_ok("<input type=text value=42>");
    let input = element(&root.children[0]);
    assert_eq!(input.attrs[0].value.as_deref(), Some("text"));
    assert_eq!(input.attrs[1].value.as_deref(), Some("42"));
}

#[test]
fn unquoted_value_stops_at_slash() {
    // 按解析器约定：无引号值到空白 / `>` / `/` 为止，`/>` 仍能正确识别为自闭合。
    let source = "<div data-a=b/>";
    let root = parse_ok(source);
    let div = element(&root.children[0]);
    assert_eq!(div.attrs[0].name, "data-a");
    assert_eq!(div.attrs[0].value.as_deref(), Some("b"));
    assert!(div.self_closing);
    assert_eq!(div.span, Span::new(0, source.len()));
}

#[test]
fn raw_text_close_tag_is_case_insensitive() {
    let source = "<ScRiPt>a < b</SCRIPT>";
    let root = parse_ok(source);
    let script = element(&root.children[0]);
    assert_eq!(script.tag, "ScRiPt");
    assert_eq!(text(&script.children[0]).content, "a < b");
    assert!(!has_interpolation(&root.children));
}

#[test]
fn unclosed_tag_reports_position() {
    let error = parse_err("<div><span>");
    assert_eq!(error.kind, ErrorKind::Parse);
    assert_eq!(error.offset, 5);
    assert_eq!((error.line, error.column), (1, 6));
    assert!(error.message.contains("未闭合的标签"));
    assert!(error.message.contains("span"));
}

#[test]
fn mismatched_end_tag_reports_position() {
    let error = parse_err("<div>\n</span>");
    assert_eq!(error.kind, ErrorKind::Parse);
    assert_eq!(error.offset, 6);
    assert_eq!((error.line, error.column), (2, 1));
    assert_eq!(error.message, "结束标签 `</span>` 与 `<div>` 不匹配");
}

#[test]
fn stray_end_tag_is_an_error() {
    let error = parse_err("</div>");
    assert_eq!(error.kind, ErrorKind::Parse);
    assert_eq!(error.offset, 0);
    assert!(error.message.contains("多余的结束标签"));
}

#[test]
fn unclosed_interpolation_is_an_error() {
    let error = parse_err("Hello {{ name");
    assert_eq!(error.kind, ErrorKind::Parse);
    assert_eq!(error.offset, 6);
    assert_eq!((error.line, error.column), (1, 7));
    assert_eq!(error.message, "未闭合的插值");
}

#[test]
fn unclosed_attribute_quote_is_an_error() {
    let error = parse_err(r#"<div class="a>"#);
    assert_eq!(error.kind, ErrorKind::Parse);
    assert_eq!(error.offset, 11);
    assert_eq!((error.line, error.column), (1, 12));
    assert_eq!(error.message, "属性值引号未闭合");
}

#[test]
fn unclosed_comment_is_an_error() {
    let error = parse_err("<!-- oops");
    assert_eq!(error.kind, ErrorKind::Parse);
    assert_eq!(error.offset, 0);
    assert_eq!(error.message, "注释未闭合");
}

#[test]
fn missing_attribute_name_is_an_error() {
    let error = parse_err("<div =x></div>");
    assert_eq!(error.kind, ErrorKind::Parse);
    assert_eq!(error.offset, 5);
    assert_eq!(error.message, "属性名缺失");
}

#[test]
fn multiline_error_positions() {
    let error = parse_err("<div>\n  <span>\n</div>");
    assert_eq!(error.offset, 15);
    assert_eq!((error.line, error.column), (3, 1));
    assert_eq!(error.message, "结束标签 `</div>` 与 `<span>` 不匹配");
}

#[test]
fn stray_less_than_is_text() {
    let source = "a < b {{ c }}";
    let root = parse_ok(source);
    assert_eq!(root.children.len(), 2);
    assert_eq!(text(&root.children[0]).content, "a < b ");
    let node = interpolation(&root.children[1]);
    assert_eq!(node.raw, " c ");
    assert_eq!(node.span, Span::new(6, source.len()));
}

#[test]
fn empty_element_has_no_children() {
    let source = "<div></div>";
    let root = parse_ok(source);
    let div = element(&root.children[0]);
    assert!(div.children.is_empty());
    assert_eq!(div.span, Span::new(0, source.len()));
}

#[test]
fn end_tag_allows_trailing_whitespace() {
    let source = "<div>x</div >";
    let root = parse_ok(source);
    let div = element(&root.children[0]);
    assert_eq!(div.span, Span::new(0, source.len()));
    assert_eq!(text(&div.children[0]).content, "x");
}
