use super::*;

#[test]
fn quote_semantics_match_python() {
    // quote(safe='*-._')：* 保留、空格 %20、~ 保留、+ 转义、中文逐字节大写
    assert_eq!(quote_keep_star("star*kept"), "star*kept");
    assert_eq!(quote_keep_star("has space"), "has%20space");
    assert_eq!(quote_keep_star("tilde~x"), "tilde~x");
    assert_eq!(quote_keep_star("a+b"), "a%2Bb");
    assert_eq!(quote_keep_star("bang!"), "bang%21");
    assert_eq!(quote_keep_star("eq=amp&"), "eq%3Damp%26");
    assert_eq!(quote_keep_star("pct%20"), "pct%2520");
    assert_eq!(quote_keep_star("hash#"), "hash%23");
    assert_eq!(quote_keep_star("中文"), "%E4%B8%AD%E6%96%87");
    assert_eq!(quote_keep_star(""), "");
    // quote(safe='')：* 也转义
    assert_eq!(quote_strict("star*kept"), "star%2Akept");
    // quote_plus：空格 → +
    assert_eq!(quote_plus("has space"), "has+space");
    assert_eq!(quote_plus("a~b"), "a~b");
}

#[test]
fn urlencode_matches_python() {
    let pairs = vec![
        ("k".to_owned(), "v".to_owned()),
        ("a b".to_owned(), "c+d".to_owned()),
    ];
    assert_eq!(urlencode(&pairs), "k=v&a+b=c%2Bd");
}

#[test]
fn unquote_and_split_match_python() {
    assert_eq!(unquote("a%20b%2Bc"), "a b+c"); // + 原样保留
    assert_eq!(unquote("%E4%B8%AD"), "中");
    assert_eq!(unquote("100%"), "100%"); // 非法序列原样
    assert_eq!(split_pair("a=b=c"), ("a", "b=c"));
    assert_eq!(split_pair("bare"), ("bare", ""));
}
