//! # vue-template-html
//!
//! 轻量级 **Vue 3 模板 → HTML** 编译器 / 渲染器。面向服务端与图片生成场景：
//! 没有 Vue runtime、没有 Node.js、没有 JavaScript 引擎、没有 DOM。
//!
//! ```text
//! source ──parser──▶ syntax AST ──transform──▶ render AST ──render──▶ HTML
//!                                                            │
//!                                                     data: serde_json::Value
//! ```
//!
//! ## 快速开始
//!
//! ```
//! use serde_json::json;
//! use vue_template_html::Template;
//!
//! let template = Template::compile(
//!     r#"<div class="card">
//!          <h1>{{ title }}</h1>
//!          <p v-if="visible">{{ user.name }}</p>
//!          <ul><li v-for="(item, i) in items">{{ i + 1 }}. {{ item.name }}</li></ul>
//!        </div>"#,
//! )
//! .unwrap();
//!
//! let html = template
//!     .render(&json!({
//!         "title": "Hello",
//!         "visible": true,
//!         "user": { "name": "Alice" },
//!         "items": [{ "name": "A" }, { "name": "B" }],
//!     }))
//!     .unwrap();
//!
//! assert!(html.contains("<h1>Hello</h1>"));
//! assert!(html.contains("1. A"));
//! ```
//!
//! 同一个 [`Template`] 可以用不同数据反复渲染；也可以 `Clone` 之后跨线程共享
//! （渲染本身不再持有任何可变状态）。
//!
//! ## 读取 `.vue` 的 props
//!
//! [`Sfc`] 负责拆 `<template>` / `<script>` / `<style>`，并读取 `defineProps` 声明的
//! props（inline 类型、`interface Props`、`withDefaults`、运行时对象写法、数组简写）：
//!
//! ```no_run
//! use vue_template_html::{Sfc, Template};
//!
//! let source = std::fs::read_to_string("Help.vue")?;
//! let sfc = Sfc::parse(&source)?;
//!
//! for prop in sfc.props()? {
//!     println!("{}: {:?} optional={} default={:?}", prop.name, prop.kind, prop.optional, prop.default);
//! }
//!
//! // 按默认值 / 类型生成占位数据（真实渲染请换成业务数据）
//! let data = sfc.sample_data();
//! let html = Template::compile(sfc.template())?.render(&data)?;
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! [`Sfc`] 不执行 `<script>`，也不做完整 TypeScript 解析；支持的 `defineProps` 形式见 [`Sfc`] 的文档。
//!
//! ## 支持的范围
//!
//! 文本插值、`v-if` / `v-else-if` / `v-else`、`v-for`、`v-html`、`v-text`、
//! `v-bind`（含 `:class` / `:style` 特殊处理）、`v-show`、`v-pre`、`<template>`、
//! SVG 标签。完整清单与「暂不支持」见 README。
//!
//! ## 安全边界
//!
//! 表达式是**自己解析、自己求值**的，绝不把字符串交给 JS 引擎；`v-html` 默认按
//! Vue 语义输出原始 HTML（视为可信），不可信内容请挂 [`Sanitizer`] 或把
//! [`RenderOptions::trusted_html`] 设为 `false`。

mod ast;
mod error;
mod expr;
mod options;
mod parser;
mod render;
mod serializer;
mod sfc;
mod syntax;
mod transform;

pub use error::{Error, ErrorKind};
pub use options::{CompileOptions, RenderOptions, Sanitizer, UndefinedBehavior};
pub use sfc::{PropDef, PropKind, Sfc};

use serde_json::Value;

/// 预编译好的模板。
///
/// 编译一次（解析 + 指令展开 + 表达式解析），之后每次渲染只做求值与字符串拼接。
#[derive(Debug, Clone)]
pub struct Template {
    root: ast::Root,
    source: String,
    options: CompileOptions,
}

impl Template {
    /// 用默认选项编译。
    pub fn compile(source: &str) -> Result<Self, Error> {
        Self::compile_with_options(source, CompileOptions::default())
    }

    /// 带选项编译。
    pub fn compile_with_options(source: &str, options: CompileOptions) -> Result<Self, Error> {
        let syntax = parser::parse(source)?;
        let root = transform::transform(syntax, source, options)?;
        Ok(Self {
            root,
            source: source.to_owned(),
            options,
        })
    }

    /// 编译时使用的选项。
    pub fn options(&self) -> CompileOptions {
        self.options
    }

    /// 原始模板源码（报错定位用）。
    pub fn source(&self) -> &str {
        &self.source
    }

    /// 用默认渲染选项渲染。
    pub fn render(&self, data: &Value) -> Result<String, Error> {
        self.render_with_options(data, &RenderOptions::default())
    }

    /// 带渲染选项渲染。
    pub fn render_with_options(
        &self,
        data: &Value,
        options: &RenderOptions<'_>,
    ) -> Result<String, Error> {
        render::render(&self.root, data, options, &self.source)
    }
}

/// 一次性编译 + 渲染的便捷函数（模板只渲染一次时用）。
pub fn render(source: &str, data: &Value) -> Result<String, Error> {
    Template::compile(source)?.render(data)
}
