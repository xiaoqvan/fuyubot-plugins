use super::*;

/// 把请求 JSON 解析回 `Value`，后续按字段断言（不依赖键的序列化顺序）。
fn request_value(request: &str) -> Value {
    serde_json::from_str(request).expect("请求必须是 JSON")
}

#[test]
fn key_joins_parts_with_colon() {
    assert_eq!(
        key(&["netease", "186016", "lossless"]),
        "netease:186016:lossless"
    );
    assert_eq!(key(&["douyin", "7304"]), "douyin:7304");
    assert_eq!(key(&[]), "");
}

#[test]
fn key_sanitizes_separator_whitespace_and_non_ascii() {
    // `:` 是分隔符，片段里出现必须替换，否则键的切分会歧义。
    assert_eq!(key(&["a:b", "c"]), "a_b:c");
    // 空白与控制字符。
    assert_eq!(key(&["a b", "c"]), "a_b:c");
    assert_eq!(key(&["a\tb"]), "a_b");
    // 非 ASCII：一个字符换一个 `_`。
    assert_eq!(key(&["周杰伦"]), "___");
    // 允许的字符原样保留。
    assert_eq!(key(&["a-b_c.d"]), "a-b_c.d");
    // 空片段保留原位，不合并分隔符。
    assert_eq!(key(&["", "x"]), ":x");
}

#[test]
fn migrate_request_declares_document_table() {
    let request = request_value(&migrate_request());
    assert_eq!(request["plugin"], PLUGIN);
    assert_eq!(request["tables"][0]["template"], "document");
    assert_eq!(request["tables"][0]["table"], "media_cache");
}

#[test]
fn parse_table_takes_actual_name_from_reply() {
    // per_plugin：服务原样返回。
    assert_eq!(
        parse_table(
            r#"{"ok":true,"applied":[{"template":"document","table":"media_cache","created":true,"statements":1}]}"#
        ),
        Some("media_cache".to_owned())
    );
    // shared：服务加了前缀，必须用返回值，不能硬编码。
    assert_eq!(
        parse_table(
            r#"{"ok":true,"applied":[{"template":"document","table":"fuyubot_music_media_cache","created":true,"statements":1}]}"#
        ),
        Some("fuyubot_music_media_cache".to_owned())
    );
}

#[test]
fn parse_table_rejects_failure_and_malformed_replies() {
    let denied = r#"{"ok":false,"error":{"code":"denied_sql","message":"只读库"}}"#;
    assert_eq!(parse_table(denied), None);
    assert_eq!(parse_table("not json"), None);
    assert_eq!(parse_table(r#"{"ok":true,"applied":[]}"#), None);
    assert_eq!(parse_table(r#"{"ok":true,"applied":[{"table":""}]}"#), None);

    // 失败回复要能还原出 code / message，日志里才有可读的原因。
    let reason = failure(denied).expect("失败回复必须能解析出原因");
    assert!(reason.contains("denied_sql"), "{reason}");
    assert!(reason.contains("只读库"), "{reason}");
    assert_eq!(failure(r#"{"ok":true,"applied":[]}"#), None);
    assert!(failure("not json").is_some());
}

#[test]
fn query_request_binds_key_as_param() {
    let request = request_value(&query_request("media_cache", "netease:186016:lossless"));
    assert_eq!(request["plugin"], PLUGIN);
    assert_eq!(request["sql"], "SELECT doc FROM media_cache WHERE key = ?1");
    assert_eq!(request["params"], json!(["netease:186016:lossless"]));
    assert_eq!(request["limit"], 1);
}

#[test]
fn parse_doc_reads_first_cell() {
    let reply = r#"{"ok":true,"columns":["doc"],"rows":[["{\"title\":\"x\"}"]]}"#;
    let doc = parse_doc(reply).expect("命中");
    assert_eq!(doc["title"], "x");
}

#[test]
fn parse_doc_treats_miss_and_broken_json_as_none() {
    assert_eq!(
        parse_doc(r#"{"ok":true,"columns":["doc"],"rows":[]}"#),
        None
    );
    assert_eq!(
        parse_doc(r#"{"ok":true,"columns":["doc"],"rows":[["不是 JSON"]]}"#),
        None
    );
    assert_eq!(
        parse_doc(r#"{"ok":false,"error":{"code":"denied_sql","message":"x"}}"#),
        None
    );
    assert_eq!(parse_doc("not json"), None);
}

#[test]
fn upsert_request_uses_conflict_clause_and_params() {
    let doc = json!({"title": "晴天", "quality": "lossless"});
    let request = request_value(&upsert_request(
        "media_cache",
        "netease:186016:lossless",
        &doc,
    ));
    assert_eq!(request["plugin"], PLUGIN);

    let sql = request["sql"].as_str().expect("sql 是字符串");
    assert!(
        sql.starts_with(
            "INSERT INTO media_cache (key, doc, updated_at) VALUES (?1, ?2, strftime('%s','now'))"
        ),
        "{sql}"
    );
    assert!(
        sql.contains("ON CONFLICT(key) DO UPDATE SET doc = excluded.doc"),
        "{sql}"
    );
    assert!(sql.contains("updated_at = excluded.updated_at"), "{sql}");

    assert_eq!(request["params"][0], "netease:186016:lossless");
    // 第二个参数是文档的 JSON **文本**——值永远不拼进 SQL。
    let stored: Value =
        serde_json::from_str(request["params"][1].as_str().expect("doc 参数是字符串"))
            .expect("doc 参数是 JSON 文本");
    assert_eq!(stored, doc);
}

#[test]
fn delete_request_binds_key_as_param() {
    let request = request_value(&delete_request("media_cache", "douyin:7304"));
    assert_eq!(request["plugin"], PLUGIN);
    assert_eq!(request["sql"], "DELETE FROM media_cache WHERE key = ?1");
    assert_eq!(request["params"], json!(["douyin:7304"]));
    assert!(request.get("limit").is_none());
}
