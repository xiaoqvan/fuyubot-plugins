//! 日志出口与脱敏。
//!
//! 解析链路跨了几层（`commands` → `parse` → `http`），它们都要记日志，但只有
//! `commands` 手上有 `HostApi`（宿主句柄）。所以这里放一个**进程级日志槽**：
//! 插件启动时把 `HostApi` 塞进来，其余模块直接 `log::info(…)` 即可。

use std::sync::OnceLock;

use fuyubot_abi::{HostApi, LogLevel};

/// 插件启动时写入一次。没写入时 [`log`] 静默丢弃（单测里就是这种情况）。
static HOST: OnceLock<HostApi> = OnceLock::new();

/// 装好 `HostApi`（`Plugin::init` / `start` 里各调一次都无害：`OnceLock` 只认第一次）。
pub fn install(host: HostApi) {
    let _ = HOST.set(host);
}

/// 记一条日志。宿主句柄还没装好时静默丢弃。
///
/// **不加任何前缀**：宿主自己会在日志行上打来源（`[plugins - xiaoqvan.x]` 那种），
/// 这里再加一个 `[x]` 是重复的。
pub fn log(level: LogLevel, message: &str) {
    if let Some(host) = HOST.get() {
        host.log(level, message);
    }
}

/// 过程细节：请求、解析到几块、切了几组相册。默认门槛是 Info，看不到。
pub fn debug(message: &str) {
    log(LogLevel::Debug, message);
}

/// 用户关心的**结果**（解析成功了一条、登记生效了）。
pub fn info(message: &str) {
    log(LogLevel::Info, message);
}

/// 异常但还能往下走（回退、降级到文本、某项媒体失败）。
pub fn warn(message: &str) {
    log(LogLevel::Warn, message);
}

/// 走不下去的失败。
pub fn error(message: &str) {
    log(LogLevel::Error, message);
}

/// 把一段文本截到 `limit` 个字符（按字符数，不是字节——中文不能被切断）。
pub fn shorten(text: &str, limit: usize) -> String {
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= limit {
        return text.to_owned();
    }
    let head: String = chars[..limit].iter().collect();
    format!("{head}…（共 {} 字符，已截断）", chars.len())
}

/// 从 URL 里去掉查询串，只留「谁 + 哪条路径」。
///
/// X 的媒体直链把签名（`tag=` / 时效 token）放在查询串里，整条打进日志等于把
/// 凭据写进日志文件；路径里的媒体 id 才是有用的那一半。
pub fn redact_url(url: &str) -> String {
    match url.split_once('?') {
        Some((base, _)) => format!("{base}?<查询串已省略>"),
        None => url.to_owned(),
    }
}
