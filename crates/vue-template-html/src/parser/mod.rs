//! 模板解析器：把 Vue 模板源码切成 [`crate::syntax`] 里的语法树。
//!
//! ## 职责边界
//!
//! - 识别标签 / 属性 / 文本 / 插值 / 注释；
//! - 记录每个节点的源码区间（[`Span`]）；
//! - **不解析表达式**：`{{ ... }}`、`v-if="..."` 里的表达式原文原样保留，
//!   交给 transform；
//! - **不解释指令**：`v-if` 只是名字叫 `v-if` 的属性，语义在 transform。
//!
//! ## 必须处理的细节
//!
//! - `{{ }}` 与文本混合：`Hello, {{ name }}!` → 文本、插值、文本三个节点。
//! - 插值里的 `}}` 要跳过字符串与对象字面量里的花括号（`{{ {a: 1} }}`）。
//! - `v-pre`：元素及其后代里的 `{{ }}` 不再切分，按纯文本保留（Vue 语义）。
//!   注意：`v-pre` 要等属性解析完才知道，所以「先读开标签属性，再决定怎么解析子内容」。
//! - 原始文本元素：`<script>` / `<style>` 里的 `<` 和 `{{ }}` 不是语法。
//! - void 元素：`<br>` / `<img>` / `<input>` … 不进子节点栈、不期待闭合标签。
//! - 自闭合：`<div />`（Vue 允许，HTML 不允许，但模板不是 HTML）。
//! - 标签名保留大小写；结束标签按大小写不敏感匹配（HTML）。
//! - 不匹配的结束标签、未闭合的标签、未闭合的插值 / 引号都要报带位置的
//!   [`crate::ErrorKind::Parse`] 错误。
//! - `<!DOCTYPE ...>`、`<!-- -->` 不算元素；注释可以保留为 [`crate::syntax::Comment`]，
//!   transform 会丢掉它们。
//!
//! ## 实现说明（手写扫描器）
//!
//! - 线性扫描：每个位置只找 `<` 与 `{{` 两个特殊起点，其余都归入文本；全程不用正则。
//! - 元素用显式栈 [`Frame`]，不用递归——深嵌套模板不会把调用栈打爆。
//! - 原始文本元素（`script` / `style` / `textarea`）在读完开始标签后直接找大小写
//!   不敏感的 `</tag`，中间整段成为 [`syntax::Text`]；**不做实体解码**（原文保留），
//!   也不解析 `<` 与 `{{ }}`。
//! - `v-pre` 记在栈帧上并由子元素继承；子树里 `{{ }}` 原样留在一个文本节点里。
//! - `<![CDATA[...]]>` 的**内容**按普通文本保留（不做实体解码，也不再解析其中的
//!   `<`）——这是简化处理；`<!DOCTYPE ...>` 与 `<?...?>` 直接跳过，不产生节点。
//! - 属性值与普通文本都做实体解码（`&amp;lt;` → `&lt;`），未知命名实体与非法
//!   数字实体原样保留；原始文本元素（`script` / `style` / `textarea`）与 CDATA
//!   的内容按字节原样保留，不解码。
//! - 标签外的孤立 `<`（例如文本 `a < b`）当普通文本；相邻文本节点会合并。

use crate::error::{Error, ErrorKind};
use crate::syntax::{Attribute, Comment, Element, Interpolation, Node, Root, Span, Text};

/// 解析整个模板。
///
/// 失败时返回带行 / 列的 [`Error`]；成功时 [`Root::span`] 覆盖整个源码。
pub fn parse(source: &str) -> Result<Root, Error> {
    Scanner::new(source).run()
}

/// void 元素：没有子节点、不要求结束标签。
const VOID_ELEMENTS: [&str; 14] = [
    "area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "param", "source",
    "track", "wbr",
];

/// 是否是 void 元素（大小写不敏感）。
fn is_void(tag: &str) -> bool {
    VOID_ELEMENTS
        .iter()
        .any(|void| tag.eq_ignore_ascii_case(void))
}

