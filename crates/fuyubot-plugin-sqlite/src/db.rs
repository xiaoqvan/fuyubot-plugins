//! 连接管理与 SQL 执行。
//!
//! 每个数据库文件一个 [`Connection`]，外面套 `Mutex`：SQLite 的连接本身不是
//! 为并发写设计的，而插件可能从任意线程调服务。串行化的代价是同一时刻只有一条
//! 语句在跑，换来的是「事务不会跨插件串台」。
//!
//! 安全相关的三件事都在这里落地：
//! 1. **参数绑定**（[`Params`]）：值走 `sqlite3_bind_*`，永远不拼进 SQL；
//! 2. **连接加固**：`defensive=ON`、`trusted_schema=OFF`、关闭双引号字符串兼容；
//! 3. **标识符校验**（[`crate::sql`]）：模板表名只允许 `[A-Za-z0-9_]`。

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rusqlite::config::DbConfig;
use rusqlite::types::{Value as SqliteValue, ValueRef};
use rusqlite::{Connection, OpenFlags, Statement, TransactionBehavior};
use serde_json::{Value, json};

use crate::config::{Config, Mode};
use crate::error::ServiceError;
use crate::sql;

/// 进程内所有数据库的持有者。
///
/// 它被 `Arc` 包着放进插件的全局状态：服务调用先克隆 `Arc` 再释放全局锁，
/// 这样「取管理器」和「执行 SQL」不会互相阻塞。
pub struct Manager {
    config: Config,
    connections: Mutex<HashMap<String, Arc<Mutex<Connection>>>>,
}

/// 一次调用解析出的目标：连接 + 路径 + 该调用方的表前缀。
pub struct Resolved {
    pub plugin: String,
    pub path: PathBuf,
    pub prefix: String,
    pub connection: Arc<Mutex<Connection>>,
}

impl Manager {
    pub fn new(config: Config) -> Result<Self, ServiceError> {
        if config.read_only {
            if !config.dir.is_dir() {
                return Err(ServiceError::new(
                    "io_error",
                    format!("数据库目录不存在：{}", config.dir.display()),
                ));
            }
        } else {
            std::fs::create_dir_all(&config.dir)?;
        }
        Ok(Self {
            config,
            connections: Mutex::new(HashMap::new()),
        })
    }

    pub fn config(&self) -> &Config {
        &self.config
    }

    /// 目标库路径 + 表前缀（不打开连接，`info` 用）。
    pub fn describe(&self, plugin: &str) -> (PathBuf, String) {
        (
            self.config.database_path(plugin),
            self.config.prefix_for(plugin),
        )
    }

    /// 解析调用方要用的库；第一次访问时打开连接。
    pub fn resolve(&self, plugin: &str) -> Result<Resolved, ServiceError> {
        sql::validate_plugin_name(plugin)?;
        let path = self.config.database_path(plugin);
        let key = match self.config.mode {
            Mode::Shared => self.config.file.clone(),
            Mode::PerPlugin => plugin.to_owned(),
        };

        let mut connections = self
            .connections
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let connection = match connections.get(&key) {
            Some(existing) => Arc::clone(existing),
            None => {
                let connection = Arc::new(Mutex::new(open_connection(&path, &self.config)?));
                connections.insert(key, Arc::clone(&connection));
                connection
            }
        };

        Ok(Resolved {
            plugin: plugin.to_owned(),
            path,
            prefix: self.config.prefix_for(plugin),
            connection,
        })
    }
}

