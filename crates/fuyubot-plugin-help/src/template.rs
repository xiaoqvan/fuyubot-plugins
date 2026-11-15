//! `/help` 图片版的 Vue 模板。
//!
//! 模板与样式放在 `assets/help.html`，编译期 `include_str!` 进动态库，运行时不需要
//! 额外的资源文件。交给 `fuyubot.html2img` 时带 `vue: true`，由它用
//! `vue-template-html` 编译、用本插件拼好的命令数据渲染。
//!
//! 样式是手写的（不是 Tailwind）。`hyper-render` 不跑 JS、也没有 Tailwind 运行时，
//! 所以只保留模板里用到的那一组布局 / 颜色规则，并把 `body` 的内边距压成 0，
//! 让渐变背景铺满整张图。

/// 原样交给 html2img 的模板（含 `<style>` 和根节点）。
pub const TEMPLATE: &str = include_str!("../assets/help.html");

#[cfg(test)]
#[path = "../tests/unit/template.rs"]
mod tests;
