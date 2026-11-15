use super::*;

#[test]
fn object_id_shape() {
    let id = new_id();
    assert_eq!(id.len(), 24);
    assert!(is_object_id(&id));
    assert!(!is_object_id("xyz"));
}

#[test]
fn base64_round_trip() {
    for text in ["", "a", "ab", "abc", "unban?cid=-100&u=1", "中文"] {
        let encoded = base64_encode(text.as_bytes());
        let decoded = base64_decode(&encoded).expect("合法 base64");
        assert_eq!(decoded, text.as_bytes(), "{text}");
    }
}

#[test]
fn base64_without_padding() {
    assert_eq!(base64_decode("YQ").unwrap(), b"a");
}

#[test]
fn callback_is_split() {
    let (command, params) = parse_callback("unban?cid=-100123&u=456");
    assert_eq!(command, "unban");
    assert_eq!(param_i64(&params, "cid"), Some(-100123));
    assert_eq!(param_i64(&params, "u"), Some(456));
    assert_eq!(param_i64(&params, "missing"), None);
}

#[test]
fn local_time_is_not_utc() {
    let text = fmt_local(0);
    assert!(text.starts_with("19") || text.starts_with("20"), "{text}");
    assert!(text.contains('T'), "{text}");
    assert!(!text.ends_with('Z'), "本地时间不应带 Z：{text}");

    let plain = fmt_local_plain(0);
    assert_eq!(plain.len(), 19, "{plain}");
    assert!(!plain.contains('T'), "{plain}");
    assert_eq!(local_day().len(), 10);
}
