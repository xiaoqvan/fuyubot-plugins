use super::*;

#[test]
fn sender_round_trip() {
    let user = Sender::User(5);
    assert_eq!(Sender::from_json(&user.to_json()), Some(user));
    let chat = Sender::Chat(-100);
    assert_eq!(Sender::from_json(&chat.to_json()), Some(chat));
    assert_eq!(
        Sender::from_json(&json!({"@type": "messageSenderHidden"})),
        None
    );
}

#[test]
fn rights_are_checked() {
    let admin = json!({
        "@type": "chatMemberStatusAdministrator",
        "rights": {"can_delete_messages": true, "can_restrict_members": false}
    });
    assert!(status_has_rights(&admin, &["can_delete_messages"]));
    assert!(!status_has_rights(&admin, &["can_restrict_members"]));
    assert!(status_has_rights(&admin, &[]));
    let creator = json!({"@type": "chatMemberStatusCreator"});
    assert!(status_has_rights(&creator, &["can_restrict_members"]));
    let member = json!({"@type": "chatMemberStatusMember"});
    assert!(!status_has_rights(&member, &[]));
}

#[test]
fn display_names_are_trimmed_and_fall_back() {
    assert_eq!(display_name("Ada", "Lovelace"), Some("Ada Lovelace".into()));
    assert_eq!(display_name("Ada", ""), Some("Ada".into()));
    assert_eq!(display_name("", "Lovelace"), Some("Lovelace".into()));
    assert_eq!(display_name("  ", "\t"), None);
}

#[test]
fn callback_data_shapes() {
    assert_eq!(unban_callback(-100, Sender::User(5)), "unban?cid=-100&u=5");
    assert_eq!(
        unban_callback(-100, Sender::Chat(-200)),
        "unban?cid=-100&c=-200"
    );
    assert_eq!(member_param(Sender::User(5)), "u=5");
    assert_eq!(member_param(Sender::Chat(-200)), "c=-200");
}
