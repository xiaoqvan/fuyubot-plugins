use super::*;

/// 纯文本 / Markdown 的 op 形状：宿主 `format_fields` 认这两个键。
#[test]
fn text_serializes_to_the_op_shape() {
    assert_eq!(
        text_json(&Text::markdown("你好")),
        json!({ "text": "你好", "parse_mode": "markdown" })
    );
    assert_eq!(text_json(&Text::plain("x"))["parse_mode"], "plain");
}

/// 三种输入文件的 JSON 构造器各是各的：`inputFileId` 与 `inputFileRemote` 别混。
#[test]
fn input_files_serialize_to_the_right_constructors() {
    assert_eq!(
        InputFile::local("D:/cache/a.mp4").to_json(),
        json!({"@type": "inputFileLocal", "path": "D:/cache/a.mp4"})
    );
    assert_eq!(
        InputFile::remote("https://x/a.mp4").to_json(),
        json!({"@type": "inputFileRemote", "id": "https://x/a.mp4"})
    );
    assert_eq!(
        InputFile::id(123456).to_json(),
        json!({"@type": "inputFileId", "id": 123456})
    );
}

/// 视频：`inputMessageVideo` 里 `video:inputVideo`，封面在 `inputVideo.cover`。
#[test]
fn video_content_matches_the_real_schema() {
    let media = Media::video(InputFile::remote("https://x/v.mp4"))
        .size(1080, 1920)
        .duration(12)
        .streaming(true)
        .cover(InputFile::remote("https://x/c.jpg"), 0);
    let content = content_json(&media, &text_json);
    assert_eq!(content["@type"], "inputMessageVideo");
    assert_eq!(
        content["video"]["video"],
        json!({"@type": "inputFileRemote", "id": "https://x/v.mp4"})
    );
    assert_eq!(content["video"]["width"], 1080);
    assert_eq!(content["video"]["height"], 1920);
    assert_eq!(content["video"]["duration"], 12);
    assert_eq!(content["video"]["supports_streaming"], true);
    assert_eq!(
        content["video"]["cover"],
        json!({"@type": "inputFileRemote", "id": "https://x/c.jpg"})
    );
}

/// 相册：每个内容各自带 caption；**不带 `reply_to`**（不引用原消息）。
#[test]
fn album_params_carry_caption_without_reply() {
    let params = SendAlbumParams {
        medias: vec![
            Media::photo(InputFile::remote("https://x/1.jpg")).size(1, 2),
            Media::photo(InputFile::remote("https://x/2.jpg")).size(3, 4),
        ],
        caption: Some(Text::markdown("标题")),
        timeout_ms: 1000,
    };
    let payload = album_params(42, &params, &text_json);
    assert_eq!(payload["chat_id"], 42);
    assert!(
        payload.get("reply_to").is_none(),
        "不该引用原消息：{payload}"
    );
    let contents = payload["input_message_contents"].as_array().unwrap();
    assert_eq!(contents.len(), 2);
    assert_eq!(contents[0]["@type"], "inputMessagePhoto");
    assert_eq!(contents[0]["caption"]["text"], "标题");
    assert_eq!(contents[1]["photo"]["width"], 3);
}

/// 单条 `sendMessage`：文本与媒体二选一；带媒体时 text 变 caption。
#[test]
fn send_params_text_and_media_shapes() {
    let text = send_params(
        1,
        &SendMessageParams::text(Text::markdown("hi")),
        &text_json,
    );
    assert_eq!(text["input_message_content"]["@type"], "inputMessageText");
    assert_eq!(text["input_message_content"]["text"]["text"], "hi");

    let media = SendMessageParams {
        text: Some(Text::markdown("标题")),
        media: Some(Media::video(InputFile::id(9))),
        timeout_ms: 1000,
    };
    let payload = send_params(1, &media, &text_json);
    assert_eq!(
        payload["input_message_content"]["video"]["video"],
        json!({"@type": "inputFileId", "id": 9})
    );
    assert_eq!(payload["input_message_content"]["caption"]["text"], "标题");
}

// ── sent_media：字段路径照真 td_api.tl ──────────────────────

#[test]
fn video_message_yields_file_and_cover_ids() {
    let message = json!({
        "content": {
            "@type": "messageVideo",
            "video": {
                "video": { "id": 111 },
                "width": 1080, "height": 1920, "duration": 15,
                "thumbnail": { "thumbnail": { "file": { "id": 222 } } },
            },
            "cover": { "sizes": [
                { "photo": { "id": 333 }, "width": 100, "height": 100 },
                { "photo": { "id": 444 }, "width": 400, "height": 400 },
            ]},
        },
    });
    let media = sent_media(&message).expect("视频消息应当取到 id");
    assert_eq!(media.kind, KIND_VIDEO);
    assert_eq!(media.file_id, Some(111));
    assert_eq!(media.cover_id, Some(444), "封面取最大一张");
    assert_eq!(
        (media.width, media.height, media.duration),
        (1080, 1920, 15)
    );
}

#[test]
fn photo_message_takes_the_largest_size() {
    let message = json!({
        "content": {
            "@type": "messagePhoto",
            "photo": { "sizes": [
                { "photo": { "id": 1 }, "width": 90, "height": 90 },
                { "photo": { "id": 2 }, "width": 800, "height": 600 },
            ]},
        },
    });
    let media = sent_media(&message).unwrap();
    assert_eq!(media.kind, KIND_PHOTO);
    assert_eq!(media.file_id, Some(2));
    assert_eq!((media.width, media.height), (800, 600));
}

