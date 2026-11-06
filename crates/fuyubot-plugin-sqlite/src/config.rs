//! 配置解析。
//!
//! 配置只有一处来源：`[plugin.fuyubot.sqlite]`（经 ABI 的 `get_config` 读整段）。
//! `plugin_start` 的 `config_json` 只用来拿宿主给的 `plugins_dir`——插件算不出
//! 「项目根在哪」（`current_exe()` 是 `fuyubot.exe`，不是项目目录）。
//!
//! 默认落点是 `<项目根>/data/sqlite/`：与 `plugins/` 平级，跟随项目走，
//! 不会写进宿主进程的当前目录。

use std::path::{Path, PathBuf};

use fuyubot_abi::{HostApi, LogLevel};
use serde_json::Value;

use crate::sql;

/// 配置段名（也是插件名 / 服务名）。
pub const SECTION: &str = "fuyubot.sqlite";

/// 认识的键。拼错的键会打一条 warn——静默退回默认值最难查。
const KNOWN_KEYS: &[&str] = &[
    "mode",
    "dir",
    "file",
    "table_prefix",
    "journal_mode",
    "busy_timeout_ms",
    "max_rows",
    "max_sql_bytes",
    "max_batch",
    "max_result_bytes",
    "read_only",
];

/// 数据隔离模式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// 每个调用方插件一个库文件：`<dir>/<plugin>.sqlite3`，物理隔离。
    PerPlugin,
    /// 共用一个库文件；模板建出来的表自动加 `<plugin>_` 前缀。
    Shared,
}

impl Mode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::PerPlugin => "per_plugin",
            Self::Shared => "shared",
        }
    }
}

/// `journal_mode` 可选值。只开放这几种：`off` 会牺牲崩溃安全，不给。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JournalMode {
    Wal,
    Delete,
    Truncate,
    Persist,
}

impl JournalMode {
    /// 直接拼进 `PRAGMA journal_mode=…` 的常量片段（不是用户输入）。
    pub fn pragma(self) -> &'static str {
        match self {
            Self::Wal => "PRAGMA journal_mode=WAL",
            Self::Delete => "PRAGMA journal_mode=DELETE",
            Self::Truncate => "PRAGMA journal_mode=TRUNCATE",
            Self::Persist => "PRAGMA journal_mode=PERSIST",
        }
    }
}

/// 解析后的配置。
#[derive(Debug, Clone)]
pub struct Config {
    pub mode: Mode,
    /// 数据库目录（绝对路径）。
    pub dir: PathBuf,
    /// `shared` 模式下的库文件名。
    pub file: String,
    /// 额外表前缀（两种模式都会加；默认空）。
    pub table_prefix: String,
    pub journal_mode: JournalMode,
    pub busy_timeout_ms: u64,
    pub max_rows: usize,
    pub max_sql_bytes: usize,
    pub max_batch: usize,
    pub max_result_bytes: usize,
    pub read_only: bool,
}

impl Config {
    /// 从宿主配置 + `plugin_start` 的入参解析。
    pub fn from_host(host: HostApi, start_config: &str) -> Result<Self, String> {
        let start: Value = serde_json::from_str(start_config).unwrap_or(Value::Null);
        let plugins_dir = start
            .get("plugins_dir")
            .and_then(Value::as_str)
            .filter(|dir| !dir.trim().is_empty())
            .map(PathBuf::from);
        let root = project_root(plugins_dir.as_deref());

        let section = match host.get_config(SECTION, "") {
            Some(text) => serde_json::from_str::<Value>(&text)
                .map_err(|error| format!("[plugin.{SECTION}] 不是合法 JSON：{error}"))?,
            None => Value::Null,
        };
        let section = if section.is_null() {
            Value::Object(serde_json::Map::new())
        } else {
            section
        };
        if !section.is_object() {
            return Err(format!("[plugin.{SECTION}] 必须是一张表（键值配置）"));
        }
        if let Some(object) = section.as_object() {
            for key in object.keys() {
                if !KNOWN_KEYS.contains(&key.as_str()) {
                    host.log(
                        LogLevel::Warn,
                        &format!("[plugin.{SECTION}] 未知配置项 {key:?}（拼写错了？）"),
                    );
                }
            }
        }

        let mode = match string_field(&section, "mode")?.as_deref() {
            None | Some("per_plugin") => Mode::PerPlugin,
            Some("shared") => Mode::Shared,
            Some(other) => {
                return Err(format!(
                    "mode = {other:?} 不认识（只接受 \"per_plugin\" 或 \"shared\"）"
                ));
            }
        };

        let dir = match string_field(&section, "dir")?.filter(|dir| !dir.trim().is_empty()) {
            Some(dir) => {
                let path = PathBuf::from(dir);
                if path.is_absolute() {
                    path
                } else {
                    root.join(path)
                }
            }
            None => root.join("data").join("sqlite"),
        };

        let file = string_field(&section, "file")?.unwrap_or_else(|| "fuyubot.sqlite3".to_owned());
        validate_file_name(&file)?;

        let table_prefix = string_field(&section, "table_prefix")?.unwrap_or_default();
        if !table_prefix.is_empty() {
            sql::validate_identifier(&table_prefix)
                .map_err(|error| format!("table_prefix 不合法：{error}"))?;
            if table_prefix.len() > 32 {
                return Err("table_prefix 最长 32 字节".to_owned());
            }
        }

        let journal_mode = match string_field(&section, "journal_mode")?
            .map(|mode| mode.to_ascii_lowercase())
            .as_deref()
        {
            None | Some("wal") => JournalMode::Wal,
            Some("delete") => JournalMode::Delete,
            Some("truncate") => JournalMode::Truncate,
            Some("persist") => JournalMode::Persist,
            Some(other) => {
                return Err(format!(
                    "journal_mode = {other:?} 不支持（只接受 wal / delete / truncate / persist）"
                ));
            }
        };

        let busy_timeout_ms = u64_field(&section, "busy_timeout_ms")?.unwrap_or(3_000);
        if busy_timeout_ms > 600_000 {
            return Err("busy_timeout_ms 不能超过 600000（10 分钟）".to_owned());
        }

        let max_rows = usize_field(&section, "max_rows")?.unwrap_or(1_000);
        if !(1..=100_000).contains(&max_rows) {
            return Err("max_rows 必须在 1..=100000 之间".to_owned());
        }

        let max_sql_bytes =
            usize_field(&section, "max_sql_bytes")?.unwrap_or(sql::DEFAULT_MAX_SQL_BYTES);
        if !(1_024..=10 * 1024 * 1024).contains(&max_sql_bytes) {
            return Err("max_sql_bytes 必须在 1024..=10485760 之间".to_owned());
        }

        let max_batch = usize_field(&section, "max_batch")?.unwrap_or(100);
        if !(1..=10_000).contains(&max_batch) {
            return Err("max_batch 必须在 1..=10000 之间".to_owned());
        }

        let max_result_bytes =
            usize_field(&section, "max_result_bytes")?.unwrap_or(8 * 1024 * 1024);
        if !(64 * 1024..=256 * 1024 * 1024).contains(&max_result_bytes) {
            return Err("max_result_bytes 必须在 65536..=268435456 之间".to_owned());
        }

        let read_only = bool_field(&section, "read_only")?.unwrap_or(false);

        Ok(Self {
            mode,
            dir,
            file,
            table_prefix,
            journal_mode,
            busy_timeout_ms,
            max_rows,
            max_sql_bytes,
            max_batch,
            max_result_bytes,
            read_only,
        })
    }

