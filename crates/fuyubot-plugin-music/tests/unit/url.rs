use super::*;

#[test]
fn host_ignores_port_userinfo_and_case() {
    assert_eq!(
        host_of("https://Music.163.com:443/song?id=1").as_deref(),
        Some("music.163.com")
    );
    assert_eq!(
        host_of("https://user:pass@example.com/track/x").as_deref(),
        Some("example.com")
    );
    assert_eq!(
        host_of("youtu.be/abc").as_deref(),
        Some("youtu.be"),
        "没有协议时按第一段兜底（first_link_where 只喂带协议的链接）"
    );
    assert_eq!(
        host_of("https://music.youtube.com.evil.tld/watch?v=x").as_deref(),
        Some("music.youtube.com.evil.tld")
    );
    assert_eq!(host_of("https://youtu.be/abc").as_deref(), Some("youtu.be"));
}

#[test]
fn punctuation_is_trimmed_from_links() {
    assert_eq!(
        trim_punctuation("https://youtu.be/x)"),
        "https://youtu.be/x"
    );
    assert_eq!(
        trim_punctuation("https://example.com/track/x，"),
        "https://example.com/track/x"
    );
    assert_eq!(
        trim_punctuation("https://youtu.be/x?si=1。"),
        "https://youtu.be/x?si=1"
    );
    assert_eq!(trim_punctuation("plain"), "plain");
}

#[test]
fn first_link_where_skips_other_hosts() {
    let text = "看这个 https://example.com/a 还有 https://youtu.be/dQw4w9WgXcQ 很好听";
    let found = first_link_where(text, |url| host_of(url).as_deref() == Some("youtu.be"));
    assert_eq!(found.as_deref(), Some("https://youtu.be/dQw4w9WgXcQ"));
}

#[test]
fn percent_encode_keeps_unreserved_and_encodes_cjk() {
    assert_eq!(percent_encode("Shape of You"), "Shape%20of%20You");
    assert_eq!(percent_encode("a-b_c.d~e"), "a-b_c.d~e");
    assert_eq!(percent_encode("晴天"), "%E6%99%B4%E5%A4%A9");
    assert_eq!(percent_encode("a&b=c"), "a%26b%3Dc");
}

#[test]
fn query_value_is_case_insensitive_and_ignores_empty() {
    assert_eq!(
        query_value("https://x/watch?V=abc&v=def", "v").as_deref(),
        Some("abc")
    );
    assert_eq!(
        query_value("https://x/watch?v=abc#frag", "v").as_deref(),
        Some("abc")
    );
    assert_eq!(query_value("https://x/watch?v=", "v"), None);
    assert_eq!(query_value("https://x/watch", "v"), None);
}
