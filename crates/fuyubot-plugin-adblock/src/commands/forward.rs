//! `/addfwdbl`、`/rmfwdbl`：转发黑名单（来自这些频道/超级群的消息一律处置）。
//!
//! 只收 `-100` 开头的频道/超级群 ID：普通群与用户没有「频道转发」这种来源。

use fuyubot_simplify::Command;
use serde_json::Value;

use crate::api::Sender;
use crate::flow;
use crate::runtime::Runtime;

fn is_channel_or_supergroup(id: i64) -> bool {
    id.to_string().starts_with("-100")
}

/// `/addfwdbl <chat_id|@username>`
pub fn add(rt: &Runtime, command: &Command, sender: Sender) {
    let chat_id = command.chat_id;
    let reply_to = Some(command.message_id);
    if !flow::ensure_bot_admin(rt, chat_id, sender, reply_to) {
        return;
    }
    let words = super::words(command);
    let Some(raw) = words.first() else {
        flow::send_tip(
            rt,
            chat_id,
            "用法: /addfwdbl <chat_id|@username>\n要求: chat_id 必须 -100 开头\n例: /addfwdbl -1001234567890\n例: /addfwdbl @durov",
            reply_to,
            20,
        );
        return;
    };

    let target = if raw.starts_with('@') {
        let username = raw.trim_start_matches('@').trim();
        if username.is_empty() {
            flow::send_tip(
                rt,
                chat_id,
                "无效的用户名，请使用 /addfwdbl @username",
                reply_to,
                10,
            );
            return;
        }
        match rt.td.search_public_chat(username) {
            Ok(chat) => match chat.get("id").and_then(Value::as_i64) {
                Some(id) => id,
                None => {
                    flow::send_tip(
                        rt,
                        chat_id,
                        &format!("未找到用户名对应的对话: @{username}"),
                        reply_to,
                        10,
                    );
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
                );
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
                    &format!("无效参数: {raw}\n请使用数字 chat_id 或 @username。"),
                    reply_to,
                    10,
                );
                return;
            }
        }
    };

    if !is_channel_or_supergroup(target) {
        flow::send_tip(
            rt,
            chat_id,
            &format!("chat_id 必须是 -100 开头的频道/超级群 ID，当前: {target}"),
            reply_to,
            10,
        );
        return;
    }
    match rt.db.add_forward(target) {
        Ok(id) => flow::send_tip(
            rt,
            chat_id,
            &format!("✅ 已将 {target} 添加到转发黑名单，记录 ID: {id}"),
            reply_to,
            20,
        ),
        Err(error) => flow::send_tip(rt, chat_id, &format!("❌ 添加失败: {error}"), reply_to, 20),
    };
}

/// `/rmfwdbl <chat_id>`
pub fn remove(rt: &Runtime, command: &Command, sender: Sender) {
    let chat_id = command.chat_id;
    let reply_to = Some(command.message_id);
    if !flow::ensure_bot_admin(rt, chat_id, sender, reply_to) {
        return;
    }
    let words = super::words(command);
    let Some(raw) = words.first() else {
        flow::send_tip(
            rt,
            chat_id,
            "用法: /rmfwdbl <chat_id>\n要求: chat_id 必须 -100 开头\n例: /rmfwdbl -1001234567890",
            reply_to,
            20,
        );
        return;
    };
    let Ok(target) = raw.parse::<i64>() else {
        flow::send_tip(
            rt,
            chat_id,
            &format!("无效的 chat_id: {raw}，请提供数字 ID。"),
            reply_to,
            10,
        );
        return;
    };
    if !is_channel_or_supergroup(target) {
        flow::send_tip(
            rt,
            chat_id,
            &format!("chat_id 必须是 -100 开头的频道/超级群 ID，当前: {target}"),
            reply_to,
            10,
        );
        return;
    }
    if rt.db.remove_forward(target) {
        flow::send_tip(
            rt,
            chat_id,
            &format!("✅ 已将 {target} 从转发黑名单移除。"),
            reply_to,
            20,
        );
    } else {
        flow::send_tip(
            rt,
            chat_id,
            &format!("⚠️ 未找到 chat_id 为 {target} 的转发黑名单记录。"),
            reply_to,
            20,
        );
    }
}