    /// 表前缀：`shared` 模式下强制带调用方插件名前缀；两种模式都叠加 `table_prefix`。
    pub fn prefix_for(&self, plugin: &str) -> String {
        let mut prefix = self.table_prefix.clone();
        if self.mode == Mode::Shared {
            prefix.push_str(&sql::sanitize_prefix(plugin));
            prefix.push('_');
        }
        prefix
    }

    /// 某个插件的库文件路径。
    pub fn database_path(&self, plugin: &str) -> PathBuf {
        match self.mode {
            Mode::Shared => self.dir.join(&self.file),
            Mode::PerPlugin => self.dir.join(format!("{plugin}.sqlite3")),
        }
    }
}

/// 项目根：`plugins_dir` 的上一级；拿不到就退回当前目录。
fn project_root(plugins_dir: Option<&Path>) -> PathBuf {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let Some(dir) = plugins_dir else {
        return cwd;
    };
    let absolute = if dir.is_absolute() {
        dir.to_path_buf()
    } else {
        cwd.join(dir)
    };
    absolute.parent().map(Path::to_path_buf).unwrap_or(absolute)
}

fn validate_file_name(file: &str) -> Result<(), String> {
    if file.trim().is_empty() {
        return Err("file 不能为空".to_owned());
    }
    if file.len() > 128 {
        return Err("file 最长 128 字节".to_owned());
    }
    if file == "." || file == ".." || file.contains("..") {
        return Err("file 不能包含 ..".to_owned());
    }
    if file.contains('/') || file.contains('\\') || file.contains('\0') {
        return Err("file 只能是文件名，不能带路径分隔符".to_owned());
    }
    Ok(())
}

fn string_field(section: &Value, key: &str) -> Result<Option<String>, String> {
    match section.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text)) => Ok(Some(text.clone())),
        Some(other) => Err(format!("配置项 {key} 应为字符串，实际是 {other}")),
    }
}

fn u64_field(section: &Value, key: &str) -> Result<Option<u64>, String> {
    match section.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Number(number)) => number
            .as_u64()
            .map(Some)
            .ok_or_else(|| format!("配置项 {key} 应为非负整数")),
        Some(other) => Err(format!("配置项 {key} 应为整数，实际是 {other}")),
    }
}

fn usize_field(section: &Value, key: &str) -> Result<Option<usize>, String> {
    Ok(u64_field(section, key)?.map(|value| value as usize))
}

fn bool_field(section: &Value, key: &str) -> Result<Option<bool>, String> {
    match section.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Bool(value)) => Ok(Some(*value)),
        Some(other) => Err(format!("配置项 {key} 应为 true/false，实际是 {other}")),
    }
}

#[cfg(test)]
impl Config {
    /// 测试用的最小配置：临时目录 + 指定模式。
    pub fn for_tests(dir: PathBuf, mode: Mode) -> Self {
        Self {
            mode,
            dir,
            file: "shared.sqlite3".to_owned(),
            table_prefix: String::new(),
            journal_mode: JournalMode::Wal,
            busy_timeout_ms: 2_000,
            max_rows: 100,
            max_sql_bytes: sql::DEFAULT_MAX_SQL_BYTES,
            max_batch: 100,
            max_result_bytes: 8 * 1024 * 1024,
            read_only: false,
        }
    }
}
