use super::*;

fn commands() -> Vec<Value> {
    vec![
        json!({ "name": "start", "description": "开始使用", "plugin": "fuyubot.start" }),
        json!({ "name": "help", "description": "这条帮助", "plugin": "xiaoqvan.help" }),
    ]
}

#[test]
fn command_is_parsed_from_the_update_envelope() {
    let update = json!({
        "@type": "updateNewMessage",
        "message": {
            "@type": "message",
            "id": 101,
            "chat_id": 777,
            "sender_id": { "@type": "messageSenderUser", "user_id": 888 },
            "is_outgoing": false,
            "content": {
                "@type": "messageText",
                "text": { "@type": "formattedText", "text": "/Help@SomeBot  参数 ", "entities": [] }
            }
        }
    });
    let command = command_of(&update).expect("应该解析出命令");
    assert!(command.is("help"));
    assert_eq!(command.chat_id, 777);
    assert_eq!(command.message_id, 101);
    assert_eq!(command.args, "参数");
    assert_eq!(command.addressed_to.as_deref(), Some("SomeBot"));
    assert_eq!(command.sender_user_id, Some(888));
    // 传成 `update["message"]` 就一条命令都收不到。
    assert!(command_of(&update["message"]).is_none());
}

#[test]
fn outgoing_or_plain_messages_are_not_commands() {
    let outgoing = json!({
        "message": {
            "id": 2, "chat_id": 1, "is_outgoing": true,
            "content": { "@type": "messageText", "text": { "text": "/help" } }
        }
    });
    assert!(command_of(&outgoing).is_none());

    let plain = json!({
        "message": {
            "id": 2, "chat_id": 1,
            "content": { "@type": "messageText", "text": { "text": "hello" } }
        }
    });
    assert!(command_of(&plain).is_none());
}

#[test]
fn addressed_commands_only_answer_when_named() {
    let command = Command {
        chat_id: 1,
        message_id: 2,
        name: "help".to_owned(),
        args: String::new(),
        addressed_to: Some("SomeBot".to_owned()),
        sender_user_id: Some(42),
        text: "/help@SomeBot".to_owned(),
    };
    assert!(command.is_for(&["somebot".to_owned()]));
    assert!(!command.is_for(&["otherbot".to_owned()]));
    // 身份未知时不吞命令。
    assert!(command.is_for(&[]));
}

/// 展示名映射：与宿主 `list_plugins` 报告里的 `display_name` 同形。
fn display_names() -> BTreeMap<String, String> {
    BTreeMap::from([
        ("xiaoqvan.help".to_owned(), "帮助".to_owned()),
        ("fuyubot.start".to_owned(), "启动问候".to_owned()),
    ])
}

#[test]
fn render_request_groups_by_plugin() {
    let config = Config::default();
    let mut list = commands();
    // 一条没有归属的（宿主自己 / 静态基线）。
    list.push(json!({ "name": "plugins", "description": "看看装了哪些插件" }));
    let (request, stats) = build_render_request(&config, &list, &display_names());
    assert_eq!(request["vue"], true);
    assert_eq!(request["width"], 760);
    assert_eq!(stats.total, 3);
    assert_eq!(stats.plugins, 2, "没有归属的那条不算来源插件");

    let cards = request["data"]["data"].as_array().unwrap();
    // 组按插件名排序（fuyubot.start < xiaoqvan.help），「其它」永远最后。
    assert_eq!(cards.len(), 3);
    assert_eq!(cards[0]["name"], "启动问候（start）");
    assert_eq!(cards[1]["name"], "帮助（help）");
    assert_eq!(cards[1]["commands"].as_array().unwrap().len(), 1);
    assert_eq!(cards[1]["desc"], "共 1 条命令");
    assert_eq!(cards[2]["name"], OTHER_GROUP);
    assert_eq!(cards[2]["commands"][0]["name"], "plugins");
}

