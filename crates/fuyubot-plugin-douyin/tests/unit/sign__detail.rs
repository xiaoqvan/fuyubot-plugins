use super::*;

#[test]
fn jar_parsing_keeps_values_verbatim() {
    let jar = parse_cookie_jar(" a=1 ; b=x=y ;UIFID=U;empty=");
    assert_eq!(jar.get("a").map(String::as_str), Some("1"));
    assert_eq!(jar.get("b").map(String::as_str), Some("x=y"));
    assert_eq!(jar.get("UIFID").map(String::as_str), Some("U"));
}

#[test]
fn no_uifid_means_no_request() {
    assert!(build_detail_request("1", "ttwid=x", 0, 0, &mut || 0.5).is_none());
}

#[test]
fn request_shape_is_the_platforms() {
    let request = build_detail_request(
        "42",
        "UIFID=U1;s_v_web_id=V1;ttwid=T1",
        1762600000,
        1762600000000,
        &mut || 0.5,
    )
    .expect("有 uifid 就有请求");
    // 平台顺序：…&aweme_id=&msToken=&a_bogus=&verifyFp=&fp=&uifid=&timestamp=&x-secsdk-web-signature=
    let order = [
        "aweme_id=42",
        "&msToken=",
        "&a_bogus=",
        "&verifyFp=V1",
        "&fp=V1",
        "&uifid=U1",
        "&timestamp=1762600000",
        "&x-secsdk-web-signature=",
    ];
    let mut cursor = 0;
    for needle in order {
        let found = request.url[cursor..]
            .find(needle)
            .unwrap_or_else(|| panic!("顺序里缺 {needle}"));
        cursor += found + needle.len();
    }
    let names: Vec<&str> = request
        .headers
        .iter()
        .map(|(name, _)| name.as_str())
        .collect();
    assert_eq!(
        names,
        ["uifid", "x-secsdk-web-signature", "x-secsdk-web-expire"]
    );
}
