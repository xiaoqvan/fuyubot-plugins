//! 编译 / 渲染选项。
//!
//! 两套选项刻意分开：
//!
//! - [`CompileOptions`] 影响**模板本身**（严格语法检查），在 [`Template::compile`] 时固定；
//! - [`RenderOptions`] 影响**这一次渲染**（数据、限额、安全策略），同一个模板可以用不同
//!   选项渲染多次。
//!
//! [`Template::compile`]: crate::Template::compile

use crate::Error;

/// 编译选项。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CompileOptions {
    /// 严格模式：
    ///
    /// - 不认识的 `v-*` 指令、`@event`、`#slot` 报 [`ErrorKind::Unsupported`]，
    ///   而不是静默丢弃；
    /// - `v-for` 别名不合法、重复别名等也直接报错。
    ///
    /// [`ErrorKind::Unsupported`]: crate::ErrorKind::Unsupported
    pub strict: bool,
}

/// 未定义变量的处理策略。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum UndefinedBehavior {
    /// 当作 `undefined` / `null`：插值输出空串，条件按假处理（默认）。
    #[default]
    Lenient,
    /// 直接报 [`ErrorKind::Render`] 错误。
    ///
    /// [`ErrorKind::Render`]: crate::ErrorKind::Render
    Error,
}

/// 渲染选项。
pub struct RenderOptions<'a> {
    /// 未定义变量策略（默认 [`UndefinedBehavior::Lenient`]）。
    pub undefined: UndefinedBehavior,
    /// 单个 `v-for` 最多迭代多少次（默认 10_000）。
    ///
    /// 超限报 [`ErrorKind::Limit`]，避免异常数据把进程拖死。
    ///
    /// [`ErrorKind::Limit`]: crate::ErrorKind::Limit
    pub max_iterations: usize,
    /// 最大嵌套深度（元素 / 条件 / 循环递归，默认 128）。
    pub max_depth: usize,
    /// 严格渲染：未定义变量与非法运算一律报错（即使 `undefined` 是 Lenient）。
    pub strict: bool,
    /// `v-html` 的安全策略。
    ///
    /// - `Some(sanitizer)`：先交给它清洗；
    /// - `None` + `trusted_html = true`：原样输出（默认，等价 Vue 的 v-html）；
    /// - `None` + `trusted_html = false`：按普通文本转义。
    pub sanitizer: Option<&'a dyn Sanitizer>,
    /// `v-html` 的内容是否可信（默认 `true`，与 Vue 语义一致）。
    pub trusted_html: bool,
}

impl Default for RenderOptions<'_> {
    fn default() -> Self {
        Self {
            undefined: UndefinedBehavior::Lenient,
            max_iterations: 10_000,
            max_depth: 128,
            strict: false,
            sanitizer: None,
            trusted_html: true,
        }
    }
}

impl<'a> RenderOptions<'a> {
    pub fn new() -> Self {
        Self::default()
    }

    /// 挂一个 HTML Sanitizer。
    pub fn sanitizer(mut self, sanitizer: &'a dyn Sanitizer) -> Self {
        self.sanitizer = Some(sanitizer);
        self
    }

    /// 未定义变量报错。
    pub fn strict_undefined(mut self) -> Self {
        self.undefined = UndefinedBehavior::Error;
        self
    }

    /// 设置单个循环的迭代上限。
    pub fn max_iterations(mut self, max: usize) -> Self {
        self.max_iterations = max;
        self
    }
}

/// HTML sanitizer 扩展接口。
///
/// 引擎本身**不做** HTML 清洗（白名单策略差异太大，而且容易做错）。需要处理不可信
/// `v-html` 时，实现这个 trait 并挂到 [`RenderOptions::sanitizer`] 上：
///
/// ```no_run
/// use vue_template_html::{Error, Sanitizer};
///
/// struct Whitelist;
///
/// impl Sanitizer for Whitelist {
///     fn sanitize(&self, html: &str) -> Result<String, Error> {
///         // 真实实现：解析 + 白名单过滤……
///         Ok(html.to_owned())
///     }
/// }
/// ```
pub trait Sanitizer {
    /// 清洗一段 HTML；返回 `Err` 会终止本次渲染。
    fn sanitize(&self, html: &str) -> Result<String, Error>;
}
