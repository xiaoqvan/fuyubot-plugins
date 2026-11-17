use super::*;

#[test]
fn date_round_trip() {
    let ms = 1_767_225_600_000;
    let encoded = date(ms);
    assert_eq!(date_ms(&encoded), Some(ms));
    // TS 写入的 relaxed 形态
    assert_eq!(date_ms(&json!({"$date": "2026-01-01T00:00:00Z"})), Some(ms));
    // Extended JSON v1
    assert_eq!(date_ms(&json!({"$date": ms})), Some(ms));
}

#[test]
fn config_parses_legacy_fields() {
    let doc = json!({
        "chat_id": -100,
        "type": "group",
        "limit_mode": "ban",
        "Whitelist": [1, 2],
        "spam_fileId": ["a"],
        "spam_sticker": ["b"],
        "spam_tips": false,
        "spam_tips_meg_delay": 5,
        "ai_review_enabled": true,
        "check_new_member": true,
        "like_guard_enabled": true,
        "like_guard_mode": "ad",
        "delete_join_message": true,
        "delete_join_community_message": true,
        "delete_join_invite_message": true,
        "unpin_channel_pins": true,
        "ai_api_key": "k",
        "ai_base_url": "https://x",
        "ai_model": "m"
    });
    let config = GroupConfig::from_doc(&doc);
    assert_eq!(config.kind, "group");
    assert_eq!(config.limit_mode, LimitMode::Ban);
    assert_eq!(config.whitelist, vec![1, 2]);
    assert_eq!(config.spam_file_id, vec!["a"]);
    assert!(!config.spam_tips);
    assert_eq!(config.spam_tips_meg_delay, 5);
    assert!(config.ai_review_enabled);
    assert!(config.check_new_member);
    assert!(config.like_guard_enabled);
    assert_eq!(config.like_guard_mode, LikeGuardMode::Ad);
    assert!(config.delete_join_message);
    assert!(config.delete_join_community_message);
    assert!(config.delete_join_invite_message);
    assert!(config.unpin_channel_pins);
    assert!(config.has_custom_ai());
}

#[test]
fn config_defaults_when_fields_missing() {
    let config = GroupConfig::from_doc(&json!({"chat_id": 1}));
    assert!(config.spam_tips);
    assert_eq!(config.spam_tips_meg_delay, 30);
    assert_eq!(config.limit_mode, LimitMode::Mute);
    assert!(!config.ai_review_enabled);
    // 新功能默认关闭：必须由群管理员显式打开。
    assert!(!config.like_guard_enabled);
    // 默认只封禁回应权限，不跟随广告。
    assert_eq!(config.like_guard_mode, LikeGuardMode::Reactions);
    assert!(!config.delete_join_message);
    assert!(!config.delete_join_community_message);
    assert!(!config.delete_join_invite_message);
    assert!(!config.unpin_channel_pins);
}

#[test]
fn doc_id_reads_object_id() {
    assert_eq!(
        doc_id(&json!({"_id": {"$oid": "507f1f77bcf86cd799439011"}})),
        "507f1f77bcf86cd799439011"
    );
    assert_eq!(doc_id(&json!({"_id": "abc"})), "abc");
}

#[test]
fn watch_doc_parses() {
    let doc = json!({
        "chat_id": -100,
        "user_id": 7,
        "join_at": {"$date": "2026-01-01T00:00:00Z"},
        "expires_at": {"$date": "2026-01-02T00:00:00Z"},
        "join_name": "n",
        "join_bio": "b",
        "profile_changed": true,
        "recheck_count": 2,
        "clean_message_count": 3
    });
    let watch = NewMemberWatch::from_doc(&doc);
    assert_eq!(watch.user_id, 7);
    assert!(watch.profile_changed);
    assert_eq!(watch.recheck_count, 2);
    assert_eq!(watch.clean_message_count, 3);
    assert!(watch.expires_at > watch.join_at);
}