/// 组标题没有展示名时退回短名；展示名里已含短名时不重复追加。
#[test]
fn group_titles_fall_back_to_the_short_name() {
    let names = BTreeMap::from([("xiaoqvan.adblock".to_owned(), "adblock".to_owned())]);
    assert_eq!(group_title("xiaoqvan.adblock", &names), "adblock");
    assert_eq!(group_title("fuyubot.other", &names), "other");

    // 插件包 `plugin.json` 的 name 常见形态：英文名里已经带上了短名。
    let names = BTreeMap::from([
        ("xiaoqvan.adblock".to_owned(), "Adblock".to_owned()),
        ("xiaoqvan.help".to_owned(), "Help (Image)".to_owned()),
    ]);
    assert_eq!(group_title("xiaoqvan.adblock", &names), "Adblock");
    assert_eq!(group_title("xiaoqvan.help", &names), "Help (Image)");

    // 中文展示名不含短名：补一个括号，点明是哪份插件。
    let names = BTreeMap::from([("xiaoqvan.adblock".to_owned(), "广告拦截".to_owned())]);
    assert_eq!(
        group_title("xiaoqvan.adblock", &names),
        "广告拦截（adblock）"
    );
}

#[test]
fn long_tables_are_truncated_with_a_note() {
    let config = Config {
        max_commands: 2,
        ..Config::default()
    };
    let many: Vec<Value> = (0..5)
        .map(|index| {
            json!({
                "name": format!("cmd{index}"),
                "description": "x",
                "plugin": "fuyubot.demo",
            })
        })
        .collect();
    let (request, stats) = build_render_request(&config, &many, &BTreeMap::new());
    assert_eq!(stats.total, 5);
    assert_eq!(stats.shown, 2);
    let cards = request["data"]["data"].as_array().unwrap();
    // 一个来源只画一张卡：2 条真实命令 + 1 条截断提示。
    assert_eq!(cards.len(), 1);
    let items = cards[0]["commands"].as_array().unwrap();
    assert_eq!(items.len(), 3);
    assert_eq!(items[2]["name"], "…");
    assert!(items[2]["desc"].as_str().unwrap().contains("还有 3 条"));
}

#[test]
fn help_mode_accepts_t_and_text() {
    assert_eq!(HelpMode::from_args(""), HelpMode::Image);
    assert_eq!(HelpMode::from_args("  t "), HelpMode::Text);
    assert_eq!(HelpMode::from_args("TEXT"), HelpMode::Text);
    // 别的参数维持老行为：出图。
    assert_eq!(HelpMode::from_args("foo"), HelpMode::Image);
}

#[test]
fn photo_content_matches_tdlib_shape() {
    let content = photo_content("/tmp/help.jpg");
    assert_eq!(content["@type"], "inputMessagePhoto");
    assert_eq!(content["photo"]["@type"], "inputPhoto");
    assert_eq!(content["photo"]["photo"]["@type"], "inputFileLocal");
    assert_eq!(content["photo"]["photo"]["path"], "/tmp/help.jpg");
}

/// 文字版的 `text` 必须是 `formattedText` 对象；直接塞字符串会被 TDLib 以
/// `Expected Object, but receive String` 拒绝（历史上 `/help text` 就挂在这）。
#[test]
fn text_content_wraps_the_text_in_formatted_text() {
    let content = text_content("/help — 这条帮助");
    assert_eq!(content["@type"], "inputMessageText");
    assert_eq!(content["text"]["@type"], "formattedText");
    assert_eq!(content["text"]["text"], "/help — 这条帮助");
    assert_eq!(content["text"]["entities"], json!([]));
    assert!(content["text"].is_object(), "text 不能是字符串：{content}");
    assert_eq!(content["link_preview_options"]["is_disabled"], true);
}

