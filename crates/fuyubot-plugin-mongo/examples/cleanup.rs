//! 运维小工具：按过滤条件查看/删除文档（默认只统计，加 `--apply` 才删）。
//!
//! ```bash
//! cargo run -p fuyubot-plugin-mongo --example cleanup -- torrents '{"title":{"$regex":"战车椅子"}}'
//! cargo run -p fuyubot-plugin-mongo --example cleanup -- torrents '{"title":{"$regex":"战车椅子"}}' --apply
//! ```
//!
//! 为什么要这个：联调时「种子记录」会把同一条番剧挡住（去重生效），
//! 想重跑一次链路就得手动清掉那一条。没有 mongosh 的机器上，这是唯一办法。

use fuyubot_plugin_mongo::api::{call, configure};
use serde_json::{Value, json};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() < 2 {
        eprintln!("用法：cleanup <collection> <filter-json> [--apply]");
        std::process::exit(2);
    }
    let collection = args[0].clone();
    let filter: Value = match serde_json::from_str(&args[1]) {
        Ok(value) => value,
        Err(error) => {
            eprintln!("filter 不是合法 JSON：{error}");
            std::process::exit(2);
        }
    };
    let apply = args.iter().any(|arg| arg == "--apply");

    let uri = std::env::var("MONGO_URI").unwrap_or_else(|_| "mongodb://127.0.0.1:27017".to_owned());
    let db = std::env::var("MONGO_DB").unwrap_or_else(|_| "xqanime".to_owned());
    if let Err(error) = configure(&uri, &db) {
        eprintln!("连接失败：{error}");
        std::process::exit(1);
    }

    let found = call(
        "find",
        json!({ "collection": collection, "filter": filter, "projection": { "_id": 0 }, "limit": 20 }),
    );
    match found {
        Ok(reply) => {
            let docs = reply.get("docs").and_then(Value::as_array).cloned().unwrap_or_default();
            println!("{} 命中 {} 条：", collection, docs.len());
            for doc in &docs {
                println!("  {}", serde_json::to_string(doc).unwrap_or_default());
            }
        }
        Err(error) => {
            eprintln!("查询失败：{error}");
            std::process::exit(1);
        }
    }

    if !apply {
        println!("（干跑；加 --apply 才会真删）");
        return;
    }
    match call(
        "delete",
        json!({ "collection": collection, "filter": filter, "many": true }),
    ) {
        Ok(reply) => println!("已删除：{reply}"),
        Err(error) => {
            eprintln!("删除失败：{error}");
            std::process::exit(1);
        }
    }
}
