use super::*;

#[test]
fn cached_files_serialise_as_input_file_id() {
    let media = Media::Audio {
        file: InputFile::id(1234),
        caption: None,
        album_cover_thumbnail: Some(Thumbnail::new(InputFile::id(5678))),
        duration: 269,
        title: "晴天".to_owned(),
        performer: "周杰伦".to_owned(),
    };
    let params = SendMessageParams {
        text: Some(Text::plain("x")),
        media: Some(media),
        timeout_ms: DEFAULT_TIMEOUT_MS,
    };
    let payload = send_params(1, &params, &|text: &Text| plain_text(text));
    assert_eq!(
        payload.pointer("/input_message_content/@type"),
        Some(&json!("inputMessageAudio"))
    );
    assert_eq!(
        payload.pointer("/input_message_content/audio/@type"),
        Some(&json!("inputAudio"))
    );
    assert_eq!(
        payload.pointer("/input_message_content/audio/audio/@type"),
        Some(&json!("inputFileId"))
    );
    assert_eq!(
        payload.pointer("/input_message_content/audio/audio/id"),
        Some(&json!(1234))
    );
    assert_eq!(
        payload.pointer("/input_message_content/audio/album_cover_thumbnail/thumbnail/@type"),
        Some(&json!("inputFileId"))
    );
    assert_eq!(
        payload.pointer("/input_message_content/audio/album_cover_thumbnail/thumbnail/id"),
        Some(&json!(5678))
    );
    // 不引用原消息：`sendMessage` 参数里不该有 `reply_to`。
    assert!(
        payload.get("reply_to").is_none(),
        "本插件不引用原消息：{payload}"
    );
}

#[test]
fn video_files_serialise_as_input_file_id_too() {
    let media = Media::Video {
        file: InputFile::id(11),
        caption: None,
        thumbnail: Some(Thumbnail::new(InputFile::id(12))),
        cover: None,
        start_timestamp: None,
        duration: 33,
        width: 1920,
        height: 1080,
        supports_streaming: true,
        has_spoiler: false,
    };
    let params = SendMessageParams {
        text: Some(Text::plain("t")),
        media: Some(media),
        timeout_ms: DEFAULT_TIMEOUT_MS,
    };
    let payload = send_params(1, &params, &|text: &Text| plain_text(text));
    assert_eq!(
        payload.pointer("/input_message_content/video/video/@type"),
        Some(&json!("inputFileId"))
    );
    assert_eq!(
        payload.pointer("/input_message_content/video/video/id"),
        Some(&json!(11))
    );
    assert_eq!(
        payload.pointer("/input_message_content/video/supports_streaming"),
        Some(&json!(true))
    );
}

#[test]
fn sent_media_reads_audio_and_its_cover() {
    let message = json!({
        "content": {
            "@type": "messageAudio",
            "audio": {
                "duration": 269,
                "audio": { "@type": "file", "id": 42 },
                "album_cover_thumbnail": {
                    "width": 320,
                    "height": 320,
                    "file": { "@type": "file", "id": 43 }
                }
            }
        }
    });
    let media = sent_media(&message).expect("音频消息应当取得到");
    assert_eq!(media.kind, KIND_AUDIO);
    assert_eq!(media.file_id, Some(42));
    assert_eq!(media.cover_id, Some(43));
    assert_eq!(media.duration, 269);
}

#[test]
fn sent_media_reads_video_cover_from_the_largest_photo() {
    let message = json!({
        "content": {
            "@type": "messageVideo",
            "video": {
                "duration": 33,
                "width": 1920,
                "height": 1080,
                "video": { "@type": "file", "id": 7 },
                "thumbnail": { "file": { "@type": "file", "id": 8 } }
            },
            "cover": {
                "sizes": [
                    { "width": 100, "height": 100, "photo": { "@type": "file", "id": 100 } },
                    { "width": 200, "height": 200, "photo": { "@type": "file", "id": 200 } }
                ]
            }
        }
    });
    let media = sent_media(&message).expect("视频消息应当取得到");
    assert_eq!(media.kind, KIND_VIDEO);
    assert_eq!(media.file_id, Some(7));
    assert_eq!(media.cover_id, Some(200), "取最大的一张");
    assert_eq!(media.width, 1920);
    assert_eq!(media.height, 1080);
}

#[test]
fn zero_file_ids_are_treated_as_absent() {
    let message = json!({
        "content": {
            "@type": "messageAudio",
            "audio": { "audio": { "@type": "file", "id": 0 } }
        }
    });
    let media = sent_media(&message).expect("还是媒体消息");
    assert_eq!(media.file_id, None);
    assert!(sent_media(&json!({})).is_none());
    // 非媒体内容（文本 / 贴纸）是 `other`：没有文件 id，调用方按「不能缓存」处理。
    let text = sent_media(&json!({"content": {"@type": "messageText"}})).expect("other");
    assert_eq!(text.kind, KIND_OTHER);
    assert_eq!(text.file_id, None);
}

#[test]
fn commands_are_parsed_from_real_update_shapes() {
    let update = json!({
        "message": {
            "id": 7,
            "chat_id": -100123,
            "is_outgoing": false,
            "content": {
                "@type": "messageText",
                "text": { "text": "/music163@SomeBot 186016 lossless" }
            }
        }
    });
    let command = command_of(&update).expect("应当认得命令");
    assert_eq!(command.name, "music163");
    assert_eq!(command.args, "186016 lossless");
    assert_eq!(command.addressed_to.as_deref(), Some("SomeBot"));
    assert!(command.is_for(&["somebot".to_owned()]));
    assert!(!command.is_for(&["other".to_owned()]));
    assert!(command.is("MUSIC163"));

    let plain = json!({
        "message": {
            "id": 8,
            "chat_id": 1,
            "content": { "@type": "messageText", "text": { "text": "/soda" } }
        }
    });
    let command = command_of(&plain).expect("裸命令");
    assert!(command.args.is_empty());
    assert!(command.addressed_to.is_none());
    assert!(command.is_for(&[]), "不知道自己是谁时不过滤");

    // 自己发的 / 非文本都不算命令。
    let outgoing = json!({
        "message": {
            "id": 9,
            "chat_id": 1,
            "is_outgoing": true,
            "content": { "@type": "messageText", "text": { "text": "/soda" } }
        }
    });
    assert!(command_of(&outgoing).is_none());

    // 频道帖例外：自家频道里发的 `/soda` 是命令，解析后要编辑这条原帖。
    let channel_command = json!({
        "message": {
            "id": 10,
            "chat_id": -100777_i64,
            "is_outgoing": true,
            "is_channel_post": true,
            "content": { "@type": "messageText", "text": { "text": "/soda 1" } }
        }
    });
    assert!(
        command_of(&channel_command).is_some_and(|command| command.is_channel_post),
        "自家频道的命令不能因为 is_outgoing 被吞掉"
    );

    // 群里匿名频道马甲（is_channel_post=false）不是频道帖。
    let group_persona = json!({
        "message": {
            "id": 11,
            "chat_id": -100888_i64,
            "is_channel_post": false,
            "sender_id": { "@type": "messageSenderChat", "chat_id": -100999_i64 },
            "content": { "@type": "messageText", "text": { "text": "/soda 1" } }
        }
    });
    assert!(!command_of(&group_persona).unwrap().is_channel_post);

    assert!(
        command_of(&json!({"message": {"id": 1, "content": {"@type": "messagePhoto"}}})).is_none()
    );
}
