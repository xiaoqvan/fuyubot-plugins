use super::*;

#[test]
fn plain_urls_are_picked_up_from_the_arguments() {
    assert_eq!(
        first_url("/dy https://v.douyin.com/abc/").as_deref(),
        Some("https://v.douyin.com/abc/")
    );
    // 命令名本身不是链接，不能把它当候选
    assert_eq!(first_url("no url here"), None);
}

/// 分享链接常被中文标点包住：`看看这个：https://v.douyin.com/xx/。`
#[test]
fn trailing_punctuation_is_stripped() {
    assert_eq!(
        first_douyin_url("看看这个：https://v.douyin.com/xx/。").as_deref(),
        Some("https://v.douyin.com/xx/")
    );
    assert_eq!(
        first_douyin_url("(https://www.douyin.com/video/123)").as_deref(),
        Some("https://www.douyin.com/video/123")
    );
}

#[test]
fn only_douyin_hosts_trigger_automatic_parsing() {
    assert!(first_douyin_url("https://v.douyin.com/a/").is_some());
    assert!(first_douyin_url("https://www.douyin.com/video/1").is_some());
    assert!(first_douyin_url("https://www.iesdouyin.com/share/video/1/").is_some());
    // 别的站不能误触发
    assert!(first_douyin_url("https://www.youtube.com/watch?v=1").is_none());
    assert!(first_douyin_url("https://v.kuaishou.com/a/").is_none());
    assert!(first_douyin_url("没有链接").is_none());
}

/// `douyin.com` 当成**子串**去 contains 会误伤：下面的链接 host 是
/// `douyin.com.evil.test` 和 `example.com`，都不是抖音。
#[test]
fn lookalike_domains_do_not_match() {
    assert!(
        first_douyin_url("https://douyin.com.evil.test/x").is_none(),
        "别家域名不能替它触发解析"
    );
    assert!(
        first_douyin_url("https://example.com/douyin").is_none(),
        "路径里有 douyin 也不算"
    );
    assert!(first_douyin_url("https://notdouyin.com/x").is_none());
    assert!(first_douyin_url("https://bilibili.com/video/1").is_none());
}

#[test]
fn host_extraction_handles_scheme_port_and_case() {
    assert_eq!(
        host_of("https://v.douyin.com/abc/").as_deref(),
        Some("v.douyin.com")
    );
    assert_eq!(
        host_of("HTTP://WWW.DOUYIN.COM/x").as_deref(),
        Some("www.douyin.com")
    );
    assert_eq!(
        host_of("https://douyin.com:443/x").as_deref(),
        Some("douyin.com")
    );
    assert_eq!(host_of("https://douyin.com").as_deref(), Some("douyin.com"));
    assert_eq!(
        host_of("https://douyin.com?a=1").as_deref(),
        Some("douyin.com")
    );
    assert_eq!(
        host_of("https://u:p@douyin.com/x").as_deref(),
        Some("douyin.com")
    );
    assert!(is_douyin_host("douyin.com"));
    assert!(is_douyin_host("v.douyin.com"));
    assert!(is_douyin_host("iesdouyin.com"));
    assert!(!is_douyin_host("evil-douyin.com"));
    assert!(!is_douyin_host("douyin.com.evil.test"));

    // 汽水音乐的链接虽然也在 *.douyin.com 下，但归 xiaoqvan.music 管。
    assert!(!is_douyin_link("https://qishui.douyin.com/s/iXvvC5YV/"));
    assert!(!is_douyin_link(
        "https://music.douyin.com/qishui/share/ugc_video?ugc_video_id=1"
    ));
    assert!(!is_douyin_link(
        "https://music.douyin.com/qishui/share/track?track_id=1"
    ));
    // 同一台主机上的非汽水路径仍然归抖音。
    assert!(is_douyin_link("https://music.douyin.com/other/video/123"));
    assert!(is_douyin_link("https://v.douyin.com/abc/"));

    // 自动识别：混合文本里只挑抖音链接，不能把汽水链接抢过来。
    assert!(first_douyin_url("看看 https://qishui.douyin.com/s/iXvvC5YV/").is_none());
    assert_eq!(
        first_douyin_url("两个链接 https://v.douyin.com/abc/ 和 https://qishui.douyin.com/s/x/")
            .as_deref(),
        Some("https://v.douyin.com/abc/")
    );
}

