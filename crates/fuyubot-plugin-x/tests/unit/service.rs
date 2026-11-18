use super::*;

const API: &str = include_str!("../fixtures/status_article.json");

#[test]
fn parses_wrapped_api_object() {
    let input = format!(r#"{{"api":{API}}}"#);
    let post = parse_post(&input).unwrap();
    assert!(!post.title().is_empty());
}

#[test]
fn parses_raw_api_root() {
    let post = parse_post(API).unwrap();
    assert!(!post.title().is_empty());
}

#[test]
fn parses_api_json_string() {
    let input = json!({ "api": API }).to_string();
    let post = parse_post(&input).unwrap();
    assert!(!post.title().is_empty());
}

#[test]
fn rejects_bad_input() {
    assert!(parse_post("{}").is_err());
    assert!(parse_post("not json").is_err());
    assert!(parse_post(r#"{"api":"not json"}"#).is_err());
}
