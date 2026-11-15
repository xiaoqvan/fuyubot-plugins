use super::*;

#[test]
fn parses_payload() {
    assert_eq!(
        parse_payload("aiConfig_-1001234567890"),
        Some(-1_001_234_567_890)
    );
    assert_eq!(parse_payload("aiConfig_123"), None);
    assert_eq!(parse_payload("something"), None);
}

#[test]
fn normalizes_urls() {
    assert_eq!(
        normalize_url("https://api.openai.com/v1/"),
        Some("https://api.openai.com/v1".to_owned())
    );
    assert_eq!(normalize_url("ftp://x"), None);
    assert_eq!(normalize_url("https://no-dot"), None);
}