/// 内容按原始文本处理的元素（大小写不敏感）。
fn is_raw_text(tag: &str) -> bool {
    ["script", "style", "textarea"]
        .iter()
        .any(|raw| tag.eq_ignore_ascii_case(raw))
}

/// HTML 空白（标签内与属性之间）。
const fn is_whitespace(byte: u8) -> bool {
    matches!(byte, b' ' | b'\t' | b'\n' | b'\r' | 0x0c)
}

/// 标签名 / 结束标签名的终止字符。
const fn is_tag_name_end(byte: u8) -> bool {
    is_whitespace(byte) || byte == b'>' || byte == b'/'
}

/// 属性名的终止字符（`:` / `@` / `#` / `[` 都允许出现在名字里）。
const fn is_attribute_name_end(byte: u8) -> bool {
    is_whitespace(byte) || byte == b'=' || byte == b'>' || byte == b'/'
}

/// 无引号属性值的终止字符（按需求：空白、`>`、`/`）。
const fn is_unquoted_value_end(byte: u8) -> bool {
    is_whitespace(byte) || byte == b'>' || byte == b'/'
}

/// 扫描器遇到的下一个特殊起点。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Special {
    /// `<`：可能是标签、注释、声明。
    Tag,
    /// `{{`：插值。
    Interpolation,
}

/// 一个还没闭合的元素（显式栈的一帧）。
struct Frame {
    tag: String,
    attrs: Vec<Attribute>,
    children: Vec<Node>,
    /// 开标签 `<` 的偏移，未闭合时报错用。
    start: usize,
    /// 子内容是否处于 `v-pre` 模式（父级 `v-pre` 会继承下来）。
    pre: bool,
}

/// 一次开标签扫描的结果。
struct StartTag {
    tag: String,
    attrs: Vec<Attribute>,
    /// 显式 `/>`（void 元素另算）。
    self_closing: bool,
    /// 开标签 `>` 之后的位置。
    end: usize,
}

/// 手写扫描器。
struct Scanner<'a> {
    source: &'a str,
    bytes: &'a [u8],
    /// 下一个待扫描字节。
    pos: usize,
    /// 未闭合元素栈（栈顶是当前父元素）。
    frames: Vec<Frame>,
    /// 根节点的子节点。
    children: Vec<Node>,
}

impl<'a> Scanner<'a> {
    fn new(source: &'a str) -> Self {
        Self {
            source,
            bytes: source.as_bytes(),
            pos: 0,
            frames: Vec::new(),
            children: Vec::new(),
        }
    }

    /// 扫描到源码结束。
    fn run(mut self) -> Result<Root, Error> {
        loop {
            if self.pos >= self.bytes.len() {
                break;
            }
            let pre = self.current_pre();
            let Some((at, special)) = self.next_special(self.pos, pre) else {
                let end = self.bytes.len();
                self.push_text(self.pos, end);
                self.pos = end;
                break;
            };
            if at > self.pos {
                self.push_text(self.pos, at);
                self.pos = at;
            }
            match special {
                Special::Interpolation => {
                    let start = at;
                    let (raw_start, raw_end, end) = self.scan_interpolation(start)?;
                    let node = Interpolation {
                        raw: self.source[raw_start..raw_end].to_owned(),
                        span: Span::new(start, end),
                        raw_start,
                    };
                    self.push_node(Node::Interpolation(node));
                    self.pos = end;
                }
                Special::Tag => self.handle_tag(at)?,
            }
        }

        if let Some(frame) = self.frames.last() {
            return Err(self.err(frame.start, format!("未闭合的标签 `<{}>`", frame.tag)));
        }
        Ok(Root {
            children: self.children,
            span: Span::new(0, self.source.len()),
        })
    }

