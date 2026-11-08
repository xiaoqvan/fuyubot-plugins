//! 模板 AST → 渲染 AST：把指令展开、表达式解析好。
//!
//! 这一层做的事（对应 Vue 的 `transform` 阶段）：
//!
//! | 输入 | 输出 |
//! | --- | --- |
//! | `v-if` / `v-else-if` / `v-else` 连续分支 | [`ast::Node::If`] |
//! | `v-for="item in items"` | [`ast::Node::For`] |
//! | `<template>` | [`ast::Node::Template`] |
//! | `v-html` / `v-text` / `v-show` | [`ast::Element`] 上的字段 |
//! | `:attr` / `v-bind:attr` | [`ast::Binding`] |
//! | `v-pre` | 跳过整棵子树（去掉 `v-pre` 属性本身） |
//! | 未知 `v-*` / `@event` / `#slot` | 丢弃；`strict` 下报错 |
//!
//! 顺序与 Vue 一致：**`v-if` 优先于 `v-for`**（同一元素上两者都有时，`v-if`
//! 看不到 `v-for` 的局部变量）。

use crate::ast;
use crate::error::{Error, ErrorKind};
use crate::expr;
use crate::options::CompileOptions;
use crate::syntax::{self, Attribute, Node, Span};

/// 把语法树转换成渲染树。
pub fn transform(
    root: syntax::Root,
    source: &str,
    options: CompileOptions,
) -> Result<ast::Root, Error> {
    let ctx = Ctx { source, options };
    let children = ctx.children(root.children, false, false)?;
    Ok(ast::Root {
        children,
        span: root.span,
    })
}

struct Ctx<'a> {
    source: &'a str,
    options: CompileOptions,
}

