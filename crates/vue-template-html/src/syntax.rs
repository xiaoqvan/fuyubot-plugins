//! 解析阶段（syntax）的 AST：尽量贴着模板源码，**不解析表达式**。
//!
//! Parser 只负责把模板切成节点：元素、属性、文本、插值、注释。
//! 指令语义（`v-if` / `v-for` / `:class` …）与表达式解析都在 [`crate::transform`]
//! 里做——这样 Parser 可以独立测试，表达式引擎也能独立替换。
//!
//! 最终渲染用的是 [`crate::ast`]（带已解析的 [`crate::expr::Expr`]）。

/// 源码区间：字节偏移，`start..end`，左闭右开。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Span {
    pub start: usize,
    pub end: usize,
}

impl Span {
    pub const fn new(start: usize, end: usize) -> Self {
        Self { start, end }
    }

    /// 用另一个区间覆盖自己（例如把元素区间扩到结束标签）。
    pub fn cover(self, other: Span) -> Self {
        Self {
            start: self.start.min(other.start),
            end: self.end.max(other.end),
        }
    }
}

/// 整个模板。
#[derive(Debug, Clone, PartialEq)]
pub struct Root {
    pub children: Vec<Node>,
    pub span: Span,
}

/// 解析阶段的节点。
#[derive(Debug, Clone, PartialEq)]
pub enum Node {
    Element(Element),
    Text(Text),
    /// `{{ expression }}`；`raw` 是花括号里的原文，表达式在 transform 里解析。
    Interpolation(Interpolation),
    Comment(Comment),
}

/// 一个元素（含 `<template>`；它也是普通元素，交给 transform 处理）。
#[derive(Debug, Clone, PartialEq)]
pub struct Element {
    /// 原样保留大小写（SVG 的 `linearGradient` / `clipPath` 要靠它）。
    pub tag: String,
    pub attrs: Vec<Attribute>,
    pub children: Vec<Node>,
    /// `<br>` / `<img>` 这类 void 元素与 `<x />` 自闭合都记为 `true`。
    pub self_closing: bool,
    pub span: Span,
}

/// 属性：`class="a"`、`disabled`、`:src="url"` 都先按原样保存。
#[derive(Debug, Clone, PartialEq)]
pub struct Attribute {
    /// 原样属性名（含指令前缀，例如 `v-bind:class`、`:src`、`v-if`）。
    pub name: String,
    /// 引号里的原文；`None` = 无值属性。
    pub value: Option<String>,
    pub span: Span,
}

/// 解析后的指令（供 transform / 文档与 AST 消费者使用）。
///
/// 当前 transform 用更轻量的按名字匹配来避免分配，所以这个类型暂时只作为
/// 公开 AST 的文档性结构保留。
#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Directive {
    /// 指令名，不含 `v-`：`if` / `else-if` / `else` / `for` / `html` / `text` /
    /// `show` / `pre` / `bind` / `on` / `model` / `slot`。
    pub name: String,
    /// `v-bind:class` 里的 `class`；`v-if` 这类没有参数。
    pub arg: Option<String>,
    /// 指令表达式原文（`None` = 无值指令，例如 `v-else`、`v-pre`）。
    pub value: Option<String>,
    /// `.prop` / `.camel` 这类修饰符（本引擎基本不用，但保留解析结果）。
    pub modifiers: Vec<String>,
    pub span: Span,
}

/// 纯文本。`{{ }}` 已被 Parser 切走，这里不会再含插值。
#[derive(Debug, Clone, PartialEq)]
pub struct Text {
    pub content: String,
    pub span: Span,
}

/// 插值节点。
#[derive(Debug, Clone, PartialEq)]
pub struct Interpolation {
    /// 花括号里的原文，例如 ` user.name + 1 `。
    pub raw: String,
    /// 包含 `{{` `}}` 的完整区间。
    pub span: Span,
    /// 表达式在模板里的起点（`raw` 的第 0 个字节对应的模板偏移），用于报错定位。
    pub raw_start: usize,
}

/// HTML 注释。默认不输出（Vue 生产模式也不输出注释）。
#[derive(Debug, Clone, PartialEq)]
pub struct Comment {
    pub content: String,
    pub span: Span,
}
