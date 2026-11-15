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
