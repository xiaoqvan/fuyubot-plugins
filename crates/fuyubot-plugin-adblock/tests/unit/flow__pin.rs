use super::*;
use serde_json::json;

fn message_with(sender: Value, content: Value) -> Value {
    json!({
        "chat_id": -100,
        "id": 10,
        "sender_id": sender,
        "content": content,
    })
}

#[test]
fn pinned_message_id_is_read_from_the_service_message() {
    let pin = message_with(
        json!({"@type": "messageSenderChat", "chat_id": -200}),
        json!({"@type": "messagePinMessage", "message_id": 42}),
    );
    assert_eq!(pinned_message_id(&pin), Some(42));

    // 0 = 被置顶的原消息已删除。
    let deleted = message_with(
        json!({"@type": "messageSenderChat", "chat_id": -200}),
        json!({"@type": "messagePinMessage", "message_id": 0}),
    );
    assert_eq!(pinned_message_id(&deleted), Some(0));

    let text = message_with(
        json!({"@type": "messageSenderUser", "user_id": 7}),
        json!({"@type": "messageText"}),
    );
    assert_eq!(pinned_message_id(&text), None);
    assert_eq!(pinned_message_id(&json!({})), None);
}

#[test]
fn only_the_bound_channel_counts() {
    let from_channel = message_with(
        json!({"@type": "messageSenderChat", "chat_id": -200}),
        json!({"@type": "messageText"}),
    );
    assert!(is_sender_channel(&from_channel, -200));
    // 别的频道 / 群自身（匿名管理员）都不算。
    assert!(!is_sender_channel(&from_channel, -201));
    assert!(!is_sender_channel(&from_channel, -100));

    let from_user = message_with(
        json!({"@type": "messageSenderUser", "user_id": 7}),
        json!({"@type": "messageText"}),
    );
    assert!(!is_sender_channel(&from_user, -200));

    let anonymous = message_with(
        json!({"@type": "messageSenderChat", "chat_id": -100}),
        json!({"@type": "messageText"}),
    );
    assert!(!is_sender_channel(&anonymous, -200));
    assert!(!is_sender_channel(&json!({}), -200));
}
