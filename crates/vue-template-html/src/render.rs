//! 渲染：遍历渲染 AST + 作用域，输出 HTML。
//!
//! 这是引擎的「运行时」：表达式求值、条件判断、循环、属性合并、HTML 安全策略
//! 都在这里。它不做解析，也不直接做字符串转义（转义交给 [`crate::serializer`]）。
//!
//! ## 作用域
//!
//! 外层数据是 `&serde_json::Value`；`v-for` 的局部变量放在逐层压栈的 frame 里。
//! 查找顺序是「最内层 frame → 外层 frame → 根数据对象」，所以循环变量会**遮蔽**
//! 外层同名变量。数组/对象元素是 `into_iter` **移动**进 frame 的，不复制整棵树。
//!
//! ## 限额
//!
//! - 单个 `v-for` 超过 [`RenderOptions::max_iterations`] → [`ErrorKind::Limit`]；
//! - 嵌套深度超过 [`RenderOptions::max_depth`] → [`ErrorKind::Limit`]。
//!
//! [`ErrorKind::Limit`]: crate::ErrorKind::Limit

use serde_json::{Map, Value};

use crate::ast::{self, display, number_text};
use crate::error::{Error, ErrorKind};
use crate::expr::{self, Expr, Scope as ExprScope};
use crate::options::{RenderOptions, UndefinedBehavior};
use crate::serializer::{self, Attr, Html};
use crate::syntax::Span;

/// 用给定数据渲染整棵树。
pub fn render(
    root: &ast::Root,
    data: &Value,
    options: &RenderOptions<'_>,
    source: &str,
) -> Result<String, Error> {
    let mut state = State {
        source,
        options,
        depth: 0,
    };
    let mut scope = Scope::new(data);
    // 模板长度是个不错的容量估计，能省掉大半重分配。
    let mut html = Html::with_capacity(source.len());
    render_nodes(&root.children, &mut scope, &mut html, &mut state)?;
    Ok(html.into_string())
}

struct State<'a, 'o, 'r> {
    source: &'a str,
    options: &'o RenderOptions<'r>,
    depth: usize,
}

impl State<'_, '_, '_> {
    fn error(&self, kind: ErrorKind, span: Span, message: impl Into<String>) -> Error {
        Error::new(kind, self.source, span.start, message)
    }

    fn enter(&mut self, span: Span) -> Result<(), Error> {
        if self.depth >= self.options.max_depth {
            return Err(self.error(
                ErrorKind::Limit,
                span,
                format!(
                    "嵌套深度超过上限 {}（可能是模板里有异常递归）",
                    self.options.max_depth
                ),
            ));
        }
        self.depth += 1;
        Ok(())
    }

    fn leave(&mut self) {
        self.depth = self.depth.saturating_sub(1);
    }
}

// ── 作用域 ────────────────────────────────────────────────────────

/// 渲染期作用域链。
pub(crate) struct Scope<'a> {
    root: &'a Value,
    frames: Vec<Vec<(String, Value)>>,
}

impl<'a> Scope<'a> {
    pub(crate) fn new(root: &'a Value) -> Self {
        Self {
            root,
            frames: Vec::new(),
        }
    }

    fn push(&mut self) {
        self.frames.push(Vec::new());
    }

    fn bind(&mut self, name: &str, value: Value) {
        if let Some(frame) = self.frames.last_mut() {
            frame.push((name.to_owned(), value));
        }
    }

    fn pop(&mut self) {
        self.frames.pop();
    }
}

impl ExprScope for Scope<'_> {
    fn get(&self, name: &str) -> Option<&Value> {
        for frame in self.frames.iter().rev() {
            for (key, value) in frame.iter().rev() {
                if key == name {
                    return Some(value);
                }
            }
        }
        self.root.as_object().and_then(|object| object.get(name))
    }
}

// ── 节点 ──────────────────────────────────────────────────────────