    /// 当前（父元素）是否处于 `v-pre` 模式。
    fn current_pre(&self) -> bool {
        self.frames.last().is_some_and(|frame| frame.pre)
    }

    /// 构造一个带位置的解析错误。
    fn err(&self, offset: usize, message: impl Into<String>) -> Error {
        Error::new(ErrorKind::Parse, self.source, offset, message)
    }

    /// 追加节点；相邻的文本节点会被合并，避免把 `a < b` 切成碎片。
    fn push_node(&mut self, node: Node) {
        let children = match self.frames.last_mut() {
            Some(frame) => &mut frame.children,
            None => &mut self.children,
        };
        if let Node::Text(text) = &node
            && let Some(Node::Text(previous)) = children.last_mut()
        {
            previous.content.push_str(&text.content);
            previous.span.end = text.span.end;
            return;
        }
        children.push(node);
    }

    /// 追加一段普通文本（解码实体）；空区间直接跳过。
    ///
    /// 原始文本元素不走这里（[`Scanner::scan_raw_text_element`] 直接构造文本节点），
    /// 所以 `script` / `style` / `textarea` 的内容保持字节原样。
    fn push_text(&mut self, start: usize, end: usize) {
        if start >= end {
            return;
        }
        let content = decode_entities(&self.source[start..end]);
        self.push_node(Node::Text(Text {
            content,
            span: Span::new(start, end),
        }));
    }

    /// 找下一个 `<` 或 `{{`；`v-pre` 模式只找 `<`。
    fn next_special(&self, from: usize, pre: bool) -> Option<(usize, Special)> {
        let mut i = from;
        while i < self.bytes.len() {
            match self.bytes[i] {
                b'<' => return Some((i, Special::Tag)),
                b'{' if !pre && self.bytes.get(i + 1) == Some(&b'{') => {
                    return Some((i, Special::Interpolation));
                }
                _ => i += 1,
            }
        }
        None
    }

    /// 处理 `<`：注释 / 声明 / 结束标签 / 开标签，不成立时当普通文本。
    fn handle_tag(&mut self, at: usize) -> Result<(), Error> {
        match self.bytes.get(at + 1).copied() {
            Some(b'!') => self.handle_declaration(at),
            Some(b'?') => self.skip_until_gt(at),
            // `</` 后面不是字母时不是结束标签（例如文本 `a </ b`）。
            Some(b'/') if self.bytes.get(at + 2).is_some_and(u8::is_ascii_alphabetic) => {
                self.handle_end_tag(at)
            }
            Some(next) if next.is_ascii_alphabetic() => self.handle_start_tag(at),
            _ => {
                // `<` 后面不是标签（例如文本 `a < b`）：吃掉这一个字节当文本。
                self.push_text(at, at + 1);
                self.pos = at + 1;
                Ok(())
            }
        }
    }

    /// `<!-- -->`、`<![CDATA[...]]>`、`<!DOCTYPE ...>` 等。
    fn handle_declaration(&mut self, at: usize) -> Result<(), Error> {
        if self.source[at..].starts_with("<!--") {
            self.handle_comment(at)
        } else if self.source[at..].starts_with("<![CDATA[") {
            self.handle_cdata(at)
        } else {
            self.skip_until_gt(at)
        }
    }

    /// 跳过 `<!...>` / `<?...?>` 这类声明（不产生节点）。
    fn skip_until_gt(&mut self, at: usize) -> Result<(), Error> {
        match self.source[at..].find('>') {
            Some(relative) => {
                self.pos = at + relative + 1;
                Ok(())
            }
            None => Err(self.err(at, "声明未闭合")),
        }
    }

    /// 注释节点：保留 `<!--` 与 `-->` 之间的原文。
    fn handle_comment(&mut self, at: usize) -> Result<(), Error> {
        let content_start = at + "<!--".len();
        let Some(relative) = self.source[content_start..].find("-->") else {
            return Err(self.err(at, "注释未闭合"));
        };
        let content_end = content_start + relative;
        let end = content_end + "-->".len();
        self.push_node(Node::Comment(Comment {
            content: self.source[content_start..content_end].to_owned(),
            span: Span::new(at, end),
        }));
        self.pos = end;
        Ok(())
    }

