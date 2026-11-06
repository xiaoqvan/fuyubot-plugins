//! JSON 服务协议：`fuyubot.sqlite` 的 `info / query / execute / batch / templates /
//! migrate / tables`。
//!
//! 所有方法都是「JSON 进、JSON 出」。成功带 `"ok":true`，失败统一是：
//!
//! ```json
//! {"ok":false,"error":{"code":"denied_sql","message":"…"}}
//! ```
//!
//! `code` 是稳定的（写分支用），`message` 是给人看的。
//!
//! **调用方必须显式给 `plugin`**：ABI 的 `ServiceMethodFn` 只有
//! `(context, input_json)`，没有「调用者是谁」——宿主不传身份，服务也不会去猜。
//! 所以隔离是**协作式**的：插件名由调用方声明；同进程的原生插件本来就能
//! 绕过任何边界，这里的目标是防「数据注入」，不是防「恶意插件」。

use std::sync::MutexGuard;

use rusqlite::Connection;
use serde_json::{Value, json};

use crate::db::{self, Manager};
use crate::error::ServiceError;
use crate::sql;
use crate::templates::{self, Migration};

/// 服务名（与插件名一致）。
pub const SERVICE_NAME: &str = "fuyubot.sqlite";

/// 服务方法名。顺序就是注册到 `ServiceVTable` 里的顺序。
pub const METHODS: &[&str] = &[
    "info",
    "query",
    "execute",
    "batch",
    "templates",
    "migrate",
    "tables",
];

/// 分发一次调用，返回 JSON 字符串（永远不 panic、永远返回合法 JSON）。
pub fn dispatch(manager: &Manager, method: &str, input: &str) -> String {
    let result = match method {
        "info" => info(manager, input),
        "query" => query(manager, input),
        "execute" => execute(manager, input),
        "batch" => batch(manager, input),
        "templates" => list_templates(input),
        "migrate" => migrate(manager, input),
        "tables" => tables(manager, input),
        other => Err(ServiceError::new(
            "unknown_method",
            format!("没有方法 {other:?}（可用：{}）", METHODS.join(", ")),
        )),
    };
    match result {
        Ok(value) => value.to_string(),
        Err(error) => error_json(&error),
    }
}

/// 统一的错误 JSON。
pub fn error_json(error: &ServiceError) -> String {
    json!({
        "ok": false,
        "error": { "code": error.code, "message": error.message },
    })
    .to_string()
}

fn info(manager: &Manager, input: &str) -> Result<Value, ServiceError> {
    let request = parse_input(input)?;
    let config = manager.config();
    let mut body = json!({
        "ok": true,
        "service": SERVICE_NAME,
        "version": env!("CARGO_PKG_VERSION"),
        "abi": fuyubot_abi::ABI_VERSION,
        "mode": config.mode.as_str(),
        "dir": config.dir.display().to_string(),
        "shared_file": config.file,
        "table_prefix": config.table_prefix,
        "read_only": config.read_only,
        "limits": {
            "max_rows": config.max_rows,
            "max_sql_bytes": config.max_sql_bytes,
            "max_batch": config.max_batch,
            "max_result_bytes": config.max_result_bytes,
            "busy_timeout_ms": config.busy_timeout_ms,
        },
        "sqlite": rusqlite::version(),
        "templates": templates::template_names(),
    });

    if let Some(plugin) = optional_plugin(&request)? {
        let (path, prefix) = manager.describe(&plugin);
        body["plugin"] = json!(plugin);
        body["database"] = json!(path.display().to_string());
        body["prefix"] = json!(prefix);
    }
    Ok(body)
}

fn query(manager: &Manager, input: &str) -> Result<Value, ServiceError> {
    let request = parse_input(input)?;
    let plugin = require_plugin(&request)?;
    let sql_text = require_str(&request, "sql")?;
    let statement = sql::ensure_single(sql_text, manager.config().max_sql_bytes)?;
    let params = db::params_from_json(request.get("params"))?;
    let limit = request
        .get("limit")
        .map(|value| {
            value
                .as_u64()
                .filter(|limit| *limit > 0)
                .map(|limit| limit as usize)
                .ok_or_else(|| ServiceError::bad_request("limit 必须是正整数"))
        })
        .transpose()?
        .unwrap_or(manager.config().max_rows)
        .min(manager.config().max_rows);

    let resolved = manager.resolve(&plugin)?;
    let connection = lock(&resolved.connection);
    let outcome = db::query(&connection, statement, &params, limit)?;
    let row_count = outcome.rows.len();
    let body = json!({
        "ok": true,
        "plugin": resolved.plugin,
        "database": resolved.path.display().to_string(),
        "columns": outcome.columns,
        "rows": outcome.rows,
        "row_count": row_count,
        "truncated": outcome.truncated,
    });
    enforce_size(&body, manager.config().max_result_bytes)?;
    Ok(body)
}

