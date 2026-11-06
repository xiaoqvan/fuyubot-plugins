//! 表结构模板：把「建表 + 索引」这种重复劳动固化下来。
//!
//! 模板是**幂等**的（`CREATE TABLE/INDEX IF NOT EXISTS`），重复 `migrate` 不会报错，
//! 也不会改已有表的数据——它只负责「这张表应该长这样」，不负责版本演进。
//! 需要改字段时由插件自己写 `ALTER TABLE`（`execute` 允许 DDL）。
//!
//! 表名占位符：
//! - `{table}` → 加过双引号的完整表名（如 `"demo_kv"`）；
//! - `{name}`  → 未加引号的完整表名（只用在索引名里，值本身已过标识符校验）。
//!
//! 两个占位符都只由**服务**渲染：调用方给的是 `table` 片段，必须过
//! [`crate::sql::validate_identifier`]，不存在把 SQL 拼进表名的口子。

use rusqlite::{Connection, TransactionBehavior};
use serde_json::{Value, json};

use crate::error::ServiceError;
use crate::sql;

/// 一个内置模板。
pub struct Template {
    pub name: &'static str,
    pub description: &'static str,
    /// 默认表名（完整表名前还会拼上模式前缀）。
    pub table: &'static str,
    pub statements: &'static [&'static str],
}

/// 内置模板表。名字是稳定的 API，别随便改。
pub const TEMPLATES: &[Template] = &[
    Template {
        name: "kv",
        description: "键值表：key 唯一，value 是文本（JSON 也可以自己序列化后存）",
        table: "kv",
        statements: &[
            "CREATE TABLE IF NOT EXISTS {table} (\n  key        TEXT PRIMARY KEY NOT NULL,\n  value      TEXT NOT NULL,\n  updated_at INTEGER NOT NULL DEFAULT (strftime('%s','now'))\n)",
        ],
    },
    Template {
        name: "log",
        description: "日志表：按时间追加，带 created_at 索引",
        table: "log",
        statements: &[
            "CREATE TABLE IF NOT EXISTS {table} (\n  id         INTEGER PRIMARY KEY AUTOINCREMENT,\n  level      TEXT NOT NULL,\n  message    TEXT NOT NULL,\n  created_at INTEGER NOT NULL DEFAULT (strftime('%s','now'))\n)",
            "CREATE INDEX IF NOT EXISTS \"{name}_created\" ON {table}(created_at)",
        ],
    },
    Template {
        name: "counter",
        description: "计数器表：name 唯一，value 是整数，适合做自增序号 / 统计",
        table: "counter",
        statements: &[
            "CREATE TABLE IF NOT EXISTS {table} (\n  name       TEXT PRIMARY KEY NOT NULL,\n  value      INTEGER NOT NULL DEFAULT 0,\n  updated_at INTEGER NOT NULL DEFAULT (strftime('%s','now'))\n)",
        ],
    },
    Template {
        name: "document",
        description: "JSON 文档表：key 唯一，doc 存序列化后的 JSON 文本",
        table: "document",
        statements: &[
            "CREATE TABLE IF NOT EXISTS {table} (\n  key        TEXT PRIMARY KEY NOT NULL,\n  doc        TEXT NOT NULL,\n  updated_at INTEGER NOT NULL DEFAULT (strftime('%s','now'))\n)",
        ],
    },
    Template {
        name: "queue",
        description: "队列表：status + id 索引，适合「取出待处理任务」这类场景",
        table: "queue",
        statements: &[
            "CREATE TABLE IF NOT EXISTS {table} (\n  id         INTEGER PRIMARY KEY AUTOINCREMENT,\n  payload    TEXT NOT NULL,\n  status     TEXT NOT NULL DEFAULT 'pending',\n  created_at INTEGER NOT NULL DEFAULT (strftime('%s','now')),\n  updated_at INTEGER NOT NULL DEFAULT (strftime('%s','now'))\n)",
            "CREATE INDEX IF NOT EXISTS \"{name}_status\" ON {table}(status, id)",
        ],
    },
];

/// 按名字找模板。
pub fn find(name: &str) -> Option<&'static Template> {
    TEMPLATES.iter().find(|template| template.name == name)
}

/// 一次 `migrate` 请求：用哪个模板、建到哪张表（`None` 用模板默认名）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Migration {
    pub template: String,
    pub table: Option<String>,
}

/// 一次成功应用的结果。
#[derive(Debug, Clone)]
pub struct Applied {
    pub template: &'static str,
    pub table: String,
    pub created: bool,
    pub statements: usize,
}

impl Applied {
    pub fn to_json(&self) -> Value {
        json!({
            "template": self.template,
            "table": self.table,
            "created": self.created,
            "statements": self.statements,
        })
    }
}

/// 把模板渲染成可执行 SQL。
pub fn render(template: &Template, full_table: &str) -> Vec<String> {
    let quoted = sql::quote_identifier(full_table);
    template
        .statements
        .iter()
        .map(|statement| {
            statement
                .replace("{table}", &quoted)
                .replace("{name}", full_table)
        })
        .collect()
}

/// 在**一个事务**里应用全部模板。任一失败就整体回滚。
pub fn apply(
    conn: &mut Connection,
    prefix: &str,
    migrations: &[Migration],
) -> Result<Vec<Applied>, ServiceError> {
    if migrations.is_empty() {
        return Ok(Vec::new());
    }
    let transaction = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let mut applied = Vec::with_capacity(migrations.len());
    for migration in migrations {
        let template = find(&migration.template).ok_or_else(|| {
            ServiceError::bad_request(format!(
                "没有名为 {:?} 的模板（可用：{}）",
                migration.template,
                template_names().join(", ")
            ))
        })?;
        let suffix = migration.table.as_deref().unwrap_or(template.table);
        sql::validate_identifier(suffix)?;
        let full = format!("{prefix}{suffix}");
        sql::validate_identifier(&full)?;

        let existed: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1)",
            [&full],
            |row| row.get(0),
        )?;

        let statements = render(template, &full);
        for statement in &statements {
            transaction.execute_batch(statement)?;
        }
        applied.push(Applied {
            template: template.name,
            table: full,
            created: !existed,
            statements: statements.len(),
        });
    }
    transaction.commit()?;
    Ok(applied)
}

/// 模板名列表（错误信息与 `info` 用）。
pub fn template_names() -> Vec<&'static str> {
    TEMPLATES.iter().map(|template| template.name).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_both_placeholders() {
        let log = find("log").expect("log 模板");
        let rendered = render(log, "demo_log");
        assert!(rendered[0].contains("\"demo_log\""));
        assert!(rendered[1].contains("\"demo_log_created\""));
        assert!(!rendered.iter().any(|sql| sql.contains('{')));
    }

    #[test]
    fn template_names_are_unique() {
        let mut names = template_names();
        let total = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), total);
    }
}