    /// CDATA：只保留内容，按普通文本处理（简化语义，见模块文档）。
    fn handle_cdata(&mut self, at: usize) -> Result<(), Error> {
        let content_start = at + "<![CDATA[".len();
        let Some(relative) = self.source[content_start..].find("]]>") else {
            return Err(self.err(at, "CDATA 段未闭合"));
        };
        let content_end = content_start + relative;
        // CDATA 内容按字节原样保留，不做实体解码。
        if content_start < content_end {
            let content = self.source[content_start..content_end].to_owned();
            self.push_node(Node::Text(Text {
                content,
                span: Span::new(content_start, content_end),
            }));
        }
        self.pos = content_end + "]]>".len();
        Ok(())
    }

    /// 扫描结束标签，返回（名字，`>` 之后的位置）。
    fn scan_end_tag(&self, at: usize) -> Result<(&'a str, usize), Error> {
        let len = self.bytes.len();
        let name_start = at + 2;
        if name_start >= len || !self.bytes[name_start].is_ascii_alphabetic() {
            return Err(self.err(at, "结束标签缺少标签名"));
        }
        let mut i = name_start;
        while i < len && !is_tag_name_end(self.bytes[i]) {
            i += 1;
        }
        let name = &self.source[name_start..i];
        let mut j = i;
        while j < len && is_whitespace(self.bytes[j]) {
            j += 1;
        }
        if j >= len || self.bytes[j] != b'>' {
            return Err(self.err(at, format!("结束标签 `</{name}>` 未闭合")));
        }
        Ok((name, j + 1))
    }

    /// 处理结束标签：与栈顶元素大小写不敏感地比对，然后闭合。
    fn handle_end_tag(&mut self, at: usize) -> Result<(), Error> {
        let (name, end) = self.scan_end_tag(at)?;
        let Some(frame) = self.frames.last() else {
            return Err(self.err(at, format!("多余的结束标签 `</{name}>`")));
        };
        if !frame.tag.eq_ignore_ascii_case(name) {
            return Err(self.err(
                at,
                format!("结束标签 `</{name}>` 与 `<{}>` 不匹配", frame.tag),
            ));
        }
        let frame = self.frames.pop().expect("栈里至少有当前元素");
        self.pos = end;
        self.push_node(Node::Element(Element {
            tag: frame.tag,
            attrs: frame.attrs,
            children: frame.children,
            self_closing: false,
            span: Span::new(frame.start, end),
        }));
        Ok(())
    }

    /// 处理开标签；根据 void / 自闭合 / 原始文本决定是否进了节点栈。
    fn handle_start_tag(&mut self, start: usize) -> Result<(), Error> {
        let start_tag = self.scan_start_tag(start)?;
        let StartTag {
            tag,
            attrs,
            self_closing,
            end,
        } = start_tag;
        // 先记住继承来的 v-pre，再决定这一层的子内容模式。
        let inherited_pre = self.current_pre();
        let pre = inherited_pre || attrs.iter().any(|attr| attr.name == "v-pre");

        self.pos = end;
        if self_closing || is_void(&tag) {
            self.push_node(Node::Element(Element {
                tag,
                attrs,
                children: Vec::new(),
                self_closing: true,
                span: Span::new(start, end),
            }));
        } else if is_raw_text(&tag) {
            let element = self.scan_raw_text_element(start, end, tag, attrs, pre)?;
            self.push_node(Node::Element(element));
        } else {
            self.frames.push(Frame {
                tag,
                attrs,
                children: Vec::new(),
                start,
                pre,
            });
        }
        Ok(())
    }

