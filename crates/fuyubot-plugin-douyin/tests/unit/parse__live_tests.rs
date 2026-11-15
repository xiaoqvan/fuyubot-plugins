use super::*;

#[tokio::test]
#[ignore = "需要联网 + 浏览器 cookie（TEST_DOUYIN_COOKIES）"]
async fn live_detail_api() {
    let path = std::env::var("TEST_DOUYIN_COOKIES").expect("TEST_DOUYIN_COOKIES 指向 cookies.json");
    let aweme_id = std::env::var("TEST_DOUYIN_AWEME_ID").expect("TEST_DOUYIN_AWEME_ID 指定作品 id");
    let raw = std::fs::read_to_string(&path).expect("读得到 cookie 文件");
    let cookie: Value = serde_json::from_str(&raw).expect("cookie 文件是 JSON");
    let cookie = cookie
        .get("cookies")
        .and_then(Value::as_str)
        .expect("cookies 字段存在")
        .to_owned();

    let agent = http::agent().expect("建 HTTP 客户端");
    let target = Target {
        kind: "video".to_owned(),
        id: aweme_id,
    };
    let item = fetch_aweme_item_api(&agent, &target, &cookie)
        .await
        .expect("API 路线应返回 aweme_detail");
    assert_eq!(
        str_field(&item, "aweme_id").as_deref(),
        Some(target.id.as_str())
    );
    let info = build_video_info(&item);
    assert!(
        info.video.is_some()
            || info
                .images
                .as_ref()
                .is_some_and(|images| !images.is_empty())
    );
}
