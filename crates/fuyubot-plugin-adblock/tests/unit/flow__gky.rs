use super::*;

#[test]
fn parses_cantonese_format() {
    let body = "TGID: 8775864277\n已經封左 👉 販賣廣告\n如被誤封就搵 機械人";
    assert_eq!(
        parse_ban_record(body),
        Some((8_775_864_277, "販賣廣告".to_owned()))
    );
}

#[test]
fn parses_english_format() {
    let body = "TGID: 42 Banned 👉 spam links\nmore";
    assert_eq!(parse_ban_record(body), Some((42, "spam links".to_owned())));
}

#[test]
fn rejects_other_messages() {
    assert_eq!(parse_ban_record("普通聊天 TGID: 42"), None);
    assert_eq!(parse_ban_record("TGID: abc 已經封左 👉 x"), None);
    assert_eq!(parse_ban_record("TGID: 42 其它 👉 x"), None);
}
