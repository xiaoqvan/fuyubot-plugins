use super::*;

#[test]
fn detail_prefers_record_id() {
    let hit = SpamHit::hit("Text", Some("abc".to_owned()), "hasAdKeyword");
    let detail = match &hit.id {
        Some(id) => format!("\n - Type: {}\n - ID: `{id}`", hit.kind),
        None => format!("\n - Type: {}\n - Reason: {}", hit.kind, hit.details),
    };
    assert!(detail.contains("ID: `abc`"));
    assert!(!detail.contains("Reason"));
}