fn execute(manager: &Manager, input: &str) -> Result<Value, ServiceError> {
    let request = parse_input(input)?;
    let plugin = require_plugin(&request)?;
    let sql_text = require_str(&request, "sql")?;
    let statement = sql::ensure_single(sql_text, manager.config().max_sql_bytes)?;
    let params = db::params_from_json(request.get("params"))?;

    let resolved = manager.resolve(&plugin)?;
    let connection = lock(&resolved.connection);
    let outcome = db::execute(&connection, statement, &params)?;
    Ok(json!({
        "ok": true,
        "plugin": resolved.plugin,
        "database": resolved.path.display().to_string(),
        "changes": outcome.changes,
        "last_insert_rowid": outcome.last_insert_rowid,
    }))
}

fn batch(manager: &Manager, input: &str) -> Result<Value, ServiceError> {
    let request = parse_input(input)?;
    let plugin = require_plugin(&request)?;
    let list = request
        .get("statements")
        .and_then(Value::as_array)
        .ok_or_else(|| ServiceError::bad_request("缺少 statements 数组"))?;
    if list.is_empty() {
        return Err(ServiceError::bad_request("statements 不能为空"));
    }
    if list.len() > manager.config().max_batch {
        return Err(ServiceError::bad_request(format!(
            "一次最多 {} 条语句，这里是 {}",
            manager.config().max_batch,
            list.len()
        )));
    }

    let mut statements = Vec::with_capacity(list.len());
    for (index, item) in list.iter().enumerate() {
        let object = item
            .as_object()
            .ok_or_else(|| ServiceError::bad_request(format!("statements[{index}] 必须是对象")))?;
        let sql_text = object
            .get("sql")
            .and_then(Value::as_str)
            .filter(|sql| !sql.trim().is_empty())
            .ok_or_else(|| {
                ServiceError::bad_request(format!("statements[{index}].sql 必须是非空字符串"))
            })?;
        let statement =
            sql::ensure_single(sql_text, manager.config().max_sql_bytes).map_err(|error| {
                ServiceError::new(
                    error.code,
                    format!("statements[{index}]: {}", error.message),
                )
            })?;
        let params = db::params_from_json(object.get("params"))?;
        statements.push((statement.to_owned(), params));
    }

    let resolved = manager.resolve(&plugin)?;
    let mut connection = lock(&resolved.connection);
    let outcomes = db::batch(&mut connection, &statements)?;
    let results: Vec<Value> = outcomes
        .iter()
        .map(|outcome| {
            json!({
                "changes": outcome.changes,
                "last_insert_rowid": outcome.last_insert_rowid,
            })
        })
        .collect();
    Ok(json!({
        "ok": true,
        "plugin": resolved.plugin,
        "database": resolved.path.display().to_string(),
        "count": results.len(),
        "results": results,
    }))
}

fn list_templates(input: &str) -> Result<Value, ServiceError> {
    let _ = parse_input(input)?;
    let list: Vec<Value> = templates::TEMPLATES
        .iter()
        .map(|template| {
            json!({
                "name": template.name,
                "description": template.description,
                "table": template.table,
                "statements": template.statements,
            })
        })
        .collect();
    Ok(json!({
        "ok": true,
        "placeholders": ["{table}", "{name}"],
        "templates": list,
    }))
}