/// 打开并加固一个连接。
fn open_connection(path: &Path, config: &Config) -> Result<Connection, ServiceError> {
    let flags = if config.read_only {
        OpenFlags::SQLITE_OPEN_READ_ONLY
    } else {
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE
    };
    let connection = Connection::open_with_flags(path, flags).map_err(|error| {
        ServiceError::new(
            "cannot_open",
            format!("打开 {} 失败：{error}", path.display()),
        )
    })?;

    connection.busy_timeout(Duration::from_millis(config.busy_timeout_ms))?;

    if !config.read_only {
        // journal_mode 必须在 defensive 之前设：defensive 会拒绝一些写库参数。
        connection.query_row(config.journal_mode.pragma(), [], |row| {
            row.get::<_, String>(0)
        })?;
        connection.execute_batch("PRAGMA synchronous=NORMAL")?;
    }
    connection.execute_batch("PRAGMA foreign_keys=ON")?;

    // 防「恶意 schema」：视图/触发器里不能调用有副作用的东西。
    connection.set_db_config(DbConfig::SQLITE_DBCONFIG_DEFENSIVE, true)?;
    connection.set_db_config(DbConfig::SQLITE_DBCONFIG_TRUSTED_SCHEMA, false)?;
    // 关掉「双引号找不到列就退回字符串字面量」这条 SQLite 历史兼容行为：
    // 让 `"foo"` 永远是标识符，和我们的扫描器假设一致。
    connection.set_db_config(DbConfig::SQLITE_DBCONFIG_DQS_DML, false)?;
    connection.set_db_config(DbConfig::SQLITE_DBCONFIG_DQS_DDL, false)?;

    Ok(connection)
}

/// 绑定到语句上的参数。
#[derive(Debug, Clone, PartialEq)]
pub enum Params {
    None,
    /// 位置参数：`?` / `?1` / `?2`。
    Positional(Vec<SqliteValue>),
    /// 命名参数：`:name` / `@name` / `$name`（键可以带前缀，也可以不带）。
    Named(Vec<(String, SqliteValue)>),
}

/// 一句写操作的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecOutcome {
    pub changes: u64,
    pub last_insert_rowid: i64,
}

/// 一次查询的结果。`rows` 是「按列顺序的数组」，比对象更省空间、也没有重名列问题。
#[derive(Debug, Clone)]
pub struct QueryOutcome {
    pub columns: Vec<String>,
    pub rows: Vec<Value>,
    pub truncated: bool,
}

/// `execute` 的入口：执行一条**写语句**（DDL 也算）。
pub fn execute(
    connection: &Connection,
    sql: &str,
    params: &Params,
) -> Result<ExecOutcome, ServiceError> {
    let mut statement = connection.prepare(sql)?;
    bind(&mut statement, params)?;
    let changes = statement.raw_execute()?;
    Ok(ExecOutcome {
        changes: changes as u64,
        last_insert_rowid: connection.last_insert_rowid(),
    })
}

/// `query` 的入口：只接受**只读语句**（`sqlite3_stmt_readonly` 判定）。
pub fn query(
    connection: &Connection,
    sql: &str,
    params: &Params,
    limit: usize,
) -> Result<QueryOutcome, ServiceError> {
    let mut statement = connection.prepare(sql)?;
    if !statement.readonly() {
        return Err(ServiceError::new(
            "not_readonly",
            "query 只接受只读语句（SELECT / WITH ... SELECT / EXPLAIN）；写操作请用 execute 或 batch",
        ));
    }
    bind(&mut statement, params)?;

    let columns: Vec<String> = statement
        .column_names()
        .iter()
        .map(|name| (*name).to_owned())
        .collect();
    let mut rows = Vec::new();
    let mut truncated = false;
    {
        let mut cursor = statement.raw_query();
        while let Some(row) = cursor.next()? {
            if rows.len() >= limit {
                truncated = true;
                break;
            }
            let mut values = Vec::with_capacity(columns.len());
            for index in 0..columns.len() {
                values.push(json_value(row.get_ref(index)?));
            }
            rows.push(Value::Array(values));
        }
    }
    Ok(QueryOutcome {
        columns,
        rows,
        truncated,
    })
}

/// `batch` 的入口：整批包在一个 `IMMEDIATE` 事务里，任一失败全部回滚。
pub fn batch(
    connection: &mut Connection,
    statements: &[(String, Params)],
) -> Result<Vec<ExecOutcome>, ServiceError> {
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let mut results = Vec::with_capacity(statements.len());
    for (index, (sql, params)) in statements.iter().enumerate() {
        let mut statement = transaction.prepare(sql).map_err(|error| {
            ServiceError::new(
                "sql_error",
                format!("第 {} 条语句准备失败：{error}", index + 1),
            )
        })?;
        bind(&mut statement, params)?;
        let changes = statement.raw_execute().map_err(|error| {
            ServiceError::new(
                "sql_error",
                format!("第 {} 条语句执行失败：{error}", index + 1),
            )
        })?;
        results.push(ExecOutcome {
            changes: changes as u64,
            last_insert_rowid: transaction.last_insert_rowid(),
        });
    }
    transaction.commit()?;
    Ok(results)
}

