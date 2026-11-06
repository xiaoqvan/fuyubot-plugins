//! `/qadid`、`/delad`、`/qban`：查黑名单记录、删黑名单记录、查封禁历史。
//!
//! 这三个是**运维工具**，输出的都是原始记录，不做美化——排查时看到的就是库里存的东西。

use crate::simplify::Command;
use serde_json::Value;

use crate::api::Sender;
use crate::flow;
use crate::runtime::Runtime;
use crate::util::{fmt_utc, truncate};

/// 单条消息的安全长度。
const MAX_LEN: usize = crate::store::MAX_TEXT;

/// `/qadid <ID>`
pub async fn query_ad_id(rt: &Runtime, command: &Command, sender: Sender) {
    let chat_id = command.chat_id;
    let reply_to = Some(command.message_id);
    if !flow::ensure_bot_admin(rt, chat_id, sender, reply_to).await {
        return;
    }
    let words = super::words(command);
    let Some(id) = words.first() else {
        flow::send_tip(rt, chat_id, "用法: /qadid <ID>", reply_to, 10).await;
        return;
    };
    let Some(entry) = rt.db.find_blacklist(id).await else {
        flow::send_tip(
            rt,
            chat_id,
            &format!("未在黑名单中找到 ID: {id}"),
            reply_to,
            10,
        )
        .await;
        return;
    };
    let json = serde_json::to_string_pretty(&entry.record).unwrap_or_default();
    let body = format!(
        "集合: {}\n记录:\n```{}```",
        entry.collection.name(),
        truncate(&json, MAX_LEN)
    );
    flow::send_tip(rt, chat_id, &body, reply_to, 30).await;
}

/// `/delad <ID>`
pub async fn delete_ad(rt: &Runtime, command: &Command, sender: Sender) {
    let chat_id = command.chat_id;
    let reply_to = Some(command.message_id);
    if !flow::ensure_bot_admin(rt, chat_id, sender, reply_to).await {
        return;
    }
    let words = super::words(command);
    let Some(id) = words.first() else {
        flow::send_tip(rt, chat_id, "用法: /delad <ID>", reply_to, 10).await;
        return;
    };
    match rt.db.delete_blacklist(id).await {
        Some(collection) => {
            flow::send_tip(
                rt,
                chat_id,
                &format!("✅ 已从集合 {} 删除记录 {id}", collection.name()),
                reply_to,
                20,
            )
            .await
        }
        None => {
            flow::send_tip(
                rt,
                chat_id,
                &format!("未能删除记录: 未找到 ID {id}"),
                reply_to,
                10,
            )
            .await
        }
    };
}

/// `/qban <tgid|@username> [limit]`
pub async fn query_ban(rt: &Runtime, command: &Command, sender: Sender) {
    let chat_id = command.chat_id;
    let reply_to = Some(command.message_id);
    // 原版是 `scope: all`：谁都能查。这里保留。
    let _ = sender;
    let words = super::words(command);
    let Some(raw) = words.first() else {
        flow::send_tip(
            rt,
            chat_id,
            "用法: /qban <tgid|@username> [limit]",
            reply_to,
            15,
        )
        .await;
        return;
    };
    let limit = words
        .get(1)
        .and_then(|word| word.parse::<usize>().ok())
        .unwrap_or(10);

    let tgid = if raw.starts_with('@') {
        let username = raw.trim_start_matches('@').trim();
        if username.is_empty() {
            flow::send_tip(
                rt,
                chat_id,
                "用户名不能为空，请使用 /qban @username",
                reply_to,
                10,
            )
            .await;
            return;
        }
        match rt.td.search_public_chat(username).await {
            Ok(chat) => match chat.get("id").and_then(Value::as_i64) {
                Some(id) => id,
                None => {
                    flow::send_tip(
                        rt,
                        chat_id,
                        &format!("未找到用户名对应的对象: @{username}"),
                        reply_to,
                        10,
                    )
                    .await;
                    return;
                }
            },
            Err(error) => {
                flow::send_tip(
                    rt,
                    chat_id,
                    &format!("解析用户名失败: @{username}，{error}"),
                    reply_to,
                    10,
                )
                .await;
                return;
            }
        }
    } else {
        match raw.parse::<i64>() {
            Ok(id) => id,
            Err(_) => {
                flow::send_tip(
                    rt,
                    chat_id,
                    &format!("参数格式错误: {raw}\n请使用数字 TGID 或 @username。"),
                    reply_to,
                    10,
                )
                .await;
                return;
            }
        }
    };

    let records = rt.db.ban_records_by_tgid(tgid, limit).await;
    if records.is_empty() {
        flow::send_tip(
            rt,
            chat_id,
            &format!("未找到 TGID {tgid} 的封禁记录"),
            reply_to,
            10,
        )
        .await;
        return;
    }

    let lines = records
        .iter()
        .enumerate()
        .map(|(index, record)| {
            format!(
                "{}. action={} spam_type={}\n   record_id={}\n   trigger_db_id={}\n   chat_id={}\n   createdAt={}",
                index + 1,
                record.action,
                record.spam_type,
                record.id,
                record.trigger_db_id.as_deref().unwrap_or("null"),
                record.chat_id,
                fmt_utc(record.created_at)
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n");
    let body = truncate(
        &format!(
            "TGID {tgid} 的封禁记录(最近 {} 条):\n\n{lines}",
            records.len()
        ),
        MAX_LEN,
    );
    flow::send_tip(rt, chat_id, &body, reply_to, 30).await;
}