    /// 扫描开标签（不修改 `pos`）。
    fn scan_start_tag(&self, start: usize) -> Result<StartTag, Error> {
        let len = self.bytes.len();
        let mut i = start + 1;
        while i < len && !is_tag_name_end(self.bytes[i]) {
            i += 1;
        }
        let tag = self.source[start + 1..i].to_owned();
        let mut attrs = Vec::new();
        let mut self_closing = false;
        let end;
        loop {
            while i < len && is_whitespace(self.bytes[i]) {
                i += 1;
            }
            if i >= len {
                return Err(self.err(start, format!("标签 `<{tag}>` 未闭合")));
            }
            match self.bytes[i] {
                b'>' => {
                    end = i + 1;
                    break;
                }
                b'/' if self.bytes.get(i + 1) == Some(&b'>') => {
                    end = i + 2;
                    self_closing = true;
                    break;
                }
                b'/' => {
                    // 孤立斜杠：忽略一个字节，继续找属性或 `>`。
                    i += 1;
                }
                _ => {
                    let (attribute, next) = self.scan_attribute(i)?;
                    attrs.push(attribute);
                    i = next;
                }
            }
        }
        Ok(StartTag {
            tag,
            attrs,
            self_closing,
            end,
        })
    }

    /// 扫描一个属性：名字、可选的 `=` 与三种写法的值。
    fn scan_attribute(&self, start: usize) -> Result<(Attribute, usize), Error> {
        let len = self.bytes.len();
        let mut i = start;
        while i < len && !is_attribute_name_end(self.bytes[i]) {
            i += 1;
        }
        if i == start {
            return Err(self.err(start, "属性名缺失"));
        }
        let name = self.source[start..i].to_owned();

        let mut j = i;
        while j < len && is_whitespace(self.bytes[j]) {
            j += 1;
        }
        if j >= len || self.bytes[j] != b'=' {
            // 无值属性（`disabled` / `v-pre`）。
            return Ok((
                Attribute {
                    name,
                    value: None,
                    span: Span::new(start, i),
                },
                i,
            ));
        }

        j += 1;
        while j < len && is_whitespace(self.bytes[j]) {
            j += 1;
        }
        if j >= len {
            return Err(self.err(start, format!("属性 `{name}` 的值未闭合")));
        }

        let (value, end) = match self.bytes[j] {
            quote @ (b'"' | b'\'') => {
                let value_start = j + 1;
                let mut k = value_start;
                while k < len && self.bytes[k] != quote {
                    k += 1;
                }
                if k >= len {
                    return Err(self.err(j, "属性值引号未闭合"));
                }
                (decode_entities(&self.source[value_start..k]), k + 1)
            }
            _ => {
                let value_start = j;
                let mut k = j;
                while k < len && !is_unquoted_value_end(self.bytes[k]) {
                    k += 1;
                }
                (decode_entities(&self.source[value_start..k]), k)
            }
        };
        Ok((
            Attribute {
                name,
                value: Some(value),
                span: Span::new(start, end),
            },
            end,
        ))
    }

    /// 原始文本元素：内容整段成为文本节点，直到大小写不敏感的 `</tag`。
    fn scan_raw_text_element(
        &mut self,
        start: usize,
        content_start: usize,
        tag: String,
        attrs: Vec<Attribute>,
        pre: bool,
    ) -> Result<Element, Error> {
        let Some(close) = self.find_raw_close(content_start, &tag) else {
            return Err(self.err(start, format!("未闭合的标签 `<{tag}>`")));
        };
        let (close_name, end) = self.scan_end_tag(close)?;
        if !close_name.eq_ignore_ascii_case(&tag) {
            // find_raw_close 已保证匹配；这里只是防御。
            return Err(self.err(
                close,
                format!("结束标签 `</{close_name}>` 与 `<{tag}>` 不匹配"),
            ));
        }
        let children = if close > content_start {
            if tag.eq_ignore_ascii_case("textarea") && !pre {
                // `<textarea>` 是 RCDATA：不解析标签，但 `{{ }}` 照常插值
                // （`<textarea>{{ text }}</textarea>` 是 Vue 里的常见写法）。
                self.scan_rcdata(content_start, close)?
            } else {
                vec![Node::Text(Text {
                    content: self.source[content_start..close].to_owned(),
                    span: Span::new(content_start, close),
                })]
            }
        } else {
            Vec::new()
        };
        self.pos = end;
        Ok(Element {
            tag,
            attrs,
            children,
            self_closing: false,
            span: Span::new(start, end),
        })
    }