/// 回复入参的形状是跨仓契约：`chat_id` / `message_id` 指命令消息，`payload` 装内容。
#[test]
fn reply_input_carries_command_ids() {
    let input = reply_input(
        -100,
        9,
        30_000,
        json!({"input_message_content": {"@type": "inputMessageText"}}),
    );
    assert_eq!(input["chat_id"], -100);
    assert_eq!(input["message_id"], 9);
    assert_eq!(input["timeout_ms"], 30_000);
    assert_eq!(
        input["payload"]["input_message_content"]["@type"],
        "inputMessageText"
    );
}

#[test]
fn fallback_text_lists_every_command_by_group() {
    let text = help_text(&commands(), &display_names());
    assert!(text.contains("/start — 开始使用"), "{text}");
    assert!(text.contains("/help — 这条帮助"), "{text}");
    // 与图片同一分组：每个来源一段。
    assert!(text.contains("【帮助（help）】"), "{text}");
    assert!(text.contains("【启动问候（start）】"), "{text}");
}

#[test]
fn fallback_text_is_truncated_by_utf16_units() {
    let long: Vec<Value> = (0..500)
        .map(|index| json!({ "name": format!("cmd{index}"), "description": "很长的说明 🚀" }))
        .collect();
    let text = help_text(&long, &BTreeMap::new());
    assert!(text.encode_utf16().count() <= FALLBACK_TEXT_LIMIT + 10);
    assert!(
        text.ends_with("（已截断）"),
        "{}",
        &text[text.len().saturating_sub(20)..]
    );
}

/// 权限过滤按宿主命令表的三个字段走；缺字段时按宿主默认值
/// （`user` / `private + group` / `hidden = false`）。
#[test]
fn visibility_follows_role_scopes_and_hidden() {
    let user_command = json!({ "name": "x", "role": "user", "scopes": ["private", "group"] });
    assert!(entry_visible(
        &user_command,
        GlobalRole::User,
        ChatKind::Private,
        false
    ));
    assert!(entry_visible(
        &user_command,
        GlobalRole::User,
        ChatKind::Group,
        false
    ));
    assert!(!entry_visible(
        &user_command,
        GlobalRole::User,
        ChatKind::Channel,
        false
    ));

    // 老登记没有 role / scopes / hidden：按默认值。
    let legacy = json!({ "name": "legacy" });
    assert!(entry_visible(
        &legacy,
        GlobalRole::User,
        ChatKind::Group,
        false
    ));
    assert!(!entry_visible(
        &legacy,
        GlobalRole::User,
        ChatKind::Channel,
        false
    ));

    // chat_admin 是「本会话管理员」，不是全局 admin。
    let chat_admin = json!({ "name": "a", "role": "chat_admin", "scopes": ["group"] });
    assert!(entry_visible(
        &chat_admin,
        GlobalRole::User,
        ChatKind::Group,
        true
    ));
    assert!(!entry_visible(
        &chat_admin,
        GlobalRole::User,
        ChatKind::Group,
        false
    ));

    // admin 要全局管理员 / 主人；owner 只要主人。
    let admin = json!({ "name": "g", "role": "admin", "scopes": ["private"] });
    assert!(entry_visible(
        &admin,
        GlobalRole::Admin,
        ChatKind::Private,
        false
    ));
    assert!(!entry_visible(
        &admin,
        GlobalRole::User,
        ChatKind::Private,
        false
    ));
    let owner = json!({ "name": "o", "role": "owner", "scopes": ["private"] });
    assert!(entry_visible(
        &owner,
        GlobalRole::Owner,
        ChatKind::Private,
        false
    ));
    assert!(!entry_visible(
        &owner,
        GlobalRole::Admin,
        ChatKind::Private,
        false
    ));

    // hidden 只是不进列表，与角色无关。
    let hidden = json!({ "name": "h", "hidden": true, "scopes": ["private"] });
    assert!(!entry_visible(
        &hidden,
        GlobalRole::Owner,
        ChatKind::Private,
        true
    ));

    // 兼容写法与未知值：认不出就当 user / 不匹配。
    assert_eq!(Role::parse("chat-admin"), Some(Role::ChatAdmin));
    assert_eq!(Role::parse("chatadmin"), Some(Role::ChatAdmin));
    assert_eq!(Role::parse("nope"), None);
    assert!(scope_allows("supergroup", ChatKind::Group));
    assert!(!scope_allows("channel", ChatKind::Group));
}

