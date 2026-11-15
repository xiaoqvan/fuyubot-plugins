use super::*;

/// 传参层次是这条链上最容易错的地方：`command_of` 吃的是整条 update。
#[test]
fn command_is_parsed_from_the_update_envelope() {
    let update = serde_json::json!({
        "@type": "updateNewMessage",
        "message": {
            "@type": "message",
            "id": 101,
            "chat_id": 777,
            "sender_id": { "@type": "messageSenderUser", "user_id": 777 },
            "is_outgoing": false,
            "content": {
                "@type": "messageText",
                "text": { "@type": "formattedText", "text": "/addkw ads 甲 乙 丙", "entities": [] }
            }
        }
    });
    let command = crate::simplify::command_of(&update).expect("应该解析出命令");
    assert_eq!(command.name, "addkw");
    assert_eq!(command.chat_id, 777);
    assert_eq!(command.args, "ads 甲 乙 丙");
    // 传成 `update["message"]` 就一条命令都收不到——把这条钉死。
    assert!(crate::simplify::command_of(&update["message"]).is_none());
}

#[test]
fn command_words_split_on_whitespace() {
    let update = serde_json::json!({
        "message": {
            "id": 1,
            "chat_id": 2,
            "content": {
                "@type": "messageText",
                "text": { "@type": "formattedText", "text": "/adconfig  limit   ban" }
            }
        }
    });
    let command = crate::simplify::command_of(&update).expect("命令");
    assert_eq!(words(&command), vec!["limit", "ban"]);
}

/// 注册表是「什么算命令」的唯一真相：不是命令的文本必须返回 `None`，
/// 这样消息流水线才会继续按普通消息检测。
#[test]
fn only_registered_names_are_commands() {
    assert!(spec::find("adconfig").is_some());
    assert!(spec::find("接单").is_none());
}

/// 登记到宿主命令表时，scope / permission 必须翻译成 `scopes` / `role`——
/// 少了它们，宿主闸门与 `/help` 都只能按最宽松的默认值处理。
#[test]
fn registration_payload_carries_role_and_scopes() {
    let payload = |name: &str| -> Value {
        serde_json::from_str(&command_payload(spec::find(name).expect("命令存在")))
            .expect("登记入参是 JSON")
    };

    let addkw = payload("addkw");
    assert_eq!(addkw["name"], "addkw");
    assert_eq!(addkw["role"], "admin");
    assert_eq!(addkw["scopes"], serde_json::json!(["private"]));

    let adkew = payload("adkew");
    assert_eq!(adkew["role"], "owner");

    let adconfig = payload("adconfig");
    assert_eq!(adconfig["role"], "user");
    assert_eq!(adconfig["scopes"], serde_json::json!(["group"]));

    let qban = payload("qban");
    assert_eq!(qban["scopes"], serde_json::json!(["private", "group"]));
}
