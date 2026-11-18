use super::*;

/// 解析层也要挡住汽水音乐：命令层那道挡的是「用户消息」，不是「所有调用方」。
///
/// 这条不联网也能验——守卫在发任何请求**之前**就返回了。
/// 断言的是「早退」而不是「错误文案好看」：只要它没去 HEAD，
/// 就不会出现「重试两次再报一个看不懂的错」那种旧症状。
#[tokio::test]
async fn qishui_links_are_rejected_before_any_request() {
    let agent = crate::http::agent().expect("建 HTTP 客户端");
    for url in [
        "https://qishui.douyin.com/s/iXvvC5YV/",
        "https://www.qishui.com/track/7304719759323564095",
        "https://music.douyin.com/qishui/share/track?track_id=1",
    ] {
        // cookie 故意给非空：否则可能被「没有 cookie」那条分支先接住，
        // 测到的就不是域名守卫了。
        let error = get_douyin(&agent, url, "a=1")
            .await
            .expect_err("汽水链接必须被拒绝");
        assert!(error.0.contains("汽水"), "{}：{}", url, error.0);
    }
}

/// 抖音自己的链接不该被上面那道守卫误伤。
#[test]
fn ordinary_douyin_links_are_not_mistaken_for_qishui() {
    for url in [
        "https://v.douyin.com/abc/",
        "https://www.douyin.com/video/7401234567890123456",
        "https://music.douyin.com/other/video/1",
    ] {
        assert!(
            !crate::commands::is_qishui_url(url),
            "{url} 不是汽水链接，不能挡掉"
        );
    }
}

#[test]
fn target_is_read_off_the_path_when_already_there() {
    let cases = [
        (
            "https://www.douyin.com/video/1234567890",
            "video",
            "1234567890",
        ),
        ("http://www.douyin.com/note/42?x=1", "note", "42"),
        ("https://www.douyin.com/slides/7/?from=x", "slides", "7"),
        ("https://www.douyin.com/notes/9#top", "notes", "9"),
    ];
    for (link, kind, id) in cases {
        let target = target_from_path(link).unwrap_or_else(|| panic!("认不出 {link}"));
        assert_eq!(target.kind, kind, "{link} 的类型");
        assert_eq!(target.id, id, "{link} 的 id");
    }
}

#[test]
fn paths_without_an_aweme_segment_are_not_targets() {
    for link in [
        "https://www.douyin.com/",
        "https://www.douyin.com/user/MS4wLjABAAAA",
        "https://v.douyin.com/abc123/",
        "https://www.douyin.com/search/视频",
    ] {
        assert!(
            target_from_path(link).is_none(),
            "{link} 不该被认成作品链接"
        );
    }
}

#[test]
fn short_links_are_not_targets_but_full_paths_are() {
    // 短链必须走 302（这里没有网络，只验「路径版本」被认出来）
    assert!(target_from_path("https://v.douyin.com/iABCDEFG/").is_none());
    assert!(target_from_path("https://www.iesdouyin.com/share/video/7001/").is_some());
}

/// TS 版那条正则在这儿换成手写扫描，同一个样本的行为必须一致。
#[test]
fn id_must_be_digits_right_after_the_segment() {
    assert!(target_from_path("https://www.douyin.com/video/").is_none());
    assert!(target_from_path("https://www.douyin.com/video/abc").is_none());
    let target = target_from_path("https://www.douyin.com/video/12a34").unwrap();
    assert_eq!(target.id, "12", "非数字处截断");
}

#[test]
fn router_data_is_extracted_with_balanced_braces() {
    let html = r#"<script>window._ROUTER_DATA = {"a":1,"b":{"c":"}不是结尾"},"d":"\\"} </script>"#;
    let raw = extract_router_data(html).expect("应当抠出对象");
    let value: Value = serde_json::from_str(raw).expect("抠出来的必须是合法 JSON");
    assert_eq!(value["a"], 1);
    assert_eq!(value["b"]["c"], "}不是结尾");
}

#[test]
fn router_data_is_none_without_the_marker() {
    assert!(extract_router_data("<html><body>nothing</body></html>").is_none());
    assert!(extract_router_data("_ROUTER_DATA = ").is_none());
}

#[test]
fn the_last_candidate_wins() {
    let list = serde_json::json!({ "url_list": ["https://a.example/1", "https://b.example/2"] });
    assert_eq!(
        last_url(list.get("url_list")).as_deref(),
        Some("https://b.example/2")
    );
    assert_eq!(last_url(None), None);
    assert_eq!(
        last_url(Some(&serde_json::json!({"url_list": []}))),
        None,
        "空列表不能当成有"
    );
}

