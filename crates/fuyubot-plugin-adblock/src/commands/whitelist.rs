//! `/whitelist`、`/rmwhitelist`：管理群白名单（白名单用户不会被检测）。
//!
//! 参数解析与 TS 版一致：支持数字 ID、`@username`、以及「回复某条消息」。
//! 所有提示 10 秒后自删，避免刷屏。

use crate::simplify::Command;
use serde_json::Value;

use crate::api::Sender;
use crate::flow;
use crate::log;
use crate::runtime::Runtime;

const USAGE: &str = "用法：\n/whitelist add <ID|@username|reply>\n/whitelist rm <ID|@username|reply>\n或使用 /rmwhitelist <ID|@username|reply> 来直接移除";

pub fn handle(
    rt: &Runtime,
    command: &Command,
    sender: Sender,
    message: &Value,
    default_action: &str,
) {
    let chat_id = command.chat_id;
    let reply_to = Some(command.message_id);

    // Bot 没有删消息的权限时原版是静默返回：连提示都发不出去的局面。
    if !flow::am_i_admin(rt, chat_id) {
        return;
    }
    if !flow::ensure_group(rt, chat_id, reply_to) {
        return;
    }
    if !flow::ensure_admin(rt, chat_id, sender, reply_to) {
        return;
    }

    let words = super::words(command);
    let mut action = words.first().map(|word| word.to_lowercase());
    let mut target = words.get(1).cloned();
    if !action
        .as_deref()
        .is_some_and(|action| matches!(action, "add" | "rm"))
    {
        // `/rmwhitelist 123`：第一个词其实是目标。
        action = Some(default_action.to_owned());
        target = words.first().cloned();
    }

    let Some(action) = action.filter(|action| matches!(action.as_str(), "add" | "rm")) else {
        flow::send_tip(rt, chat_id, &format!("无效的操作。\n{USAGE}"), reply_to, 10);
        return;
    };

    let Some(id) = resolve_id(rt, command, message, target.as_deref()) else {
        flow::send_tip(
            rt,
            chat_id,
            "请提供有效的参数。例如：\n/whitelist add <ID|@username>\n或回复用户的消息并使用 /whitelist add reply",
            reply_to,
            10,
        );
        return;
    };

    match action.as_str() {
        "add" => {
            rt.db.set_whitelisted(chat_id, id, true);
            flow::send_tip(
                rt,
                chat_id,
                &format!("✅ 已将 {id} 添加到群组白名单。"),
                reply_to,
                10,
            );
            log::info(&rt.host, format!("群 {chat_id} 白名单 +{id}"));
        }
        _ => {
            rt.db.set_whitelisted(chat_id, id, false);
            flow::send_tip(
                rt,
                chat_id,
                &format!("✅ 已将 {id} 从群组白名单中移除。"),
                reply_to,
                10,
            );
            log::info(&rt.host, format!("群 {chat_id} 白名单 -{id}"));
        }
    }
}

/// `reply` / 数字 ID / `@username` → 一个 TGID。
fn resolve_id(
    rt: &Runtime,
    command: &Command,
    message: &Value,
    target: Option<&str>,
) -> Option<i64> {
    match target {
        None | Some("reply") => {
            // 回复某条消息 → 那条消息的发送者
            let reply = message.get("reply_to").filter(|reply| {
                reply.get("@type").and_then(Value::as_str) == Some("messageReplyToMessage")
            })?;
            let reply_id = reply.get("message_id").and_then(Value::as_i64)?;
            let replied = rt.td.message(command.chat_id, reply_id)?;
            let sender = crate::flow::sender_of(&replied)?;
            Some(sender.id())
        }
        Some(target) if target.starts_with('@') => {
            let username = target.trim_start_matches('@').trim();
            if username.is_empty() {
                return None;
            }
            match rt.td.search_public_chat(username) {
                Ok(chat) => chat.get("id").and_then(Value::as_i64),
                Err(error) => {
                    let message = if error.message.contains("USERNAME_NOT_OCCUPIED") {
                        "该用户名不存在或未公开。"
                    } else if error.message.contains("USERNAME_INVALID") {
                        "用户名格式无效。"
                    } else {
                        "无法找到该用户名的用户。请检查用户名是否正确。"
                    };
                    flow::send_tip(rt, command.chat_id, message, Some(command.message_id), 10);
                    None
                }
            }
        }
        Some(target) => target.parse::<i64>().ok(),
    }
}