impl Ctx<'_> {
    fn error(&self, kind: ErrorKind, span: Span, message: impl Into<String>) -> Error {
        Error::new(kind, self.source, span.start, message)
    }

    /// 转换一组兄弟节点（同时负责把 `v-if` 链拼起来）。
    fn children(
        &self,
        nodes: Vec<Node>,
        in_pre: bool,
        in_svg: bool,
    ) -> Result<Vec<ast::Node>, Error> {
        let mut rest = nodes;
        let mut out = Vec::with_capacity(rest.len());

        while !rest.is_empty() {
            let node = rest.remove(0);
            match node {
                Node::Comment(_) => {}
                Node::Text(text) => out.push(ast::Node::Text(ast::Text {
                    content: text.content,
                    span: text.span,
                })),
                Node::Interpolation(interpolation) => {
                    out.push(ast::Node::Interpolation(self.interpolation(interpolation)?));
                }
                Node::Element(element) => {
                    if in_pre {
                        out.push(self.element(element, true, in_svg)?);
                    } else if has_attr(&element, "v-if") {
                        out.push(self.if_chain(element, &mut rest, in_svg)?);
                    } else if has_attr(&element, "v-else") || has_attr(&element, "v-else-if") {
                        return Err(self.error(
                            ErrorKind::Parse,
                            element.span,
                            "v-else / v-else-if 前面没有对应的 v-if",
                        ));
                    } else {
                        out.push(self.element(element, false, in_svg)?);
                    }
                }
            }
        }
        Ok(out)
    }

    /// `v-if` + 后续连续的 `v-else-if` / `v-else`。
    fn if_chain(
        &self,
        first: syntax::Element,
        rest: &mut Vec<Node>,
        in_svg: bool,
    ) -> Result<ast::Node, Error> {
        let span = first.span;
        let condition = self.directive_expr(&first, "v-if")?;
        let mut branches = vec![ast::IfBranch {
            condition,
            children: vec![self.branch_element(first, in_svg)?],
            span,
        }];
        let mut otherwise = None;

        // 允许分支之间夹空白文本 / 注释（Vue 的 condense 也会把它们丢掉）。
        while let Some(index) = next_significant(rest) {
            let Node::Element(element) = &rest[index] else {
                break;
            };
            let is_else_if = has_attr(element, "v-else-if");
            let is_else = has_attr(element, "v-else");
            if !is_else_if && !is_else {
                break;
            }
            if otherwise.is_some() {
                return Err(self.error(
                    ErrorKind::Parse,
                    element.span,
                    "v-else 之后不能再有 v-else-if / v-else",
                ));
            }

            // 真正消费：把中间空白一起吞掉。
            let element = rest.remove(index);
            rest.drain(..index);
            let Node::Element(element) = element else {
                unreachable!("上面已经确认是 Element");
            };

            if is_else {
                otherwise = Some(vec![self.branch_element(element, in_svg)?]);
                break;
            }
            let branch_span = element.span;
            let condition = self.directive_expr(&element, "v-else-if")?;
            branches.push(ast::IfBranch {
                condition,
                children: vec![self.branch_element(element, in_svg)?],
                span: branch_span,
            });
        }

        Ok(ast::Node::If(ast::If {
            branches,
            otherwise,
            span,
        }))
    }

    /// 分支元素：去掉 `v-if` / `v-else-if` / `v-else` 之后当普通元素转换。
    fn branch_element(&self, element: syntax::Element, in_svg: bool) -> Result<ast::Node, Error> {
        self.element(element, false, in_svg)
    }

    fn interpolation(&self, node: syntax::Interpolation) -> Result<ast::Interpolation, Error> {
        // `{{}}` / `{{ }}` 在 Vue 里合法，输出空串。
        if node.raw.trim().is_empty() {
            return Ok(ast::Interpolation {
                expr: expr::Expr::Literal {
                    value: serde_json::Value::Null,
                    span: node.span,
                },
                span: node.span,
            });
        }
        let expr = parse_expr_at(
            &node.raw,
            node.raw_start,
            self.source,
            node.span,
            "插值表达式",
        )?;
        Ok(ast::Interpolation {
            expr,
            span: node.span,
        })
    }

    fn element(
        &self,
        element: syntax::Element,
        in_pre: bool,
        in_svg: bool,
    ) -> Result<ast::Node, Error> {
        let declares_pre = !in_pre && has_attr(&element, "v-pre");
        // `v-pre` 子树里的属性一律原样保留（Vue 不编译它们）。
        let raw_pre = in_pre || declares_pre;

        if raw_pre {
            // 只有「声明 v-pre 的那个元素」自己的 `v-pre` 不输出；v-pre 子树里
            // 后代的 `v-pre` 属性按字面保留（Vue 的 v-pre 就是不编译子树）。
            let attrs = element
                .attrs
                .iter()
                .filter(|attr| !(declares_pre && is_attr_name(attr, "v-pre")))
                .map(|attr| ast::Attribute {
                    name: attr.name.clone(),
                    value: attr.value.clone(),
                    span: attr.span,
                })
                .collect();
            let children = self.children(element.children, true, in_svg)?;
            let child_svg = in_svg || element.tag.eq_ignore_ascii_case("svg");
            return Ok(ast::Node::Element(ast::Element {
                tag: element.tag,
                attrs,
                bindings: Vec::new(),
                v_html: None,
                v_text: None,
                v_show: None,
                children,
                self_closing: element.self_closing,
                in_svg: child_svg,
                span: element.span,
            }));
        }

        let parsed = self.parse_attrs(&element)?;
        let child_svg = in_svg || element.tag.eq_ignore_ascii_case("svg");
        let children = self.children(element.children, false, child_svg)?;

        // `<template>` 只是逻辑容器，不输出标签。
        if element.tag.eq_ignore_ascii_case("template") {
            let node = ast::Node::Template(ast::Template {
                children,
                span: element.span,
            });
            return Ok(match parsed.for_spec {
                Some(spec) => ast::Node::For(ast::For {
                    source: spec.source,
                    aliases: spec.aliases,
                    key: parsed.key,
                    children: vec![node],
                    span: element.span,
                }),
                None => node,
            });
        }

        let element_node = ast::Node::Element(ast::Element {
            tag: element.tag,
            attrs: parsed.static_attrs,
            bindings: parsed.bindings,
            v_html: parsed.v_html,
            v_text: parsed.v_text,
            v_show: parsed.v_show,
            children,
            self_closing: element.self_closing,
            in_svg: child_svg,
            span: element.span,
        });

        Ok(match parsed.for_spec {
            Some(spec) => ast::Node::For(ast::For {
                source: spec.source,
                aliases: spec.aliases,
                key: parsed.key,
                children: vec![element_node],
                span: element.span,
            }),
            None => element_node,
        })
    }

    fn parse_attrs(&self, element: &syntax::Element) -> Result<ParsedAttrs, Error> {
        let mut parsed = ParsedAttrs::default();

        for attr in &element.attrs {
            match classify(&attr.name) {
                AttrClass::Static => {
                    // `key` / `ref` 是 Vue 的编译期属性，不输出。
                    if is_attr_name(attr, "key") || is_attr_name(attr, "ref") {
                        continue;
                    }
                    // Vue 3 不支持属性值里的 mustache（`title="{{ x }}"`），
                    // 严格模式直接点出来，免得用户以为它生效了。
                    if self.options.strict
                        && attr
                            .value
                            .as_deref()
                            .is_some_and(|value| value.contains("{{"))
                    {
                        return Err(self.error(
                            ErrorKind::Unsupported,
                            attr.span,
                            "属性值里不支持 `{{ }}`，请改用 `:attr` / `v-bind:attr`",
                        ));
                    }
                    parsed.static_attrs.push(ast::Attribute {
                        name: attr.name.clone(),
                        value: attr.value.clone(),
                        span: attr.span,
                    });
                }
                AttrClass::If | AttrClass::ElseIf | AttrClass::Else => {
                    // 由 `if_chain` 处理；到这里说明是同一元素上的重复指令。
                    if parsed.saw_condition {
                        return Err(self.error(
                            ErrorKind::Parse,
                            attr.span,
                            "同一个元素上出现了多个 v-if / v-else-if / v-else",
                        ));
                    }
                    parsed.saw_condition = true;
                }
                AttrClass::For => {
                    if parsed.for_spec.is_some() {
                        return Err(self.error(
                            ErrorKind::Parse,
                            attr.span,
                            "同一个元素上只能有一个 v-for",
                        ));
                    }
                    let raw = require_value(self, attr, "v-for")?;
                    let base = attribute_value_offset(self.source, attr);
                    parsed.for_spec = Some(self.parse_for(raw, attr.span, base)?);
                }
                AttrClass::Html => {
                    let raw = require_value(self, attr, "v-html")?;
                    if parsed.v_text.is_some() {
                        return Err(self.error(
                            ErrorKind::Parse,
                            attr.span,
                            "v-html 与 v-text 不能同时使用",
                        ));
                    }
                    parsed.v_html = Some(self.attr_expr(raw, attr)?);
                }
                AttrClass::Text => {
                    let raw = require_value(self, attr, "v-text")?;
                    if parsed.v_html.is_some() {
                        return Err(self.error(
                            ErrorKind::Parse,
                            attr.span,
                            "v-html 与 v-text 不能同时使用",
                        ));
                    }
                    parsed.v_text = Some(self.attr_expr(raw, attr)?);
                }
                AttrClass::Show => {
                    let raw = require_value(self, attr, "v-show")?;
                    parsed.v_show = Some(self.attr_expr(raw, attr)?);
                }
                AttrClass::Pre => {}
                AttrClass::Bind { name } => {
                    let raw = require_value(self, attr, &format!("v-bind:{name}"))?;
                    let value = self.attr_expr(raw, attr)?;
                    if name == "key" {
                        parsed.key = Some(value);
                    } else {
                        parsed.bindings.push(ast::Binding {
                            name: name.to_owned(),
                            value,
                            span: attr.span,
                        });
                    }
                }
                AttrClass::Unsupported(name) => {
                    if self.options.strict {
                        return Err(self.error(
                            ErrorKind::Unsupported,
                            attr.span,
                            format!("不支持的指令 / 语法 `{name}`"),
                        ));
                    }
                }
            }
        }

        if parsed.v_html.is_some() || parsed.v_text.is_some() {
            // 内容被指令覆盖：子节点不用再渲染，但已经在 `element()` 里转换过了。
        }
        Ok(parsed)
    }

    fn parse_for(&self, raw: &str, span: Span, base: usize) -> Result<ForSpec, Error> {
        let (left, right) = split_for_source(raw).ok_or_else(|| {
            self.error(
                ErrorKind::Parse,
                span,
                "v-for 语法应为 `item in items` 或 `(item, index) in items`",
            )
        })?;
        let source_raw = right.trim();
        if source_raw.is_empty() {
            return Err(self.error(ErrorKind::Parse, span, "v-for 缺少被遍历的表达式"));
        }
        let aliases =
            parse_aliases(left).map_err(|message| self.error(ErrorKind::Parse, span, message))?;
        let source = parse_expr_at(source_raw, base, self.source, span, "v-for 表达式")?;

        Ok(ForSpec {
            aliases,
            source,
            span,
        })
    }

    /// 属性值表达式：错误位置尽量落在值上（找不到就退回属性起点）。
    fn attr_expr(&self, raw: &str, attr: &Attribute) -> Result<expr::Expr, Error> {
        let base = attribute_value_offset(self.source, attr);
        parse_expr_at(raw, base, self.source, attr.span, "指令表达式")
    }

    fn directive_expr(&self, element: &syntax::Element, name: &str) -> Result<expr::Expr, Error> {
        let attr = element
            .attrs
            .iter()
            .find(|attr| is_attr_name(attr, name))
            .ok_or_else(|| {
                self.error(
                    ErrorKind::Parse,
                    element.span,
                    format!("缺少 {name} 的属性值"),
                )
            })?;
        let raw = require_value(self, attr, name)?;
        self.attr_expr(raw, attr)
    }
}

