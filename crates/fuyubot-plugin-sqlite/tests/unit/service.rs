use std::path::PathBuf;

use super::*;
use crate::config::{Config, Mode};

/// 测试临时目录：同 `db::tests::temp_dir` 的约定——放 `target/test-tmp`，
/// 不用系统 `TEMP`（MSYS / 受限环境里它可能是 `/tmp` 或干脆建不了目录）。
fn temp_dir(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("时钟")
        .as_nanos();
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("target/test-tmp/fuyubot-plugin-sqlite")
        .join(format!("svc-{tag}-{}-{nanos}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("建临时目录");
    dir
}

fn manager(tag: &str, mode: Mode) -> (Manager, PathBuf) {
    let dir = temp_dir(tag);
    let manager = Manager::new(Config::for_tests(dir.clone(), mode)).expect("Manager");
    (manager, dir)
}

fn call(manager: &Manager, method: &str, input: Value) -> Value {
    call_as(manager, None, method, input)
}

/// 带调用者身份的调用：`Some(..)` 模拟「某个插件在调」，`None` 模拟宿主自己调。
fn call_as(manager: &Manager, caller: Option<&str>, method: &str, input: Value) -> Value {
    let text = dispatch(manager, caller, method, &input.to_string());
    serde_json::from_str(&text).expect("返回值必须是 JSON")
}

#[test]
fn info_does_not_need_a_plugin() {
    let (manager, dir) = manager("info", Mode::PerPlugin);
    let body = call(&manager, "info", json!({}));
    assert_eq!(body["ok"], json!(true));
    assert_eq!(body["mode"], json!("per_plugin"));
    assert_eq!(body["service"], json!(SERVICE_NAME));
    assert!(body["templates"].as_array().unwrap().len() >= 5);
    drop(manager);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn missing_plugin_is_a_clear_error() {
    let (manager, dir) = manager("no-plugin", Mode::PerPlugin);
    let body = call(&manager, "query", json!({"sql": "SELECT 1"}));
    assert_eq!(body["ok"], json!(false));
    assert_eq!(body["error"]["code"], json!("bad_request"));
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("plugin")
    );
    drop(manager);
    let _ = std::fs::remove_dir_all(dir);
}

/// ABI v6：宿主给得出调用者时，`plugin` 可以省，服务用调用者自己的名字。
#[test]
fn caller_identity_fills_in_plugin() {
    let (manager, dir) = manager("caller", Mode::PerPlugin);
    let created = call_as(
        &manager,
        Some("demo.bot"),
        "execute",
        json!({"sql": "CREATE TABLE t (v TEXT)"}),
    );
    assert_eq!(created["ok"], json!(true));
    assert_eq!(created["plugin"], json!("demo.bot"));

    // 显式写自己（旧写法）照样能用。
    let body = call_as(
        &manager,
        Some("demo.bot"),
        "query",
        json!({"plugin": "demo.bot", "sql": "SELECT count(*) FROM t"}),
    );
    assert_eq!(body["rows"], json!([[0]]));
    drop(manager);
    let _ = std::fs::remove_dir_all(dir);
}

/// ABI v6：声明别人名字（v5 时代能冒用）现在被拒，错误码稳定。
#[test]
fn spoofing_another_plugin_is_rejected() {
    let (manager, dir) = manager("spoof", Mode::PerPlugin);
    let body = call_as(
        &manager,
        Some("attacker"),
        "query",
        json!({"plugin": "victim", "sql": "SELECT 1"}),
    );
    assert_eq!(body["ok"], json!(false));
    assert_eq!(body["error"]["code"], json!("caller_mismatch"));
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("attacker")
    );
    drop(manager);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn query_and_execute_round_trip() {
    let (manager, dir) = manager("roundtrip", Mode::PerPlugin);
    let created = call(
        &manager,
        "execute",
        json!({"plugin": "demo", "sql": "CREATE TABLE t (v TEXT)"}),
    );
    assert_eq!(created["ok"], json!(true));

    let inserted = call(
        &manager,
        "execute",
        json!({"plugin": "demo", "sql": "INSERT INTO t (v) VALUES (?1)", "params": ["hi"]}),
    );
    assert_eq!(inserted["changes"], json!(1));
    assert_eq!(inserted["last_insert_rowid"], json!(1));

    let body = call(
        &manager,
        "query",
        json!({"plugin": "demo", "sql": "SELECT v FROM t WHERE v = ?1", "params": ["hi"]}),
    );
    assert_eq!(body["rows"], json!([["hi"]]));
    assert_eq!(body["columns"], json!(["v"]));
    assert_eq!(body["truncated"], json!(false));

    drop(manager);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn stacked_statements_are_rejected_by_the_service() {
    let (manager, dir) = manager("stacked", Mode::PerPlugin);
    let body = call(
        &manager,
        "execute",
        json!({"plugin": "demo", "sql": "INSERT INTO t VALUES (1); DROP TABLE t"}),
    );
    assert_eq!(body["error"]["code"], json!("multiple_statements"));
    drop(manager);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn migrate_then_tables() {
    let (manager, dir) = manager("migrate", Mode::Shared);
    let body = call(
        &manager,
        "migrate",
        json!({"plugin": "demo.bot", "templates": ["kv", "log"]}),
    );
    assert_eq!(body["ok"], json!(true));
    assert_eq!(body["prefix"], json!("demo_bot_"));
    let applied = body["applied"].as_array().unwrap();
    assert_eq!(applied.len(), 2);
    assert_eq!(applied[0]["table"], json!("demo_bot_kv"));
    assert_eq!(applied[0]["created"], json!(true));

    let tables = call(&manager, "tables", json!({"plugin": "demo.bot"}));
    assert_eq!(tables["ok"], json!(true));
    let names: Vec<&str> = tables["tables"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|table| table["name"].as_str())
        .collect();
    assert_eq!(names, vec!["demo_bot_kv", "demo_bot_log"]);

    // 再跑一次是幂等的：表已经存在，created=false。
    let again = call(
        &manager,
        "migrate",
        json!({"plugin": "demo.bot", "tables": [{"template": "kv"}]}),
    );
    assert_eq!(again["applied"][0]["created"], json!(false));

    drop(manager);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn unknown_template_lists_the_available_ones() {
    let (manager, dir) = manager("unknown-template", Mode::PerPlugin);
    let body = call(
        &manager,
        "migrate",
        json!({"plugin": "demo", "templates": ["nope"]}),
    );
    assert_eq!(body["error"]["code"], json!("bad_request"));
    assert!(body["error"]["message"].as_str().unwrap().contains("kv"));
    drop(manager);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn batch_is_atomic_through_the_service() {
    let (manager, dir) = manager("svc-batch", Mode::PerPlugin);
    call(
        &manager,
        "execute",
        json!({"plugin": "demo", "sql": "CREATE TABLE b (v INTEGER)"}),
    );
    let body = call(
        &manager,
        "batch",
        json!({
            "plugin": "demo",
            "statements": [
                {"sql": "INSERT INTO b (v) VALUES (1)"},
                {"sql": "INSERT INTO b (v) VALUES (2)"}
            ]
        }),
    );
    assert_eq!(body["ok"], json!(true));
    assert_eq!(body["count"], json!(2));

    let count = call(
        &manager,
        "query",
        json!({"plugin": "demo", "sql": "SELECT count(*) FROM b"}),
    );
    assert_eq!(count["rows"], json!([[2]]));

    drop(manager);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn unknown_method_is_reported() {
    let (manager, dir) = manager("unknown-method", Mode::PerPlugin);
    let body = call(&manager, "nope", json!({}));
    assert_eq!(body["error"]["code"], json!("unknown_method"));
    drop(manager);
    let _ = std::fs::remove_dir_all(dir);
}