/// 列出表结构（`prefix` 非空时只报该前缀下的表）。
pub fn list_tables(connection: &Connection, prefix: &str) -> Result<Vec<Value>, ServiceError> {
    let mut statement = connection.prepare(
        "SELECT name FROM sqlite_master \
         WHERE type = 'table' AND name NOT LIKE 'sqlite\\_%' ESCAPE '\\' \
         ORDER BY name LIMIT 200",
    )?;
    let names: Vec<String> = statement
        .query_map([], |row| row.get(0))?
        .collect::<Result<_, _>>()?;

    let mut tables = Vec::new();
    for name in names {
        if !prefix.is_empty() && !name.starts_with(prefix) {
            continue;
        }
        let quoted = name.replace('"', "\"\"");
        let mut columns_statement =
            connection.prepare(&format!("PRAGMA table_info(\"{quoted}\")"))?;
        let columns: Vec<Value> = columns_statement
            .query_map([], |row| {
                let default: Option<String> = row.get(4)?;
                Ok(json!({
                    "name": row.get::<_, String>(1)?,
                    "type": row.get::<_, String>(2)?,
                    "not_null": row.get::<_, i64>(3)? != 0,
                    "primary_key": row.get::<_, i64>(5)? != 0,
                    "default": default,
                }))
            })?
            .collect::<Result<_, _>>()?;
        tables.push(json!({ "name": name, "columns": columns }));
    }
    Ok(tables)
}

/// 把 JSON 参数转成可绑定形式。
pub fn params_from_json(value: Option<&Value>) -> Result<Params, ServiceError> {
    match value {
        None | Some(Value::Null) => Ok(Params::None),
        Some(Value::Array(items)) => {
            let values = items
                .iter()
                .map(value_to_param)
                .collect::<Result<Vec<_>, _>>()?;
            Ok(Params::Positional(values))
        }
        Some(Value::Object(map)) => {
            let mut pairs = Vec::with_capacity(map.len());
            for (name, value) in map {
                pairs.push((name.clone(), value_to_param(value)?));
            }
            Ok(Params::Named(pairs))
        }
        Some(other) => Err(ServiceError::bad_request(format!(
            "params 只能是数组（位置参数）或对象（命名参数），实际是 {other}"
        ))),
    }
}

fn value_to_param(value: &Value) -> Result<SqliteValue, ServiceError> {
    Ok(match value {
        Value::Null => SqliteValue::Null,
        Value::Bool(flag) => SqliteValue::Integer(i64::from(*flag)),
        Value::Number(number) => {
            if let Some(integer) = number.as_i64() {
                SqliteValue::Integer(integer)
            } else if let Some(real) = number.as_f64() {
                if !real.is_finite() {
                    return Err(ServiceError::bad_request("参数里的浮点数不能是 NaN / inf"));
                }
                SqliteValue::Real(real)
            } else {
                return Err(ServiceError::bad_request(format!(
                    "数字参数 {number} 既不是 i64 也不是 f64"
                )));
            }
        }
        Value::String(text) => SqliteValue::Text(text.clone()),
        // 数字数组 = BLOB（每个元素 0..=255）。要存 JSON 数组请用 {"$json": [...]}。
        Value::Array(items) => {
            let mut bytes = Vec::with_capacity(items.len());
            for item in items {
                let byte = item.as_u64().filter(|byte| *byte <= 255).ok_or_else(|| {
                    ServiceError::bad_request("BLOB 参数只接受 0..=255 的整数数组")
                })?;
                bytes.push(byte as u8);
            }
            SqliteValue::Blob(bytes)
        }
        Value::Object(map) => {
            if let Some(blob) = map.get("$blob") {
                let text = blob
                    .as_str()
                    .ok_or_else(|| ServiceError::bad_request("$blob 应为十六进制字符串"))?;
                SqliteValue::Blob(decode_hex(text)?)
            } else if let Some(inner) = map.get("$json") {
                SqliteValue::Text(serde_json::to_string(inner).map_err(|error| {
                    ServiceError::bad_request(format!("$json 序列化失败：{error}"))
                })?)
            } else {
                return Err(ServiceError::bad_request(
                    "对象参数只支持 {\"$blob\":\"hex\"} 或 {\"$json\": ...}；\
                     普通对象请用 $json 明确表示要存 JSON 文本",
                ));
            }
        }
    })
}