    /// RCDATA 内容（textarea）：只识别 `{{ }}`，不解析标签；文本里的实体照常解码。
    fn scan_rcdata(&self, start: usize, end: usize) -> Result<Vec<Node>, Error> {
        let mut nodes = Vec::new();
        let mut cursor = start;
        while cursor < end {
            let Some(offset) = self.source[cursor..end].find("{{") else {
                self.push_text_node(&mut nodes, cursor, end);
                break;
            };
            let open = cursor + offset;
            if open > cursor {
                self.push_text_node(&mut nodes, cursor, open);
            }
            let (raw_start, raw_end, close) = self.scan_interpolation(open)?;
            if close > end {
                return Err(self.err(open, "未闭合的插值（textarea 内容里没有对应的 `}}`）"));
            }
            nodes.push(Node::Interpolation(Interpolation {
                raw: self.source[raw_start..raw_end].to_owned(),
                span: Span::new(open, close),
                raw_start,
            }));
            cursor = close;
        }
        Ok(nodes)
    }

    fn push_text_node(&self, nodes: &mut Vec<Node>, start: usize, end: usize) {
        if start >= end {
            return;
        }
        nodes.push(Node::Text(Text {
            content: decode_entities(&self.source[start..end]),
            span: Span::new(start, end),
        }));
    }

    /// 从 `from` 开始找 `</tag`（大小写不敏感，后面必须是空白 / `>` / `/` / EOF）。
    fn find_raw_close(&self, from: usize, tag: &str) -> Option<usize> {
        let bytes = self.bytes;
        let tag_bytes = tag.as_bytes();
        let mut i = from;
        while i + 1 < bytes.len() {
            if bytes[i] == b'<' && bytes[i + 1] == b'/' {
                let name_start = i + 2;
                let name_end = name_start + tag_bytes.len();
                if name_end <= bytes.len()
                    && bytes[name_start..name_end].eq_ignore_ascii_case(tag_bytes)
                {
                    match bytes.get(name_end).copied() {
                        None | Some(b'>') | Some(b'/') => return Some(i),
                        Some(byte) if is_whitespace(byte) => return Some(i),
                        Some(_) => {}
                    }
                }
            }
            i += 1;
        }
        None
    }

    /// 扫描插值：返回（raw 起点，raw 终点，`}}` 之后的位置）。
    ///
    /// 跳过 `'` / `"` / `` ` `` 字符串，并跟踪嵌套花括号，所以
    /// `{{ {a: 1} }}` 与 `{{ fn({a: 1}) }}` 都能找到正确的 `}}`。
    fn scan_interpolation(&self, start: usize) -> Result<(usize, usize, usize), Error> {
        let bytes = self.bytes;
        let len = bytes.len();
        let mut i = start + 2;
        let mut depth = 0usize;
        while i < len {
            match bytes[i] {
                quote @ (b'\'' | b'"' | b'`') => {
                    i += 1;
                    let mut closed = false;
                    while i < len {
                        match bytes[i] {
                            // 反斜杠转义：跳过下一个字节。即使下一个字节是多字节字符的
                            // 首字节，这里也只会多走一个字节，后面按 ASCII 比较仍安全。
                            b'\\' => i += 2,
                            byte if byte == quote => {
                                i += 1;
                                closed = true;
                                break;
                            }
                            _ => i += 1,
                        }
                    }
                    if !closed {
                        return Err(self.err(start, "未闭合的插值"));
                    }
                }
                b'{' => {
                    depth += 1;
                    i += 1;
                }
                b'}' => {
                    if depth > 0 {
                        depth -= 1;
                        i += 1;
                    } else if bytes.get(i + 1) == Some(&b'}') {
                        return Ok((start + 2, i, i + 2));
                    } else {
                        i += 1;
                    }
                }
                _ => i += 1,
            }
        }
        Err(self.err(start, "未闭合的插值"))
    }
}