/// 图片版在私聊里放宽：主人看全部，管理员除 `owner` 命令外全部。
#[test]
fn image_view_widens_private_chat_for_owner_and_admin() {
    // 群 / 频道里的 admin 命令：主人私聊也能看见（图片版专属放宽）。
    let admin_only = json!({ "name": "g", "role": "admin", "scopes": ["private"] });
    let owner_only = json!({ "name": "o", "role": "owner", "scopes": ["private"] });
    let group_only = json!({ "name": "c", "role": "chat_admin", "scopes": ["group"] });
    let hidden = json!({ "name": "h", "role": "user", "hidden": true, "scopes": ["private"] });

    // 主人：私聊什么都有（hidden 仍排除）。
    assert!(entry_visible_to_image(
        &admin_only,
        GlobalRole::Owner,
        ChatKind::Private,
        false
    ));
    assert!(entry_visible_to_image(
        &owner_only,
        GlobalRole::Owner,
        ChatKind::Private,
        false
    ));
    assert!(entry_visible_to_image(
        &group_only,
        GlobalRole::Owner,
        ChatKind::Private,
        false
    ));
    assert!(!entry_visible_to_image(
        &hidden,
        GlobalRole::Owner,
        ChatKind::Private,
        false
    ));

    // 管理员：看得见 admin 命令，看不见 owner 命令。
    assert!(entry_visible_to_image(
        &admin_only,
        GlobalRole::Admin,
        ChatKind::Private,
        false
    ));
    assert!(!entry_visible_to_image(
        &owner_only,
        GlobalRole::Admin,
        ChatKind::Private,
        false
    ));

    // 普通用户：与文字版一致，不放宽。
    assert!(!entry_visible_to_image(
        &admin_only,
        GlobalRole::User,
        ChatKind::Private,
        false
    ));
    assert!(!entry_visible_to_image(
        &group_only,
        GlobalRole::User,
        ChatKind::Private,
        false
    ));

    // 放宽只发生在私聊：群里仍按 scopes 裁剪。
    assert!(!entry_visible_to_image(
        &admin_only,
        GlobalRole::Owner,
        ChatKind::Group,
        true
    ));
}

/// 条目上的颜色 / scope 图标数据：四种角色四种描边，scope 按原始声明来。
#[test]
fn render_items_carry_role_color_and_scopes() {
    let item = render_item(&json!({
        "name": "adaiadd",
        "description": "仅 Bot 主人",
        "role": "owner",
        "scopes": ["private"],
    }))
    .expect("应能渲染成条目");
    assert_eq!(item["name"], "adaiadd");
    assert_eq!(item["color"], "#c9a2fb", "主人=淡紫");
    // 阴影是中性灰，不能跟着描边染色（同色阴影会把 1px 描边糊粗）。
    let shadow = item["shadow"].as_str().unwrap();
    assert!(!shadow.contains("201,162,251") && !shadow.contains("168,85,247"));
    assert_eq!(item["scopes"]["private"], true);
    assert_eq!(item["scopes"]["group"], false);
    assert_eq!(item["scopes"]["channel"], false);

    let every = render_item(&json!({
        "name": "help",
        "role": "chat_admin",
        "scopes": ["private", "group", "channel"],
    }))
    .unwrap();
    assert_eq!(every["color"], "#f2d268", "群管=淡黄");
    assert_eq!(every["scopes"]["private"], true);
    assert_eq!(every["scopes"]["group"], true);
    assert_eq!(every["scopes"]["channel"], true);

    assert_eq!(
        render_item(&json!({ "name": "g", "role": "admin" })).unwrap()["color"],
        "#7fd8a0",
        "管理员=淡绿"
    );
    assert_eq!(
        render_item(&json!({ "name": "x" })).unwrap()["color"],
        "#ffffff",
        "普通用户=白色"
    );
    // 缺 scopes 按宿主默认 private + group。
    assert_eq!(entry_scopes(&json!({ "name": "x" })), (true, true, false));
    // 认不出的 scope 忽略。
    assert_eq!(
        entry_scopes(&json!({ "scopes": ["group", "nope"] })),
        (false, true, false)
    );
    // 截断提示不是命令：不着色、不画图标。
    let note = placeholder_item("…", "还有 3 条命令未显示");
    assert_eq!(note["scopes"]["private"], false);
    assert_eq!(note["color"], "#ffffff");
}

