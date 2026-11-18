use super::*;

#[test]
fn defaults_match_the_telegram_album_hard_limit() {
    let config = Config::default();
    assert_eq!(config.album_limit, ALBUM_MAX);
    assert_eq!(config.album_limit, 10);
    assert!(config.auto_parse);
    assert!(config.image_card);
    assert!(config.fallback_text);
    assert_eq!(config.card_format, "jpg");
}

#[test]
fn album_limit_is_clamped_into_two_to_ten() {
    assert_eq!(Config::from_json(r#"{"album_limit": 99}"#).album_limit, 10);
    assert_eq!(Config::from_json(r#"{"album_limit": 1}"#).album_limit, 2);
    assert_eq!(Config::from_json(r#"{"album_limit": 5}"#).album_limit, 5);
}

#[test]
fn broken_json_falls_back_to_defaults() {
    let config = Config::from_json("{ not json");
    assert_eq!(config.album_limit, ALBUM_MAX);
    assert_eq!(config.api_base, DEFAULT_API_BASE);
}

#[test]
fn api_base_drops_the_trailing_slash() {
    let config = Config::from_json(r#"{"api_base": "https://example.test/"}"#);
    assert_eq!(config.api_base, "https://example.test");
    // 非 http(s) 的地址不接受，保持默认。
    let config = Config::from_json(r#"{"api_base": "ftp://example.test"}"#);
    assert_eq!(config.api_base, DEFAULT_API_BASE);
}

#[test]
fn format_only_accepts_jpg_and_png() {
    assert_eq!(
        Config::from_json(r#"{"card_format": "JPG"}"#).card_format,
        "jpg"
    );
    assert_eq!(
        Config::from_json(r#"{"card_format": "png"}"#).card_format,
        "png"
    );
    assert_eq!(
        Config::from_json(r#"{"card_format": "webp"}"#).card_format,
        DEFAULT_CARD_FORMAT
    );
}

#[test]
fn numbers_are_clamped_to_sane_ranges() {
    let config = Config::from_json(r#"{"text_chunk": 99999, "send_timeout_ms": 1}"#);
    assert_eq!(config.text_chunk, 4000);
    assert_eq!(config.send_timeout_ms, 5_000);
}
