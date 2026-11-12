//! 日志门面：所有日志都经宿主 `host.log` 打，级别与 `[plugins].log_level` 一致。
//!
//! 为什么不自己写文件 / 自己装订阅器：宿主已经把插件日志统一成
//!
//! ```text
//! 2026-11-11 13:45:00 [plugins - fuyubot.message-log] INFO  正文
//! ```
//!
//! 并同时投递到控制台、TUI 日志页和 `logs/fuyubot.log`。插件再往里塞时间戳或
//! 插件名只会重复；这一层只做三件事：统一前缀（没有）、按级别转发、
//! **兜底压平换行**（`render` 已经把正文里的换行转义成 `\n`，这里只是最后一道保险）。

use fuyubot_abi::{HostApi, LogLevel};

fn emit(host: &HostApi, level: LogLevel, message: &str) {
    let flat = message.replace(['\n', '\r'], " ");
    host.log(level, &flat);
}

pub fn info(host: &HostApi, message: impl AsRef<str>) {
    emit(host, LogLevel::Info, message.as_ref());
}

pub fn warn(host: &HostApi, message: impl AsRef<str>) {
    emit(host, LogLevel::Warn, message.as_ref());
}

#[allow(dead_code)]
pub fn error(host: &HostApi, message: impl AsRef<str>) {
    emit(host, LogLevel::Error, message.as_ref());
}

#[allow(dead_code)]
pub fn debug(host: &HostApi, message: impl AsRef<str>) {
    emit(host, LogLevel::Debug, message.as_ref());
}