#[derive(Default)]
struct ParsedAttrs {
    static_attrs: Vec<ast::Attribute>,
    bindings: Vec<ast::Binding>,
    v_html: Option<expr::Expr>,
    v_text: Option<expr::Expr>,
    v_show: Option<expr::Expr>,
    for_spec: Option<ForSpec>,
    key: Option<expr::Expr>,
    saw_condition: bool,
}

struct ForSpec {
    aliases: Vec<String>,
    source: expr::Expr,
    #[allow(dead_code)]
    span: Span,
}

/// 指令分类（不分配名字）。
#[derive(Debug, PartialEq, Eq)]
enum AttrClass<'a> {
    Static,
    If,
    ElseIf,
    Else,
    For,
    Html,
    Text,
    Show,
    Pre,
    Bind { name: &'a str },
    Unsupported(&'a str),
}

fn classify(name: &str) -> AttrClass<'_> {
    if let Some(rest) = name.strip_prefix(':') {
        let name = strip_modifiers(rest);
        if name.is_empty() || name.starts_with('[') {
            return AttrClass::Unsupported(name);
        }
        return AttrClass::Bind { name };
    }
    if name.starts_with('@') || name.starts_with('#') {
        return AttrClass::Unsupported(name);
    }
    let Some(body) = name.strip_prefix("v-") else {
        return AttrClass::Static;
    };
    let body = strip_modifiers(body);
    let (base, arg) = match body.split_once(':') {
        Some((base, arg)) => (base, Some(strip_modifiers(arg))),
        None => (body, None),
    };
    match base {
        "if" => AttrClass::If,
        "else-if" => AttrClass::ElseIf,
        "else" => AttrClass::Else,
        "for" => AttrClass::For,
        "html" => AttrClass::Html,
        "text" => AttrClass::Text,
        "show" => AttrClass::Show,
        "pre" => AttrClass::Pre,
        "bind" => match arg {
            Some(arg) if !arg.is_empty() && !arg.starts_with('[') => AttrClass::Bind { name: arg },
            _ => AttrClass::Unsupported(name),
        },
        _ => AttrClass::Unsupported(name),
    }
}