fn render_nodes(
    nodes: &[ast::Node],
    scope: &mut Scope<'_>,
    out: &mut Html,
    state: &mut State<'_, '_, '_>,
) -> Result<(), Error> {
    for node in nodes {
        render_node(node, scope, out, state)?;
    }
    Ok(())
}

fn render_node(
    node: &ast::Node,
    scope: &mut Scope<'_>,
    out: &mut Html,
    state: &mut State<'_, '_, '_>,
) -> Result<(), Error> {
    match node {
        ast::Node::Text(text) => {
            out.text(&text.content);
            Ok(())
        }
        ast::Node::Interpolation(interpolation) => {
            let value = eval(&interpolation.expr, scope, state, interpolation.span)?;
            out.text(&display(&value));
            Ok(())
        }
        ast::Node::If(node) => {
            state.enter(node.span)?;
            let result = render_if(node, scope, out, state);
            state.leave();
            result
        }
        ast::Node::For(node) => {
            state.enter(node.span)?;
            let result = render_for(node, scope, out, state);
            state.leave();
            result
        }
        ast::Node::Template(node) => {
            state.enter(node.span)?;
            let result = render_nodes(&node.children, scope, out, state);
            state.leave();
            result
        }
        ast::Node::Element(element) => {
            state.enter(element.span)?;
            let result = render_element(element, scope, out, state);
            state.leave();
            result
        }
    }
}

fn render_if(
    node: &ast::If,
    scope: &mut Scope<'_>,
    out: &mut Html,
    state: &mut State<'_, '_, '_>,
) -> Result<(), Error> {
    for branch in &node.branches {
        if expr::truthy(&eval(&branch.condition, scope, state, branch.span)?) {
            return render_nodes(&branch.children, scope, out, state);
        }
    }
    match &node.otherwise {
        Some(children) => render_nodes(children, scope, out, state),
        None => Ok(()),
    }
}

fn render_for(
    node: &ast::For,
    scope: &mut Scope<'_>,
    out: &mut Html,
    state: &mut State<'_, '_, '_>,
) -> Result<(), Error> {
    let value = eval(&node.source, scope, state, node.span)?;
    let max = state.options.max_iterations;

    match value {
        Value::Array(items) => {
            if items.len() > max {
                return Err(state.error(
                    ErrorKind::Limit,
                    node.span,
                    format!("v-for 迭代 {} 次，超过上限 {max}", items.len()),
                ));
            }
            for (index, item) in items.into_iter().enumerate() {
                scope.push();
                bind_alias(&mut *scope, &node.aliases, 0, item);
                bind_alias(&mut *scope, &node.aliases, 1, Value::from(index as u64));
                bind_alias(&mut *scope, &node.aliases, 2, Value::Null);
                let result = render_nodes(&node.children, scope, out, state);
                scope.pop();
                result?;
            }
            Ok(())
        }
        Value::Object(object) => {
            if object.len() > max {
                return Err(state.error(
                    ErrorKind::Limit,
                    node.span,
                    format!("v-for 迭代 {} 次，超过上限 {max}", object.len()),
                ));
            }
            for (index, (key, value)) in into_ordered(object).into_iter().enumerate() {
                scope.push();
                bind_alias(&mut *scope, &node.aliases, 0, value);
                bind_alias(&mut *scope, &node.aliases, 1, Value::String(key));
                bind_alias(&mut *scope, &node.aliases, 2, Value::from(index as u64));
                let result = render_nodes(&node.children, scope, out, state);
                scope.pop();
                result?;
            }
            Ok(())
        }
        Value::Number(number) => {
            let count = number.as_f64().unwrap_or(0.0).floor();
            if count > max as f64 {
                return Err(state.error(
                    ErrorKind::Limit,
                    node.span,
                    format!("v-for 迭代 {count} 次，超过上限 {max}"),
                ));
            }
            if count.is_nan() || count <= 0.0 {
                return Ok(());
            }
            for index in 1..=(count as u64) {
                scope.push();
                bind_alias(&mut *scope, &node.aliases, 0, Value::from(index));
                bind_alias(&mut *scope, &node.aliases, 1, Value::from(index - 1));
                bind_alias(&mut *scope, &node.aliases, 2, Value::Null);
                let result = render_nodes(&node.children, scope, out, state);
                scope.pop();
                result?;
            }
            Ok(())
        }
        Value::Null => Ok(()),
        other => Err(state.error(
            ErrorKind::Render,
            node.span,
            format!(
                "v-for 只能遍历数组 / 对象 / 数字，实际是 {}",
                type_name(&other)
            ),
        )),
    }
}