fn bind(statement: &mut Statement<'_>, params: &Params) -> Result<(), ServiceError> {
    let expected = statement.parameter_count();
    match params {
        Params::None => {
            if expected != 0 {
                return Err(ServiceError::new(
                    "param_count",
                    format!(
                        "SQL 需要 {expected} 个参数，但没给 params（缺参数会被当成 NULL，所以这里直接报错）"
                    ),
                ));
            }
        }
        Params::Positional(values) => {
            if values.len() != expected {
                return Err(ServiceError::new(
                    "param_count",
                    format!("SQL 需要 {expected} 个参数，实际给了 {}", values.len()),
                ));
            }
            for (index, value) in values.iter().enumerate() {
                statement.raw_bind_parameter(index + 1, value)?;
            }
        }
        Params::Named(pairs) => {
            let mut bound = BTreeSet::new();
            for (name, value) in pairs {
                let index = parameter_index(statement, name)?;
                statement.raw_bind_parameter(index, value)?;
                bound.insert(index);
            }
            if bound.len() != expected {
                return Err(ServiceError::new(
                    "param_count",
                    format!(
                        "SQL 需要 {expected} 个命名参数，实际绑定了 {} 个（名字对不上或漏了）",
                        bound.len()
                    ),
                ));
            }
        }
    }
    Ok(())
}

fn parameter_index(statement: &Statement<'_>, name: &str) -> Result<usize, ServiceError> {
    let candidates: Vec<String> =
        if name.starts_with(':') || name.starts_with('@') || name.starts_with('$') {
            vec![name.to_owned()]
        } else {
            vec![format!(":{name}"), format!("@{name}"), format!("${name}")]
        };
    for candidate in &candidates {
        if let Some(index) = statement.parameter_index(candidate)? {
            return Ok(index);
        }
    }
    Err(ServiceError::new(
        "unknown_param",
        format!(
            "SQL 里没有名为 {name:?} 的占位符（试过 :{} / @{} / ${}）",
            name, name, name
        ),
    ))
}

fn json_value(value: ValueRef<'_>) -> Value {
    match value {
        ValueRef::Null => Value::Null,
        ValueRef::Integer(integer) => Value::from(integer),
        ValueRef::Real(real) => {
            if real.is_finite() {
                Value::from(real)
            } else {
                let label = if real.is_nan() {
                    "nan"
                } else if real.is_sign_positive() {
                    "inf"
                } else {
                    "-inf"
                };
                json!({ "$real": label })
            }
        }
        ValueRef::Text(bytes) => match std::str::from_utf8(bytes) {
            Ok(text) => Value::String(text.to_owned()),
            // TEXT 列里塞了非 UTF-8 字节（CAST 出来的）：按 BLOB 回传，别丢信息。
            Err(_) => json!({ "$blob": encode_hex(bytes) }),
        },
        ValueRef::Blob(bytes) => json!({ "$blob": encode_hex(bytes) }),
    }
}

fn decode_hex(text: &str) -> Result<Vec<u8>, ServiceError> {
    let bytes = text.as_bytes();
    if !bytes.len().is_multiple_of(2) {
        return Err(ServiceError::bad_request("$blob 的十六进制长度必须是偶数"));
    }
    let mut out = Vec::with_capacity(bytes.len() / 2);
    for chunk in bytes.chunks(2) {
        let high = hex_digit(chunk[0])?;
        let low = hex_digit(chunk[1])?;
        out.push((high << 4) | low);
    }
    Ok(out)
}

fn hex_digit(byte: u8) -> Result<u8, ServiceError> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        b'A'..=b'F' => Ok(byte - b'A' + 10),
        _ => Err(ServiceError::bad_request("$blob 不是合法的十六进制")),
    }
}

fn encode_hex(bytes: &[u8]) -> String {
    use std::fmt::Write;

    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

#[cfg(test)]
mod tests {
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
}
