//! 错误类型：带模板源位置（行 / 列 / 字节偏移）的统一错误。
//!
//! 解析、表达式、渲染、限额、Sanitizer 五类错误共用这一个类型——调用方只需要
//! 一个 `Result<T, Error>`，不用为每一层再包一次。

use std::fmt;

/// 错误分类。`strict` 模式下不可用的语法也会归到 [`ErrorKind::Unsupported`]。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    /// 模板语法错误（标签不匹配、属性没闭合、插值没闭合……）。
    Parse,
    /// 表达式语法 / 求值错误。
    Expression,
    /// 渲染期错误（未定义变量、非法数据、循环次数超限……）。
    Render,
    /// 资源限额（最大循环次数 / 最大递归深度）。
    Limit,
    /// Sanitizer 拒绝了这段 HTML。
    Sanitize,
    /// 语法本身合法，但本引擎不支持（严格模式下才报）。
    Unsupported,
}

impl ErrorKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Parse => "模板解析错误",
            Self::Expression => "表达式错误",
            Self::Render => "渲染错误",
            Self::Limit => "资源限额",
            Self::Sanitize => "HTML 安全错误",
            Self::Unsupported => "不支持的语法",
        }
    }
}

/// 编译 / 渲染错误。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error {
    pub kind: ErrorKind,
    pub message: String,
    /// 相对模板源起点的字节偏移（从 0 开始）。
    pub offset: usize,
    /// 1 开始的行号。
    pub line: usize,
    /// 1 开始的列号（按字符数，不是字节）。
    pub column: usize,
}

impl Error {
    /// 按源文件与字节偏移构造。
    ///
    /// `offset` 必须落在 UTF-8 字符边界上（解析器只会在字符边界上报错）；
    /// 越界会被夹到 `source.len()`。
    pub(crate) fn new(
        kind: ErrorKind,
        source: &str,
        offset: usize,
        message: impl Into<String>,
    ) -> Self {
        let offset = offset.min(source.len());
        let (line, column) = line_column(source, offset);
        Self {
            kind,
            message: message.into(),
            offset,
            line,
            column,
        }
    }
}

impl Error {
    /// 构造一个「没有模板位置」的错误（位置为 1:1）。
    ///
    /// 主要给扩展点用：例如 [`Sanitizer`](crate::Sanitizer) 的实现想拒绝一段 HTML，
    /// 又不在模板的某一处，就可以 `Error::custom(ErrorKind::Sanitize, "…")`。
    pub fn custom(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            offset: 0,
            line: 1,
            column: 1,
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}（第 {} 行第 {} 列）：{}",
            self.kind.label(),
            self.line,
            self.column,
            self.message
        )
    }
}

impl std::error::Error for Error {}

/// 字节偏移 → (行, 列)，都从 1 开始。
pub(crate) fn line_column(source: &str, offset: usize) -> (usize, usize) {
    let offset = offset.min(source.len());
    let prefix = &source[..offset];
    let line = prefix.bytes().filter(|byte| *byte == b'\n').count() + 1;
    let line_start = prefix.rfind('\n').map(|index| index + 1).unwrap_or(0);
    let column = prefix[line_start..].chars().count() + 1;
    (line, column)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_column_counts_chars_not_bytes() {
        let source = "ab\n中文x";
        let offset = source.find('x').unwrap();
        assert_eq!(line_column(source, offset), (2, 3));
    }

    #[test]
    fn error_clamps_offset() {
        let error = Error::new(ErrorKind::Parse, "abc", 99, "boom");
        assert_eq!(error.offset, 3);
        assert_eq!(error.line, 1);
        assert_eq!(error.column, 4);
    }
}
