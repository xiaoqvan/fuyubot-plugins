//! 取消群绑定频道的自动置顶。
//!
//! 群绑定（linked）频道发帖后，Telegram 会把帖子系统转发到群并自动置顶。本模块在
//! 三种信号上兜底撤掉这个置顶：
//!
//! 1. `updateNewMessage` 里的频道帖子本身（`is_pinned = true`）；
//! 2. `messagePinMessage` 置顶服务消息（置顶者是绑定频道，或被置顶的消息来自绑定频道）；
//! 3. `updateMessageIsPinned`（`is_pinned = true`，再查消息发送者）。
//!
//! 判定只认**本群绑定的那个频道**（`supergroupFullInfo.linked_chat_id`）：匿名管理员
//! 发成「群自身」、别人的频道身份都不动。需要「置顶消息」权限；没有权限只记 debug。

use serde_json::Value;

use crate::api::{Sender, message_sender};
use crate::flow;
use crate::log;
use crate::runtime::Runtime;

/// 置顶服务消息里被置顶的消息 id；普通消息返回 `None`。
///
/// TDLib 允许这里是 `0`：表示被置顶的原消息已经删除，这时只能靠置顶者判断。
pub fn pinned_message_id(message: &Value) -> Option<i64> {
    if message.pointer("/content/@type").and_then(Value::as_str) != Some("messagePinMessage") {
        return None;
    }
    message
        .pointer("/content/message_id")
        .and_then(Value::as_i64)
}

/// 这条消息的发送者是不是指定频道。
pub fn is_sender_channel(message: &Value, channel_id: i64) -> bool {
    matches!(message_sender(message), Some(Sender::Chat(id)) if id == channel_id)
}

/// 群绑定的频道 id；没绑定 / 不是超级群 / 取不到时 `None`。
async fn linked_channel(rt: &Runtime, chat_id: i64) -> Option<i64> {
    rt.td.linked_channel_id(chat_id).await
}

/// 开关开着、并且这个 chat 是群。
async fn enabled(rt: &Runtime, chat_id: i64) -> bool {
    flow::is_group(rt, chat_id).await && rt.db.config_or_default(chat_id).await.unpin_channel_pins
}

/// 撤掉一条置顶；失败只记 debug（没权限 / 已经取消 / 消息不存在都很常见）。
async fn unpin(rt: &Runtime, chat_id: i64, message_id: i64, reason: &str) {
    match rt.td.unpin(chat_id, message_id).await {
        Ok(()) => log::info(
            &rt.host,
            format!("[取消置顶] 群 {chat_id} 已取消频道置顶 msg={message_id}（{reason}）"),
        ),
        Err(error) => log::debug(
            &rt.host,
            format!("[取消置顶] 取消群 {chat_id} 的置顶失败（{reason}）：{error}"),
        ),
    }
}

/// 新消息是绑定频道自动转发过来、且已经被 Telegram 置顶时，直接撤掉。
///
/// 不消费消息：频道帖子后面还要走正常的消息链路。
pub async fn handle_channel_post(rt: &Runtime, message: &Value) {
    if message.get("is_pinned").and_then(Value::as_bool) != Some(true) {
        return;
    }
    let Some(Sender::Chat(channel_id)) = message_sender(message) else {
        return;
    };
    let Some(chat_id) = message.get("chat_id").and_then(Value::as_i64) else {
        return;
    };
    let Some(linked) = linked_channel(rt, chat_id).await else {
        return;
    };
    if channel_id != linked {
        return;
    }
    if !enabled(rt, chat_id).await {
        return;
    }
    if !rt.td.am_i_admin(chat_id, &["can_pin_messages"]).await {
        return;
    }
    if let Some(message_id) = message.get("id").and_then(Value::as_i64) {
        unpin(rt, chat_id, message_id, "频道帖子自动置顶").await;
    }
}

/// 处理置顶服务消息：置顶者是绑定频道、或被置顶的消息来自绑定频道就撤掉。
///
/// 返回 `true` 表示这条服务消息已归本模块处理，调用方不再走后面的消息链路。
pub async fn handle_pin_message(rt: &Runtime, message: &Value) -> bool {
    let Some(pinned_id) = pinned_message_id(message) else {
        return false;
    };
    let Some(chat_id) = message.get("chat_id").and_then(Value::as_i64) else {
        return false;
    };
    if !enabled(rt, chat_id).await {
        return false;
    }
    let Some(linked) = linked_channel(rt, chat_id).await else {
        return true;
    };
    if !rt.td.am_i_admin(chat_id, &["can_pin_messages"]).await {
        log::debug(
            &rt.host,
            format!("[取消置顶] 群 {chat_id} 缺少「置顶消息」权限，跳过"),
        );
        return true;
    }
    let by_linked = is_sender_channel(message, linked);
    let pinned = if pinned_id == 0 {
        None
    } else {
        match rt.cache.recent_message(chat_id, pinned_id) {
            Some(cached) => Some(cached),
            None => rt.td.message(chat_id, pinned_id).await,
        }
    };
    let pinned_linked = pinned
        .as_ref()
        .is_some_and(|pinned| is_sender_channel(pinned, linked));
    if (by_linked || pinned_linked) && pinned_id != 0 {
        unpin(rt, chat_id, pinned_id, "置顶服务消息").await;
    }
    true
}

/// 处理 `updateMessageIsPinned`：绑定频道的消息被置顶（自动或手动）就撤掉。
pub async fn handle_pinned_update(rt: &Runtime, update: &Value) {
    if update.get("is_pinned").and_then(Value::as_bool) != Some(true) {
        return;
    }
    let Some(chat_id) = update.get("chat_id").and_then(Value::as_i64) else {
        return;
    };
    let Some(message_id) = update.get("message_id").and_then(Value::as_i64) else {
        return;
    };
    if !enabled(rt, chat_id).await {
        return;
    }
    let Some(linked) = linked_channel(rt, chat_id).await else {
        return;
    };
    let message = match rt.cache.recent_message(chat_id, message_id) {
        Some(cached) => Some(cached),
        None => rt.td.message(chat_id, message_id).await,
    };
    if !message
        .as_ref()
        .is_some_and(|message| is_sender_channel(message, linked))
    {
        return;
    }
    if !rt.td.am_i_admin(chat_id, &["can_pin_messages"]).await {
        return;
    }
    unpin(rt, chat_id, message_id, "updateMessageIsPinned").await;
}

#[cfg(test)]
#[path = "../../tests/unit/flow__pin.rs"]
mod tests;