/// 汽水音乐的三种链接都要认出来（短链、落地页视频、落地页曲目）。
///
/// ⚠️ `qishui.com` 是**独立域名**，不是 `douyin.com` 的子域——
/// 只按 `*.douyin.com` 判会漏掉它，用户看到的症状就是「汽水链接还是被抖音插件抢走」。
#[test]
fn qishui_links_are_routed_away_from_douyin() {
    for url in [
        "https://qishui.douyin.com/s/iXvvC5YV/",
        "https://qishui.douyin.com/s/iXvvC5YV",
        "https://www.qishui.com/track/7304719759323564095",
        "https://qishui.com/track/1",
        "https://music.douyin.com/qishui/share/track?track_id=1",
        "https://music.douyin.com/qishui/share/ugc_video?ugc_video_id=1",
        // 大小写不敏感：用户从别处粘过来的链接可能被转成大写
        "https://MUSIC.douyin.com/QISHUI/share/track?track_id=1",
    ] {
        assert_eq!(route_of(url), Route::Qishui, "{url} 应归音乐插件");
    }
}

/// 判据只该命中汽水自己，不能把抖音或同主机的其它路径一起让出去。
#[test]
fn qishui_matching_does_not_swallow_other_douyin_links() {
    // `music.douyin.com` 上非 `/qishui/` 的路径仍归抖音
    assert_eq!(
        route_of("https://music.douyin.com/other/video/123"),
        Route::Douyin
    );
    // 普通的抖音作品
    assert_eq!(route_of("https://v.douyin.com/abc/"), Route::Douyin);
    assert_eq!(
        route_of("https://www.douyin.com/video/7401234567890123456"),
        Route::Douyin
    );
    // 带 qishui 字样的**别的域名**不算（防止 `qishui.com.evil.test` 那种）
    assert_eq!(
        route_of("https://qishui.com.evil.test/x"),
        Route::Foreign,
        "把目标域名当前缀不能命中"
    );
    assert_eq!(route_of("https://notqishui.com/x"), Route::Foreign);
    assert_eq!(route_of("https://bilibili.com/video/1"), Route::Foreign);
}

/// `/dy` 里挑链接的三条规则。
#[test]
fn the_command_picks_the_douyin_link_first() {
    // ① 只有抖音 → 解析
    match pick_for_command("https://v.douyin.com/abc/") {
        Selection::Douyin(link) => assert_eq!(link, "https://v.douyin.com/abc/"),
        other => panic!("应当交给抖音解析，实际 {other:?}"),
    }

    // ② 只有汽水 → 让出去（并且真的认出来是哪条，日志里要用）
    match pick_for_command("听听这个 https://qishui.douyin.com/s/iXvvC5YV/") {
        Selection::Qishui(link) => assert_eq!(link, "https://qishui.douyin.com/s/iXvvC5YV/"),
        other => panic!("应当让给音乐插件，实际 {other:?}"),
    }

    // ③ 两个都在、汽水排在**前面** → 仍然解析抖音那条。
    // 这是最容易写错的一条：先按顺序取第一个链接就会把抖音漏掉，
    // 用户看到的是「我明明带了抖音链接，它却说这是汽水的」。
    match pick_for_command(
        "https://qishui.douyin.com/s/x/ 和 https://www.douyin.com/video/7401234567890123456",
    ) {
        Selection::Douyin(link) => {
            assert_eq!(link, "https://www.douyin.com/video/7401234567890123456")
        }
        other => panic!("抖音链接优先，实际 {other:?}"),
    }

    // ④ 没有链接
    assert!(matches!(pick_for_command("帮我解析一下"), Selection::None));

    // ⑤ 别的域名：保持老行为（交给解析层自己报错），不在这里新加拒绝
    match pick_for_command("https://example.com/x") {
        Selection::Douyin(link) => assert_eq!(link, "https://example.com/x"),
        other => panic!("非抖音域名应维持旧行为，实际 {other:?}"),
    }
}

