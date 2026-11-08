//! 一层薄薄的日志包装：拿不到 `HostApi` 的地方（纯函数测试）也能静默降级。

use fuyubot_abi::{HostApi, LogLevel};

pub fn error(host: &HostApi, message: impl AsRef<str>) {
    host.log(LogLevel::Error, message.as_ref());
}

pub fn warn(host: &HostApi, message: impl AsRef<str>) {
    host.log(LogLevel::Warn, message.as_ref());
}

pub fn info(host: &HostApi, message: impl AsRef<str>) {
    host.log(LogLevel::Info, message.as_ref());
}
