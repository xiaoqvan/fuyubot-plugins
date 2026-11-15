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
#[path = "../tests/unit/serializer.rs"]
mod tests;
