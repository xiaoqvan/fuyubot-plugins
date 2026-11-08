//! 轻量表达式引擎：Vue 模板里 `{{ }}` / `v-if` / `:class` 用的那点 JavaScript 子集。
//!
//! **不执行 JavaScript**：没有函数调用、没有 `eval`、没有 `new`、没有赋值、没有
//! 箭头函数。只有字面量、变量、成员/下标访问、一元/二元运算、三元与数组/对象字面量。
//!
//! ## 语义（对齐 JavaScript / Vue）
//!
//! - 真值判断按 JS：`false` / `0` / `""` / `null` / `undefined` 为假，其余为真
//!   （包括空数组、空对象——JS 里它们也是真值）。
//! - `&&` / `||` **返回操作数本身**（`a || "default"`、`flag && "yes"` 靠这个）。
//! - `+`：任一侧是字符串就做字符串拼接，否则做数值加法。
//! - `===` / `!==`：类型不同直接不等；`==` / `!=` 做最小限度的宽松比较
//!   （`null == undefined`、数字与字符串/布尔的互转）。
//! - `!` 取反；一元 `-` 把值转数字；一元 `+` 也转数字。
//! - 访问 `null` / `undefined` 的成员是错误（JS 的 TypeError）；Lenient 模式把它
//!   当作 `undefined`（渲染为空），严格模式报错。
//!
//! ## 对象字面量
//!
//! 支持 `{ color: color, fontSize: size + 'px', "line-height": 1.5 }` 与简写
//! `{ color }`——动态 `:class` / `:style` 需要它们。

mod eval;
mod lexer;
mod parser;

#[cfg(test)]
mod tests;

pub use eval::truthy;

use serde_json::Value;

use crate::options::UndefinedBehavior;
use crate::syntax::Span;

/// 表达式 AST。
#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    /// 字面量：字符串 / 数字 / 布尔 / `null` / 数组 / 对象以外的值。
    Literal {
        value: Value,
        span: Span,
    },
    /// 变量名（从作用域链里查）。
    Var {
        name: String,
        span: Span,
    },
    /// 成员访问：`a.b` 或 `a[expr]`。
    Member {
        object: Box<Expr>,
        property: Box<Expr>,
        span: Span,
    },
    Unary {
        op: UnaryOp,
        operand: Box<Expr>,
        span: Span,
    },
    Binary {
        op: BinaryOp,
        left: Box<Expr>,
        right: Box<Expr>,
        span: Span,
    },
    Ternary {
        condition: Box<Expr>,
        consequent: Box<Expr>,
        alternate: Box<Expr>,
        span: Span,
    },
    Array {
        items: Vec<Expr>,
        span: Span,
    },
    /// 对象字面量；键已经规范化成字符串。
    Object {
        entries: Vec<(String, Expr)>,
        span: Span,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnaryOp {
    /// `!`
    Not,
    /// `-`
    Negate,
    /// `+`
    Plus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinaryOp {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    /// `===`
    EqStrict,
    /// `!==`
    NeStrict,
    /// `==`
    EqLoose,
    /// `!=`
    NeLoose,
    Lt,
    Le,
    Gt,
    Ge,
    And,
    Or,
}

/// 表达式语法错误。`offset` 相对**表达式字符串**起点。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExprError {
    pub offset: usize,
    pub message: String,
}

impl ExprError {
    pub fn new(offset: usize, message: impl Into<String>) -> Self {
        Self {
            offset,
            message: message.into(),
        }
    }
}

/// 表达式求值错误。`offset` 相对表达式起点（可能为 0，例如运行期类型错误）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvalError {
    pub offset: usize,
    pub message: String,
}

impl EvalError {
    pub fn new(offset: usize, message: impl Into<String>) -> Self {
        Self {
            offset,
            message: message.into(),
        }
    }
}

/// 变量查找接口：渲染层用它实现「v-for 局部变量 → 外层数据」的作用域链。
pub trait Scope {
    fn get(&self, name: &str) -> Option<&Value>;
}

/// 解析一段表达式。
pub fn parse(source: &str) -> Result<Expr, ExprError> {
    parser::parse(source)
}

/// 求值。
pub fn eval(
    expr: &Expr,
    scope: &dyn Scope,
    undefined: UndefinedBehavior,
) -> Result<Value, EvalError> {
    eval::eval(expr, scope, undefined)
}