fn bind_alias(scope: &mut Scope<'_>, aliases: &[String], index: usize, value: Value) {
    if let Some(name) = aliases.get(index) {
        scope.bind(name, value);
    }
}

/// 对象遍历。默认 `serde_json` 用 BTreeMap（按键排序）；开启 `preserve_order`
/// 特性后是插入顺序，与 JS 的对象属性顺序一致。
fn into_ordered(object: Map<String, Value>) -> Vec<(String, Value)> {
    object.into_iter().collect()
}

fn type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "布尔值",
        Value::Number(_) => "数字",
        Value::String(_) => "字符串",
        Value::Array(_) => "数组",
        Value::Object(_) => "对象",
    }
}

// ── 元素与属性 ────────────────────────────────────────────────────

fn render_element(
    element: &ast::Element,
    scope: &mut Scope<'_>,
    out: &mut Html,
    state: &mut State<'_, '_, '_>,
) -> Result<(), Error> {
    let void = serializer::is_void(&element.tag);

    // 静态属性：class / style 走合并逻辑，其它原样输出。
    let mut attrs: Vec<Attr> = Vec::with_capacity(element.attrs.len() + element.bindings.len());
    let mut classes: Vec<String> = Vec::new();
    let mut styles: Vec<(String, String)> = Vec::new();

    for attr in &element.attrs {
        if attr.name.eq_ignore_ascii_case("class") {
            if let Some(value) = &attr.value {
                classes.extend(value.split_whitespace().map(str::to_owned));
            }
        } else if attr.name.eq_ignore_ascii_case("style") {
            if let Some(value) = &attr.value {
                merge_style_string(&mut styles, value);
            }
        } else {
            attrs.push(Attr {
                name: attr.name.clone(),
                value: attr.value.clone(),
            });
        }
    }

    // 动态绑定。
    for binding in &element.bindings {
        let value = eval(&binding.value, scope, state, binding.span)?;
        if binding.name.eq_ignore_ascii_case("class") {
            normalize_class(&value, &mut classes);
        } else if binding.name.eq_ignore_ascii_case("style") {
            normalize_style(&value, &mut styles);
        } else if let Some(attr) = attr_from_value(&binding.name, &value) {
            attrs.push(attr);
        }
    }

    // v-show：只动内联 display，元素本身保留。
    if let Some(condition) = &element.v_show {
        let value = eval(condition, scope, state, element.span)?;
        if !expr::truthy(&value) {
            set_style(&mut styles, "display", "none");
        }
    }

    if !classes.is_empty() {
        // Vue 不做去重：`class="a a" :class="'a'"` 就是 `a a a`。
        attrs.push(Attr::new("class", classes.join(" ")));
    }
    if !styles.is_empty() {
        attrs.push(Attr::new("style", serialize_style(&styles)));
    }

    out.start_tag(&element.tag, &attrs, element.self_closing);

    if let Some(html_expr) = &element.v_html {
        let value = eval(html_expr, scope, state, element.span)?;
        let text = display(&value);
        let html = html_policy(&text, state, element.span)?;
        out.raw(&html);
    } else if let Some(text_expr) = &element.v_text {
        let value = eval(text_expr, scope, state, element.span)?;
        out.text(&display(&value));
    } else if is_raw_text_tag(&element.tag) {
        // `<script>` / `<style>` 的内容是原始文本：不能再做 HTML 转义，
        // 否则 `a < b` 会变成 `a &lt; b`，JS/CSS 直接坏掉。
        for child in &element.children {
            match child {
                ast::Node::Text(text) => out.raw(&text.content),
                other => render_node(other, scope, out, state)?,
            }
        }
    } else {
        render_nodes(&element.children, scope, out, state)?;
    }

    if !void {
        // 即使模板写的是 `<div/>`，HTML 里也要补结束标签。
        out.end_tag(&element.tag);
    }
    Ok(())
}

