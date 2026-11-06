//! 真机联调：连本机 MongoDB 跑一遍完整 CRUD。
//!
//! ```bash
//! cargo run -p fuyubot-plugin-mongo --example live
//! # 自定义连接串：
//! MONGO_URI=mongodb://127.0.0.1:27017 cargo run -p fuyubot-plugin-mongo --example live
//! ```
//!
//! **只在独立的测试库里跑**（默认 `xqanime_it`），跑完把文档清空，
//! 不会碰正式库 `xqanime`。每一步都打印实际返回值，失败立刻退出。

use fuyubot_plugin_mongo::api::{call, configure};
use serde_json::{Value, json};

fn main() {
    let uri = std::env::var("MONGO_URI").unwrap_or_else(|_| "mongodb://127.0.0.1:27017".to_owned());
    let db = std::env::var("MONGO_TEST_DB").unwrap_or_else(|_| "xqanime_it".to_owned());
    let collection = "smoke";

    println!("连接 {uri} / 测试库 {db}");
    if let Err(err) = configure(&uri, &db) {
        fail("configure", &err);
    }

    step("ping", call("ping", json!({})));
    step("collections", call("collections", json!({ "db": db })));

    // 干净起跑：先清掉上次残留。
    let _ = call(
        "delete",
        json!({ "collection": collection, "filter": {}, "allow_empty": true }),
    );

    step(
        "ensure_index",
        call(
            "ensure_index",
            json!({
                "collection": collection,
                "keys": { "id": 1 },
                "unique": true,
                "name": "uniq_id"
            }),
        ),
    );

    step(
        "insert (3 docs)",
        call(
            "insert",
            json!({
                "collection": collection,
                "docs": [
                    { "id": 400602, "name": "葬送のフリーレン", "name_cn": "葬送的芙莉莲",
                      "score": 8.9, "tags": ["奇幻", "冒险"], "qi": 100 },
                    { "id": 302189, "name": "SPY×FAMILY", "name_cn": "间谍过家家",
                      "score": 7.8, "tags": ["搞笑"], "qi": 50 },
                    { "id": 485, "name": "CLANNAD", "name_cn": "团子大家族",
                      "score": 8.4, "tags": ["治愈"], "qi": 80 }
                ]
            }),
        ),
    );

    // 唯一索引：重复 id 必须被拒。
    match call(
        "insert",
        json!({ "collection": collection, "docs": { "id": 400602, "name": "重复" } }),
    ) {
        Ok(value) => fail("unique index", &format!("重复 id 竟然插入成功：{value}")),
        Err(err) if err.contains("11000") || err.to_lowercase().contains("duplicate") => {
            println!("✓ unique index 拒绝重复 id：{}", brief(&err));
        }
        Err(err) => fail("unique index", &err),
    }

    step(
        "count",
        call("count", json!({ "collection": collection, "filter": {} })),
    );

    step(
        "find (score >= 8.4, 按 score 降序)",
        call(
            "find",
            json!({
                "collection": collection,
                "filter": { "score": { "$gte": 8.4 } },
                "sort": { "score": -1 },
                "projection": { "_id": 0, "id": 1, "name_cn": 1, "score": 1 }
            }),
        ),
    );

    step(
        "find_one",
        call(
            "find_one",
            json!({ "collection": collection, "filter": { "id": 302189 } }),
        ),
    );

    step(
        "update (patch)",
        call(
            "update",
            json!({
                "collection": collection,
                "filter": { "id": 302189 },
                "patch": { "score": 8.0, "reviewed": true }
            }),
        ),
    );

    step(
        "update (upsert 新番)",
        call(
            "update",
            json!({
                "collection": collection,
                "filter": { "id": 999001 },
                "update": { "$set": { "name_cn": "新番占位", "score": 6.6 } },
                "upsert": true
            }),
        ),
    );

    step(
        "aggregate (按 tags 展开统计)",
        call(
            "aggregate",
            json!({
                "collection": collection,
                "pipeline": [
                    { "$unwind": "$tags" },
                    { "$group": { "_id": "$tags", "n": { "$sum": 1 } } },
                    { "$sort": { "n": -1, "_id": 1 } }
                ]
            }),
        ),
    );

    step(
        "distinct (tags)",
        call(
            "distinct",
            json!({ "collection": collection, "field": "tags" }),
        ),
    );

    step(
        "delete (id >= 900000)",
        call(
            "delete",
            json!({ "collection": collection, "filter": { "id": { "$gte": 900000 } } }),
        ),
    );

    // 收尾：清空测试集合，别留垃圾。
    match call(
        "delete",
        json!({ "collection": collection, "filter": {}, "allow_empty": true }),
    ) {
        Ok(value) => println!("\n清理完成：{value}"),
        Err(err) => println!("\n清理失败（不影响结论）：{err}"),
    }

    println!("\n全部通过：storage.mongo 与本机 MongoDB 联调成功。");
}

fn step(label: &str, result: Result<Value, String>) {
    match result {
        Ok(value) => println!("✓ {label}: {}", brief(&value.to_string())),
        Err(err) => fail(label, &err),
    }
}

fn brief(text: &str) -> String {
    const LIMIT: usize = 220;
    if text.chars().count() <= LIMIT {
        return text.to_owned();
    }
    let head: String = text.chars().take(LIMIT).collect();
    format!("{head}…（共 {} 字符）", text.chars().count())
}

fn fail(label: &str, err: &str) -> ! {
    eprintln!("✗ {label} 失败：{err}");
    std::process::exit(1);
}