fn strip_modifiers(name: &str) -> &str {
    name.split('.').next().unwrap_or(name)
}

fn is_attr_name(attr: &Attribute, name: &str) -> bool {
    attr.name == name
}

fn has_attr(element: &syntax::Element, name: &str) -> bool {
    element.attrs.iter().any(|attr| is_attr_name(attr, name))
}

/// 下一个「有意义」的兄弟节点下标：跳过空白文本与注释。
fn next_significant(nodes: &[Node]) -> Option<usize> {
    nodes.iter().position(|node| match node {
        Node::Comment(_) => false,
        Node::Text(text) => !text.content.trim().is_empty(),
        _ => true,
    })
}

fn require_value<'a>(ctx: &Ctx<'_>, attr: &'a Attribute, name: &str) -> Result<&'a str, Error> {
    attr.value
        .as_deref()
        .ok_or_else(|| ctx.error(ErrorKind::Parse, attr.span, format!("`{name}` 缺少表达式")))
}

fn parse_expr_at(
    raw: &str,
    base: usize,
    source: &str,
    span: Span,
    what: &str,
) -> Result<expr::Expr, Error> {
    expr::parse(raw)
        .map_err(|error| {
            let offset = base.saturating_add(error.offset).min(source.len());
            Error::new(
                ErrorKind::Expression,
                source,
                offset,
                format!("{what}：{}", error.message),
            )
        })
        .map_err(|mut error| {
            // 位置溢出到区间之外时，至少指向节点起点，别给出误导性的行号。
            if error.offset > span.end {
                error = Error::new(
                    ErrorKind::Expression,
                    source,
                    span.start,
                    error.message.clone(),
                );
            }
            error
        })
}

/// `:src="url"` 里值在模板里的起点（用于表达式报错定位）。
fn attribute_value_offset(source: &str, attr: &Attribute) -> usize {
    let Some(text) = source.get(attr.span.start..attr.span.end) else {
        return attr.span.start;
    };
    let Some(equal) = text.find('=') else {
        return attr.span.start;
    };
    let after = &text[equal + 1..];
    let trimmed = after.trim_start();
    let skipped = after.len() - trimmed.len();
    let quote = trimmed
        .chars()
        .next()
        .filter(|ch| *ch == '"' || *ch == '\'')
        .is_some();
    attr.span.start + equal + 1 + skipped + usize::from(quote)
}