/// `/dy <汽水链接>` 的文案必须把「该找谁」说清楚。
///
/// 只说「解析失败」会让用户反复重发——链接本身是好的，只是不归这个插件管。
#[test]
fn the_qishui_rejection_message_says_where_to_go() {
    let text = QISHUI_REJECTION;
    assert!(text.contains("汽水"), "{text}");
    assert!(text.contains("音乐"), "要指出归音乐插件管：{text}");
    assert!(
        text.contains("直接") || text.contains("发出来"),
        "要告诉用户该怎么办：{text}"
    );
    // 别把命令名写成 `/soda` 这种用户用不上的形式——自动识别就够了。
    assert!(!text.contains("/soda"), "不该让用户去敲命令：{text}");
}

/// 插件目录来自 `plugin_start` 的 config；缺 / 坏 JSON 时退回 `plugins/`。
/// （实现在 runtime 里——`start` 要用它读 cookie、定下载目录。）
#[test]
fn plugins_dir_comes_from_the_config_or_defaults() {
    use std::path::PathBuf;
    let config = r#"{"plugins_dir":"D:/bots/demo/plugins"}"#;
    assert_eq!(
        crate::runtime::plugins_dir_from(config),
        PathBuf::from("D:/bots/demo/plugins")
    );
    // 配置缺 / 不是 JSON → 退回 plugins/
    assert_eq!(
        crate::runtime::plugins_dir_from(""),
        PathBuf::from("plugins")
    );
    assert_eq!(
        crate::runtime::plugins_dir_from("not json"),
        PathBuf::from("plugins")
    );
    assert_eq!(
        crate::runtime::plugins_dir_from("{}"),
        PathBuf::from("plugins")
    );
}

#[test]
fn the_command_name_is_stable() {
    // 改了它，用户的 `/dy` 就失灵了——钉住。
    assert_eq!(COMMAND, "dy");
}

/// 《更新 → Message》这一步单独钉住：字段路径错一个字，自动识别就整个哑掉。
#[test]
fn update_new_message_is_read_into_a_message() {
    // chat_id 是 int53：不能写成裸字面量（`json!` 会把它推成 i32 然后溢出），
    // 这样写才能保证它是 i64。
    let chat_id: i64 = -1001234567890;
    let update = serde_json::json!({
        "@type": "updateNewMessage",
        "message": {
            "@type": "message",
            "id": 22,
            "chat_id": chat_id,
            "is_outgoing": false,
            "sender_id": { "@type": "messageSenderUser", "user_id": 42 },
            "content": {
                "@type": "messageText",
                "text": { "@type": "formattedText", "text": "https://v.douyin.com/x/", "entities": [] },
            },
        },
    });
    let message = message_of(&update).expect("文本消息应当解得出");
    assert_eq!(message.chat_id, chat_id);
    assert_eq!(message.message_id, 22);
    assert_eq!(message.text, "https://v.douyin.com/x/");
    assert!(!message.is_outgoing);
    assert!(!message.is_channel_post);
}

/// 频道判定用 TDLib 的 `is_channel_post`；群里「匿名频道马甲」的发送者也是
/// `messageSenderChat`，但 `is_channel_post` 为 false，不能走编辑。
#[test]
fn channel_posts_are_detected_by_the_tdlib_flag() {
    let update = |chat_id: i64, is_channel_post: bool, sender: serde_json::Value| {
        serde_json::json!({
            "@type": "updateNewMessage",
            "message": {
                "@type": "message",
                "id": 5,
                "chat_id": chat_id,
                "is_outgoing": false,
                "is_channel_post": is_channel_post,
                "sender_id": sender,
                "content": {
                    "@type": "messageText",
                    "text": { "@type": "formattedText", "text": "https://v.douyin.com/x/", "entities": [] },
                },
            },
        })
    };
    let post = message_of(&update(
        -100777,
        true,
        serde_json::json!({ "@type": "messageSenderChat", "chat_id": -100777 }),
    ))
    .unwrap();
    assert!(post.is_channel_post, "频道帖要认出来");
    assert!(post.sender_is_chat);
    assert!(!post.is_forwarded);

    // 群里匿名频道马甲：发送者是关联频道 -100999，当前会话是 -100888。
    let anonymous = message_of(&update(
        -100888,
        false,
        serde_json::json!({ "@type": "messageSenderChat", "chat_id": -100999 }),
    ))
    .unwrap();
    assert!(anonymous.sender_is_chat);
    assert!(
        !anonymous.is_channel_post,
        "群里的匿名频道马甲不是频道帖，不能去编辑别人的消息"
    );

    let user = message_of(&update(
        -100777,
        false,
        serde_json::json!({ "@type": "messageSenderUser", "user_id": 1 }),
    ))
    .unwrap();
    assert!(!user.sender_is_chat);
    assert!(!user.is_channel_post, "普通群成员不是频道帖");
}

