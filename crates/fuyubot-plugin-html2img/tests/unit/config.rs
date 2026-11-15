use super::*;

#[test]
fn image_format_parses_aliases() {
    assert_eq!(ImageFormat::parse("PNG"), Some(ImageFormat::Png));
    assert_eq!(ImageFormat::parse(" jpeg "), Some(ImageFormat::Jpg));
    assert_eq!(ImageFormat::parse("webp"), None);
    assert_eq!(ImageFormat::default(), ImageFormat::Jpg);
    assert_eq!(ImageFormat::Jpg.as_str(), "jpg");
}

#[test]
fn defaults_are_valid() {
    let config = Config::default();
    assert_eq!(config.width, 1024);
    assert!(config.auto_height);
    assert!(config.within_budget(config.width, config.height, config.scale));
}

#[test]
fn clamp_keeps_values_in_range() {
    let config = Config::default();
    assert_eq!(config.clamp_width(5), 64);
    assert_eq!(config.clamp_width(99_999), config.max_width);
    assert_eq!(config.clamp_height(1), 64);
    assert_eq!(config.clamp_scale(f32::NAN), config.scale);
    assert_eq!(config.clamp_scale(99.0), config.max_scale);
    assert_eq!(config.clamp_scale(-1.0), 0.1);
}

#[test]
fn budget_rejects_oversized_images() {
    let config = Config::default();
    // 1920 × 6000 × 4 > 1600 万像素。
    assert!(!config.within_budget(1920, 6000, 2.0));
    assert!(config.within_budget(800, 600, 1.0));
}

#[test]
fn font_family_rejects_css_injection() {
    assert!(safe_font_family(DEFAULT_FONT_FAMILY));
    assert!(!safe_font_family("sans-serif; } body { background: red"));
    assert!(!safe_font_family("   "));
}
