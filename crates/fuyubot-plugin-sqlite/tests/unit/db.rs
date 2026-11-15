use std::path::PathBuf;

use serde_json::json;

use super::*;
use crate::config::Mode;

/// 测试临时目录：仓库约定放 `target/test-tmp`，**不用系统 `TEMP`**。
///
/// `std::env::temp_dir()` 在 MSYS（git bash）里会变成 `/tmp`，在受限环境里可能
/// 指向一个建不了目录的地方（`OS error 5`）——那样一批测试会莫名全红。
/// 落在 `target/` 下跟 `cargo clean` 一起清掉，也不会污染系统临时目录。
fn temp_dir(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("时钟")
        .as_nanos();
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("target/test-tmp/fuyubot-plugin-sqlite")
        .join(format!("{tag}-{}-{nanos}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("建临时目录");
    dir
}

fn manager(tag: &str, mode: Mode) -> (Manager, PathBuf) {
    let dir = temp_dir(tag);
    let manager = Manager::new(Config::for_tests(dir.clone(), mode)).expect("建 Manager");
    (manager, dir)
}

fn run(
    manager: &Manager,
    plugin: &str,
    sql: &str,
    params: &Value,
) -> Result<ExecOutcome, ServiceError> {
    let resolved = manager.resolve(plugin)?;
    let connection = resolved
        .connection
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    execute(&connection, sql, &params_from_json(Some(params))?)
}

fn select(
    manager: &Manager,
    plugin: &str,
    sql: &str,
    params: &Value,
) -> Result<QueryOutcome, ServiceError> {
    let resolved = manager.resolve(plugin)?;
    let connection = resolved
        .connection
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    query(
        &connection,
        sql,
        &params_from_json(Some(params))?,
        manager.config().max_rows,
    )
}

#[test]
fn parameters_are_never_interpreted() {
    let (manager, dir) = manager("injection", Mode::PerPlugin);
    run(
        &manager,
        "demo",
        "CREATE TABLE t (name TEXT NOT NULL)",
        &json!([]),
    )
    .expect("建表");
    let payload = "x'); DROP TABLE t; --";
    run(
        &manager,
        "demo",
        "INSERT INTO t (name) VALUES (?1)",
        &json!([payload]),
    )
    .expect("插入");

    let found = select(
        &manager,
        "demo",
        "SELECT name FROM t WHERE name = ?1",
        &json!([payload]),
    )
    .expect("查询");
    assert_eq!(found.rows, vec![json!([payload])]);

    // 注入串没有把表删掉。
    let count = select(&manager, "demo", "SELECT count(*) FROM t", &json!([])).expect("计数");
    assert_eq!(count.rows, vec![json!([1])]);

    drop(manager);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn classic_or_injection_does_not_match() {
    let (manager, dir) = manager("or-injection", Mode::PerPlugin);
    run(
        &manager,
        "demo",
        "CREATE TABLE users (name TEXT)",
        &json!([]),
    )
    .unwrap();
    run(
        &manager,
        "demo",
        "INSERT INTO users (name) VALUES ('admin')",
        &json!([]),
    )
    .unwrap();
    let rows = select(
        &manager,
        "demo",
        "SELECT name FROM users WHERE name = ?1",
        &json!(["' OR '1'='1"]),
    )
    .unwrap();
    assert!(
        rows.rows.is_empty(),
        "注入串不该匹配任何行：{:?}",
        rows.rows
    );

    drop(manager);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn per_plugin_databases_are_isolated() {
    let (manager, dir) = manager("isolation", Mode::PerPlugin);
    for plugin in ["alpha", "beta"] {
        run(
            &manager,
            plugin,
            "CREATE TABLE t (v INTEGER NOT NULL)",
            &json!([]),
        )
        .unwrap();
        run(
            &manager,
            plugin,
            "INSERT INTO t (v) VALUES (?1)",
            &json!([1]),
        )
        .unwrap();
    }
    run(
        &manager,
        "alpha",
        "INSERT INTO t (v) VALUES (2)",
        &json!([]),
    )
    .unwrap();

    let alpha = select(&manager, "alpha", "SELECT v FROM t ORDER BY v", &json!([])).unwrap();
    let beta = select(&manager, "beta", "SELECT v FROM t ORDER BY v", &json!([])).unwrap();
    assert_eq!(alpha.rows, vec![json!([1]), json!([2])]);
    assert_eq!(beta.rows, vec![json!([1])]);
    assert_ne!(
        manager.describe("alpha").0,
        manager.describe("beta").0,
        "两个插件不该共用文件"
    );

    drop(manager);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn shared_mode_applies_the_plugin_prefix() {
    let (manager, dir) = manager("shared", Mode::Shared);
    let prefix = manager.config().prefix_for("demo.bot");
    assert_eq!(prefix, "demo_bot_");

    let applied = {
        let resolved = manager.resolve("demo.bot").unwrap();
        let mut connection = resolved
            .connection
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        crate::templates::apply(
            &mut connection,
            &prefix,
            &[crate::templates::Migration {
                template: "kv".to_owned(),
                table: None,
            }],
        )
        .unwrap()
    };
    assert_eq!(applied[0].table, "demo_bot_kv");

    run(
        &manager,
        "demo.bot",
        "INSERT INTO demo_bot_kv (key, value) VALUES (?1, ?2)",
        &json!(["k", "v"]),
    )
    .unwrap();

    // 另一个插件在同一个库里建同名模板，表名被前缀区分开。
    let other_prefix = manager.config().prefix_for("other");
    {
        let resolved = manager.resolve("other").unwrap();
        let mut connection = resolved
            .connection
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        crate::templates::apply(
            &mut connection,
            &other_prefix,
            &[crate::templates::Migration {
                template: "kv".to_owned(),
                table: None,
            }],
        )
        .unwrap();
    }
    let rows = select(
        &manager,
        "other",
        "SELECT count(*) FROM other_kv",
        &json!([]),
    )
    .unwrap();
    assert_eq!(rows.rows, vec![json!([0])]);

    let tables = {
        let resolved = manager.resolve("demo.bot").unwrap();
        let connection = resolved
            .connection
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        list_tables(&connection, &prefix).unwrap()
    };
    let names: Vec<&str> = tables
        .iter()
        .filter_map(|table| table.get("name").and_then(Value::as_str))
        .collect();
    assert_eq!(names, vec!["demo_bot_kv"]);

    drop(manager);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn batch_rolls_back_on_error() {
    let (manager, dir) = manager("batch", Mode::PerPlugin);
    run(
        &manager,
        "demo",
        "CREATE TABLE b (v INTEGER NOT NULL)",
        &json!([]),
    )
    .unwrap();
    let resolved = manager.resolve("demo").unwrap();
    let mut connection = resolved
        .connection
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let statements = vec![
        (
            "INSERT INTO b (v) VALUES (?1)".to_owned(),
            params_from_json(Some(&json!([1]))).unwrap(),
        ),
        (
            "INSERT INTO missing_table (v) VALUES (1)".to_owned(),
            Params::None,
        ),
    ];
    assert!(batch(&mut connection, &statements).is_err());
    drop(connection);

    let count = select(&manager, "demo", "SELECT count(*) FROM b", &json!([])).unwrap();
    assert_eq!(count.rows, vec![json!([0])], "失败的 batch 必须整体回滚");

    drop(manager);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn query_rejects_writes() {
    let (manager, dir) = manager("readonly", Mode::PerPlugin);
    run(&manager, "demo", "CREATE TABLE q (v INTEGER)", &json!([])).unwrap();
    let error = select(&manager, "demo", "DELETE FROM q", &json!([])).unwrap_err();
    assert_eq!(error.code, "not_readonly");

    drop(manager);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn parameter_counts_are_strict() {
    let (manager, dir) = manager("params", Mode::PerPlugin);
    run(&manager, "demo", "CREATE TABLE p (v INTEGER)", &json!([])).unwrap();
    let error = select(&manager, "demo", "SELECT ?1", &json!([])).unwrap_err();
    assert_eq!(error.code, "param_count");

    let error = select(&manager, "demo", "SELECT ?1", &json!([1, 2])).unwrap_err();
    assert_eq!(error.code, "param_count");
    drop(manager);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn named_parameters_work() {
    let (manager, dir) = manager("named", Mode::PerPlugin);
    run(&manager, "demo", "CREATE TABLE n (v TEXT)", &json!([])).unwrap();
    run(
        &manager,
        "demo",
        "INSERT INTO n (v) VALUES (:value)",
        &json!({"value": "hello"}),
    )
    .unwrap();
    let rows = select(
        &manager,
        "demo",
        "SELECT v FROM n WHERE v = :value",
        &json!({":value": "hello"}),
    )
    .unwrap();
    assert_eq!(rows.rows, vec![json!(["hello"])]);
    drop(manager);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn blobs_round_trip_as_hex() {
    let (manager, dir) = manager("blob", Mode::PerPlugin);
    run(&manager, "demo", "CREATE TABLE b (v BLOB)", &json!([])).unwrap();
    run(
        &manager,
        "demo",
        "INSERT INTO b (v) VALUES (?1)",
        &json!([[1, 2, 255]]),
    )
    .unwrap();
    let rows = select(&manager, "demo", "SELECT v FROM b", &json!([])).unwrap();
    assert_eq!(rows.rows, vec![json!([{"$blob": "0102ff"}])]);
    drop(manager);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn row_limit_truncates() {
    let (manager, dir) = manager("limit", Mode::PerPlugin);
    run(&manager, "demo", "CREATE TABLE l (v INTEGER)", &json!([])).unwrap();
    for value in 0..5 {
        run(
            &manager,
            "demo",
            "INSERT INTO l (v) VALUES (?1)",
            &json!([value]),
        )
        .unwrap();
    }
    let rows = {
        let resolved = manager.resolve("demo").unwrap();
        let connection = resolved
            .connection
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        query(&connection, "SELECT v FROM l ORDER BY v", &Params::None, 2).unwrap()
    };
    assert_eq!(rows.rows.len(), 2);
    assert!(rows.truncated);

    drop(manager);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn read_only_connections_can_read_but_not_write() {
    let dir = temp_dir("read-only");
    {
        let manager = Manager::new(Config::for_tests(dir.clone(), Mode::PerPlugin)).unwrap();
        run(
            &manager,
            "demo",
            "CREATE TABLE r (v INTEGER NOT NULL)",
            &json!([]),
        )
        .unwrap();
        run(
            &manager,
            "demo",
            "INSERT INTO r (v) VALUES (?1)",
            &json!([7]),
        )
        .unwrap();
    }

    let mut config = Config::for_tests(dir.clone(), Mode::PerPlugin);
    config.read_only = true;
    let manager = Manager::new(config).unwrap();
    let rows = select(&manager, "demo", "SELECT v FROM r", &json!([])).unwrap();
    assert_eq!(rows.rows, vec![json!([7])]);
    let error = run(&manager, "demo", "INSERT INTO r (v) VALUES (8)", &json!([])).unwrap_err();
    assert_eq!(error.code, "read_only", "{error}");

    drop(manager);
    let _ = std::fs::remove_dir_all(dir);
}
