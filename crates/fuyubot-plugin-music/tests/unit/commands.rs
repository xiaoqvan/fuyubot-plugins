use super::*;

#[test]
fn quality_is_the_last_token_only_when_it_is_known() {
    assert_eq!(
        split_args(Platform::Netease, "186016 lossless"),
        ("186016".to_owned(), Some("lossless"))
    );
    assert_eq!(
        split_args(Platform::Netease, "周杰伦 晴天"),
        ("周杰伦 晴天".to_owned(), None),
        "最后一段不是音质时整串都是查询"
    );
    assert_eq!(
        split_args(
            Platform::Netease,
            "https://music.163.com/song?id=1 jymaster"
        ),
        (
            "https://music.163.com/song?id=1".to_owned(),
            Some("jymaster")
        )
    );
    assert_eq!(
        split_args(Platform::Soda, "7304719759323564095 highest"),
        ("7304719759323564095".to_owned(), Some("highest"))
    );
}

#[test]
fn a_lone_quality_word_is_a_search_query() {
    // `/music163 lossless`、`/soda High` 都是正常搜索词，不能当成「只给了音质」。
    assert_eq!(
        split_args(Platform::Netease, "lossless"),
        ("lossless".to_owned(), None)
    );
    assert_eq!(
        split_args(Platform::Soda, "High"),
        ("High".to_owned(), None)
    );
    assert_eq!(
        split_args(Platform::Netease, "320"),
        ("320".to_owned(), None)
    );
    assert_eq!(split_args(Platform::Netease, ""), (String::new(), None));
}

#[test]
fn a_quality_word_after_a_plain_keyword_is_not_a_quality() {
    // 尾词像音质，但前半段不是链接 / ID —— 整串都是搜索词。
    assert_eq!(
        split_args(Platform::Netease, "Shape of You high"),
        ("Shape of You high".to_owned(), None)
    );
    assert_eq!(
        split_args(Platform::Soda, "Lo-Fi best"),
        ("Lo-Fi best".to_owned(), None)
    );
}

#[test]
fn usage_mentions_the_command_and_its_qualities() {
    let text = usage(Platform::Netease);
    assert!(text.contains("/music163 <关键词>"), "{text}");
    assert!(text.contains("exhigh"), "{text}");
    let text = usage(Platform::Soda);
    assert!(text.contains("/soda <关键词>"), "{text}");
    assert!(text.contains("lossless"), "{text}");
}

#[test]
fn audio_extension_is_always_a_mime_recognisable_one() {
    assert_eq!(audio_extension("mp3"), "mp3");
    assert_eq!(audio_extension("flac"), "flac");
    assert_eq!(audio_extension("MP4"), "m4a");
    assert_eq!(audio_extension(""), "mp3");
    assert_eq!(audio_extension("weird"), "mp3");
}

#[test]
fn update_new_message_is_read_into_a_message() {
    let chat_id: i64 = -1001234567890;
    let update = serde_json::json!({
        "@type": "updateNewMessage",
        "message": {
            "@type": "message",
            "id": 22,
            "chat_id": chat_id,
            "is_outgoing": false,
            "content": {
                "@type": "messageText",
                "text": { "@type": "formattedText", "text": "https://www.qishui.com/track/1", "entities": [] },
            },
        },
    });
    let message = message_of(&update).expect("文本消息应当解得出");
    assert_eq!(message.chat_id, chat_id);
    assert!(!message.is_outgoing);
    assert!(!message.is_channel_post);
    assert!(!message.sender_is_chat);
    assert!(!message.is_forwarded);
}

