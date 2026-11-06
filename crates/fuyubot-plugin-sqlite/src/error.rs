//! 服务错误：一个稳定的 `code` + 一句给人看的 `message`。
//!
//! 跨 ABI 的返回值只能是 JSON，所以错误也走 JSON：`{"ok":false,"error":{...}}`。
//! `code` 用来写分支（不随文案变化），`message` 用来查日志。

use std::fmt;

use crate::sql::SqlError;

/// JSON 服务返回的错误。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceError {
    pub code: &'static str,
    pub message: String,
}

impl ServiceError {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    /// 入参不合法（缺字段 / 类型不对）。`code` 统一是 `bad_request`。
    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::new("bad_request", message)
    }
}

impl fmt::Display for ServiceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for ServiceError {}

impl From<SqlError> for ServiceError {
    fn from(error: SqlError) -> Self {
        Self::new(error.code, error.message)
    }
}

impl From<rusqlite::Error> for ServiceError {
    fn from(error: rusqlite::Error) -> Self {
        use rusqlite::ErrorCode;

        let code = match &error {
            rusqlite::Error::SqliteFailure(failure, _) => match failure.code {
                ErrorCode::DatabaseBusy => "database_busy",
                ErrorCode::DatabaseLocked => "database_locked",
                ErrorCode::ReadOnly => "read_only",
                ErrorCode::CannotOpen => "cannot_open",
                ErrorCode::DiskFull => "disk_full",
                _ => "sql_error",
            },
            _ => "sql_error",
        };
        Self::new(code, error.to_string())
    }
}

impl From<std::io::Error> for ServiceError {
    fn from(error: std::io::Error) -> Self {
        Self::new("io_error", error.to_string())
    }
}