#[test]
fn watermark_and_bitrate_are_rewritten() {
    assert_eq!(
        unwatermark("https://x/aweme/v1/playwm/?video_id=1&ratio=720p"),
        "https://x/aweme/v1/play/?video_id=1&ratio=1080p"
    );
    assert_eq!(unwatermark("https://x/play/?a=1"), "https://x/play/?a=1");
}

/// 60fps 优先，即使分辨率略低——这是 TS 版 `pickBestBitRate` 的原行为。
#[test]
fn sixty_fps_beats_bigger_resolution() {
    let bit_rates = serde_json::json!([
        { "FPS": 30, "play_addr": { "url_list": ["https://a/1"], "width": 1920, "height": 1080 } },
        { "FPS": 60, "play_addr": { "url_list": ["https://b/2"], "width": 1280, "height": 720 } },
    ]);
    let best = pick_best_bit_rate(Some(&bit_rates)).expect("应当选出一条");
    assert_eq!(best["FPS"], 60);
}

#[test]
fn without_60fps_the_largest_area_wins() {
    let bit_rates = serde_json::json!([
        { "FPS": 30, "play_addr": { "url_list": ["https://a/1"], "width": 720, "height": 1280 } },
        { "FPS": 30, "play_addr": { "url_list": ["https://b/2"], "width": 1920, "height": 1080 } },
    ]);
    let best = pick_best_bit_rate(Some(&bit_rates)).expect("应当选出一条");
    assert_eq!(
        last_url(best.pointer("/play_addr/url_list")).as_deref(),
        Some("https://b/2")
    );
}

/// 没有 `url_list` 的候选不能当选：那是一条下不动的「幽灵」清晰度。
#[test]
fn candidates_without_urls_are_skipped() {
    let bit_rates = serde_json::json!([
        { "FPS": 60, "play_addr": { "width": 1920, "height": 1080 } },
        { "FPS": 30, "play_addr": { "url_list": ["https://b/2"], "width": 640, "height": 360 } },
    ]);
    let best = pick_best_bit_rate(Some(&bit_rates)).expect("应当选出能用的那条");
    assert_eq!(
        last_url(best.pointer("/play_addr/url_list")).as_deref(),
        Some("https://b/2")
    );
    // 全都不能用 → None（不是随手指一条）
    let empty = serde_json::json!([{ "play_addr": {} }]);
    assert!(pick_best_bit_rate(Some(&empty)).is_none());
    assert!(pick_best_bit_rate(None).is_none());
}

#[test]
fn item_shape_becomes_video_info() {
    let item = serde_json::json!({
        "aweme_id": "7401",
        "desc": "测试标题 #话题#话题2",
        "video": {
            "duration": 15000,
            "width": 576,
            "height": 1024,
            "cover": { "url_list": ["https://cover/1"] },
            "origin_cover": { "url_list": ["https://origin/1"] },
            "play_addr": { "url_list": ["https://playwm/fallback"] },
            "bit_rate": [{
                "FPS": 30,
                "play_addr": {
                    "url_list": ["https://playwm/pickme?ratio=720p"],
                    "width": 1080, "height": 1920,
                },
            }],
        },
        "author": { "nickname": "作者X", "sec_uid": "MS4w", "uid": "99" },
        "music": { "id": 7, "mid": "m1", "title": "歌名", "play_url": { "url_list": ["https://m/1"] }, "author": "演唱者" },
    });
    let info = build_video_info(&item);
    assert_eq!(info.detail_id, "7401");
    assert_eq!(info.title, "测试标题 #话题#话题2");
    assert_eq!(
        info.cover_url, "https://origin/1",
        "origin_cover 优先于 cover"
    );
    assert_eq!(
        info.video.as_ref().map(|v| v.video_url.as_str()),
        Some("https://play/pickme?ratio=1080p"),
        "取 bit_rate 那条，并去水印抬码率"
    );
    assert_eq!(
        info.video.as_ref().and_then(|v| v.duration),
        Some(15),
        "毫秒转秒"
    );
    assert_eq!(info.video.as_ref().map(|v| v.width), Some(1080));
    assert_eq!(info.author.name, "作者X");
    assert_eq!(
        info.author.home_url.as_deref(),
        Some("https://www.douyin.com/user/MS4w")
    );
    assert_eq!(info.music.as_ref().map(|m| m.name.as_str()), Some("歌名"));
    assert!(info.images.is_none());
    assert!(info.cooperation_info.is_empty());
}

