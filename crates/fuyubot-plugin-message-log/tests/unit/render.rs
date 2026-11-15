use super::*;
use serde_json::json;

#[test]
fn text_message_renders_chat_sender_and_body() {
    let message = json!({
        "chat_id": -1001234567890i64,
        "sender_id": {"@type": "messageSenderUser", "user_id": 123},
        "content": {
            "@type": "messageText",
            "text": {"@type": "formattedText", "text": "你好\n世界"}
        }
    });
    let chat = chat_label(-1001234567890, Some("开发群"));
    let sender = sender_label(message.get("sender_id"), Some("张三"), None);
    assert_eq!(
        render(&message, &chat, &sender, 0),
        "[开发群(id:-1001234567890)][张三(id:123)]: 你好\\n世界"
    );
}

#[test]
fn private_chat_uses_the_reference_tag_and_no_title_lookup() {
    assert_eq!(chat_label(123, Some("张三")), "Private");
    assert_eq!(chat_label(123, None), "Private");
    assert_eq!(
        user_label(123, Some("张三")),
        "张三(id:123)",
        "名字和 id 用同一个括号"
    );
    assert_eq!(user_label(123, None), "未知用户(id:123)");
    assert_eq!(
        chat_sender_label(-1001, Some("公告频道")),
        "公告频道(id:-1001)"
    );
    assert_eq!(chat_sender_label(-1001, None), "未知频道(id:-1001)");
}

#[test]
fn unknown_sender_still_gets_a_line() {
    assert_eq!(sender_label(None, None, None), "未知发送者");
    assert_eq!(
        sender_label(Some(&json!({"@type": "messageSenderHidden"})), None, None),
        "未知发送者"
    );
}

#[test]
fn channel_sender_uses_the_chat_title() {
    let sender = json!({"@type": "messageSenderChat", "chat_id": -1001});
    assert_eq!(
        sender_label(Some(&sender), None, Some("公告频道")),
        "公告频道(id:-1001)"
    );
    assert_eq!(
        sender_label(Some(&sender), None, None),
        "未知频道(id:-1001)"
    );
}

#[test]
fn extra_tags_follow_the_reference_order_and_include_quote_origin() {
    let message = json!({
        "topic_id": {"@type": "messageTopicForum", "forum_topic_id": 7},
        "reply_to": {
            "@type": "messageReplyToMessage",
            "chat_id": -1001,
            "message_id": 42,
            "quote": {"text": {"text": "旧消息"}},
            "origin": {"@type": "messageOriginChannel", "chat_id": -1001, "message_id": 1}
        },
        "forward_info": {
            "origin": {"@type": "messageOriginUser", "sender_user_id": 9}
        }
    });
    assert_eq!(
        extra_tags(&message),
        "[topic:forum:7][reply_chat:-1001][reply_msg:42][quote][quote_from_channel:-1001][forward_from_user:9]"
    );
}

#[test]
fn reply_without_quote_uses_reply_to_origin() {
    let reply = json!({
        "@type": "messageReplyToMessage",
        "chat_id": -1001,
        "message_id": 42,
        "origin": {
            "@type": "messageOriginChat",
            "sender_chat_id": -1002,
            "author_signature": "匿名管理员"
        }
    });
    assert_eq!(
        reply_tags(&reply),
        "[reply_chat:-1001][reply_msg:42][reply_to_chat:-1002(匿名管理员)]"
    );
}

#[test]
fn forwarded_channel_keeps_message_id() {
    let forward = json!({
        "origin": {
            "@type": "messageOriginChannel",
            "chat_id": -1001,
            "message_id": 99
        }
    });
    assert_eq!(
        forward_tags(&forward),
        "[forward_from_channel:-1001][forward_msg_id:99]"
    );
}

#[test]
fn photo_has_file_id_and_caption() {
    let content = json!({
        "@type": "messagePhoto",
        "photo": {"sizes": [
            {"photo": {"remote": {"unique_id": "small"}}},
            {"photo": {"remote": {"unique_id": "large"}}}
        ]},
        "caption": {"text": "风景"}
    });
    assert_eq!(
        content_text(Some(&content), 0),
        "[图片 file_id:large] 风景",
        "取最大一档的 unique_id"
    );
}

#[test]
fn sticker_keeps_all_ids_the_reference_prints() {
    let content = json!({
        "@type": "messageSticker",
        "sticker": {
            "id": 11,
            "set_id": 22,
            "emoji": "🙂",
            "sticker": {"remote": {"unique_id": "file-33"}}
        }
    });
    assert_eq!(
        content_text(Some(&content), 0),
        "[贴纸 🙂 id:11 set_id:22 file_id:file-33]"
    );
}

#[test]
fn document_and_audio_markers_are_readable() {
    let document = json!({
        "@type": "messageDocument",
        "document": {
            "file_name": "报表.pdf",
            "document": {"remote": {"unique_id": "doc-1"}}
        },
        "caption": {"text": "请查收"}
    });
    assert_eq!(
        content_text(Some(&document), 0),
        "[文件 报表.pdf file_id:doc-1] 请查收"
    );

    let audio = json!({
        "@type": "messageAudio",
        "audio": {
            "duration": 225,
            "title": "晴天",
            "performer": "周杰伦",
            "audio": {"remote": {"unique_id": "audio-1"}}
        }
    });
    assert_eq!(
        content_text(Some(&audio), 0),
        "[音频 晴天 - 周杰伦 03:45 file_id:audio-1]"
    );
}

#[test]
fn unknown_content_still_has_a_marker() {
    assert_eq!(content_text(None, 0), "[空内容]");
    assert_eq!(
        content_text(Some(&json!({"@type": "messageFutureThing"})), 0),
        "[messageFutureThing]"
    );
    assert_eq!(
        content_text(Some(&json!({"@type": "messageUnsupported"})), 0),
        "[不支持的消息类型]"
    );
}

#[test]
fn newlines_are_escaped_and_control_characters_neutralized() {
    assert_eq!(sanitize("a\nb\r\tc", 0), "a\\nb\\r\\tc");
    assert_eq!(sanitize("\u{1b}[31mred", 0), "\\u{1b}[31mred");
}

#[test]
fn truncation_is_by_chars_and_reports_the_original_length() {
    assert_eq!(sanitize("一二三四五", 3), "一二三…（共 5 字符）");
    assert_eq!(sanitize("abc", 10), "abc", "没超就不动");
    assert_eq!(sanitize("abc", 0), "abc", "0 = 不截断");
}

#[test]
fn rich_message_collects_plain_text_blocks() {
    let content = json!({
        "@type": "messageRichMessage",
        "message": {"blocks": [
            {"@type": "pageBlockParagraph", "text": {
                "@type": "richTexts",
                "texts": [
                    {"@type": "richTextPlain", "text": "第一段"},
                    {"@type": "richTextBold", "text": {"@type": "richTextPlain", "text": "加粗"}}
                ]
            }},
            {"@type": "pageBlockDivider"}
        ]}
    });
    assert_eq!(content_text(Some(&content), 0), "第一段 加粗");
}
