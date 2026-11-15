use super::api::call;
use mongodb::bson::{Bson, Document};
use serde_json::json;

/// JSON ↔ BSON 的转换逻辑不依赖数据库，单独验证。
#[test]
fn extended_json_is_accepted_for_filters() {
    // 这里走的是 api 内部同一套 serde_json → Document 转换。
    let value = json!({ "_id": { "$oid": "507f1f77bcf86cd799439011" }, "id": 42 });
    let doc: Document = serde_json::from_value(value).unwrap();
    assert!(matches!(doc.get("_id"), Some(Bson::ObjectId(_))));
    assert_eq!(doc.get_i32("id").unwrap(), 42);
}

#[test]
fn unconfigured_calls_fail_with_a_clear_message() {
    // 没调 configure 时不能 panic，要给业务侧可读的错误。
    let err = call("find", json!({ "collection": "anime" })).unwrap_err();
    assert!(err.contains("未初始化") || err.contains("未就绪"), "{err}");
}

#[test]
fn unknown_method_is_rejected() {
    let err = call("drop_everything", json!({})).unwrap_err();
    assert!(err.contains("未知方法"), "{err}");
}