/// 自动识别的过滤与频道落点：与抖音插件同一套口径。
#[test]
fn channel_posts_and_channel_identity_forwards_are_classified() {
    let update = |chat_id: i64,
                  outgoing: bool,
                  is_channel_post: bool,
                  sender: serde_json::Value,
                  forward: Option<serde_json::Value>| {
        let mut message = serde_json::json!({
            "@type": "message",
            "id": 9,
            "chat_id": chat_id,
            "is_outgoing": outgoing,
            "is_channel_post": is_channel_post,
            "sender_id": sender,
            "content": {
                "@type": "messageText",
                "text": { "@type": "formattedText", "text": "https://music.163.com/song?id=1", "entities": [] },
            },
        });
        if let Some(forward) = forward {
            message["forward_info"] = forward;
        }
        serde_json::json!({ "@type": "updateNewMessage", "message": message })
    };

    let channel_persona = serde_json::json!({
        "@type": "messageSenderChat",
        "chat_id": -100999_i64
    });
    let forwarded = serde_json::json!({
        "origin": { "@type": "messageOriginChannel", "chat_id": -100999_i64, "message_id": 1 }
    });
    let user = serde_json::json!({ "@type": "messageSenderUser", "user_id": 42 });

    // 群 + 频道身份 + 转发 → 跳过
    let group_forward = message_of(&update(
        -100888,
        false,
        false,
        channel_persona.clone(),
        Some(forwarded.clone()),
    ))
    .unwrap();
    assert!(
        auto_job(&group_forward).is_none(),
        "群里频道身份发出的转发消息不该自动解析"
    );

    // 群 + 频道身份但非转发 → 照常
    let group_plain = message_of(&update(
        -100888,
        false,
        false,
        channel_persona.clone(),
        None,
    ))
    .unwrap();
    assert!(auto_job(&group_plain).is_some());

    // 普通成员转发 → 照常
    let user_forward = message_of(&update(
        -100888,
        false,
        false,
        user,
        Some(forwarded.clone()),
    ))
    .unwrap();
    assert!(auto_job(&user_forward).is_some());

    // 自家频道自己发的帖子：outgoing 也要解析，并走编辑
    let channel_post = message_of(&update(
        -100777,
        true,
        true,
        serde_json::json!({ "@type": "messageSenderChat", "chat_id": -100777_i64 }),
        Some(forwarded),
    ))
    .unwrap();
    let job = auto_job(&channel_post).expect("频道帖要解析");
    assert!(job.target.is_edit(), "频道帖走编辑");
}

#[test]
fn non_text_messages_are_ignored() {
    let update = serde_json::json!({
        "@type": "updateNewMessage",
        "message": { "@type": "message", "id": 1, "chat_id": 1,
            "content": { "@type": "messagePhoto", "photo": {} } },
    });
    assert!(message_of(&update).is_none());
    assert!(message_of(&serde_json::json!({})).is_none());
}

#[test]
fn sqlite_is_the_only_soft_dependency() {
    assert_eq!(
        <MusicPlugin as Plugin>::soft_inject(),
        vec![cache::SERVICE.to_owned()]
    );
    assert!(<MusicPlugin as Plugin>::inject().is_empty());
}

#[test]
fn the_service_names_are_pinned() {
    assert_eq!(HOST_COMMANDS, "fuyubot.host.commands");
    assert_eq!(cache::SERVICE, "fuyubot.sqlite");
}

/// 登记入参带 `role` / `scopes`：宿主闸门与 `/help` 都按它们过滤。
#[test]
fn registration_payload_carries_role_and_scopes() {
    let input = registration_payload(NETEASE_COMMAND, NETEASE_DESCRIPTION);
    assert_eq!(input["name"], "music163");
    assert_eq!(input["role"], "user");
    assert_eq!(
        input["scopes"],
        serde_json::json!(["private", "group", "channel"]),
        "频道里命令要能编辑原帖"
    );
}

#[test]
fn command_names_are_stable() {
    // 改了它们，用户的 /music163、/soda 就失灵了。
    assert_eq!(NETEASE_COMMAND, "music163");
    assert_eq!(SODA_COMMAND, "soda");
    assert_eq!(Platform::Netease.name(), "网易云音乐");
    assert_eq!(Platform::Soda.name(), "汽水音乐");
}