#[test]
fn image_albums_collect_both_pictures_and_embedded_videos() {
    let item = serde_json::json!({
        "images": [
            { "height": 100, "width": 200, "url_list": ["https://i/1"] },
            {
                "height": 300, "width": 400,
                "url_list": ["https://i/2"],
                "video": { "bit_rate": [{ "FPS": 60, "play_addr": {
                    "url_list": ["https://playwm/v2"], "width": 9, "height": 16 } }] },
            },
        ],
    });
    let info = build_video_info(&item);
    assert_eq!(info.images.as_ref().map(Vec::len), Some(2));
    let videos = info.image_videos.expect("应当收出嵌入的视频");
    assert_eq!(videos.len(), 1, "只有第二页带着视频");
    assert_eq!(videos[0].video_url, "https://play/v2");
    assert_eq!(videos[0].fps, Some(60));
}

#[test]
fn an_empty_image_list_is_not_an_album() {
    let info = build_video_info(&serde_json::json!({ "images": [] }));
    assert!(info.images.is_none());
    assert!(info.image_videos.is_none());
}

#[test]
fn missing_fields_fall_back_instead_of_panicking() {
    // 抖音的响应形状不稳定：缺一大片字段也要能出一个 VideoInfo。
    let info = build_video_info(&serde_json::json!({}));
    assert_eq!(info.detail_id, "");
    assert!(info.video.is_none());
    assert_eq!(info.author.name, "");
    assert!(info.music.is_none());
}

#[test]
fn duration_is_rounded_to_seconds() {
    // ⚠️ 必须有 `play_addr.url_list`：`video` 有没有值取决于能不能取出直链，
    // 「只有 duration」在 TS 版同样产不出 `video`。
    let with_url = |ms: i64| {
        serde_json::json!({
            "video": {
                "duration": ms,
                "play_addr": { "url_list": ["https://play/x"] },
            },
        })
    };
    assert_eq!(
        build_video_info(&with_url(1500)).video.unwrap().duration,
        Some(1)
    );
    assert_eq!(
        build_video_info(&with_url(999)).video.unwrap().duration,
        Some(0)
    );
    assert_eq!(
        build_video_info(&with_url(15_000)).video.unwrap().duration,
        Some(15)
    );
}

/// 有 `duration` 但取不出直链时整段 `video` 都是 `None`——与 TS 版一致：
/// 一个只有时长、没有地址的东西发出去没有任何意义。
#[test]
fn a_video_without_a_playable_url_is_dropped() {
    let item = serde_json::json!({ "video": { "duration": 1500 } });
    assert!(build_video_info(&item).video.is_none());
}

/// 回放里看到抖音把**无效**短链 302 到首页（`https://www.douyin.com`），
/// 而不是报错——所以「拿到 Location」不等于「拿到了作品」。
/// 这两条路径必须分开：首页没有作品段，`target_from_path` 必须返回 None。
#[test]
fn a_redirect_to_the_homepage_is_not_an_aweme() {
    assert!(target_from_path("https://www.douyin.com").is_none());
    assert!(target_from_path("https://www.douyin.com/").is_none());
    assert!(target_from_path("https://www.iesdouyin.com/").is_none());
}

/// 真短链的 302 目标长这样——它是「短链 → 分享页」这条路的输入，
/// 回放里只看到了首页那种，所以这里把真形状钉住。
#[test]
fn the_real_redirect_shape_is_recognized() {
    for url in [
        "https://www.iesdouyin.com/share/video/7401234567890123456/",
        "https://www.douyin.com/video/7401234567890123456",
        "https://www.iesdouyin.com/share/slides/7401234567890123456/?region=CN",
    ] {
        let target = target_from_path(url).unwrap_or_else(|| panic!("认不出 {url}"));
        assert_eq!(target.id, "7401234567890123456", "{url} 的 id");
    }
}

// ── cookie：配置优先、旧文件兜底 ────────────────────────────────

/// 配置值是一段 JSON 文本：字符串值带 JSON 引号，要能剥掉。
#[test]
fn config_cookie_is_parsed_from_json_text() {
    assert_eq!(
        cookie_from_json("\"ttwid=1; msToken=abc\"").as_deref(),
        Some("ttwid=1; msToken=abc")
    );
    // 前后空白（配置文件里常见）不影响。
    assert_eq!(
        cookie_from_json("  \" a=1 \"  ").as_deref(),
        Some("a=1"),
        "字符串本身 trim，JSON 外面的空白也吃"
    );
    // 空串 / 非字符串 / 非 JSON / 空值都当未配置。
    assert_eq!(cookie_from_json("\"\""), None);
    assert_eq!(cookie_from_json("  "), None);
    assert_eq!(cookie_from_json(""), None);
    assert_eq!(cookie_from_json("null"), None);
    assert_eq!(cookie_from_json("123"), None);
    assert_eq!(cookie_from_json("[\"a=1\"]"), None);
    assert_eq!(cookie_from_json("not json"), None);
}

