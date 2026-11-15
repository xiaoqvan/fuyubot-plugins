use super::*;

#[test]
fn extracts_mentions() {
    let text = "@SpamUser1 你好 @ab 与 @_ok_name 和 @long_name2";
    let mentions = extract_mentions(text);
    assert!(mentions.contains(&"spamuser1".to_owned()));
    assert!(!mentions.contains(&"ab".to_owned()));
    assert!(mentions.contains(&"long_name2".to_owned()));
}

#[test]
fn strips_html_tags() {
    assert_eq!(strip_html("<b>你好</b> 世界"), "你好 世界");
}
