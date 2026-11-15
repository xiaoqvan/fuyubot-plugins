use super::*;
use serde_json::{Number, json};

#[test]
fn kebab_cases_css_properties() {
    assert_eq!(kebab_case("fontSize"), "font-size");
    assert_eq!(kebab_case("WebkitTransform"), "webkit-transform");
    assert_eq!(kebab_case("--brand-color"), "--brand-color");
}

#[test]
fn numbers_are_js_style() {
    assert_eq!(number_text(&Number::from(2)), "2");
    assert_eq!(number_text(&Number::from_f64(2.0).unwrap()), "2");
    assert_eq!(number_text(&Number::from_f64(1.5).unwrap()), "1.5");
}

#[test]
fn class_normalization() {
    let mut out = Vec::new();
    normalize_class(
        &json!(["card", null, true, json!({ "active": true })]),
        &mut out,
    );
    assert_eq!(out, ["card", "active"]);
}

#[test]
fn style_merging() {
    let mut styles = Vec::new();
    merge_style_string(&mut styles, "color: red; font-size:12px");
    normalize_style(&json!({ "color": "blue", "fontSize": 14 }), &mut styles);
    assert_eq!(
        styles,
        [
            ("color".to_owned(), "blue".to_owned()),
            ("font-size".to_owned(), "14".to_owned())
        ]
    );
}
