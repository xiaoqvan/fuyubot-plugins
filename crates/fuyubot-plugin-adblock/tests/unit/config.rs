use super::*;
use serde_json::json;

#[test]
fn chat_allowlist_scopes_the_plugin() {
    // 空列表 = 所有群（兼容旧配置）。
    let config = PluginConfig::default();
    assert!(config.chat_allowed(-100));
    assert!(config.chat_allowed(5_895_998_976));

    // 非空：只有列出的群生效；私聊始终放行（配置 / 管理命令）。
    let config = PluginConfig {
        chat_allowlist: vec![-100_111, -100_222],
        ..PluginConfig::default()
    };
    assert!(config.chat_allowed(-100_111));
    assert!(config.chat_allowed(-100_222));
    assert!(!config.chat_allowed(-100_333));
    assert!(
        config.chat_allowed(5_895_998_976),
        "私聊不受 chat_allowlist 限制"
    );
}

#[test]
fn channel_and_topic() {
    assert_eq!(channel_of(Some(json!(-100))), Some(-100));
    assert_eq!(channel_of(Some(json!("-100"))), Some(-100));
    assert_eq!(channel_of(None), None);
    assert_eq!(topic_of(Some(json!(42))), Some(42));
    // 超出 i32 的话题 id 当没配（TDLib 的 topic id 是 i32）。
    assert_eq!(topic_of(Some(json!(9_999_999_999_i64))), None);
}
