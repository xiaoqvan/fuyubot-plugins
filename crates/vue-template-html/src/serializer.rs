//! HTML 序列化：把渲染结果拼成字符串。
//!
//! 只做「正确的 HTML 字符串」这一件事：转义、void 元素、布尔属性、SVG 大小写。
//! 它不认识 Vue，也不认识表达式。
//!
//! ## 规则
//!
//! - 文本与属性值都做 HTML 转义：`&` `<` `>` `"` `'`
//!   （`&amp;` `&lt;` `&gt;` `&quot;` `&#39;`，与 Vue 的 `escapeHtml` 一致）。
//! - 转义**只做一次**：输入永远是「已经解码」的普通字符串。所以文本里字面出现的
//!   `&amp;`（五个字符）会输出成 `&amp;amp;`——这不是二次转义，而是把这段字面
//!   文本正确地表示成 HTML。调用方不要再自己转义一遍。
//! - 标签名、属性名**原样输出**，不做大小写规范化：SVG 的 `viewBox` /
//!   `preserveAspectRatio` / `xlink:href` 必须保持原样。
//! - 属性值用双引号包起来；`value = None` 表示布尔属性，只输出名字（` disabled`）。
//! - void 元素（`area base br col embed hr img input link meta param source track wbr`）
//!   输出 `<tag ...>`，没有结束标签。
//! - **非 void 元素一律输出显式 `<tag ...>` + `</tag>`**：`<div/>` 这种自闭合写法在
//!   HTML 里不合法（只在 SVG/MathML 等 foreign content 里合法），渲染层会为它补上
//!   结束标签。`self_closing` 参数因此只作为语义提示保留。
//! - `text` 转义；`raw` 原样拼接（只给可信 `v-html` 用）。

/// 一个待输出的属性。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attr {
    pub name: String,
    /// `None` = 布尔属性（`disabled`）。
    pub value: Option<String>,
}

impl Attr {
    pub fn new(name: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            value: Some(value.into()),
        }
    }

    pub fn boolean(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            value: None,
        }
    }
}

/// HTML 输出缓冲。
#[derive(Debug, Default)]
pub struct Html {
    buf: String,
}

impl Html {
    /// 空缓冲区（`with_capacity(0)` 的等价写法；测试与调用方都用得到）。
    #[allow(dead_code)]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn into_string(self) -> String {
        self.buf
    }

    /// 输出转义后的文本。
    pub fn text(&mut self, text: &str) {
        if needs_escape(text) {
            self.buf.push_str(&escape_text(text));
        } else {
            // 快速路径：没有特殊字符就不建临时 `String`，直接整段追加。
            self.buf.push_str(text);
        }
    }

    /// 原样输出（只给可信 `v-html`）。
    pub fn raw(&mut self, html: &str) {
        self.buf.push_str(html);
    }

    /// 输出开始标签。
    ///
    /// `self_closing` 只是模板里的语义提示：非 void 元素即使写成 `<div/>`，这里也
    /// 只输出 `<div>`，由调用方补 `</div>`（HTML 规范不允许 non-void 自闭合）。
    pub fn start_tag(&mut self, tag: &str, attrs: &[Attr], self_closing: bool) {
        self.buf.push('<');
        // 标签名原样写入（SVG 的 `linearGradient` 靠这个）。
        self.buf.push_str(tag);

        for attr in attrs {
            self.buf.push(' ');
            // 属性名同样原样写入：`viewBox` / `preserveAspectRatio` / `xlink:href`。
            self.buf.push_str(&attr.name);
            if let Some(value) = &attr.value {
                self.buf.push_str("=\"");
                self.push_attribute_value(value);
                self.buf.push('"');
            }
        }

        // `<br/>` 与 `<br>` 等价但一律输出后者；非 void 元素也一律 `<tag>`。
        let _ = self_closing;
        self.buf.push('>');
    }

    /// 输出结束标签（void 元素自动跳过）。
    pub fn end_tag(&mut self, tag: &str) {
        if is_void(tag) {
            return;
        }
        self.buf.push_str("</");
        self.buf.push_str(tag);
        self.buf.push('>');
    }

    /// 追加双引号内的属性值。
    fn push_attribute_value(&mut self, value: &str) {
        if needs_escape(value) {
            self.buf.push_str(&escape_attribute(value));
        } else {
            self.buf.push_str(value);
        }
    }
}

/// 缓冲区的读 / 预分配接口。
///
/// 渲染层目前只调 [`Html::with_capacity`] / `text` / `raw` / `start_tag` /
/// `end_tag` / `into_string`。下面这几个查询方法还没有生产调用方（单元测试与
/// 以后做模板片段拼接时会用），属于缓冲区的完整接口，保留并显式豁免死代码检查。
#[allow(dead_code)]
impl Html {
    /// 预留容量：整页渲染时先按模板长度估一个值，能省掉大半重分配。
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            buf: String::with_capacity(capacity),
        }
    }

    pub fn as_str(&self) -> &str {
        &self.buf
    }

    pub fn len(&self) -> usize {
        self.buf.len()
    }

    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }
}

/// 文本转义：`& < > " '` → `&amp; &lt; &gt; &quot; &#39;`。
///
/// 没有特殊字符时整段克隆一次（不逐字符 match + push）；有特殊字符时先按
/// `text.len()` 预分配，再只替换命中的那几个字节。
pub fn escape_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    push_escaped_into(&mut out, text);
    out
}

/// 属性值转义（HTML 里与文本转义同一套字符，保留单独函数方便以后区分）。
pub fn escape_attribute(value: &str) -> String {
    escape_text(value)
}

/// 是否是 void 元素（大小写不敏感）。
pub fn is_void(tag: &str) -> bool {
    const VOID_ELEMENTS: [&str; 14] = [
        "area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "param",
        "source", "track", "wbr",
    ];
    VOID_ELEMENTS
        .iter()
        .any(|void| tag.eq_ignore_ascii_case(void))
}

/// 五个需要转义的字符（全是 ASCII，所以可以按字节扫描）。
fn escape_for(byte: u8) -> Option<&'static str> {
    match byte {
        b'&' => Some("&amp;"),
        b'<' => Some("&lt;"),
        b'>' => Some("&gt;"),
        b'"' => Some("&quot;"),
        b'\'' => Some("&#39;"),
        _ => None,
    }
}

/// 有没有需要转义的字符（一次线性扫描，不分配）。
fn needs_escape(text: &str) -> bool {
    text.bytes().any(|byte| escape_for(byte).is_some())
}

/// 把 `text` 转义后追加到 `out`。
///
/// 按字节扫描是安全的：五个特殊字符都是 ASCII，命中的下标一定落在 UTF-8 字符
/// 边界上，所以 `&text[start..index]` 永远合法。没命中的区间整段拷贝，避免
/// 逐字符 push。
fn push_escaped_into(out: &mut String, text: &str) {
    let mut start = 0;
    for (index, byte) in text.bytes().enumerate() {
        let Some(escaped) = escape_for(byte) else {
            continue;
        };
        if start < index {
            out.push_str(&text[start..index]);
        }
        out.push_str(escaped);
        start = index + 1;
    }
    if start < text.len() {
        out.push_str(&text[start..]);
    }
}

#[cfg(test)]
mod tests {
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
}