/// 解码 HTML 实体；未知 / 非法的实体原样保留。
fn decode_entities(raw: &str) -> String {
    if !raw.contains('&') {
        return raw.to_owned();
    }
    let bytes = raw.as_bytes();
    let mut out = String::with_capacity(raw.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'&' {
            let start = i;
            while i < bytes.len() && bytes[i] != b'&' {
                i += 1;
            }
            out.push_str(&raw[start..i]);
            continue;
        }
        match parse_entity(&raw[i..]) {
            Some((decoded, consumed)) => {
                out.push_str(&decoded);
                i += consumed;
            }
            None => {
                out.push('&');
                i += 1;
            }
        }
    }
    out
}

/// 尝试解析 `&...;`；返回（解码结果，消耗的字节数）。
fn parse_entity(source: &str) -> Option<(String, usize)> {
    let body = source.strip_prefix('&')?;
    if let Some(number) = body.strip_prefix('#') {
        if let Some(hex) = number.strip_prefix(['x', 'X']) {
            return parse_numeric_entity(hex, 16, 3);
        }
        return parse_numeric_entity(number, 10, 2);
    }
    let end = body.find(|ch: char| !ch.is_ascii_alphanumeric())?;
    if end == 0 || !body[end..].starts_with(';') {
        return None;
    }
    let decoded = named_entity(&body[..end])?;
    Some((decoded.to_owned(), end + 2))
}

/// 解析 `&#...;` 的数字部分；`digits_start` 是数字在原始字符串里的下标。
fn parse_numeric_entity(digits: &str, radix: u32, digits_start: usize) -> Option<(String, usize)> {
    let end = digits
        .find(|ch: char| !ch.is_digit(radix))
        .unwrap_or(digits.len());
    if end == 0 || !digits[end..].starts_with(';') {
        return None;
    }
    let value = u32::from_str_radix(&digits[..end], radix).ok()?;
    let ch = char::from_u32(value)?;
    Some((ch.to_string(), digits_start + end + 1))
}

/// 命名实体表（只收常用的一小撮；未知的由调用方原样保留）。
fn named_entity(name: &str) -> Option<&'static str> {
    match name {
        "amp" => Some("&"),
        "lt" => Some("<"),
        "gt" => Some(">"),
        "quot" => Some("\""),
        "apos" => Some("'"),
        "nbsp" => Some("\u{a0}"),
        "copy" => Some("©"),
        "reg" => Some("®"),
        "trade" => Some("™"),
        "hellip" => Some("…"),
        "mdash" => Some("—"),
        "ndash" => Some("–"),
        "times" => Some("×"),
        "divide" => Some("÷"),
        "laquo" => Some("«"),
        "raquo" => Some("»"),
        "ldquo" => Some("“"),
        "rdquo" => Some("”"),
        "lsquo" => Some("‘"),
        "rsquo" => Some("’"),
        "middot" => Some("·"),
        "bull" => Some("•"),
        "deg" => Some("°"),
        "plusmn" => Some("±"),
        "frac12" => Some("½"),
        "sup2" => Some("²"),
        "sup3" => Some("³"),
        "cent" => Some("¢"),
        "pound" => Some("£"),
        "yen" => Some("¥"),
        "euro" => Some("€"),
        "sect" => Some("§"),
        "para" => Some("¶"),
        _ => None,
    }
}

#[cfg(test)]
mod tests;