/// `user_data` 的取址层数曾经写错过（解两层），症状是订阅从来不触发且不报错。
/// 这里钉的是这条指针路径真的能调到闭包。
#[test]
fn user_data_round_trip_reaches_the_closure() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    static CALLS: AtomicUsize = AtomicUsize::new(0);
    let mut owned: Box<Box<MessageFn>> = Box::new(Box::new(|_message: &Message| {
        CALLS.fetch_add(1, Ordering::SeqCst);
    }));
    let user_data = (&raw mut *owned).cast::<std::ffi::c_void>();
    let back = user_data as *const Box<MessageFn>;
    let handler = unsafe { &*back };
    handler(&Message {
        chat_id: 1,
        message_id: 1,
        text: "x".to_owned(),
        is_outgoing: false,
        sender_is_chat: false,
        is_forwarded: false,
        is_channel_post: false,
    });
    assert_eq!(CALLS.load(Ordering::SeqCst), 1, "回调必须真的调到闭包");
}

/// 缓存文档必须带着命中重发所需的全部字段。
#[test]
fn cache_documents_round_trip_through_json() {
    let meta = AudioMeta {
        title: "晴天".to_owned(),
        performer: "周杰伦".to_owned(),
        album: "叶惠美".to_owned(),
        link: "https://music.163.com/song?id=186016".to_owned(),
        quality: "极高（exhigh）".to_owned(),
        bitrate: 320_000,
        size: 10 * 1024 * 1024,
        duration: 269,
        trial: false,
    };
    let sent = SentMedia {
        kind: crate::simplify::KIND_AUDIO,
        file_id: Some(42),
        cover_id: Some(43),
        width: 0,
        height: 0,
        duration: 269,
    };
    let doc = audio_doc(&meta, &sent);
    assert_eq!(doc["v"], 1);
    assert_eq!(doc["kind"], "audio");
    assert_eq!(doc["file_id"], 42);
    assert_eq!(doc["cover_id"], 43);
    assert_eq!(doc["title"], "晴天");
    assert_eq!(doc["performer"], "周杰伦");
    assert_eq!(doc["album"], "叶惠美");
    assert_eq!(doc["link"], "https://music.163.com/song?id=186016");
    assert_eq!(doc["quality_label"], "极高（exhigh）");
    assert_eq!(doc["bitrate"], 320_000);
    assert_eq!(doc["size"], 10 * 1024 * 1024);
    assert_eq!(doc["duration"], 269);
    assert_eq!(doc["trial"], false);
    // 反过来也读得出来（命中路径依赖这些字段）。
    assert_eq!(text_of(&doc, "title"), "晴天");
    assert_eq!(u64_of(&doc, "bitrate"), 320_000);
    assert_eq!(i64_of(&doc, "duration"), 269);
}

#[test]
fn video_cache_documents_carry_playback_shape() {
    let meta = VideoMeta {
        title: "塔不灭".to_owned(),
        author: "永川塔菲".to_owned(),
        duration: 33,
        width: 1920,
        height: 1080,
        preview: true,
    };
    let sent = SentMedia {
        kind: crate::simplify::KIND_VIDEO,
        file_id: Some(7),
        cover_id: None,
        width: 1920,
        height: 1080,
        duration: 33,
    };
    let doc = video_doc(&meta, &sent);
    assert_eq!(doc["kind"], "video");
    assert_eq!(doc["file_id"], 7);
    assert_eq!(doc["width"], 1920);
    assert_eq!(doc["height"], 1080);
    assert_eq!(doc["duration"], 33);
    assert_eq!(doc["preview"], true);
    assert!(doc["cover_id"].is_null(), "没封面也要能存");
}

/// 缓存键必须用**用户请求的档位**（不是接口降级后的实际档位）。
#[test]
fn cache_keys_are_stable_and_quality_scoped() {
    assert_eq!(
        cache::key(&["netease", "186016", "lossless"]),
        "netease:186016:lossless"
    );
    assert_eq!(
        cache::key(&["soda", "7304719759323564095", "best"]),
        "soda:7304719759323564095:best"
    );
    assert_eq!(
        cache::key(&["soda-video", "7625615883351772468"]),
        "soda-video:7625615883351772468"
    );
}