fn attr_from_value(name: &str, value: &Value) -> Option<Attr> {
    match value {
        Value::Null => None,
        Value::Bool(true) if is_boolean_attr(name) => Some(Attr::boolean(name)),
        Value::Bool(false) if is_boolean_attr(name) => None,
        Value::Bool(true) => Some(Attr::new(name, "true")),
        Value::Bool(false) => Some(Attr::new(name, "false")),
        Value::Number(number) => Some(Attr::new(name, number_text(number))),
        Value::String(text) => Some(Attr::new(name, text.as_str())),
        other => Some(Attr::new(name, display(other))),
    }
}

/// HTML 布尔属性：真值输出裸属性名，假值整个省略。
fn is_boolean_attr(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "allowfullscreen"
            | "async"
            | "autofocus"
            | "autoplay"
            | "checked"
            | "controls"
            | "default"
            | "defer"
            | "disabled"
            | "formnovalidate"
            | "hidden"
            | "inert"
            | "ismap"
            | "itemscope"
            | "loop"
            | "multiple"
            | "muted"
            | "nomodule"
            | "novalidate"
            | "open"
            | "playsinline"
            | "readonly"
            | "required"
            | "reversed"
            | "selected"
    )
}

/// 内容按原始文本输出的元素（HTML 的 raw text elements）。
fn is_raw_text_tag(tag: &str) -> bool {
    tag.eq_ignore_ascii_case("script") || tag.eq_ignore_ascii_case("style")
}

// ── class ─────────────────────────────────────────────────────────

fn normalize_class(value: &Value, out: &mut Vec<String>) {
    match value {
        Value::String(text) => out.extend(text.split_whitespace().map(str::to_owned)),
        Value::Array(items) => {
            for item in items {
                normalize_class(item, out);
            }
        }
        Value::Object(map) => {
            for (name, active) in map {
                if expr::truthy(active) {
                    out.push(name.clone());
                }
            }
        }
        // Vue 对数字/布尔/空值不产生 class。
        _ => {}
    }
}

// ── style ─────────────────────────────────────────────────────────

/// `style="color:red;font-size:12px"` → 声明列表。
fn merge_style_string(out: &mut Vec<(String, String)>, text: &str) {
    for declaration in text.split(';') {
        let declaration = declaration.trim();
        if declaration.is_empty() {
            continue;
        }
        let Some((name, value)) = declaration.split_once(':') else {
            continue;
        };
        merge_style(out, name.trim(), value.trim());
    }
}

fn normalize_style(value: &Value, out: &mut Vec<(String, String)>) {
    match value {
        Value::String(text) => merge_style_string(out, text),
        Value::Array(items) => {
            for item in items {
                normalize_style(item, out);
            }
        }
        Value::Object(map) => {
            for (name, value) in map {
                // 先转 kebab-case：`lineHeight` / `zIndex` 的 unitless 判断要用它。
                let css_name = kebab_case(name);
                if let Some(value) = style_value(value) {
                    merge_style(out, &css_name, &value);
                }
            }
        }
        _ => {}
    }
}

