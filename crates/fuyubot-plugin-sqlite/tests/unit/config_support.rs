use super::*;

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
