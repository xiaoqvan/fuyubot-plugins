use super::*;

#[test]
fn quality_aliases_and_default_are_usable() {
    assert_eq!(parse_quality("LOSSLESS"), Some("lossless"));
    assert_eq!(parse_quality("无损"), Some("lossless"));
    assert_eq!(parse_quality("hq"), Some("highest"));
    assert_eq!(parse_quality("nope"), None);
    assert_eq!(parse_quality(DEFAULT_QUALITY), Some(DEFAULT_QUALITY));
}

#[test]
fn quality_ranking_prefers_lossless_then_bitrate() {
    assert!(quality_rank("lossless", "flac", 900_000) > quality_rank("highest", "mp3", 320_000));
    assert!(quality_rank("hires", "flac", 1_500_000) > quality_rank("lossless", "flac", 900_000));
    assert_eq!(quality_rank("", "", 320_000), 70);
    assert_eq!(quality_rank("medium", "aac", 128_000), 50);
}

#[test]
fn preference_caps_and_unknown_values() {
    assert_eq!(preference_rank("best"), None);
    assert_eq!(preference_rank("lossless"), Some(100));
    assert_eq!(preference_rank("highest"), Some(80));
    assert_eq!(preference_rank("medium"), Some(50));
    assert_eq!(preference_rank("low"), Some(20));
    assert_eq!(preference_rank("unknown"), None);
}

fn stream(url: &str, quality: &str, bitrate: i64, duration: f64) -> Stream {
    Stream {
        url: url.to_owned(),
        quality: quality.to_owned(),
        bitrate,
        duration,
        ..Stream::default()
    }
}

#[test]
fn picking_respects_preference_and_preview_length() {
    let full = stream("a", "lossless", 900_000, 240.0);
    let high = stream("b", "highest", 320_000, 240.0);
    let preview = stream("c", "lossless", 900_000, 30.0);
    let candidates = vec![preview.clone(), full.clone(), high.clone()];

    // 不限制：完整优先，其次音质
    assert_eq!(pick(&candidates, "best", 240).unwrap().url, "a");
    // 极高及以下：只留 b（c 的档位超了；即便不过滤也会因时长被淘汰）
    assert_eq!(pick(&candidates, "highest", 240).unwrap().url, "b");
    // 偏好档位没有候选 → 取最接近的（80），而不是直接跳到无损（100）
    assert_eq!(pick(&candidates, "low", 240).unwrap().url, "b");
    assert_eq!(pick(&candidates, "medium", 240).unwrap().url, "b");
    assert!(pick(&[], "best", 240).is_none());
}

#[test]
fn requested_quality_is_never_exceeded_when_a_suitable_candidate_exists() {
    let medium = stream("m", "medium", 65_000, 200.0);
    let higher = stream("h", "higher", 130_000, 200.0);
    let candidates = vec![higher.clone(), medium.clone()];
    assert_eq!(pick(&candidates, "medium", 200).unwrap().url, "m");
    assert_eq!(
        pick(&candidates, "low", 200).unwrap().url,
        "m",
        "没有更低时取最接近"
    );
    assert_eq!(pick(&candidates, "highest", 200).unwrap().url, "h");
    assert_eq!(pick(&candidates, "best", 200).unwrap().url, "h");
}

#[test]
fn search_payload_is_parsed_from_result_groups() {
    let value = serde_json::json!({
        "result_groups": [{
            "data": [{
                "entity": {
                    "track": {
                        "id": "7304719759323564095",
                        "name": "晴天",
                        "duration": 269,
                        "artists": [{"name": "周杰伦"}],
                        "album": {"name": "叶惠美", "url_cover": {"urls": ["https://p/1.jpg"]}},
                        "label_info": {"only_vip_playable": true}
                    }
                }
            }]
        }]
    });
    let tracks = parse_search(&value).unwrap();
    assert_eq!(tracks.len(), 1);
    assert_eq!(tracks[0].id, "7304719759323564095");
    assert_eq!(tracks[0].artist_text(), "周杰伦");
    assert!(tracks[0].is_vip);
    assert!(tracks[0].cover.ends_with(".jpg"), "{}", tracks[0].cover);
}

