//! 日志出口与脱敏。
//!
//! 解析链路跨了 `commands` → `netease`/`soda` → `http` 几层，它们都要记日志，
//! 但只有 `commands` 手上有 `HostApi`。所以这里放一个**进程级日志槽**：
//! 插件启动时把 `HostApi` 塞进来，其余模块直接 `debug()` / `warn()` 即可。
//!
//! 日志是横切关注点：不该为了它给每个纯函数多加一个参数。

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
/// **不加任何前缀**：宿主自己会在日志行上打来源（`[plugins - fuyubot.music]`）。
pub fn log(level: LogLevel, message: &str) {
    if let Some(host) = HOST.get() {
        host.log(level, message);
    }
}

/// 过程细节（请求、音质候选、命中分支）。默认 `Info` 门槛下看不到。
pub fn debug(message: &str) {
    log(LogLevel::Debug, message);
}

/// 用户关心的**结果**（解析成功、登录态是否就绪）。
pub fn info(message: &str) {
    log(LogLevel::Info, message);
}

/// 异常但还能往下走（回退到试听流、封面下载失败、临时文件清理失败）。
pub fn warn(message: &str) {
    log(LogLevel::Warn, message);
}

/// 走不下去的失败。
pub fn error(message: &str) {
    log(LogLevel::Error, message);
}

/// 把一段文本截到 `limit` 个字符（按字符数，不是字节——中文不能被切断）。
///
/// 日志里打印整页 HTML 毫无意义，而且正文里可能带着用户身份。
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
/// 两家的直链都把签名（`X-Bogus` / `play_auth` / 时效 token）塞在查询串或片段里，
/// 整条打进日志等于把凭据写进日志文件。
pub fn redact_url(url: &str) -> String {
    let base = url.split(['?', '#']).next().unwrap_or(url);
    if base.len() == url.len() {
        url.to_owned()
    } else {
        format!("{base}?<查询串已省略>")
    }
}

/// cookie 里有多少个键值对（不打内容）。
pub fn cookie_summary(cookie: &str) -> String {
    let count = cookie.split(';').filter(|part| part.contains('=')).count();
    format!("{count} 个键值对")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_lose_their_query_string_and_fragment() {
        assert_eq!(
            redact_url("https://x/song/url?id=1&token=secret"),
            "https://x/song/url?<查询串已省略>"
        );
        assert_eq!(
            redact_url("https://x/a.m4a#auth=secret"),
            "https://x/a.m4a?<查询串已省略>"
        );
        assert_eq!(redact_url("https://x/play"), "https://x/play");
    }

    #[test]
    fn long_text_is_truncated_by_chars_not_bytes() {
        let text = "一二三四五六七八九十";
        assert_eq!(shorten(text, 3), "一二三…（共 10 字符，已截断）");
        assert_eq!(shorten("abc", 10), "abc", "没超就不动");
    }

    #[test]
    fn cookie_is_summarized_without_content() {
        assert_eq!(cookie_summary("a=1;b=2;c=3"), "3 个键值对");
        assert_eq!(cookie_summary(""), "0 个键值对");
        assert_eq!(cookie_summary("a=1;junk"), "1 个键值对");
    }

    /// 装日志之前调用 log 不能崩（单测里就是没装的状态）。
    #[test]
    fn logging_before_install_is_silent_not_a_panic() {
        log(LogLevel::Info, "这条应该被丢掉");
        info("同上");
    }
}
