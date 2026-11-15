use super::*;

#[test]
fn md5_matches_the_reference() {
    // Python: hashlib.md5(b"abc").hexdigest()
    assert_eq!(md5_hex("abc"), "900150983cd24fb0d6963f7d28e17f72");
}

#[test]
fn encode_pairs_is_urlsearchparams() {
    let pairs = vec![
        ("k".to_owned(), "has space".to_owned()),
        ("star*".to_owned(), "a+b".to_owned()),
    ];
    assert_eq!(encode_pairs(&pairs), "k=has%20space&star*=a%2Bb");
}

#[test]
fn sign_appends_in_platform_order() {
    let pairs = vec![("aweme_id".to_owned(), "1".to_owned())];
    let signed = sign(&pairs, "U", 1762600000);
    assert_eq!(
        signed.query,
        format!(
            "aweme_id=1&uifid=U&timestamp=1762600000&x-secsdk-web-signature={}",
            signed.signature
        )
    );
    assert_eq!(signed.signature.len(), 32);
    assert_eq!(signed.headers.len(), 3);
}

#[test]
fn existing_uifid_keeps_its_position() {
    let pairs = vec![
        ("uifid".to_owned(), "KEEP".to_owned()),
        ("a".to_owned(), "1".to_owned()),
    ];
    let signed = sign(&pairs, "IGNORED", 1762600000);
    assert!(signed.query.starts_with("uifid=KEEP&a=1&timestamp="));
    assert!(!signed.query.contains("IGNORED"));
}
