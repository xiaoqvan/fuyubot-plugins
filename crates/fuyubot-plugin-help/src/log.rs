//! 日志小工具：把 `host.log` 的样板收在一处。

use fuyubot_abi::{HostApi, LogLevel};

pub fn info(host: &HostApi, message: impl AsRef<str>) {
    host.log(LogLevel::Info, message.as_ref());
}

pub fn warn(host: &HostApi, message: impl AsRef<str>) {
    host.log(LogLevel::Warn, message.as_ref());
}

pub fn error(host: &HostApi, message: impl AsRef<str>) {
    host.log(LogLevel::Error, message.as_ref());
}

pub fn debug(host: &HostApi, message: impl AsRef<str>) {
    host.log(LogLevel::Debug, message.as_ref());
}
