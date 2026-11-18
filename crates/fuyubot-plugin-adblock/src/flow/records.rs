//! 消息索引：给「封禁时连带撤回历史消息」提供 48 小时窗口。
//!
//! 对应 TS 版 `update/index.ts`。消息记录只做两件事：入群消息写入、被删时同步移除。
//! TTL 交给 `message_records.createdAt` 上的索引（见 `store::ensure_indexes`）。
//!
//! 顺带把**群里的用户消息文本**写进 `message_texts`（同样 48 小时 TTL），给 AI 审核
//! 当「这个人刚才在聊什么」的上下文。放库里的理由：进程重启不丢，也不占常驻内存。
//!
//! 两个入口都是 `async`：查群类型、写/删索引都要 `.await`。

use serde_json::Value;

use crate::api::{Sender, message_sender};
use crate::flow;
use crate::runtime::Runtime;
use crate::text;

/// 记下一条群消息。
///
/// 只记群、只记「不是群自己发」的消息：频道转发、匿名管理员的消息没有可处置的发送者，
/// 记下来只会让之后那批删除请求白跑。
pub async fn log_message(rt: &Runtime, message: &Value) {
    let Some(chat_id) = message.get("chat_id").and_then(Value::as_i64) else {
        return;
    };
    let Some(message_id) = message.get("id").and_then(Value::as_i64) else {
        return;
    };
    let Some(sender) = message_sender(message) else {
        return;
    };
    if sender == Sender::Chat(chat_id) {
        return;
    }
    if !flow::is_group(rt, chat_id).await {
        return;
    }
    rt.db.add_message(chat_id, sender.id(), message_id).await;
    // AI 审核的上下文文本：只记群里的用户消息，落库（48 小时 TTL）。
    if matches!(sender, Sender::User(_)) {
        let body = text::message_text(message);
        rt.db
            .add_text(chat_id, sender.id(), message_id, &body)
            .await;
    }
}

/// `updateDeleteMessages`：把已经从 Telegram 删掉的消息从索引里摘掉。
pub async fn handle_delete(rt: &Runtime, update: &Value) {
    let Some(chat_id) = update.get("chat_id").and_then(Value::as_i64) else {
        return;
    };
    if !flow::chat_enabled(rt, chat_id) {
        return;
    }
    let ids: Vec<i64> = update
        .get("message_ids")
        .and_then(Value::as_array)
        .map(|ids| ids.iter().filter_map(Value::as_i64).collect())
        .unwrap_or_default();
    if ids.is_empty() {
        return;
    }
    // 自动点赞的「连续」判定基于最近消息窗口：删掉的消息要立刻摘掉，
    // 不能让它把两边的连续段隔开。
    rt.cache.forget_messages(chat_id, &ids);
    let removed = rt.db.remove_message_ids(chat_id, &ids).await;
    if removed > 0 {
        crate::log::debug(
            &rt.host,
            format!("从 message_records 移除 {removed} 条（chat={chat_id}）"),
        );
    }
}
