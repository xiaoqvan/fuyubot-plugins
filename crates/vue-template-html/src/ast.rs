//! 渲染阶段的 AST：指令已经展开，表达式已经解析好。
//!
//! 和 [`crate::syntax`] 的分工：
//!
//! ```text
//! source ──parser──▶ syntax::Root ──transform──▶ ast::Root ──render──▶ String
//! ```
//!
//! 这里的节点可以直接渲染：`v-if` 变成 [`Node::If`]，`v-for` 变成 [`Node::For`]，
//! `<template>` 变成 [`Node::Template`]，`:attr` 变成 [`Element::bindings`]。

use serde_json::Value;

use crate::expr::Expr;
use crate::syntax::Span;

/// 整个渲染树。
#[derive(Debug, Clone, PartialEq)]
pub struct Root {
    pub children: Vec<Node>,
    pub span: Span,
}

/// 渲染节点。
#[derive(Debug, Clone, PartialEq)]
pub enum Node {
    Element(Element),
    Text(Text),
    Interpolation(Interpolation),
    /// `v-if` / `v-else-if` / `v-else` 链。
    If(If),
    /// `v-for`。
    For(For),
    /// `<template>` 逻辑容器（自身不输出标签）。
    Template(Template),
}

/// 元素。
#[derive(Debug, Clone, PartialEq)]
pub struct Element {
    /// 原样大小写（SVG 需要）。
    pub tag: String,
    /// 静态属性（已去掉指令与 `:key`）。
    pub attrs: Vec<Attribute>,
    /// 动态绑定（`v-bind` / `:`），按模板顺序。
    pub bindings: Vec<Binding>,
    /// `v-html`：输出原始 HTML（覆盖子节点）。
    pub v_html: Option<Expr>,
    /// `v-text`：输出转义文本（覆盖子节点）。
    pub v_text: Option<Expr>,
    /// `v-show`：假值时追加 `display:none`。
    pub v_show: Option<Expr>,
    /// 子节点（`v-html` / `v-text` 存在时渲染阶段忽略）。
    pub children: Vec<Node>,
    /// void 元素或显式自闭合。
    pub self_closing: bool,
    /// 是否位于 SVG 命名空间内（决定属性名大小写与自闭合策略）。
    pub in_svg: bool,
    pub span: Span,
}

/// 静态属性。
#[derive(Debug, Clone, PartialEq)]
pub struct Attribute {
    pub name: String,
    /// `None` = 布尔属性（`disabled`）。
    pub value: Option<String>,
    pub span: Span,
}

/// 动态属性绑定（`:src` / `v-bind:class`）。
#[derive(Debug, Clone, PartialEq)]
pub struct Binding {
    /// 目标属性名（`class` / `style` / `src` …）。
    pub name: String,
    pub value: Expr,
    pub span: Span,
}

/// 文本。
#[derive(Debug, Clone, PartialEq)]
pub struct Text {
    pub content: String,
    pub span: Span,
}

/// 插值。
#[derive(Debug, Clone, PartialEq)]
pub struct Interpolation {
    pub expr: Expr,
    pub span: Span,
}

/// 条件链。按顺序取第一个为真的分支；都不为真时走 `otherwise`。
#[derive(Debug, Clone, PartialEq)]
pub struct If {
    pub branches: Vec<IfBranch>,
    pub otherwise: Option<Vec<Node>>,
    pub span: Span,
}

/// 一个 `v-if` / `v-else-if` 分支。
#[derive(Debug, Clone, PartialEq)]
pub struct IfBranch {
    pub condition: Expr,
    pub children: Vec<Node>,
    pub span: Span,
}

/// 循环。
#[derive(Debug, Clone, PartialEq)]
pub struct For {
    /// 被遍历的表达式：数组 / 对象 / 数字。
    pub source: Expr,
    /// 别名，按 `(item, index)` / `(value, key, index)` 的顺序。
    pub aliases: Vec<String>,
    /// `:key` 的表达式（只做语法支持，没有 diff 语义）。
    pub key: Option<Expr>,
    pub children: Vec<Node>,
    pub span: Span,
}

/// `<template>` 逻辑容器。
#[derive(Debug, Clone, PartialEq)]
pub struct Template {
    pub children: Vec<Node>,
    pub span: Span,
}

/// 表达式求值结果转成 HTML 文本时的显示规则（`{{ }}` 与 `v-text` 共用）。
pub fn display(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::Bool(true) => "true".to_owned(),
        Value::Bool(false) => "false".to_owned(),
        Value::Number(number) => number_text(number),
        Value::String(text) => text.clone(),
        Value::Array(_) | Value::Object(_) => {
            // 与 Vue 的 `toDisplayString` 一致：对象/数组用 2 空格缩进的 JSON。
            serde_json::to_string_pretty(value).unwrap_or_default()
        }
    }
}

/// 数字 → JS 风格的字符串（`2.0` 输出 `2`，不是 `2.0`）。
pub(crate) fn number_text(number: &serde_json::Number) -> String {
    if let Some(int) = number.as_i64() {
        return int.to_string();
    }
    if let Some(int) = number.as_u64() {
        return int.to_string();
    }
    match number.as_f64() {
        Some(value) if value.fract() == 0.0 && value.abs() <= 9_007_199_254_740_992.0 => {
            format!("{}", value as i64)
        }
        Some(value) => format!("{value}"),
        None => number.to_string(),
    }
}
