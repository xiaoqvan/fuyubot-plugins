use super::*;

#[test]
fn escape_text_covers_all_five_characters() {
    assert_eq!(
        escape_text("&<>\"'"),
        "&amp;&lt;&gt;&quot;&#39;",
        "五个特殊字符各自对应的实体"
    );
}

#[test]
fn escape_text_leaves_plain_text_untouched() {
    let plain = "Hello, 世界 🌍 — 123";
    assert_eq!(escape_text(plain), plain);
}

#[test]
fn escape_text_escapes_only_once() {
    // 输入是「已经解码」的字符串：字面上的 `&amp;` 必须整体再转义一次，
    // 才不会被浏览器解析成 `&`。这不是二次转义。
    assert_eq!(escape_text("&amp;"), "&amp;amp;");
    assert_eq!(escape_text("&lt;b&gt;"), "&amp;lt;b&amp;gt;");
    assert_eq!(escape_text("Tom & Jerry"), "Tom &amp; Jerry");
}

#[test]
fn escape_text_handles_empty_string() {
    assert_eq!(escape_text(""), "");
}

#[test]
fn escape_text_handles_string_edges() {
    // 首字符、尾字符、连续替换三种边界（内部是按区间整段拷贝的）。
    assert_eq!(escape_text("<abc"), "&lt;abc");
    assert_eq!(escape_text("abc<"), "abc&lt;");
    assert_eq!(escape_text("<<>>"), "&lt;&lt;&gt;&gt;");
    assert_eq!(escape_text("中<文"), "中&lt;文");
}