/// 老库里 `chat_id` / `Whitelist` 是 BSON Double（relaxed ExtJSON 回传成浮点），
/// 显示不能变成 0 或空表。
#[test]
fn legacy_double_config_is_read() {
    let doc = json!({
        "_id": {"$oid": "68f20a006979cd56b96f1f1e"},
        "chat_id": -1001913360090.0,
        "type": "group",
        "log_channel": -1002590596233i64,
        "limit_mode": "ban",
        "spam_tips": true,
        "spam_tips_meg_delay": 60,
        "Whitelist": [7999175161i64, 7339373390.0f64, 1346301375i64, 946437650i64, 7066074524.0f64, 8714724377.0f64, 884044095i64],
        "spam_sticker": ["6470646163040108543"],
        "ai_review_enabled": true,
        "check_new_member": true
    });
    let config = GroupConfig::from_doc(&doc);
    assert_eq!(
        config.whitelist,
        vec![
            7999175161, 7339373390, 1346301375, 946437650, 7066074524, 8714724377, 884044095
        ]
    );
    assert_eq!(config.log_channel, Some(-1002590596233));
    assert_eq!(config.limit_mode, LimitMode::Ban);
    assert_eq!(config.spam_tips_meg_delay, 60);
    // 整份回写时白名单必须原样带上，否则一次 /adconfig 就把人删了。
    let written = serde_json::to_value(&config).expect("可序列化");
    assert_eq!(written["Whitelist"].as_array().map(Vec::len), Some(7));
}

/// 封禁记录 / 禁言复审 / 入群监视的 id 在老库里也都是 Double。
#[test]
fn legacy_double_ids_are_read() {
    let ban = BanRecord::from_doc(&json!({
        "_id": {"$oid": "69c9855274b1254f5d9a258a"},
        "chat_id": -1001962243895.0,
        "target_tgid": 8486124627.0,
        "target_type": "messageSenderUser",
        "action": "ban",
        "spam_type": "text",
        "spam_detail": "包含面向交易的招揽信息并附带联系入口",
        "trigger_db_id": null,
        "createdAt": {"$date": "2026-03-29T20:02:26.051Z"}
    }));
    assert_eq!(ban.chat_id, -1001962243895);
    assert_eq!(ban.target_tgid, 8486124627);
    assert_eq!(ban.id, "69c9855274b1254f5d9a258a");
    assert_eq!(ban.created_at, 1_774_814_546_051);

    let review = MuteReview::from_doc(&json!({
        "chat_id": -1001913360090.0,
        "user_id": 6098704142.0,
        "message_id": 535348379648.0,
        "reason": "r",
        "confidence": 0.93,
        "muted_at": {"$date": "2026-09-06T19:49:29.099Z"},
        "deadline": {"$date": "2026-09-06T20:49:29.099Z"},
        "status": "pending",
        "updated_at": {"$date": "2026-09-06T20:49:29.870Z"}
    }));
    assert_eq!(review.chat_id, -1001913360090);
    assert_eq!(review.user_id, 6098704142);
    assert_eq!(review.message_id, Some(535348379648));

    let watch = NewMemberWatch::from_doc(&json!({
        "chat_id": -1002238597190.0,
        "user_id": 5113424213.0,
        "join_at": {"$date": "2026-11-11T16:04:25.227Z"},
        "expires_at": {"$date": "2026-11-12T16:04:25.227Z"},
        "join_name": "n",
        "join_bio": "b",
        "recheck_count": 0
    }));
    assert_eq!(watch.chat_id, -1002238597190);
    assert_eq!(watch.user_id, 5113424213);
}

#[test]
fn numbers_accept_float_and_extended_json() {
    assert_eq!(number_i64(&json!(-1001913360090.0)), Some(-1001913360090));
    assert_eq!(number_i64(&json!(42)), Some(42));
    assert_eq!(number_i64(&json!("42")), Some(42));
    assert_eq!(number_i64(&json!({"$numberLong": "42"})), Some(42));
    assert_eq!(number_i64(&json!({"$numberDouble": "42.0"})), Some(42));
    // 非整数 / 非数字不硬转
    assert_eq!(number_i64(&json!(0.93)), None);
    assert_eq!(number_i64(&json!("abc")), None);
    assert_eq!(number_i64(&Value::Null), None);
    // 2^53 之外 f64 已不精确，宁可当作读不到
    assert_eq!(
        number_i64(&json!(9_007_199_254_740_992.0)),
        Some(9_007_199_254_740_992)
    );
    assert_eq!(number_i64(&json!(9_007_199_254_740_994.0)), None);
    // 时间戳也有同样的问题
    assert_eq!(
        date_ms(&json!({"$date": 1_765_780_792_000.0})),
        Some(1_765_780_792_000)
    );
}
