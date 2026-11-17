use super::*;

#[test]
fn video_ids_from_every_share_shape() {
    let id = "dQw4w9WgXcQ";
    let cases = [
        (
            "https://music.youtube.com/watch?v=dQw4w9WgXcQ&si=abc",
            Some(id),
        ),
        ("https://www.youtube.com/watch?v=dQw4w9WgXcQ", Some(id)),
        ("https://youtu.be/dQw4w9WgXcQ?si=x", Some(id)),
        ("https://youtu.be/dQw4w9WgXcQ)", Some(id)),
        ("https://www.youtube.com/shorts/dQw4w9WgXcQ", Some(id)),
        ("https://www.youtube.com/live/dQw4w9WgXcQ", Some(id)),
        ("youtube:video:dQw4w9WgXcQ", Some(id)),
        ("dQw4w9WgXcQ", Some(id)),
        ("https://example.com/watch?v=dQw4w9WgXcQ", None),
        ("https://www.youtube.com/watch?v=short", None),
        ("hello world", None),
    ];
    for (text, expected) in cases {
        assert_eq!(video_id_of(text).as_deref(), expected, "{text}");
    }
}

#[test]
fn links_are_recognised_by_host_only() {
    assert!(first_link("听 https://music.youtube.com/watch?v=dQw4w9WgXcQ，好听").is_some());
    assert!(first_link("https://notyoutube.com/watch?v=dQw4w9WgXcQ").is_none());
    assert!(first_link("https://youtube.com.evil.tld/watch?v=dQw4w9WgXcQ").is_none());
}

#[test]
fn music_links_are_separate_from_plain_youtube() {
    assert!(first_music_link("https://music.youtube.com/watch?v=dQw4w9WgXcQ&si=x").is_some());
    assert!(
        first_music_link("https://youtu.be/dQw4w9WgXcQ").is_none(),
        "普通视频不自动抢"
    );
    assert!(first_link("https://youtu.be/dQw4w9WgXcQ").is_some());
}

#[test]
fn playlist_id_comes_from_list_param() {
    assert_eq!(
        playlist_id_of("https://music.youtube.com/playlist?list=PL123").as_deref(),
        Some("PL123")
    );
    assert_eq!(
        playlist_id_of("https://www.youtube.com/watch?v=dQw4w9WgXcQ&list=RD123").as_deref(),
        Some("RD123")
    );
    assert_eq!(
        playlist_id_of("https://music.youtube.com/playlist?list="),
        None
    );
    assert_eq!(playlist_id_of("dQw4w9WgXcQ"), None);
}

#[test]
fn clock_parsing_accepts_two_and_three_segments() {
    assert_eq!(parse_clock("3:33"), Some(213));
    assert_eq!(parse_clock("1:02:03"), Some(3723));
    assert_eq!(parse_clock(" 3:33 "), Some(213));
    assert_eq!(parse_clock("0:00"), None);
    assert_eq!(parse_clock("213"), None);
    assert_eq!(parse_clock("a:b"), None);
}

#[test]
fn player_tries_more_than_one_client() {
    let profiles = client_profiles();
    assert!(profiles.len() >= 3);
    assert_eq!(profiles[0].0, "ANDROID_VR");
    assert_eq!(profiles[1].0, "IOS");
    assert_eq!(profiles[2].0, "ANDROID");
}

#[test]
fn quality_aliases_are_normalised() {
    assert_eq!(parse_quality("best"), Some("best"));
    assert_eq!(parse_quality("High"), Some("best"));
    assert_eq!(parse_quality("标准"), Some("medium"));
    assert_eq!(parse_quality("low"), Some("low"));
    assert_eq!(parse_quality("320"), None);
    assert!(quality_help().contains("best"));
    assert_eq!(quality_label("best"), "最佳（best）");
    assert_eq!(quality_label("weird"), "weird");
}

fn formats() -> Value {
    serde_json::json!({
        "adaptiveFormats": [
            {"itag": 139, "mimeType": "audio/mp4; codecs=\"mp4a.40.5\"", "bitrate": 50152, "contentLength": "1300631", "url": "https://x/139"},
            {"itag": 140, "mimeType": "audio/mp4; codecs=\"mp4a.40.2\"", "bitrate": 130677, "contentLength": 3449447, "url": "https://x/140"},
            {"itag": 251, "mimeType": "audio/webm; codecs=\"opus\"", "bitrate": 136544, "url": "https://x/251"},
            {"itag": 137, "mimeType": "video/mp4", "bitrate": 4000000, "url": "https://x/137"}
        ]
    })
}

