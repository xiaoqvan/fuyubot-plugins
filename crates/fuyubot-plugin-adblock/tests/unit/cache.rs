use super::*;
use serde_json::json;

#[test]
fn recent_messages_are_capped() {
    let cache = Cache::new();
    for id in 0..25 {
        cache.remember_message(-1, &json!({"id": id}));
    }
    assert!(cache.recent_message(-1, 24).is_some());
    assert!(cache.recent_message(-1, 4).is_none());
}

/// 只留兜底路径用得到的字段：多余的发送状态、编辑时间、完整 origin 都该被裁掉。
#[test]
fn recent_messages_are_slimmed() {
    let cache = Cache::new();
    let message = json!({
        "id": 7,
        "chat_id": -100,
        "sender_id": {"@type": "messageSenderUser", "user_id": 9},
        "content": {"@type": "messageText", "text": {"text": "hi"}},
        "reply_to": {"@type": "messageReplyToMessage", "message_id": 3},
        "forward_info": {
            "origin": {
                "@type": "messageOriginChannel",
                "chat_id": -1009,
                "message_id": 12,
                "author_signature": "广告号"
            }
        },
        "edit_date": 1,
        "is_outgoing": false,
        "media_album_id": 0
    });
    cache.remember_message(-100, &message);
    let kept = cache.recent_message(-100, 7).expect("应该被缓存");
    assert_eq!(kept["sender_id"]["user_id"], 9);
    assert_eq!(kept["content"]["@type"], "messageText");
    assert_eq!(kept["reply_to"]["message_id"], 3);
    assert_eq!(kept["forward_info"]["origin"]["chat_id"], -1009);
    // 兜底路径不读的字段不该留下
    assert!(kept.get("edit_date").is_none());
    assert!(kept.get("is_outgoing").is_none());
    assert!(
        kept["forward_info"]["origin"]
            .get("author_signature")
            .is_none()
    );
}

/// 处置 / 提示要靠缓存把消息送回同一条论坛话题。
#[test]
fn topic_is_remembered_per_message() {
    let cache = Cache::new();
    cache.remember_message(
        -100,
        &json!({
            "id": 7,
            "topic_id": {"@type": "messageTopicForum", "forum_topic_id": 5}
        }),
    );
    cache.remember_message(-100, &json!({"id": 8}));
    assert_eq!(
        cache.topic_of(-100, 7),
        Some(crate::simplify::Topic::forum(5))
    );
    assert_eq!(cache.topic_of(-100, 8), None);
    assert_eq!(cache.topic_of(-100, 9), None);
}

#[test]
fn pending_expires() {
    let cache = Cache::new();
    cache.begin_pending(7, -100);
    assert!(cache.pending_snapshot(7).is_some());
    cache.clear_pending(7);
    assert!(cache.pending_snapshot(7).is_none());
}
#[test]
fn recent_ids_are_oldest_first_and_deletion_forgets() {
    let cache = Cache::new();
    for id in 1..=3 {
        cache.remember_message(7, &json!({"id": id}));
    }
    assert_eq!(cache.recent_ids(7), vec![1, 2, 3]);
    // 被删除的消息要从最近窗口里摘掉，否则会挡住连续判定。
    cache.forget_messages(7, &[2]);
    assert_eq!(cache.recent_ids(7), vec![1, 3]);
    assert!(cache.recent_message(7, 2).is_none());
}
