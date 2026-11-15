use super::*;
use serde_json::json;

#[test]
fn extracts_text_and_captions() {
    let text = json!({
        "content": {"@type": "messageText", "text": {"text": "你好"}}
    });
    assert_eq!(message_text(&text), "你好");

    let photo = json!({
        "content": {"@type": "messagePhoto", "caption": {"text": "图说"}}
    });
    assert_eq!(message_text(&photo), "图说");
}

#[test]
fn rich_message_text_walks_blocks() {
    let content = json!({
        "@type": "messageRichMessage",
        "message": {
            "blocks": [
                {"@type": "pageBlockTitle", "title": {"@type": "richTextPlain", "text": "标题"}},
                {"@type": "pageBlockParagraph", "text": {"@type": "richTexts", "texts": [
                    {"@type": "richTextPlain", "text": "第一段"},
                    {"@type": "richTextBold", "text": {"@type": "richTextPlain", "text": "加粗"}}
                ]}}
            ]
        }
    });
    let text = rich_message_text(&content);
    assert!(text.contains("标题"), "{text}");
    assert!(text.contains("第一段"), "{text}");
    assert!(text.contains("加粗"), "{text}");
}

#[test]
fn bold_is_converted_to_markdown_v2() {
    assert_eq!(render_markdown_v2("**粗体**"), "*粗体*");
    assert_eq!(render_markdown_v2("**a.b**"), "*a\\.b*");
}

#[test]
fn reserved_characters_are_escaped() {
    assert_eq!(render_markdown_v2("a.b-c!"), "a\\.b\\-c\\!");
    assert_eq!(render_markdown_v2("#Tag"), "\\#Tag");
}

#[test]
fn inline_code_keeps_dots() {
    assert_eq!(render_markdown_v2("`a.b`"), "`a.b`");
    assert_eq!(render_markdown_v2("`a\\b`"), "`a\\\\b`");
}

#[test]
fn links_keep_url_unescaped() {
    let rendered = render_markdown_v2("[我](tg://user?id=1)");
    assert_eq!(rendered, "[我](tg://user?id=1)");
}

#[test]
fn unmatched_bold_marker_is_escaped() {
    assert_eq!(render_markdown_v2("**"), "\\*\\*");
    assert_eq!(render_markdown_v2("a*b"), "a\\*b");
}

#[test]
fn confidence_is_percentage() {
    assert_eq!(confidence_percent(0.876), "88%");
    assert_eq!(confidence_percent(0.0), "0%");
}

#[test]
fn photo_ids_are_collected() {
    let content = json!({
        "photo": {"sizes": [
            {"photo": {"remote": {"unique_id": "a"}}},
            {"photo": {"remote": {"unique_id": "b"}}},
            {"photo": {}}
        ]}
    });
    assert_eq!(photo_unique_ids(&content), vec!["a", "b"]);
}

#[test]
fn forum_topic_is_read_from_message() {
    let forum = json!({
        "topic_id": {"@type": "messageTopicForum", "forum_topic_id": 42}
    });
    assert_eq!(topic_of(&forum), Some(Topic::forum(42)));
    // 非论坛话题 / 没话题 / id 越界都按普通消息处理
    assert_eq!(
        topic_of(&json!({"topic_id": {"@type": "messageTopicSavedMessages"}})),
        None
    );
    assert_eq!(topic_of(&json!({"id": 1})), None);
    assert_eq!(
        topic_of(
            &json!({"topic_id": {"@type": "messageTopicForum", "forum_topic_id": 9_999_999_999_i64}})
        ),
        None
    );
}
#[test]
fn id_label_is_copyable_code() {
    let label = with_id("[小王](tg://user?id=1)", 701234567);
    assert!(label.contains("(ID: `701234567`)"), "{label}");
    let rendered = render_markdown_v2(&label);
    // 行内代码保留原样（客户端点一下就能复制），括号照常转义。
    assert!(rendered.contains("`701234567`"), "{rendered}");
}