#[test]
fn escape_attribute_escapes_both_quote_styles() {
    assert_eq!(
        escape_attribute(r#"a"b'c"#),
        "a&quot;b&#39;c",
        "双引号包裹的属性值也要转义单引号"
    );
    assert_eq!(escape_attribute("x & y"), "x &amp; y");
}

#[test]
fn text_writes_escaped_content() {
    let mut html = Html::new();
    html.text("<b>hi</b>");
    assert_eq!(html.as_str(), "&lt;b&gt;hi&lt;/b&gt;");
}

#[test]
fn text_writes_plain_unicode_verbatim() {
    let mut html = Html::new();
    // 非 ASCII 字符原样输出，只有 `&` 需要转义。
    html.text("中文 & 表情 🎉 都不该受影响");
    html.text("，只有 & 会变");
    assert_eq!(
        html.as_str(),
        "中文 &amp; 表情 🎉 都不该受影响，只有 &amp; 会变"
    );
}

#[test]
fn raw_does_not_escape() {
    let mut html = Html::new();
    html.raw("<b>&amp;</b>");
    assert_eq!(html.as_str(), "<b>&amp;</b>", "raw 只给可信 v-html");
}

#[test]
fn start_tag_without_attributes() {
    let mut html = Html::new();
    html.start_tag("div", &[], false);
    assert_eq!(html.as_str(), "<div>");
}

#[test]
fn start_tag_with_attributes() {
    let mut html = Html::new();
    html.start_tag(
        "a",
        &[Attr::new("href", "/x?a=1&b=2"), Attr::new("class", "link")],
        false,
    );
    assert_eq!(html.as_str(), r#"<a href="/x?a=1&amp;b=2" class="link">"#);
}

#[test]
fn start_tag_escapes_quotes_in_attribute_values() {
    let mut html = Html::new();
    html.start_tag("div", &[Attr::new("title", r#"say "hi" & don't"#)], false);
    assert_eq!(
        html.as_str(),
        r#"<div title="say &quot;hi&quot; &amp; don&#39;t">"#
    );
}

#[test]
fn start_tag_outputs_boolean_attributes() {
    let mut html = Html::new();
    html.start_tag(
        "input",
        &[Attr::boolean("disabled"), Attr::boolean("required")],
        false,
    );
    assert_eq!(html.as_str(), "<input disabled required>");
}

#[test]
fn void_element_never_self_closes() {
    let mut html = Html::new();
    html.start_tag("br", &[], true);
    assert_eq!(html.as_str(), "<br>", "void 元素忽略 self_closing");

    let mut html = Html::new();
    html.start_tag("img", &[Attr::new("src", "a.png")], true);
    assert_eq!(html.as_str(), r#"<img src="a.png">"#);
}

#[test]
fn end_tag_skips_void_elements() {
    let mut html = Html::new();
    html.start_tag("br", &[], false);
    html.end_tag("br");
    html.end_tag("BR");
    html.end_tag("input");
    assert_eq!(html.as_str(), "<br>", "void 元素不输出结束标签");

    html.end_tag("div");
    assert_eq!(html.as_str(), "<br></div>", "非 void 元素照常输出");
}

#[test]
fn self_closing_non_void_element_opens_normally() {
    let mut html = Html::new();
    html.start_tag("div", &[], true);
    html.end_tag("div");
    assert_eq!(html.as_str(), "<div></div>", "HTML 里 non-void 不能自闭合");
}

#[test]
fn self_closing_with_attributes() {
    let mut html = Html::new();
    html.start_tag("path", &[Attr::new("d", "M0 0 L10 10")], true);
    html.end_tag("path");
    assert_eq!(html.as_str(), r#"<path d="M0 0 L10 10"></path>"#);
}

#[test]
fn empty_attribute_value_keeps_quotes() {
    let mut html = Html::new();
    html.start_tag("div", &[Attr::new("data-x", "")], false);
    assert_eq!(html.as_str(), r#"<div data-x="">"#);
}

#[test]
fn svg_tag_and_attribute_case_is_preserved() {
    let mut html = Html::new();
    html.start_tag(
        "svg",
        &[
            Attr::new("viewBox", "0 0 24 24"),
            Attr::new("preserveAspectRatio", "xMidYMid meet"),
        ],
        false,
    );
    html.start_tag("linearGradient", &[], false);
    html.end_tag("linearGradient");
    html.end_tag("svg");
    assert_eq!(
        html.as_str(),
        concat!(
            r#"<svg viewBox="0 0 24 24" preserveAspectRatio="xMidYMid meet">"#,
            "<linearGradient></linearGradient>",
            "</svg>",
        ),
        "SVG 名字一个字母都不许小写化"
    );
}

#[test]
fn svg_namespaced_attribute_is_not_lowercased() {
    let mut html = Html::new();
    html.start_tag("use", &[Attr::new("xlink:href", "#icon")], true);
    html.end_tag("use");
    assert_eq!(html.as_str(), r##"<use xlink:href="#icon"></use>"##);
}

#[test]
fn is_void_is_case_insensitive() {
    for tag in ["area", "base", "br", "col", "embed", "hr", "img", "input"] {
        assert!(is_void(tag), "{tag} 是 void 元素");
    }
    for tag in ["link", "meta", "param", "source", "track", "wbr"] {
        assert!(is_void(tag), "{tag} 是 void 元素");
    }
    assert!(is_void("BR"));
    assert!(is_void("Img"));
    assert!(is_void("wBr"));
    for tag in ["div", "bodY", "svg", "", "brr"] {
        assert!(!is_void(tag), "{tag} 不是 void 元素");
    }
}

#[test]
fn with_capacity_preallocates_and_starts_empty() {
    let mut html = Html::with_capacity(128);
    assert_eq!(html.len(), 0);
    assert!(html.is_empty());
    assert!(html.buf.capacity() >= 128, "容量应至少是请求值");

    html.raw("<p>");
    assert_eq!(html.len(), 3);
    assert!(!html.is_empty());
    assert!(html.buf.capacity() >= 128, "写入不该丢掉预留的容量");
}

#[test]
fn len_and_is_empty_track_writes() {
    let mut html = Html::new();
    assert!(html.is_empty());
    assert_eq!(html.len(), 0);

    html.text("ab");
    assert_eq!(html.len(), 2);
    assert!(!html.is_empty());

    // 转义后长度按输出算：`&` 变成 `&amp;`，从 1 字节变 5 字节。
    html.text("&");
    assert_eq!(html.len(), 7);
    assert_eq!(html.as_str(), "ab&amp;");
}

#[test]
fn into_string_returns_content() {
    let mut html = Html::new();
    html.start_tag("p", &[], false);
    html.text("hi");
    html.end_tag("p");
    assert_eq!(html.into_string(), "<p>hi</p>");
}

#[test]
fn attribute_value_is_escaped_only_once() {
    let mut html = Html::new();
    html.start_tag("div", &[Attr::new("data-raw", "&amp;")], false);
    assert_eq!(html.as_str(), r#"<div data-raw="&amp;amp;">"#);
}

#[test]
fn attributes_without_value_are_never_quoted() {
    let mut html = Html::new();
    html.start_tag("option", &[Attr::boolean("selected")], false);
    assert_eq!(html.as_str(), "<option selected>");
}

#[test]
fn builds_a_small_document() {
    let mut html = Html::with_capacity(64);
    html.raw("<!DOCTYPE html>");
    html.start_tag("div", &[Attr::new("class", "card")], false);
    html.start_tag(
        "img",
        &[Attr::new("src", "a.png"), Attr::boolean("hidden")],
        false,
    );
    html.end_tag("img"); // 必须被跳过
    html.start_tag("p", &[], false);
    html.text("a < b & c");
    html.end_tag("p");
    html.end_tag("div");

    assert_eq!(
        html.into_string(),
        concat!(
            "<!DOCTYPE html>",
            r#"<div class="card">"#,
            r#"<img src="a.png" hidden>"#,
            "<p>a &lt; b &amp; c</p>",
            "</div>",
        )
    );
}
