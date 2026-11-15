use super::*;
use serde_json::json;

#[test]
fn channel_and_topic() {
    assert_eq!(channel_of(Some(json!(-100))), Some(-100));
    assert_eq!(channel_of(Some(json!("-100"))), Some(-100));
    assert_eq!(channel_of(None), None);
    assert_eq!(topic_of(Some(json!(42))), Some(42));
    // 超出 i32 的话题 id 当没配（TDLib 的 topic id 是 i32）。
    assert_eq!(topic_of(Some(json!(9_999_999_999_i64))), None);
}
