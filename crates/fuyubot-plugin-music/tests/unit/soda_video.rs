use super::*;

#[test]
fn video_ids_are_recognised_in_every_shape() {
    assert_eq!(
        video_id_of(
            "https://music.douyin.com/qishui/share/ugc_video?ugc_video_id=7625615883351772468&x=1"
        )
        .as_deref(),
        Some("7625615883351772468")
    );
    assert_eq!(
        video_id_of(r#"{"video_id":"7625615883351772468"}"#).as_deref(),
        Some("7625615883351772468")
    );
    assert_eq!(
        video_id_of("%2Fugc_video%2F7625615883351772468").as_deref(),
        Some("7625615883351772468")
    );
    assert_eq!(video_id_of("https://www.qishui.com/track/123"), None);
    assert_eq!(video_id_of("没有 ID"), None);
}

#[test]
fn router_data_is_extracted_with_brace_matching() {
    let html = r#"<html><script>window._ROUTER_DATA = {"loaderData":{"ugc_video_page":{"videoOptions":{"videoName":"a}b","duration":3}}}};</script></html>"#;
    let value = router_data(html).expect("应该能解出来");
    assert_eq!(
        value.pointer("/loaderData/ugc_video_page/videoOptions/videoName"),
        Some(&Value::String("a}b".to_owned())),
        "字符串里的右花括号不能提前结束"
    );
    assert!(router_data("<html>没有数据</html>").is_none());
}

#[test]
fn video_options_are_mapped_to_a_post() {
    let value = serde_json::json!({
        "loaderData": {"ugc_video_page": {"videoOptions": {
            "url": "https://cdn/v.mp4",
            "duration": 33.855,
            "width": 1920,
            "height": 1080,
            "videoName": "塔不灭 #永雏塔菲",
            "artistName": "永川塔菲",
            "coverURL": "https://cdn/c.jpg",
            "status_code": 0,
            "previewStart": 0,
            "previewEnd": 33.855
        }}}
    });
    let options = value
        .pointer("/loaderData/ugc_video_page/videoOptions")
        .unwrap();
    assert_eq!(
        options.get("duration").and_then(Value::as_f64),
        Some(33.855)
    );
    assert_eq!(int_of(options, "width"), 1920);
    assert_eq!(string_of(options, "artistName"), "永川塔菲");
}

#[test]
fn share_url_carries_the_encoded_id() {
    let url = share_url("7625615883351772468");
    assert!(url.contains("ugc_video_id=7625615883351772468"), "{url}");
    assert!(
        url.starts_with("https://music.douyin.com/qishui/share/ugc_video"),
        "{url}"
    );
}