#[test]
fn track_ids_are_recognised_in_every_shape() {
    assert_eq!(
        track_id_of("7304719759323564095").as_deref(),
        Some("7304719759323564095")
    );
    assert_eq!(
        track_id_of("https://www.qishui.com/track/7304719759323564095").as_deref(),
        Some("7304719759323564095")
    );
    assert_eq!(
        track_id_of("https://x/?track_id=7304719759323564095").as_deref(),
        Some("7304719759323564095")
    );
    assert_eq!(
        track_id_of("%2Ftrack%2F7304719759323564095").as_deref(),
        Some("7304719759323564095")
    );
    assert_eq!(track_id_of("周杰伦"), None);
}

#[test]
fn share_text_tells_tracks_from_ugc_videos() {
    assert_eq!(
        classify_text("https://www.qishui.com/track/7304719759323564095"),
        Some(Share::Track("7304719759323564095".to_owned()))
    );
    assert_eq!(
        classify_text(
            "https://music.douyin.com/qishui/share/ugc_video?ugc_video_id=7625615883351772468"
        ),
        Some(Share::Video("7625615883351772468".to_owned()))
    );
    // 短链本身看不出类型，需要联网跟随重定向。
    assert_eq!(classify_text("https://qishui.douyin.com/s/iXvvC5YV/"), None);
}

#[test]
fn only_soda_hosts_are_picked_up() {
    assert!(first_link("https://www.qishui.com/track/1").is_some());
    assert!(first_link("https://qishui.douyin.com/s/abc/").is_some());
    assert!(first_link("https://qishui.com.evil.test/track/1").is_none());
    assert!(first_link("https://music.163.com/song?id=1").is_none());
    // 短链的最终落地页
    assert!(
        first_link("https://music.douyin.com/qishui/share/track?track_id=7649060217267111937")
            .is_some()
    );
    assert!(first_link("https://music.douyin.com/other/share?x=1").is_none());
}

#[test]
fn video_model_is_flattened_with_inherited_auth_and_duration() {
    let model = serde_json::json!({
        "video_model": {
            "duration": 240,
            "encrypt_info": {"spade_a": "auth-value"},
            "gear_list": {
                "lossless": {
                    "main_play_url": "https://cdn/lossless.m4a",
                    "format": "m4a",
                    "bitrate": 900000,
                    "size": 1000
                }
            }
        }
    });
    let mut out = Vec::new();
    collect_model(&model, "", "", 0.0, &mut out);
    assert_eq!(out.len(), 1, "{out:?}");
    assert_eq!(out[0].main_play_url, "https://cdn/lossless.m4a");
    assert_eq!(out[0].play_auth, "auth-value");
    assert_eq!(out[0].quality, "lossless", "档位从 key 名猜出来");
    assert_eq!(out[0].duration, 240.0, "时长要能继承");
}

#[test]
fn player_info_pascal_case_is_mapped() {
    // 只测映射逻辑需要的形状，不经过网络。
    let value = serde_json::json!({
        "Result": {"Data": {"PlayInfoList": [{
            "MainPlayUrl": "https://cdn/a.m4a",
            "PlayAuth": "auth",
            "Format": "m4a",
            "Bitrate": 320000,
            "Size": 1234,
            "Duration": 240.0,
            "Quality": "highest"
        }]}}
    });
    let list = value
        .pointer("/Result/Data/PlayInfoList")
        .unwrap()
        .as_array()
        .unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(
        list[0].get("MainPlayUrl").and_then(Value::as_str),
        Some("https://cdn/a.m4a")
    );
}

#[test]
fn percent_decoding_is_only_a_fallback() {
    assert_eq!(percent_decode("a%2Fb"), "a/b");
    assert_eq!(percent_decode("no-escapes"), "no-escapes");
}

#[test]
fn the_default_is_an_unlimited_gear() {
    assert_eq!(preference_rank(DEFAULT_QUALITY), None);
}
