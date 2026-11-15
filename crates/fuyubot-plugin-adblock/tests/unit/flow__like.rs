use super::*;
use serde_json::json;

#[test]
fn reactors_collects_users_and_skips_chats() {
    let update = json!({
        "@type": "updateMessageInteractionInfo",
        "chat_id": -100,
        "message_id": 5,
        "interaction_info": {
            "reactions": {
                "reactions": [
                    {
                        "type": {"@type": "reactionTypeEmoji", "emoji": "👍"},
                        "total_count": 3,
                        "recent_sender_ids": [
                            {"@type": "messageSenderUser", "user_id": 11},
                            {"@type": "messageSenderChat", "chat_id": -1009},
                            {"@type": "messageSenderUser", "user_id": 12}
                        ]
                    },
                    {
                        "type": {"@type": "reactionTypeEmoji", "emoji": "❤"},
                        "total_count": 1,
                        "recent_sender_ids": [
                            {"@type": "messageSenderUser", "user_id": 11}
                        ]
                    }
                ]
            }
        }
    });
    // 频道身份没法限制成员；同一个人出现两次只算一次。
    assert_eq!(reactors_of(&update), vec![11, 12]);
}

#[test]
fn reactors_empty_without_reaction_info() {
    assert!(
        reactors_of(&json!({
            "@type": "updateMessageInteractionInfo",
            "interaction_info": null
        }))
        .is_empty()
    );
    assert!(reactors_of(&json!({"@type": "updateMessageInteractionInfo"})).is_empty());
    assert!(reactors_of(&json!({"interaction_info": {"reactions": {"reactions": []}}})).is_empty());
}

#[test]
fn tip_text_matches_the_group_ban_format() {
    let tips = tip_text("[小王](tg://user?id=1)", "连续回应 5 条消息");
    assert_eq!(
        tips,
        "#ReactionBot\n**User ID**:[小王](tg://user?id=1)\n**Reason**: reaction_bot 连续回应 5 条消息"
    );
}
