use super::*;

#[test]
fn defaults_are_sane() {
    let config = Config::default();
    assert_eq!(config.width, 760);
    assert_eq!(config.scale, 2.0);
    assert_eq!(config.format, "jpg");
    assert!(config.fallback_text);
    assert!(!config.image.is_empty());
}

#[test]
fn placeholders_expand() {
    let config = Config::default();
    let text = config.expand("{count} 条 / {plugins} 个插件 / {version}", 7, 3);
    assert_eq!(text, format!("7 条 / 3 个插件 / {}", version_label()));
}

#[test]
fn format_aliases() {
    assert_eq!(normalize_format("JPEG").as_deref(), Some("jpg"));
    assert_eq!(normalize_format(" png ").as_deref(), Some("png"));
    assert_eq!(normalize_format("webp"), None);
}