/// 自动识别的过滤：群里「发送人是频道 + 转发」的消息直接跳过；普通转发照常。
#[test]
fn channel_identity_forwards_are_skipped_in_groups() {
    let message = |chat_id: i64,
                   is_channel_post: bool,
                   sender: serde_json::Value,
                   forward_info: Option<serde_json::Value>| {
        let mut update = serde_json::json!({
            "@type": "updateNewMessage",
            "message": {
                "@type": "message",
                "id": 7,
                "chat_id": chat_id,
                "is_outgoing": false,
                "is_channel_post": is_channel_post,
                "sender_id": sender,
                "content": {
                    "@type": "messageText",
                    "text": { "@type": "formattedText", "text": "https://v.douyin.com/x/", "entities": [] },
                },
            },
        });
        if let Some(forward) = forward_info {
            update["message"]["forward_info"] = forward;
        }
        message_of(&update).unwrap()
    };

    let channel_persona = serde_json::json!({
        "@type": "messageSenderChat",
        "chat_id": -100999_i64
    });
    let forwarded = serde_json::json!({
        "origin": { "@type": "messageOriginChannel", "chat_id": -100999_i64, "message_id": 1 }
    });
    let user = serde_json::json!({ "@type": "messageSenderUser", "user_id": 42 });

    // 群（不是频道帖）+ 频道身份 + 转发 → 跳过
    assert!(
        auto_target(&message(
            -100888,
            false,
            channel_persona.clone(),
            Some(forwarded.clone())
        ))
        .is_none(),
        "群里频道身份发出的转发消息不该自动解析"
    );
    // 频道身份但**不是转发** → 照常解析（只是普通新消息）
    assert!(
        auto_target(&message(-100888, false, channel_persona.clone(), None)).is_some(),
        "群里频道身份的普通发言仍然解析"
    );
    // 普通用户转发 → 照常解析
    assert!(
        auto_target(&message(
            -100888,
            false,
            user.clone(),
            Some(forwarded.clone())
        ))
        .is_some(),
        "普通群成员转发的抖音链接仍然解析"
    );
    // 真正的频道帖即使是转发也照常解析，并走编辑
    let channel = serde_json::json!({
        "@type": "messageSenderChat",
        "chat_id": -100777_i64
    });
    let (target, _) = auto_target(&message(-100777, true, channel, Some(forwarded))).unwrap();
    assert!(target.is_edit(), "频道帖走编辑而不是新发");
}

/// 自己发的频道帖仍要解析（自家频道里发链接 → 编辑成视频）；自己发的群消息不解析。
#[test]
fn outgoing_channel_posts_are_still_recognized() {
    let update = |chat_id: i64, is_channel_post: bool, sender: serde_json::Value| {
        serde_json::json!({
            "@type": "updateNewMessage",
            "message": {
                "@type": "message",
                "id": 8,
                "chat_id": chat_id,
                "is_outgoing": true,
                "is_channel_post": is_channel_post,
                "sender_id": sender,
                "content": {
                    "@type": "messageText",
                    "text": { "@type": "formattedText", "text": "https://v.douyin.com/x/", "entities": [] },
                },
            },
        })
    };

    let channel = message_of(&update(
        -100777,
        true,
        serde_json::json!({ "@type": "messageSenderChat", "chat_id": -100777 }),
    ))
    .unwrap();
    let (target, _) = auto_target(&channel).expect("自己发的频道帖也要解析");
    assert!(target.is_edit(), "并且走编辑");

    let group = message_of(&update(
        -100888,
        false,
        serde_json::json!({ "@type": "messageSenderUser", "user_id": 42 }),
    ))
    .unwrap();
    assert!(
        auto_target(&group).is_none(),
        "自己发的群消息仍然跳过，避免套娃"
    );
}

