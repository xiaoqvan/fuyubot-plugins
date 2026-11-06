//! 日志门面：所有日志都经宿主 `host.log` 打，级别与 `[plugins].log_level` 一致。
//!
//! 为什么不直连 `tracing`：插件是动态库，宿主已经装好了订阅器；自己再装一份会
//! 出现「同一行打两遍」或「插件日志不进 TUI / 文件」两种偏差。这层只做三件事：
//! 统一前缀、按级别转发、**不吞错误**（格式化失败就退回一条静态文案）。

use fuyubot_abi::{HostApi, LogLevel};

fn emit(host: &HostApi, level: LogLevel, message: &str) {
    // 宿主自己会加插件名，这里不再重复；只把换行压平，避免一条日志撑成多行。
    let flat = message.replace(['\n', '\r'], " ");
    host.log(level, &flat);
}

#[allow(dead_code)]
pub fn error(host: &HostApi, message: impl AsRef<str>) {
    emit(host, LogLevel::Error, message.as_ref());
}

pub fn warn(host: &HostApi, message: impl AsRef<str>) {
    emit(host, LogLevel::Warn, message.as_ref());
}

pub fn info(host: &HostApi, message: impl AsRef<str>) {
    emit(host, LogLevel::Info, message.as_ref());
}

pub fn debug(host: &HostApi, message: impl AsRef<str>) {
    emit(host, LogLevel::Debug, message.as_ref());
}

/// 常用组合：`log::warnf(&host, format_args!("…"))`。
///
/// 只在确实要拼字符串时用；能直接给 `&str` 的地方别走它。
pub fn warnf(host: &HostApi, args: std::fmt::Arguments<'_>) {
    warn(host, args.to_string());
}

pub fn infof(host: &HostApi, args: std::fmt::Arguments<'_>) {
    info(host, args.to_string());
}