fn style_value(value: &Value) -> Option<String> {
    match value {
        Value::Null | Value::Bool(_) => None,
        // Vue 3 不会给数字自动补 `px`：`{ width: 100 }` 输出 `width:100`（无效声明），
        // 需要单位请自己拼 `100 + 'px'`。这里保持同样的语义。
        Value::Number(number) => Some(number_text(number)),
        Value::String(text) => Some(text.clone()),
        // 数组/对象嵌在 style 值里没有意义，按显示字符串处理。
        other => Some(display(other)),
    }
}

fn merge_style(out: &mut Vec<(String, String)>, name: &str, value: &str) {
    if let Some(entry) = out
        .iter_mut()
        .find(|(existing, _)| existing.eq_ignore_ascii_case(name))
    {
        entry.1 = value.to_owned();
    } else {
        out.push((name.to_owned(), value.to_owned()));
    }
}

fn set_style(out: &mut Vec<(String, String)>, name: &str, value: &str) {
    merge_style(out, name, value);
}

fn serialize_style(styles: &[(String, String)]) -> String {
    styles
        .iter()
        .map(|(name, value)| format!("{name}: {value}"))
        .collect::<Vec<_>>()
        .join("; ")
}

/// `fontSize` → `font-size`（自定义属性 `--x` 原样保留）。
pub(crate) fn kebab_case(name: &str) -> String {
    if name.starts_with("--") {
        return name.to_owned();
    }
    let mut out = String::with_capacity(name.len() + 4);
    for (index, ch) in name.chars().enumerate() {
        if ch.is_ascii_uppercase() {
            if index > 0 {
                out.push('-');
            }
            out.push(ch.to_ascii_lowercase());
        } else {
            out.push(ch);
        }
    }
    out
}

// ── 求值与 HTML 安全 ──────────────────────────────────────────────

fn eval(
    expr: &Expr,
    scope: &Scope<'_>,
    state: &State<'_, '_, '_>,
    span: Span,
) -> Result<Value, Error> {
    let undefined = if state.options.strict {
        UndefinedBehavior::Error
    } else {
        state.options.undefined
    };
    expr::eval(expr, scope, undefined)
        .map_err(|error| Error::new(ErrorKind::Render, state.source, span.start, error.message))
}

fn html_policy(text: &str, state: &State<'_, '_, '_>, span: Span) -> Result<String, Error> {
    if let Some(sanitizer) = state.options.sanitizer {
        return sanitizer
            .sanitize(text)
            .map_err(|error| state.error(ErrorKind::Sanitize, span, error.message));
    }
    if state.options.trusted_html {
        Ok(text.to_owned())
    } else {
        Ok(serializer::escape_text(text))
    }
}

// ── 数字格式 ──────────────────────────────────────────────────────

// 数字格式化与 `ast::display` 共用一份实现（`2.0` 输出 `2`）。

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Number, json};

    #[test]
    fn kebab_cases_css_properties() {
        assert_eq!(kebab_case("fontSize"), "font-size");
        assert_eq!(kebab_case("WebkitTransform"), "webkit-transform");
        assert_eq!(kebab_case("--brand-color"), "--brand-color");
    }

    #[test]
    fn numbers_are_js_style() {
        assert_eq!(number_text(&Number::from(2)), "2");
        assert_eq!(number_text(&Number::from_f64(2.0).unwrap()), "2");
        assert_eq!(number_text(&Number::from_f64(1.5).unwrap()), "1.5");
    }

    #[test]
    fn class_normalization() {
        let mut out = Vec::new();
        normalize_class(
            &json!(["card", null, true, json!({ "active": true })]),
            &mut out,
        );
        assert_eq!(out, ["card", "active"]);
    }

    #[test]
    fn style_merging() {
        let mut styles = Vec::new();
        merge_style_string(&mut styles, "color: red; font-size:12px");
        normalize_style(&json!({ "color": "blue", "fontSize": 14 }), &mut styles);
        assert_eq!(
            styles,
            [
                ("color".to_owned(), "blue".to_owned()),
                ("font-size".to_owned(), "14".to_owned())
            ]
        );
    }
}
