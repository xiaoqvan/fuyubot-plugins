//! SQL 文本的**静态防线**：切语句、拒绝危险语句、校验标识符。
//!
//! 这一层只做「不执行 SQL 就能判定」的检查。真正的防注入靠**参数绑定**
//! （见 [`crate::db`]）：值永远不进 SQL 文本，只按占位符传给 SQLite。
//!
//! 设计取舍：
//! - 字符串 / 引号标识符 / 注释里的 `;` 不算语句分隔——先用 [`mask`] 把它们
//!   抹成空格再切，避免「`SELECT ';'` 被当成两条语句」。
//! - 危险语句按**首关键字**拒（ATTACH / PRAGMA / VACUUM / 事务控制…），
//!   危险函数按「名字后面跟 `(`」拒（`load_extension(`），不误伤同名列。
//! - 标识符只允许 `[A-Za-z_][A-Za-z0-9_]*`，且不许碰 `sqlite_` 保留前缀：
//!   模板建表、共享库表前缀都走同一个校验。

use std::fmt;

/// 一次 SQL 静态检查失败。`code` 是稳定的机器可读码，`message` 给人看。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SqlError {
    pub code: &'static str,
    pub message: String,
}

impl SqlError {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

impl fmt::Display for SqlError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for SqlError {}

/// 一条语句允许的最大字节数（可被配置覆盖，见 [`crate::config`]）。
pub const DEFAULT_MAX_SQL_BYTES: usize = 64 * 1024;

/// 标识符长度上限。SQLite 本身没限制，这是我们自己的防呆线。
pub const MAX_IDENTIFIER: usize = 64;

/// 只允许出现在语句开头的**危险关键字**。
///
/// - `ATTACH` / `DETACH`：能读写任意文件路径，等于把 `sqlite3` 变成文件 API。
/// - `PRAGMA`：能关掉 `trusted_schema`、改 `journal_mode`、开 `writable_schema`，
///   服务自己管的连接参数不该让调用方改。
/// - `VACUUM`：`VACUUM INTO 'x'` 是写文件；普通 VACUUM 也会长时间独占锁。
/// - 事务控制：连接是**多个插件共享**的，谁 `BEGIN` 谁就该负责收尾；一旦忘了
///   `COMMIT`，后面所有插件都会被拖在一个未提交事务里。要原子写请用 `batch`。
const DENIED_KEYWORDS: &[&str] = &[
    "ATTACH",
    "DETACH",
    "PRAGMA",
    "VACUUM",
    "BEGIN",
    "COMMIT",
    "END",
    "ROLLBACK",
    "SAVEPOINT",
    "RELEASE",
];

/// 危险函数名（小写）。只有后面紧跟 `(` 时才算调用，避免误伤同名列。
///
/// `load_extension` 在 bundled SQLite 里默认就是关的（rusqlite 不调
/// `sqlite3_enable_load_extension`），这里是纵深防御。
const FORBIDDEN_FUNCTIONS: &[&str] = &["load_extension", "readfile", "writefile"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScanState {
    Normal,
    Single,
    Double,
    Backtick,
    Bracket,
    LineComment,
    BlockComment,
}

/// 把字符串 / 引号标识符 / 注释抹成空格，**保持字节长度不变**。
///
/// 之后所有扫描（切 `;`、找首关键字、找函数名）都在抹过的副本上做：
/// 引号里的内容不可能影响结构判断，而切片仍然对着原文。
/// 抹过的副本 + 引号标识符的内容区间（后者用来识别 `"load_extension"(...)`）。
struct Masked {
    bytes: Vec<u8>,
    /// 每段是原文里的「引号内」字节区间（不含引号本身）。
    quoted: Vec<(usize, usize)>,
}

fn mask(sql: &str) -> Result<Masked, SqlError> {
    let bytes = sql.as_bytes();
    let mut out = bytes.to_vec();
    let mut quoted = Vec::new();
    let mut quote_start = 0usize;
    let mut state = ScanState::Normal;
    let mut i = 0usize;
    while i < bytes.len() {
        let b = bytes[i];
        match state {
            ScanState::Normal => match b {
                b'\'' => {
                    out[i] = b' ';
                    state = ScanState::Single;
                }
                b'"' => {
                    out[i] = b' ';
                    quote_start = i + 1;
                    state = ScanState::Double;
                }
                b'`' => {
                    out[i] = b' ';
                    quote_start = i + 1;
                    state = ScanState::Backtick;
                }
                b'[' => {
                    out[i] = b' ';
                    quote_start = i + 1;
                    state = ScanState::Bracket;
                }
                b'-' if bytes.get(i + 1) == Some(&b'-') => {
                    out[i] = b' ';
                    out[i + 1] = b' ';
                    state = ScanState::LineComment;
                    i += 1;
                }
                b'/' if bytes.get(i + 1) == Some(&b'*') => {
                    out[i] = b' ';
                    out[i + 1] = b' ';
                    state = ScanState::BlockComment;
                    i += 1;
                }
                _ => {}
            },
            ScanState::Single => {
                out[i] = b' ';
                if b == b'\'' {
                    if bytes.get(i + 1) == Some(&b'\'') {
                        // SQL 的转义写法：'' 是字面量里的一个引号。
                        out[i + 1] = b' ';
                        i += 1;
                    } else {
                        state = ScanState::Normal;
                    }
                }
            }
            ScanState::Double => {
                out[i] = b' ';
                if b == b'"' {
                    if bytes.get(i + 1) == Some(&b'"') {
                        out[i + 1] = b' ';
                        i += 1;
                    } else {
                        quoted.push((quote_start, i));
                        state = ScanState::Normal;
                    }
                }
            }
            ScanState::Backtick => {
                out[i] = b' ';
                if b == b'`' {
                    quoted.push((quote_start, i));
                    state = ScanState::Normal;
                }
            }
            ScanState::Bracket => {
                out[i] = b' ';
                if b == b']' {
                    quoted.push((quote_start, i));
                    state = ScanState::Normal;
                }
            }
            ScanState::LineComment => {
                out[i] = b' ';
                if b == b'\n' {
                    state = ScanState::Normal;
                }
            }
            ScanState::BlockComment => {
                out[i] = b' ';
                if b == b'*' && bytes.get(i + 1) == Some(&b'/') {
                    out[i + 1] = b' ';
                    state = ScanState::Normal;
                    i += 1;
                }
            }
        }
        i += 1;
    }

    match state {
        ScanState::Normal | ScanState::LineComment => Ok(Masked { bytes: out, quoted }),
        ScanState::Single => Err(SqlError::new("unterminated_sql", "SQL 里有没闭合的单引号")),
        ScanState::Double => Err(SqlError::new("unterminated_sql", "SQL 里有没闭合的双引号")),
        ScanState::Backtick => Err(SqlError::new("unterminated_sql", "SQL 里有没闭合的反引号")),
        ScanState::Bracket => Err(SqlError::new(
            "unterminated_sql",
            "SQL 里有没闭合的 [ 标识符",
        )),
        ScanState::BlockComment => Err(SqlError::new(
            "unterminated_sql",
            "SQL 里有没闭合的 /* 注释",
        )),
    }
}

/// 按顶层 `;` 切语句（空片段自动丢掉）。
pub fn split_statements(sql: &str) -> Result<Vec<&str>, SqlError> {
    let masked = mask(sql)?.bytes;
    let mut out = Vec::new();
    let mut start = 0usize;
    for (index, byte) in masked.iter().enumerate() {
        if *byte == b';' {
            let fragment = &sql[start..index];
            if !is_blank(fragment) {
                out.push(fragment);
            }
            start = index + 1;
        }
    }
    let tail = &sql[start..];
    if !is_blank(tail) {
        out.push(tail);
    }
    Ok(out)
}

/// 去掉首尾空白与注释后是不是空的。
fn is_blank(fragment: &str) -> bool {
    skip_trivia(fragment).is_empty()
}

fn skip_trivia(mut text: &str) -> &str {
    loop {
        text = text.trim_start();
        if let Some(rest) = text.strip_prefix("--") {
            match rest.find('\n') {
                Some(index) => text = &rest[index + 1..],
                None => return "",
            }
        } else if let Some(rest) = text.strip_prefix("/*") {
            match rest.find("*/") {
                Some(index) => text = &rest[index + 2..],
                None => return "",
            }
        } else {
            return text;
        }
    }
}

/// 语句的首关键字（大写）；空语句 / 纯注释返回 `None`。
pub fn first_keyword(sql: &str) -> Result<Option<String>, SqlError> {
    let masked = mask(sql)?.bytes;
    let text = String::from_utf8_lossy(&masked);
    let trimmed = text.trim_start();
    let end = trimmed
        .find(|c: char| !c.is_ascii_alphabetic())
        .unwrap_or(trimmed.len());
    if end == 0 {
        return Ok(None);
    }
    Ok(Some(trimmed[..end].to_ascii_uppercase()))
}

/// 找出一段标识符里被禁的函数名。
fn forbidden_name(name: &str) -> Option<&'static str> {
    let lowered = name.to_ascii_lowercase();
    FORBIDDEN_FUNCTIONS
        .iter()
        .copied()
        .find(|candidate| *candidate == lowered)
}

/// 找出第一个被禁的「函数调用」，返回函数名。
///
/// 两种写法都管：裸标识符（`load_extension(`）和引号标识符（`"load_extension"(`、
/// `` `load_extension`( ``、`[load_extension](`）——SQLite 允许用引号写函数名。
fn forbidden_call(sql: &str) -> Result<Option<&'static str>, SqlError> {
    let masked = mask(sql)?;
    let bytes = &masked.bytes;
    let mut i = 0usize;
    while i < bytes.len() {
        let b = bytes[i];
        if b.is_ascii_alphabetic() || b == b'_' {
            let start = i;
            i += 1;
            while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                i += 1;
            }
            if let Some(name) = forbidden_name(&sql[start..i]) {
                let mut j = i;
                while j < bytes.len() && bytes[j].is_ascii_whitespace() {
                    j += 1;
                }
                if bytes.get(j) == Some(&b'(') {
                    return Ok(Some(name));
                }
            }
            continue;
        }
        i += 1;
    }

    for (start, end) in &masked.quoted {
        let Some(name) = forbidden_name(&sql[*start..*end]) else {
            continue;
        };
        let mut j = end + 1; // 跳过闭引号
        while j < bytes.len() && bytes[j].is_ascii_whitespace() {
            j += 1;
        }
        if bytes.get(j) == Some(&b'(') {
            return Ok(Some(name));
        }
    }
    Ok(None)
}

