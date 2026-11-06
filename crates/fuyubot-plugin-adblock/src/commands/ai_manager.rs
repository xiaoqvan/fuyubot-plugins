//! `/adaiset`：管理「内置 AI 授权群」。
//!
//! `storage.mongo` 里的 `ai_default_chats` 集合就是授权名单：群要么自己配接口，
//! 要么在这个名单里（用插件配置的默认接口）。TS 版还有一张写死的群名单，
//! 这里**不搬**——那是一次性部署的产物，搬进通用插件只会让别的部署莫名奇妙用上别人
//! 的额度。

use crate::simplify::Command;
use serde_json::Value;

use crate::api::Sender;
use crate::flow;
use crate::log;
use crate::runtime::Runtime;

const USAGE: &str = "🤖 **内置 AI 授权群管理**\n\n用法:\n  /adaiset add <chat_id 或 @群用户名>  — 添加授权群\n  /adaiset rm <chat_id>  — 移除授权群\n  /adaiset list  — 查看所有授权群\n\n示例:\n  /adaiset add -1001234567890\n  /adaiset add @mygroup\n  /adaiset rm -1001234567890\n  /adaiset list";

pub async fn handle(rt: &Runtime, command: &Command, sender: Sender) {
    let chat_id = command.chat_id;
    let reply_to = Some(command.message_id);
    if !flow::ensure_owner(rt, chat_id, sender, reply_to).await {
        return;
    }
    let words = super::words(command);
    let Some(sub) = words.first().map(|word| word.to_lowercase()) else {
        flow::send_tip(rt, chat_id, USAGE, reply_to, 60).await;
        return;
    };
    match sub.as_str() {
        "add" => add(rt, chat_id, reply_to, words.get(1)).await,
        "rm" => remove(rt, chat_id, reply_to, words.get(1)).await,
        "list" => list(rt, chat_id, reply_to).await,
        other => {
            flow::send_tip(
                rt,
                chat_id,
                &format!("❌ 未知子命令: {other}\n可用子命令：add、rm、list"),
                reply_to,
                10,
            )
            .await;
        }
    }
}

async fn add(rt: &Runtime, chat_id: i64, reply_to: Option<i64>, target: Option<&String>) {
    let Some(target) = target else {
        flow::send_tip(
            rt,
            chat_id,
            "❌ 请提供 chat_id 或 @群用户名，例如：/adaiset add -1001234567890 或 /adaiset add @mygroup",
            reply_to,
            15,
        ).await;
        return;
    };
    let (resolved, title) = if let Some(username) = target.strip_prefix('@') {
        match rt.td.search_public_chat(username).await {
            Ok(chat) => match chat.get("id").and_then(Value::as_i64) {
                Some(id) => (
                    id,
                    chat.get("title")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_owned(),
                ),
                None => {
                    flow::send_tip(
                        rt,
                        chat_id,
                        &format!(
                            "❌ 无法通过 @{username} 找到对应群组，请确认用户名正确且 Bot 已加入该群。"
                        ),
                        reply_to,
                        15,
                    ).await;
                    return;
                }
            },
            Err(error) => {
                flow::send_tip(
                    rt,
                    chat_id,
                    &format!("❌ 解析 @{username} 失败: {error}"),
                    reply_to,
                    15,
                )
                .await;
                return;
            }
        }
    } else {
        match target.parse::<i64>() {
            Ok(id) if id < 0 => (id, String::new()),
            _ => {
                flow::send_tip(
                    rt,
                    chat_id,
                    &format!("❌ 无效的 chat_id: {target}，群组 chat_id 必须为负数。"),
                    reply_to,
                    15,
                )
                .await;
                return;
            }
        }
    };

    if rt.db.add_ai_default_chat(resolved, &title).await {
        flow::send_tip(
            rt,
            chat_id,
            &format!(
                "✅ 已将群 `{resolved}`（{}）添加到内置 AI 授权群列表。",
                if title.is_empty() {
                    "未知名称"
                } else {
                    &title
                }
            ),
            reply_to,
            30,
        )
        .await;
        log::info(
            &rt.host,
            format!("Bot 主人将群 {resolved}（{title}）添加到内置 AI 授权群"),
        );
    } else {
        flow::send_tip(
            rt,
            chat_id,
            &format!(
                "ℹ️ 群 `{resolved}`（{}）已在授权列表中。",
                if title.is_empty() {
                    "未知名称"
                } else {
                    &title
                }
            ),
            reply_to,
            15,
        )
        .await;
    }
}

async fn remove(rt: &Runtime, chat_id: i64, reply_to: Option<i64>, target: Option<&String>) {
    let Some(target) = target else {
        flow::send_tip(
            rt,
            chat_id,
            "❌ 请提供要移除的 chat_id，例如：/adaiset rm -1001234567890",
            reply_to,
            15,
        )
        .await;
        return;
    };
    match target.parse::<i64>() {
        Ok(id) if id < 0 => {
            if rt.db.remove_ai_default_chat(id).await {
                flow::send_tip(
                    rt,
                    chat_id,
                    &format!("✅ 已将群 `{id}` 从内置 AI 授权群列表中移除。"),
                    reply_to,
                    20,
                )
                .await;
                log::info(
                    &rt.host,
                    format!("Bot 主人将群 {id} 从内置 AI 授权群列表中移除"),
                );
            } else {
                flow::send_tip(
                    rt,
                    chat_id,
                    "❌ 移除失败，该群可能不在授权列表中。",
                    reply_to,
                    15,
                )
                .await;
            }
        }
        _ => {
            flow::send_tip(
                rt,
                chat_id,
                &format!("❌ 无效的 chat_id: {target}，群组 chat_id 必须为负数。"),
                reply_to,
                15,
            )
            .await;
        }
    }
}

async fn list(rt: &Runtime, chat_id: i64, reply_to: Option<i64>) {
    let chats = rt.db.list_ai_default_chats().await;
    if chats.is_empty() {
        flow::send_tip(rt, chat_id, "📋 内置 AI 授权群列表为空。", reply_to, 15).await;
        return;
    }
    let mut lines = vec!["📋 **内置 AI 授权群列表：**\n".to_owned()];
    for (id, title) in &chats {
        let link = if *id < -1_000_000_000_000 {
            format!("  [跳转](https://t.me/c/{}/1)", &id.to_string()[4..])
        } else {
            String::new()
        };
        lines.push(format!(
            "• `{id}`  {}{link}",
            if title.is_empty() {
                "（未知名称）".to_owned()
            } else {
                format!("**{title}**")
            }
        ));
    }
    lines.push(format!("\n共 {} 个群", chats.len()));
    lines.push("\n💡 群主可在群内使用 /adconfig 配置自定义 AI 接口覆盖内置默认配置".to_owned());
    flow::send_tip(rt, chat_id, &lines.join("\n"), reply_to, 60).await;
}