fn migrate(manager: &Manager, input: &str) -> Result<Value, ServiceError> {
    let request = parse_input(input)?;
    let plugin = require_plugin(&request)?;

    let mut migrations = Vec::new();
    if let Some(names) = request.get("templates") {
        let names = names
            .as_array()
            .ok_or_else(|| ServiceError::bad_request("templates 必须是数组"))?;
        for name in names {
            let name = name
                .as_str()
                .ok_or_else(|| ServiceError::bad_request("templates 的元素必须是字符串"))?;
            migrations.push(Migration {
                template: name.to_owned(),
                table: None,
            });
        }
    }
    if let Some(entries) = request.get("tables") {
        let entries = entries
            .as_array()
            .ok_or_else(|| ServiceError::bad_request("tables 必须是数组"))?;
        for (index, entry) in entries.iter().enumerate() {
            let template = entry
                .get("template")
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    ServiceError::bad_request(format!("tables[{index}].template 必须是非空字符串"))
                })?;
            let table = match entry.get("table") {
                None | Some(Value::Null) => None,
                Some(Value::String(table)) => {
                    sql::validate_identifier(table)?;
                    Some(table.clone())
                }
                Some(_) => {
                    return Err(ServiceError::bad_request(format!(
                        "tables[{index}].table 必须是字符串"
                    )));
                }
            };
            migrations.push(Migration {
                template: template.to_owned(),
                table,
            });
        }
    }
    if migrations.is_empty() {
        return Err(ServiceError::bad_request(
            "templates 或 tables 至少给一个（先用 templates 方法看有哪些模板）",
        ));
    }
    if migrations.len() > manager.config().max_batch {
        return Err(ServiceError::bad_request(format!(
            "一次最多 {} 个模板，这里是 {}",
            manager.config().max_batch,
            migrations.len()
        )));
    }

    let resolved = manager.resolve(&plugin)?;
    let mut connection = lock(&resolved.connection);
    let applied = templates::apply(&mut connection, &resolved.prefix, &migrations)?;
    Ok(json!({
        "ok": true,
        "plugin": resolved.plugin,
        "database": resolved.path.display().to_string(),
        "prefix": resolved.prefix,
        "applied": applied.iter().map(|applied| applied.to_json()).collect::<Vec<_>>(),
    }))
}

fn tables(manager: &Manager, input: &str) -> Result<Value, ServiceError> {
    let request = parse_input(input)?;
    let plugin = require_plugin(&request)?;
    let resolved = manager.resolve(&plugin)?;
    let connection = lock(&resolved.connection);
    let tables = db::list_tables(&connection, &resolved.prefix)?;
    let body = json!({
        "ok": true,
        "plugin": resolved.plugin,
        "database": resolved.path.display().to_string(),
        "prefix": resolved.prefix,
        "tables": tables,
    });
    enforce_size(&body, manager.config().max_result_bytes)?;
    Ok(body)
}

fn parse_input(input: &str) -> Result<Value, ServiceError> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Ok(json!({}));
    }
    let value: Value = serde_json::from_str(trimmed)
        .map_err(|error| ServiceError::bad_request(format!("入参不是合法 JSON：{error}")))?;
    if !value.is_object() {
        return Err(ServiceError::bad_request("入参必须是 JSON 对象"));
    }
    Ok(value)
}

fn require_plugin(request: &Value) -> Result<String, ServiceError> {
    optional_plugin(request)?.ok_or_else(|| {
        ServiceError::bad_request(
            "缺少 plugin 字段：ABI 不传调用者身份，请显式写自己的插件名（如 \"fuyubot.manage\"）",
        )
    })
}

fn optional_plugin(request: &Value) -> Result<Option<String>, ServiceError> {
    match request.get("plugin") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(plugin)) if !plugin.trim().is_empty() => {
            let plugin = plugin.trim();
            sql::validate_plugin_name(plugin)?;
            Ok(Some(plugin.to_owned()))
        }
        Some(Value::String(_)) => Err(ServiceError::bad_request("plugin 不能是空字符串")),
        Some(_) => Err(ServiceError::bad_request("plugin 必须是字符串")),
    }
}

fn require_str<'a>(request: &'a Value, key: &str) -> Result<&'a str, ServiceError> {
    request
        .get(key)
        .and_then(Value::as_str)
        .filter(|text| !text.trim().is_empty())
        .ok_or_else(|| ServiceError::bad_request(format!("缺少 {key}（非空字符串）")))
}

fn lock(connection: &std::sync::Mutex<Connection>) -> MutexGuard<'_, Connection> {
    connection
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn enforce_size(body: &Value, max: usize) -> Result<(), ServiceError> {
    let size = serde_json::to_string(body)
        .map(|text| text.len())
        .unwrap_or(usize::MAX);
    if size > max {
        return Err(ServiceError::new(
            "result_too_large",
            format!(
                "结果约 {size} 字节，超过上限 {max}；请加 LIMIT、减少列或调大 max_result_bytes"
            ),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
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
        let text = dispatch(manager, method, &input.to_string());
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
}