/// 校验一条**单语句**：非空、首关键字不危险、不调用危险函数。
pub fn check_statement(sql: &str) -> Result<(), SqlError> {
    let Some(keyword) = first_keyword(sql)? else {
        return Err(SqlError::new("empty_sql", "SQL 里没有可执行的语句"));
    };
    if DENIED_KEYWORDS.contains(&keyword.as_str()) {
        return Err(SqlError::new(
            "denied_sql",
            format!(
                "不允许执行 {keyword} 语句：事务控制与 ATTACH / PRAGMA / VACUUM 由服务统一管理；\
                 多条写入请用 batch（服务会包一层事务）"
            ),
        ));
    }
    if let Some(name) = forbidden_call(sql)? {
        return Err(SqlError::new(
            "denied_sql",
            format!("不允许调用 {name}()：它绕过了数据库的边界"),
        ));
    }
    Ok(())
}

/// 供 `query` / `execute` / `batch` 用的总入口：长度、单语句、危险语句一次查完。
pub fn ensure_single(sql: &str, max_bytes: usize) -> Result<&str, SqlError> {
    if sql.len() > max_bytes {
        return Err(SqlError::new(
            "sql_too_large",
            format!("SQL 有 {} 字节，超过上限 {max_bytes}", sql.len()),
        ));
    }
    if sql.as_bytes().contains(&0) {
        return Err(SqlError::new("invalid_sql", "SQL 里不能有 NUL 字节"));
    }
    let statements = split_statements(sql)?;
    match statements.len() {
        0 => Err(SqlError::new("empty_sql", "SQL 是空的")),
        1 => {
            let statement = statements[0];
            check_statement(statement)?;
            Ok(statement)
        }
        count => Err(SqlError::new(
            "multiple_statements",
            format!("一次只允许一条语句，这里切出了 {count} 条；多条写入请用 batch"),
        )),
    }
}