/// 底部图例：**一排**共七项（四种角色 + 三种会话），角色色块与条目描边同源。
#[test]
fn legend_explains_every_role_and_scope_in_one_row() {
    let legend = legend();
    let items = legend["items"].as_array().unwrap();
    assert_eq!(
        items
            .iter()
            .map(|item| item["label"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["主人", "管理员", "群管理", "用户", "私聊", "群组", "频道"]
    );
    // 前四项是角色（画色块），后三项是会话（画图标）——模板按 kind 分支。
    assert_eq!(
        items
            .iter()
            .map(|item| item["kind"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["role", "role", "role", "role", "scope", "scope", "scope"]
    );
    // key 唯一：模板拿它做 `v-for` 的 key 与图标分支。
    let keys = items
        .iter()
        .map(|item| item["key"].as_str().unwrap())
        .collect::<BTreeSet<_>>();
    assert_eq!(keys.len(), items.len());
    // 图例色块与卡片描边必须同源，不然改了一处就会对不上。
    assert_eq!(items[0]["color"], Role::Owner.color());
    assert_eq!(items[1]["color"], Role::Admin.color());
    assert_eq!(items[2]["color"], Role::ChatAdmin.color());
    assert_eq!(items[3]["color"], Role::User.color());
    // 会话项不带颜色（它们画的是图标）。
    for item in &items[4..] {
        assert!(item.get("color").is_none(), "会话图例不该带颜色");
    }
}

/// `getChat` 的三种类型映射；supergroup 再看 `is_channel`。
#[test]
fn chat_kind_is_read_from_the_chat_object() {
    assert_eq!(
        kind_of_chat(&json!({ "type": { "@type": "chatTypePrivate" } })),
        Some(ChatKind::Private)
    );
    assert_eq!(
        kind_of_chat(&json!({ "type": { "@type": "chatTypeBasicGroup" } })),
        Some(ChatKind::Group)
    );
    assert_eq!(
        kind_of_chat(&json!({
            "type": { "@type": "chatTypeSupergroup", "is_channel": false }
        })),
        Some(ChatKind::Group)
    );
    assert_eq!(
        kind_of_chat(&json!({
            "type": { "@type": "chatTypeSupergroup", "is_channel": true }
        })),
        Some(ChatKind::Channel)
    );
    assert_eq!(kind_of_chat(&json!({})), None);
}

/// 成员状态只认群主 / 管理员；其它（成员、受限、离开）都不算。
#[test]
fn only_creator_or_administrator_counts_as_chat_admin() {
    for status in ["chatMemberStatusCreator", "chatMemberStatusAdministrator"] {
        assert!(member_is_admin(&json!({ "status": { "@type": status } })));
    }
    for status in [
        "chatMemberStatusMember",
        "chatMemberStatusRestricted",
        "chatMemberStatusLeft",
        "chatMemberStatusBanned",
    ] {
        assert!(!member_is_admin(&json!({ "status": { "@type": status } })));
    }
    assert!(!member_is_admin(&json!({})));
}
