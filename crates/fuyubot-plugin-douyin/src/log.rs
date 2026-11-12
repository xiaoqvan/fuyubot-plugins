//! 日志出口与脱敏。
//!
//! 解析链路跨了三层（`commands` → `parse` → `http`），它们都要记日志，
//! 但只有 `commands` 手上有 `HostApi`（宿主句柄）。所以这里放一个**进程级
//! 日志槽**：插件启动时把 `HostApi` 塞进来，其余模块直接 `log()` 即可。
//!
//! ⚠️ 为什么不是「每层把 host 传下去」：那样 `parse` / `http` 的签名要为每个
//! 纯函数多带一个参数，而它们本身不需要宿主——日志是横切关注点，不是依赖。

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
/// **不加任何前缀**：宿主自己会在日志行上打来源（`[plugins - fuyubot.douyin]`
/// 那种），这里再加一个 `[douyin]` 是重复的，而且正文会因此变短、不利于阅读。
pub fn log(level: LogLevel, message: &str) {
    if let Some(host) = HOST.get() {
        host.log(level, message);
    }
}

/// 过程细节：请求、返回、挑中的清晰度、命中了哪条分支。
///
/// 宿主默认门槛是 `Info`（见 `fuyubot_loader::DEFAULT_LOG_VERBOSITY`），
/// 所以这些**默认看不到**，要排查时把 `[plugins].log_level` 调成 `"debug"`。
/// 这是刻意的：正常解析一条视频会打十来行，全塞进 Info 会把日志刷满。
pub fn debug(message: &str) {
    log(LogLevel::Debug, message);
}

/// 用户关心的**结果**（解析成功了一条、注册生效了）。
///
/// 只在这里用 Info：一次操作一行结论，不是过程流水账。
pub fn info(message: &str) {
    log(LogLevel::Info, message);
}

/// 异常但还能往下走（重试、回退到下载、清理失败）。
pub fn warn(message: &str) {
    log(LogLevel::Warn, message);
}

/// 走不下去的失败。
pub fn error(message: &str) {
    log(LogLevel::Error, message);
}

/// 把一段文本截到 `limit` 个字符（按字符数，不是字节——中文不能被切断）。
///
/// 日志里打印整页 HTML 毫无意义：抖音的分享页动辄几百 KB，会把宿主日志冲掉，
/// 而且**正文里带着 cookie 和用户身份**。所以日志只留开头一小段。
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
/// 抖音的直链把签名（`X-Bogus` / `signature` / 时效 token）全放在查询串里，
/// 整条打进日志等于把凭据写进日志文件。路径里的作品 id 才是有用的那一半。
pub fn redact_url(url: &str) -> String {
    match url.split_once('?') {
        Some((base, _)) => format!("{base}?<查询串已省略>"),
        None => url.to_owned(),
    }
}

/// cookie 里有多少个键值对（不打内容）。
///
/// 只报数量：够判断「cookie 是不是空的 / 有没有过期被清空」，又不泄凭据。
pub fn cookie_summary(cookie: &str) -> String {
    let count = cookie.split(';').filter(|part| part.contains('=')).count();
    format!("{count} 个键值对")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_lose_their_query_string() {
        assert_eq!(
            redact_url("https://x/aweme/v1/play/?video_id=1&X-Bogus=secret"),
            "https://x/aweme/v1/play/?<查询串已省略>"
        );
        assert_eq!(redact_url("https://x/play"), "https://x/play");
    }

    #[test]
    fn long_text_is_truncated_by_chars_not_bytes() {
        // 中文一个字 3 字节：按字节切会切出乱码，按字符不会。
        let text = "一二三四五六七八九十";
        assert_eq!(shorten(text, 3), "一二三…（共 10 字符，已截断）");
        assert_eq!(shorten("abc", 10), "abc", "没超就不动");
    }

    #[test]
    fn cookie_is_summarized_without_content() {
        assert_eq!(cookie_summary("a=1;b=2;c=3"), "3 个键值对");
        assert_eq!(cookie_summary(""), "0 个键值对");
        // 残缺的片段（没有 `=`）不算一个 cookie
        assert_eq!(cookie_summary("a=1;junk"), "1 个键值对");
    }

    /// 装日志之前调用 log 不能崩（单测里就是没装的状态）。
    #[test]
    fn logging_before_install_is_silent_not_a_panic() {
        log(LogLevel::Info, "这条应该被丢掉");
        info("同上");
    }
}
