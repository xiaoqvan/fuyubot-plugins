use super::*;
use serde_json::json;

fn content(kind: &str) -> Value {
    json!({ "content": { "@type": kind } })
}

#[test]
fn join_message_types_are_recognized() {
    assert_eq!(
        JoinMessage::of(&content("messageChatAddMembers")),
        Some(JoinMessage::Member)
    );
    assert_eq!(
        JoinMessage::of(&content("messageChatJoinByRequest")),
        Some(JoinMessage::Member)
    );
    assert_eq!(
        JoinMessage::of(&content("messageChatJoinFromCommunity")),
        Some(JoinMessage::Community)
    );
    assert_eq!(
        JoinMessage::of(&content("messageChatJoinByLink")),
        Some(JoinMessage::Invite)
    );
    assert_eq!(JoinMessage::of(&content("messageText")), None);
    assert_eq!(JoinMessage::of(&json!({ "content": {} })), None);
    assert_eq!(JoinMessage::of(&json!({})), None);
}

#[test]
fn each_join_message_reads_its_own_toggle() {
    let config = GroupConfig::default();
    for kind in [
        JoinMessage::Member,
        JoinMessage::Community,
        JoinMessage::Invite,
    ] {
        assert!(!kind.enabled(&config), "{kind:?} 默认关闭");
    }

    let config = GroupConfig {
        delete_join_message: true,
        ..GroupConfig::default()
    };
    assert!(JoinMessage::Member.enabled(&config));
    assert!(!JoinMessage::Community.enabled(&config));
    assert!(!JoinMessage::Invite.enabled(&config));

    let config = GroupConfig {
        delete_join_community_message: true,
        ..GroupConfig::default()
    };
    assert!(!JoinMessage::Member.enabled(&config));
    assert!(JoinMessage::Community.enabled(&config));
    assert!(!JoinMessage::Invite.enabled(&config));

    let config = GroupConfig {
        delete_join_invite_message: true,
        ..GroupConfig::default()
    };
    assert!(!JoinMessage::Member.enabled(&config));
    assert!(!JoinMessage::Community.enabled(&config));
    assert!(JoinMessage::Invite.enabled(&config));
}

#[test]
fn keys_are_stable() {
    assert_eq!(JoinMessage::Member.key(), "join");
    assert_eq!(JoinMessage::Community.key(), "join_community");
    assert_eq!(JoinMessage::Invite.key(), "join_invite");
}
