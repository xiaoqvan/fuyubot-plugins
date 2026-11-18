use super::*;

#[test]
fn parses_google_segments() {
    let body = r#"[[["改完 DNS 就能绕过限制","just changed...",null,null,10],["。","",null,null,3]],null,"en"]"#;
    assert_eq!(parse(body).unwrap(), "改完 DNS 就能绕过限制。");
}

#[test]
fn rejects_bad_payloads() {
    assert!(parse("<html>").is_err());
    assert!(parse("[]").is_err());
}

#[test]
fn encodes_query_text() {
    assert_eq!(encode("a b/你"), "a%20b%2F%E4%BD%A0");
}
