use super::*;

#[test]
fn short_aliases_normalize() {
    assert_eq!(config_key("like_guard"), "like_guard_enabled");
    assert_eq!(config_key("like_guard_enabled"), "like_guard_enabled");
    assert_eq!(config_key("join"), "delete_join_message");
    assert_eq!(
        config_key("join_community"),
        "delete_join_community_message"
    );
    assert_eq!(config_key("join_invite"), "delete_join_invite_message");
    assert_eq!(config_key("unpin"), "unpin_channel_pins");
    assert_eq!(config_key("unpin_channel"), "unpin_channel_pins");
    assert_eq!(config_key("delay"), "delay");
    assert_eq!(config_key(""), "");
}

#[test]
fn toggle_table_covers_every_group_config_switch() {
    // 新增开关别漏改这里：状态页与 `/adconfig` 都按 TOGGLES 遍历。
    assert_eq!(TOGGLES.len(), 8);
    for name in [
        "ai_review_enabled",
        "check_new_member",
        "spam_tips",
        "like_guard_enabled",
        "delete_join_message",
        "delete_join_community_message",
        "delete_join_invite_message",
        "unpin_channel_pins",
    ] {
        assert!(
            TOGGLES.iter().any(|toggle| toggle.name == name),
            "缺少开关 {name}"
        );
    }
}

#[test]
fn every_toggle_can_be_read_and_written() {
    let mut config = GroupConfig::default();
    for toggle in &TOGGLES {
        let before = (toggle.get)(&config);
        (toggle.set)(&mut config, !before);
        assert_eq!((toggle.get)(&config), !before, "{} 写不进去", toggle.name);
        (toggle.set)(&mut config, before);
        assert_eq!((toggle.get)(&config), before, "{} 还原失败", toggle.name);
    }
}
