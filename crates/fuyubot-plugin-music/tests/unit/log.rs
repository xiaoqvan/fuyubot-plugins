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