#[test]
fn audio_message_reads_album_cover() {
    let message = json!({
        "content": {
            "@type": "messageAudio",
            "audio": {
                "audio": { "id": 10 }, "duration": 30,
                "album_cover_thumbnail": {
                    "width": 320, "height": 320,
                    "thumbnail": { "file": { "id": 11 } },
                },
            },
        },
    });
    let media = sent_media(&message).unwrap();
    assert_eq!(media.kind, KIND_AUDIO);
    assert_eq!(media.file_id, Some(10));
    assert_eq!(media.cover_id, Some(11));
    assert_eq!(media.duration, 30);
}

/// 0 是「空文件对象」的占位，不能当成一个可复用的 id。
#[test]
fn zero_file_ids_mean_none() {
    let message = json!({
        "content": { "@type": "messageVideo", "video": { "video": { "id": 0 } } },
    });
    let media = sent_media(&message).unwrap();
    assert_eq!(media.file_id, None);
}

/// 非媒体消息 / 形状不对的 JSON 一律 `None`，不 panic。
#[test]
fn non_media_messages_yield_nothing() {
    assert!(sent_media(&json!({"content": {"@type": "messageText"}})).is_none());
    assert!(sent_media(&json!({})).is_none());
    assert!(sent_media_list(&json!({})).is_empty());
}

/// 相册结果是消息**数组**；单条结果也接受。
#[test]
fn sent_media_list_accepts_album_arrays_and_single_messages() {
    let album = json!([
        { "content": { "@type": "messagePhoto", "photo": { "sizes": [
            { "photo": { "id": 7 }, "width": 10, "height": 10 }]}}},
        { "content": { "@type": "messagePhoto", "photo": { "sizes": [
            { "photo": { "id": 8 }, "width": 10, "height": 10 }]}}},
    ]);
    let ids: Vec<i64> = sent_media_list(&album)
        .iter()
        .filter_map(|media| media.file_id)
        .collect();
    assert_eq!(ids, vec![7, 8]);

    let single = json!({ "content": { "@type": "messagePhoto", "photo": { "sizes": [
            { "photo": { "id": 9 }, "width": 10, "height": 10 }]}}});
    assert_eq!(sent_media_list(&single).len(), 1);
}

// ── 命令解析 ────────────────────────────────────────────────

fn update_with(text: &str) -> Value {
    json!({
        "@type": "updateNewMessage",
        "message": {
            "@type": "message",
            "id": 22,
            "chat_id": 42,
            "is_outgoing": false,
            "content": {
                "@type": "messageText",
                "text": { "@type": "formattedText", "text": text, "entities": [] },
            },
        },
    })
}

#[test]
fn commands_are_parsed_with_name_and_args() {
    let command = command_of(&update_with("/dy https://v.douyin.com/abc/")).unwrap();
    assert_eq!(command.name, "dy");
    assert_eq!(command.args, "https://v.douyin.com/abc/");
    assert_eq!(command.addressed_to, None);
    assert!(command.is("DY"), "命令名不分大小写");
}

#[test]
fn addressed_commands_are_only_for_the_named_bot() {
    let command = command_of(&update_with("/dy@SomeBot x")).unwrap();
    assert_eq!(command.addressed_to.as_deref(), Some("SomeBot"));
    assert!(command.is_for(&["somebot".to_owned()]));
    assert!(!command.is_for(&["otherbot".to_owned()]));
    // 身份未知（空表）时不过滤：宁可多回一句也别吞命令。
    assert!(command.is_for(&[]));
}

#[test]
fn plain_messages_and_outgoing_messages_are_not_commands() {
    assert!(command_of(&update_with("https://v.douyin.com/abc/")).is_none());
    let mut outgoing = update_with("/dy x");
    outgoing["message"]["is_outgoing"] = json!(true);
    assert!(command_of(&outgoing).is_none());

    // 频道帖例外：自己的频道里发的 `/dy` 是命令，解析后要编辑这条原帖。
    let mut outgoing_post = update_with("/dy x");
    outgoing_post["message"]["chat_id"] = json!(-100777_i64);
    outgoing_post["message"]["is_outgoing"] = json!(true);
    outgoing_post["message"]["is_channel_post"] = json!(true);
    outgoing_post["message"]["sender_id"] =
        json!({ "@type": "messageSenderChat", "chat_id": -100777_i64 });
    assert!(
        command_of(&outgoing_post).is_some_and(|command| command.is_channel_post),
        "自家频道的 /dy 不能因为 is_outgoing 被吞掉"
    );
}

/// 频道判定只看 TDLib 的 `is_channel_post`：群里匿名频道马甲（发送者也是
/// `messageSenderChat`）不能被当成频道帖，否则 /dy 会去编辑不属于自己的群消息。
#[test]
fn only_channel_posts_use_the_channel_identity() {
    let mut update = update_with("/dy https://v.douyin.com/abc/");
    update["message"]["chat_id"] = json!(-100777_i64);
    update["message"]["is_channel_post"] = json!(true);
    update["message"]["sender_id"] =
        json!({ "@type": "messageSenderChat", "chat_id": -100777_i64 });
    assert!(
        command_of(&update).unwrap().is_channel_post,
        "频道自己发的帖子要认出来"
    );

    update["message"]["chat_id"] = json!(-100888_i64);
    update["message"]["is_channel_post"] = json!(false);
    update["message"]["sender_id"] =
        json!({ "@type": "messageSenderChat", "chat_id": -100999_i64 });
    assert!(
        !command_of(&update).unwrap().is_channel_post,
        "群里的匿名频道马甲不是频道帖"
    );

    update["message"]["sender_id"] = json!({ "@type": "messageSenderUser", "user_id": 1 });
    assert!(!command_of(&update).unwrap().is_channel_post);
}