/// 把 `v-for` 的值拆成「别名部分」和「被遍历表达式」。
///
/// 分隔符是顶层的 ` in ` 或 ` of `（跳过引号、括号内的内容）。
fn split_for_source(raw: &str) -> Option<(&str, &str)> {
    let bytes = raw.as_bytes();
    let mut depth = 0i32;
    let mut quote: Option<u8> = None;
    let mut index = 0;
    while index < bytes.len() {
        let byte = bytes[index];
        match quote {
            Some(q) => {
                if byte == b'\\' {
                    index += 2;
                    continue;
                }
                if byte == q {
                    quote = None;
                }
            }
            None => match byte {
                b'\'' | b'"' => quote = Some(byte),
                b'(' | b'[' | b'{' => depth += 1,
                b')' | b']' | b'}' => depth -= 1,
                _ => {}
            },
        }
        if quote.is_none() && depth == 0 {
            for delimiter in [" in ", " of "] {
                if raw[index..].starts_with(delimiter) {
                    let left = &raw[..index];
                    let right = &raw[index + delimiter.len()..];
                    return Some((left, right));
                }
            }
        }
        index += 1;
    }
    None
}

/// 解析 `item` / `(item, index)` / `(value, key, index)`。
fn parse_aliases(left: &str) -> Result<Vec<String>, String> {
    let left = left.trim();
    let inner = if left.starts_with('(') {
        if !left.ends_with(')') {
            return Err("v-for 的别名列表缺少右括号".to_owned());
        }
        &left[1..left.len() - 1]
    } else {
        left
    };

    let mut aliases = Vec::new();
    let mut depth = 0i32;
    let mut start = 0;
    let bytes = inner.as_bytes();
    for (index, byte) in bytes.iter().enumerate() {
        match byte {
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => depth -= 1,
            b',' if depth == 0 => {
                aliases.push(inner[start..index].trim().to_owned());
                start = index + 1;
            }
            _ => {}
        }
    }
    aliases.push(inner[start..].trim().to_owned());

    if aliases.len() > 3 {
        return Err("v-for 最多支持 3 个别名（value, key, index）".to_owned());
    }
    if aliases.iter().any(|alias| !is_identifier(alias)) {
        return Err("v-for 的别名必须是合法标识符".to_owned());
    }
    let mut seen = Vec::new();
    for alias in &aliases {
        if seen.contains(alias) {
            return Err(format!("v-for 别名 `{alias}` 重复"));
        }
        seen.push(alias.clone());
    }
    Ok(aliases)
}

fn is_identifier(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !(first.is_ascii_alphabetic() || first == '_' || first == '$') {
        return false;
    }
    chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '$')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_for_source() {
        assert_eq!(
            split_for_source("(item, i) in items.filter").map(|(l, r)| (l.trim(), r.trim())),
            Some(("(item, i)", "items.filter"))
        );
        assert_eq!(
            split_for_source("value of object").map(|(l, r)| (l.trim(), r.trim())),
            Some(("value", "object"))
        );
    }

    #[test]
    fn parses_aliases() {
        assert_eq!(parse_aliases("item").unwrap(), ["item"]);
        assert_eq!(parse_aliases("(item, index)").unwrap(), ["item", "index"]);
        assert_eq!(
            parse_aliases("(value, key, index)").unwrap(),
            ["value", "key", "index"]
        );
        assert!(parse_aliases("(item, item)").is_err());
        assert!(parse_aliases("(1, index)").is_err());
    }

    #[test]
    fn classifies_directives() {
        assert_eq!(classify("v-if"), AttrClass::If);
        assert_eq!(classify("v-else-if"), AttrClass::ElseIf);
        assert_eq!(classify(":src"), AttrClass::Bind { name: "src" });
        assert_eq!(classify("v-bind:class"), AttrClass::Bind { name: "class" });
        assert_eq!(classify("@click"), AttrClass::Unsupported("@click"));
        assert_eq!(classify("class"), AttrClass::Static);
        assert_eq!(classify("v-model"), AttrClass::Unsupported("v-model"));
    }

    #[test]
    fn finds_value_offset() {
        let source = r#"<img :src=" url ">"#;
        let attr = Attribute {
            name: ":src".to_owned(),
            value: Some(" url ".to_owned()),
            span: Span::new(5, 17),
        };
        // 值从引号后开始（含两侧空格），到结束引号前结束。
        let offset = attribute_value_offset(source, &attr);
        assert_eq!(offset, 11);
        assert_eq!(&source[offset..attr.span.end - 1], " url ");
    }
}
