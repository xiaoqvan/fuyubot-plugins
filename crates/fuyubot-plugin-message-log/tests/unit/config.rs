use super::*;

#[test]
fn defaults_match_the_documented_values() {
    let config = Config::default();
    assert!(config.enabled, "默认开启");
    assert!(!config.ignore_outgoing, "默认与 TS 版一致：发出去的也记");
    assert_eq!(config.max_text_chars, DEFAULT_MAX_TEXT_CHARS);
}

#[test]
fn partial_config_keeps_other_defaults() {
    let raw: Raw = serde_json::from_str(r#"{"ignore_outgoing": true}"#).unwrap();
    let config = Config::from_raw(raw);
    assert!(config.enabled);
    assert!(config.ignore_outgoing);
    assert_eq!(config.max_text_chars, DEFAULT_MAX_TEXT_CHARS);
}

#[test]
fn zero_means_unlimited_and_is_reported_as_such() {
    let raw: Raw = serde_json::from_str(r#"{"max_text_chars": 0}"#).unwrap();
    let config = Config::from_raw(raw);
    assert_eq!(config.max_text_chars, 0);
    assert!(config.summary().contains("不截断"));
}

#[test]
fn summary_reports_the_real_switches() {
    let config = Config {
        enabled: true,
        ignore_outgoing: true,
        max_text_chars: 100,
    };
    let summary = config.summary();
    assert!(summary.contains("记录自己发的消息=false"), "{summary}");
    assert!(summary.contains("正文上限=100 字符"), "{summary}");
}
