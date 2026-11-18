use super::*;

/// 一份「模型正常返回」的完整结果：严格校验要求 12 个字段齐全。
fn complete_result() -> Value {
    json!({
        "ad_detected": true,
        "confidence": 0.9,
        "Reason": "广告",
        "message": {"keywords": ["a"]},
        "quoted_message": {"has_ad": false, "keywords": []},
        "media": {"image_ad": false, "sticker_ad": false},
        "profile": {"name_ad": false, "bio_ad": false},
        "contact": {"contact_ad": false},
        "PersonalChannel": {"Channel_ad": false},
        "mentioned_username": {"username_ad": false},
        "link_Preview": {"title_ad": false, "description_ad": false}
    })
}

#[test]
fn ai_json_needs_core_fields() {
    assert!(ModerationResult::from_ai_json(&json!({"Reason": "x"})).is_none());
    // 少一个嵌套字段 = 格式不符 → 让上层重试（TS `isValidModerationResult` 同款）。
    let mut partial = complete_result();
    partial.as_object_mut().expect("对象").remove("profile");
    assert!(ModerationResult::from_ai_json(&partial).is_none());

    let result = ModerationResult::from_ai_json(&complete_result()).expect("合法结果");
    assert!(result.ad_detected);
    assert_eq!(result.message.keywords, vec!["a"]);
}

#[test]
fn quoted_ad_forces_detection() {
    let mut value = complete_result();
    value["ad_detected"] = json!(false);
    value["confidence"] = json!(0.5);
    value["quoted_message"] = json!({"has_ad": true, "keywords": []});
    let result = ModerationResult::from_ai_json(&value).unwrap();
    assert!(result.ad_detected);
}

#[test]
fn contact_ad_alone_counts_as_ad() {
    // 联系人消息只置 contact_ad（正文为空、ad_detected=false）也必须算广告，
    // 否则会出现「AI 判了、Bot 不处置」—— 这是真机上漏拦截联系人卡片的直接原因。
    let mut value = complete_result();
    value["ad_detected"] = json!(false);
    value["Reason"] = json!("联系人名称含获利与联系方式");
    value["contact"] = json!({ "contact_ad": true });
    let result = ModerationResult::from_ai_json(&value).expect("合法结果");
    assert!(!result.ad_detected);
    assert!(result.contact.contact_ad);
    assert!(result.is_ad());

    // 三个标记都为 false 才算「不是广告」。
    let mut clean = complete_result();
    clean["ad_detected"] = json!(false);
    let clean = ModerationResult::from_ai_json(&clean).expect("合法结果");
    assert!(!clean.is_ad());
}

#[test]
fn failure_reasons_are_recognized() {
    assert!(is_failure_reason("AI服务3次调用均失败: timeout"));
    assert!(is_failure_reason("该群未配置AI接口"));
    assert!(!is_failure_reason("就是普通聊天"));
}

#[test]
fn masking_keeps_edges() {
    assert_eq!(mask_secret("sk-1234567890abcd"), "sk-1***abcd");
    assert_eq!(mask_secret("short"), "sh***");
    assert_eq!(mask_secret(""), "");
}

#[test]
fn endpoints_compare_by_connection() {
    let endpoint = |key: &str, url: &str, model: &str| ResolvedAi {
        api_key: key.to_owned(),
        base_url: url.to_owned(),
        model: model.to_owned(),
        source: AiSource::Default,
    };
    let left = endpoint("sk", "https://a", "m");
    assert!(same_endpoint(&left, &endpoint("sk", "https://a", "m")));
    assert!(!same_endpoint(&left, &endpoint("sk", "https://b", "m")));
    assert!(!same_endpoint(&left, &endpoint("sk", "https://a", "n")));
}

#[test]
fn temperature_rule() {
    assert!(forbids_custom_temperature("gpt-5.6-luna"));
    assert!(forbids_custom_temperature("o3-mini"));
    assert!(!forbids_custom_temperature("gpt-4o-mini"));
    assert!(!forbids_custom_temperature("deepseek-chat"));
}

#[test]
fn base_url_is_trimmed() {
    assert_eq!(
        trim_base_url("https://api.openai.com/v1/".to_owned()),
        "https://api.openai.com/v1"
    );
}

#[test]
fn backup_is_only_for_builtin_default_interface() {
    let endpoint = |source| ResolvedAi {
        api_key: "k".to_owned(),
        base_url: "https://x".to_owned(),
        model: "m".to_owned(),
        source,
    };
    // 群自定义接口：无论是否 force_default，都不许用内置备用模型。
    assert!(!may_use_backup(&endpoint(AiSource::Chat), false));
    assert!(!may_use_backup(&endpoint(AiSource::Chat), true));
    // 内置默认接口：主接口失败 / token 超限可以切备用。
    assert!(may_use_backup(&endpoint(AiSource::Default), false));
    // 完全没有可用接口：只有强制默认（/spam 等）的场景才让备用顶上。
    assert!(!may_use_backup(&ResolvedAi::unusable(), false));
    assert!(may_use_backup(&ResolvedAi::unusable(), true));
}

#[test]
fn failure_notification_is_throttled_per_chat() {
    // 独立 fake chat id，避免和其他用例共享全局节流表。
    let chat_id = -9_999_999_001;
    assert!(should_notify_ai_failure(chat_id));
    assert!(!should_notify_ai_failure(chat_id), "10 分钟内只提醒一次");
    reset_ai_failure_notify(chat_id);
    assert!(should_notify_ai_failure(chat_id), "重新配置后应立刻可通知");
}