#[test]
fn non_text_messages_are_ignored() {
    let update = serde_json::json!({
        "@type": "updateNewMessage",
        "message": { "@type": "message", "id": 1, "chat_id": 1,
            "content": { "@type": "messagePhoto", "photo": {} } },
    });
    assert!(message_of(&update).is_none());
    assert!(message_of(&serde_json::json!({})).is_none());
}

/// `user_data` 的取址层数曾经写错过：写成 `&raw mut **handler`（解两层，指向闭包
/// 本体），回调里再按 `*const Box<dyn Fn>` 转回去就读错了地方——症状是
/// **自动识别那条订阅从来不触发**，而且不报错、不崩。
///
/// 所以这里钉的不是地址数值，而是**这条指针路径真的能调到闭包**：
/// 按 `scan_douyin_links` 的方式取址、按 `scan_trampoline` 的方式转回来并调用，
/// 副作用必须看得见。（比地址不可靠——零大小闭包的地址可能是对齐占位值。）
#[test]
fn user_data_round_trip_reaches_the_closure() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    static CALLS: AtomicUsize = AtomicUsize::new(0);
    let mut owned: Box<Box<MessageFn>> = Box::new(Box::new(|_message: &Message| {
        CALLS.fetch_add(1, Ordering::SeqCst);
    }));

    // ① 取址（与 `scan_douyin_links` 里一致：只解一层）
    let user_data = (&raw mut *owned).cast::<std::ffi::c_void>();
    // ② 转回来并调用（与 `scan_trampoline` 里一致）
    let back = user_data as *const Box<MessageFn>;
    let handler = unsafe { &*back };
    handler(&Message {
        chat_id: 1,
        message_id: 1,
        text: "x".to_owned(),
        is_outgoing: false,
        sender_is_chat: false,
        is_forwarded: false,
        is_channel_post: false,
    });

    assert_eq!(CALLS.load(Ordering::SeqCst), 1, "回调必须真的调到闭包");
}

/// 登记入参形状必须和宿主 `register` 对得上：`name` 非空，`role` / `scopes`
/// 决定闸门与 `/help` 的可见性。
#[test]
fn registration_payload_has_the_expected_shape() {
    let input = registration_payload();
    assert_eq!(input["name"], "dy");
    assert!(
        input["description"].as_str().is_some_and(|d| !d.is_empty()),
        "说明不能为空，否则 /help 里会显示一条没有解释的命令"
    );
    assert_eq!(input["role"], "user");
    assert_eq!(
        input["scopes"],
        serde_json::json!(["private", "group", "channel"]),
        "频道里 /dy 要能用，且按编辑模式处理"
    );
}

/// 服务名是跨仓的**字符串约定**，改了它就等于换了协议，所以钉住。
#[test]
fn the_host_command_table_name_is_pinned() {
    assert_eq!(HOST_COMMANDS, "fuyubot.host.commands");
}

/// 软依赖而不是硬依赖：没有 SQL 插件时 `/dy` 照样要能用。
#[test]
fn sql_is_a_soft_dependency_not_a_hard_one() {
    assert_eq!(
        <DouyinPlugin as Plugin>::soft_inject(),
        vec![crate::cache::SERVICE.to_owned()],
        "只有 SQL 服务需要等；命令表由宿主预置"
    );
    assert!(
        <DouyinPlugin as Plugin>::inject().is_empty(),
        "不能是硬依赖：缺 SQL 时插件也要激活"
    );
    assert_eq!(crate::cache::SERVICE, "fuyubot.sqlite");
}