#[test]
fn format_picker_prefers_m4a_and_respects_quality() {
    let value = formats();
    let items: Vec<&Value> = value["adaptiveFormats"]
        .as_array()
        .unwrap()
        .iter()
        .collect();
    let audio: Vec<&Value> = items
        .iter()
        .copied()
        .filter(|format| mime_of(format).starts_with("audio/"))
        .collect();
    assert_eq!(pick_format(&audio, "best").unwrap()["itag"], 140);
    assert_eq!(pick_format(&audio, "medium").unwrap()["itag"], 140);
    assert_eq!(pick_format(&audio, "low").unwrap()["itag"], 139);
    // 未知音质按 best 处理。
    assert_eq!(pick_format(&audio, "???").unwrap()["itag"], 140);
}

#[test]
fn stream_picker_reads_url_size_and_mime() {
    let value = serde_json::json!({ "streamingData": formats() });
    let stream = pick_stream(&value, "best").unwrap();
    assert_eq!(stream.itag, 140);
    assert_eq!(stream.url, "https://x/140");
    assert_eq!(stream.size, 3449447);
    assert_eq!(stream.bitrate, 130677);
    assert_eq!(stream.extension(), "m4a");
    assert_eq!(stream_label(&stream), "YouTube m4a");
}

#[test]
fn stream_picker_rejects_cipher_only_and_empty_audio() {
    let value = serde_json::json!({"streamingData": {"adaptiveFormats": [
        {"itag": 140, "mimeType": "audio/mp4", "bitrate": 130000, "signatureCipher": "s=..."}
    ]}});
    let error = pick_stream(&value, "best").unwrap_err();
    assert!(error.contains("签名解密"), "{error}");

    let empty = serde_json::json!({"streamingData": {"adaptiveFormats": []}});
    assert!(pick_stream(&empty, "best").is_err());
}

#[test]
fn search_renderer_is_parsed() {
    let item = serde_json::json!({
        "playlistItemData": {"videoId": "dQw4w9WgXcQ"},
        "flexColumns": [
            {"musicResponsiveListItemFlexColumnRenderer": {"text": {"runs": [
                {"text": "Never Gonna Give You Up"}
            ]}}},
            {"musicResponsiveListItemFlexColumnRenderer": {"text": {"runs": [
                {"text": "Rick Astley • 6.3M views • 3:33"}
            ]}}}
        ],
        "fixedColumns": [
            {"musicResponsiveListItemFixedColumnRenderer": {"text": {"runs": [{"text": "3:33"}]}}}
        ],
        "thumbnail": {"musicThumbnailRenderer": {"thumbnail": {"thumbnails": [
            {"url": "https://i.ytimg.com/vi/dQw4w9WgXcQ/hqdefault.jpg"}
        ]}}}
    });
    let track = track_from_renderer(&item).unwrap();
    assert_eq!(track.id, "dQw4w9WgXcQ");
    assert_eq!(track.name, "Never Gonna Give You Up");
    assert_eq!(track.artist_text(), "Rick Astley");
    assert_eq!(track.duration, 213);
    assert_eq!(
        track.link(),
        "https://music.youtube.com/watch?v=dQw4w9WgXcQ"
    );
}

#[test]
fn subtitle_kind_words_become_artist() {
    assert_eq!(
        artists_of("Song • Rick Astley"),
        vec!["Rick Astley".to_owned()]
    );
    assert_eq!(
        artists_of("Album • Various Artists • 2020"),
        vec!["Various Artists".to_owned()]
    );
    assert!(artists_of("").is_empty());
}

#[test]
fn playability_messages_are_actionable() {
    let text = playability_message("LOGIN_REQUIRED", "Sign in to confirm you're not a bot");
    assert!(text.contains("youtube cookie"), "{text}");
    let text = playability_message("ERROR", "Video unavailable");
    assert!(text.contains("不能播放"), "{text}");
    assert!(playability_message("", "").contains("没有返回播放状态"));
}