/// 读取顺序：主键 `cookies` → 别名 `cookie`；`section` 必须是插件名。
#[test]
fn config_cookie_prefers_cookies_then_the_cookie_alias() {
    let read = |section: &str, key: &str| -> Option<String> {
        assert_eq!(section, CONFIG_SECTION, "段名必须是插件名");
        assert_eq!(CONFIG_SECTION, "xiaoqvan.douyin");
        match key {
            "cookies" => Some("\"from-cookies\"".to_owned()),
            "cookie" => Some("\"from-alias\"".to_owned()),
            _ => None,
        }
    };
    assert_eq!(
        cookie_from_config(read).as_deref(),
        Some("from-cookies"),
        "主键优先；别名只在主键没配时用"
    );

    // 主键为空 → 视为未配置，继续试别名。
    let alias_only = |_section: &str, key: &str| match key {
        "cookies" => Some("\"  \"".to_owned()),
        "cookie" => Some("\"from-alias\"".to_owned()),
        _ => None,
    };
    assert_eq!(
        cookie_from_config(alias_only).as_deref(),
        Some("from-alias")
    );

    // 两个都空 → 没配置。
    let empty = |_section: &str, _key: &str| Some("\"\"".to_owned());
    assert_eq!(cookie_from_config(empty), None);
    // 宿主没有这个段（get_config 返回 None）→ 没配置。
    assert_eq!(cookie_from_config(|_s, _k| None), None);
}

/// 临时插件目录；`file` 是 `Some(内容)` 时写旧 `douyin/cookies.json`。
fn cookie_dir(tag: &str, file: Option<&str>) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("douyin-cookie-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("douyin")).expect("建目录");
    if let Some(body) = file {
        std::fs::write(dir.join("douyin").join(COOKIES_FILE), body).expect("写旧 cookie 文件");
    }
    dir
}

/// 配置里有 cookie 时**根本不读旧文件**——旧文件写坏也照常成功。
#[test]
fn config_wins_and_the_legacy_file_is_not_even_read() {
    // 旧文件是坏 JSON：只要被读了就会报错，所以成功就证明没读。
    let dir = cookie_dir("config-wins", Some("这不是 JSON"));
    let read = |_section: &str, key: &str| match key {
        "cookies" => Some("\"from-config\"".to_owned()),
        _ => None,
    };
    let cookies = read_cookies_with(&read, &dir).expect("配置有值时不该碰旧文件");
    assert_eq!(cookies, "from-config");
    let _ = std::fs::remove_dir_all(&dir);
}

/// 配置没有时回退旧文件（旧格式 `{"cookies": "…"}`，与 TS 版一致）。
#[test]
fn the_legacy_file_is_used_when_config_is_missing() {
    let dir = cookie_dir("legacy", Some(r#"{"cookies":"ttwid=old; sid=1"}"#));
    let cookies = read_cookies_with(&|_s, _k| None, &dir).expect("旧文件应当被兜底读到");
    assert_eq!(cookies, "ttwid=old; sid=1");
    let _ = std::fs::remove_dir_all(&dir);
}

/// 配置里写的是空串（= 未配置）→ 回退旧文件。
#[test]
fn empty_config_falls_back_to_the_legacy_file() {
    let dir = cookie_dir("empty-config", Some(r#"{"cookies":"ttwid=old"}"#));
    let cookies =
        read_cookies_with(&|_s, _key| Some("\"\"".to_owned()), &dir).expect("应当回退旧文件");
    assert_eq!(cookies, "ttwid=old");
    let _ = std::fs::remove_dir_all(&dir);
}

/// 两者都没有 → 报错，而且错误里要写清「去哪里配」。
#[test]
fn missing_everything_reports_how_to_configure() {
    let dir = cookie_dir("missing", None);
    let error = read_cookies_with(&|_s, _k| None, &dir).expect_err("两者都没有必须报错");
    let text = error.to_string();
    assert!(text.contains("[plugin.xiaoqvan.douyin]"), "{text}");
    assert!(text.contains("cookies"), "{text}");
    assert!(text.contains("cookies.json"), "要提到旧文件仍兼容：{text}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// 旧文件存在但坏了 → 报错要带文件名（别静默当成「没配」）。
#[test]
fn a_broken_legacy_file_reports_its_reason() {
    let dir = cookie_dir("broken", Some("{not json"));
    let error = read_cookies_with(&|_s, _k| None, &dir).expect_err("坏文件必须报错");
    assert!(error.0.contains("cookies.json"), "{}", error.0);
    let _ = std::fs::remove_dir_all(&dir);
}