/// 校验一个 SQL 标识符（表名 / 列名 / 模板名）。
pub fn validate_identifier(name: &str) -> Result<(), SqlError> {
    if name.is_empty() {
        return Err(SqlError::new("bad_identifier", "标识符不能为空"));
    }
    if name.len() > MAX_IDENTIFIER {
        return Err(SqlError::new(
            "bad_identifier",
            format!("标识符 {name:?} 超过 {MAX_IDENTIFIER} 字节"),
        ));
    }
    let mut chars = name.chars();
    let first = chars.next().expect("非空");
    if !(first.is_ascii_alphabetic() || first == '_') {
        return Err(SqlError::new(
            "bad_identifier",
            format!("标识符 {name:?} 必须以 ASCII 字母或下划线开头"),
        ));
    }
    if !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return Err(SqlError::new(
            "bad_identifier",
            format!("标识符 {name:?} 只能包含 ASCII 字母、数字与下划线"),
        ));
    }
    if name.len() >= 7 && name[..7].eq_ignore_ascii_case("sqlite_") {
        return Err(SqlError::new(
            "bad_identifier",
            format!("标识符 {name:?} 用了 SQLite 保留前缀 sqlite_"),
        ));
    }
    Ok(())
}

/// 校验「调用方插件名」：它会被拼进数据库文件名与共享库表前缀，所以不能有路径分隔符。
pub fn validate_plugin_name(name: &str) -> Result<(), SqlError> {
    if name.is_empty() || name.len() > MAX_IDENTIFIER {
        return Err(SqlError::new(
            "bad_plugin",
            format!("plugin 名必须是 1..={MAX_IDENTIFIER} 字节"),
        ));
    }
    if name.contains("..") || name == "." {
        return Err(SqlError::new(
            "bad_plugin",
            format!("plugin 名 {name:?} 不能包含 .."),
        ));
    }
    let first = name.chars().next().expect("非空");
    if !(first.is_ascii_alphanumeric() || first == '_') {
        return Err(SqlError::new(
            "bad_plugin",
            format!("plugin 名 {name:?} 必须以字母、数字或下划线开头"),
        ));
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
    {
        return Err(SqlError::new(
            "bad_plugin",
            format!("plugin 名 {name:?} 只能包含 ASCII 字母、数字、下划线、点和短横线"),
        ));
    }
    Ok(())
}

/// 把插件名变成合法的表前缀片段：非 `[A-Za-z0-9_]` 一律换成 `_`，数字开头补 `_`。
pub fn sanitize_prefix(name: &str) -> String {
    let mut out = String::with_capacity(name.len() + 1);
    for c in name.chars() {
        if c.is_ascii_alphanumeric() || c == '_' {
            out.push(c);
        } else {
            out.push('_');
        }
    }
    if out.is_empty() {
        out.push('_');
    }
    let first = out.chars().next().expect("非空");
    if !(first.is_ascii_alphabetic() || first == '_') {
        out.insert(0, '_');
    }
    out
}

/// 给已经校验过的标识符加双引号。
pub fn quote_identifier(name: &str) -> String {
    debug_assert!(validate_identifier(name).is_ok());
    format!("\"{name}\"")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_on_top_level_semicolons_only() {
        assert_eq!(split_statements("SELECT 1").unwrap(), vec!["SELECT 1"]);
        assert_eq!(
            split_statements("SELECT 1; SELECT 2;").unwrap(),
            vec!["SELECT 1", " SELECT 2"]
        );
        // 字符串里的分号不是分隔符。
        assert_eq!(split_statements("SELECT ';'").unwrap(), vec!["SELECT ';'"]);
        // 注释里的分号同理。
        assert_eq!(
            split_statements("SELECT 1 -- ; drop\n").unwrap(),
            vec!["SELECT 1 -- ; drop\n"]
        );
        assert_eq!(
            split_statements("SELECT /* ; */ 1").unwrap(),
            vec!["SELECT /* ; */ 1"]
        );
        // 空语句（只有分号 / 注释）被丢掉。
        assert!(split_statements(";;  -- x\n;").unwrap().is_empty());
    }

    #[test]
    fn rejects_unterminated_literals() {
        assert_eq!(
            split_statements("SELECT 'abc").unwrap_err().code,
            "unterminated_sql"
        );
        assert_eq!(
            split_statements("SELECT 1 /* abc").unwrap_err().code,
            "unterminated_sql"
        );
    }

    #[test]
    fn keyword_detection_skips_comments() {
        assert_eq!(
            first_keyword("-- hi\n  select 1").unwrap().unwrap(),
            "SELECT"
        );
        assert_eq!(
            first_keyword("/* x */ INSERT INTO t").unwrap().unwrap(),
            "INSERT"
        );
        assert_eq!(first_keyword("   ").unwrap(), None);
    }

    #[test]
    fn rejects_stacked_statements() {
        let error = ensure_single("SELECT 1; DROP TABLE users", DEFAULT_MAX_SQL_BYTES).unwrap_err();
        assert_eq!(error.code, "multiple_statements");
    }

    #[test]
    fn rejects_dangerous_statements() {
        for sql in [
            "ATTACH DATABASE 'x.db' AS x",
            "PRAGMA writable_schema = ON",
            "VACUUM INTO 'x'",
            "BEGIN",
            "COMMIT",
            "SAVEPOINT sp",
        ] {
            let error = ensure_single(sql, DEFAULT_MAX_SQL_BYTES).unwrap_err();
            assert_eq!(error.code, "denied_sql", "{sql}");
        }
    }

    #[test]
    fn rejects_dangerous_functions_but_not_same_named_columns() {
        assert_eq!(
            ensure_single("SELECT load_extension('x')", DEFAULT_MAX_SQL_BYTES)
                .unwrap_err()
                .code,
            "denied_sql"
        );
        assert_eq!(
            ensure_single("SELECT \"load_extension\"('x')", DEFAULT_MAX_SQL_BYTES)
                .unwrap_err()
                .code,
            "denied_sql"
        );
        // 同名列 / 字符串里的名字不算调用。
        assert!(ensure_single("SELECT load_extension FROM t", DEFAULT_MAX_SQL_BYTES).is_ok());
        assert!(ensure_single("SELECT 'load_extension()'", DEFAULT_MAX_SQL_BYTES).is_ok());
    }

    #[test]
    fn keyword_checks_ignore_literal_content() {
        // 值里带 DROP 不影响「首关键字是 SELECT」。
        assert!(ensure_single("SELECT 'DROP TABLE t'", DEFAULT_MAX_SQL_BYTES).is_ok());
        assert_eq!(
            ensure_single("SELECT 1\0", DEFAULT_MAX_SQL_BYTES)
                .unwrap_err()
                .code,
            "invalid_sql"
        );
        assert_eq!(
            ensure_single("INSERT INTO t VALUES (1)\0", DEFAULT_MAX_SQL_BYTES)
                .unwrap_err()
                .code,
            "invalid_sql"
        );
    }

    #[test]
    fn identifiers_are_strict() {
        for ok in ["users", "_private", "t1", "a_b_c"] {
            assert!(validate_identifier(ok).is_ok(), "{ok}");
        }
        for bad in [
            "",
            "1table",
            "user-name",
            "a b",
            "users; DROP TABLE x",
            "users\"",
            "sqlite_master",
            "SQLITE_sequence",
        ] {
            assert!(validate_identifier(bad).is_err(), "{bad}");
        }
        assert!(validate_identifier(&"a".repeat(65)).is_err());
    }

    #[test]
    fn plugin_names_cannot_escape_the_directory() {
        for ok in ["demo", "fuyubot.manage", "a-b_c"] {
            assert!(validate_plugin_name(ok).is_ok(), "{ok}");
        }
        for bad in ["", "../x", "a/b", "a\\b", ".hidden", "-x", "a b", ".."] {
            assert!(validate_plugin_name(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn prefixes_are_sanitized() {
        assert_eq!(sanitize_prefix("demo"), "demo");
        assert_eq!(sanitize_prefix("fuyubot.manage"), "fuyubot_manage");
        assert_eq!(sanitize_prefix("1bot"), "_1bot");
        assert_eq!(sanitize_prefix("a-b"), "a_b");
    }
}
