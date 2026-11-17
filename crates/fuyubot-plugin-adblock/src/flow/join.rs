//! 入群提示消息：按群配置删除 Telegram 的入群服务消息。
//!
//! 三种提示对应 TDLib 的 `MessageContent`：
//!
//! | 群配置项 | 服务消息 | 场景 |
//! | --- | --- | --- |
//! | `delete_join_message` | `messageChatAddMembers` / `messageChatJoinByRequest` | 成员加入（被添加 / 审批通过） |
//! | `delete_join_community_message` | `messageChatJoinFromCommunity` | 成员通过社区加入 |
//! | `delete_join_invite_message` | `messageChatJoinByLink` | 成员通过邀请链接加入 |
//!
//! 撤回走 `deleteMessages(revoke = true)`，需要「删除消息」权限；没权限 / 对象不是群
//! 时只记 debug。**能不能收到这条服务消息由 Telegram 决定**（不同账号类型 / 入群方式
//! 可能不下发），收到才会删。

use serde_json::Value;

use crate::flow;
use crate::log;
use crate::runtime::Runtime;
use crate::store::GroupConfig;

/// 三种入群提示。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JoinMessage {
    /// 成员加入：被管理员添加，或入群申请被批准。
    Member,
    /// 成员通过社区加入。
    Community,
    /// 成员通过邀请链接加入。
    Invite,
}

impl JoinMessage {
    /// 从消息内容里认出这是哪一种入群提示；其它消息一律 `None`。
    pub fn of(message: &Value) -> Option<Self> {
        match message.pointer("/content/@type").and_then(Value::as_str)? {
            "messageChatAddMembers" | "messageChatJoinByRequest" => Some(Self::Member),
            "messageChatJoinFromCommunity" => Some(Self::Community),
            "messageChatJoinByLink" => Some(Self::Invite),
            _ => None,
        }
    }

    /// 群配置里对应的开关。
    pub fn enabled(self, config: &GroupConfig) -> bool {
        match self {
            Self::Member => config.delete_join_message,
            Self::Community => config.delete_join_community_message,
            Self::Invite => config.delete_join_invite_message,
        }
    }

    /// 日志里用的名字。
    pub fn key(self) -> &'static str {
        match self {
            Self::Member => "join",
            Self::Community => "join_community",
            Self::Invite => "join_invite",
        }
    }
}

/// 入群提示：群配置要求删除就撤回这条消息，返回 `true`（调用方不再走后面的链路）。
///
/// 返回 `true` 的语义是「这条消息已经归本模块处理了」——即使撤回失败（没权限 /
/// Telegram 拒了），它也是服务消息，没有继续做广告检测的意义。
pub async fn handle(rt: &Runtime, message: &Value) -> bool {
    let Some(kind) = JoinMessage::of(message) else {
        return false;
    };
    let Some(chat_id) = message.get("chat_id").and_then(Value::as_i64) else {
        return false;
    };
    let Some(message_id) = message.get("id").and_then(Value::as_i64) else {
        return false;
    };
    let config = rt.db.config_or_default(chat_id).await;
    if !kind.enabled(&config) {
        return false;
    }
    if !flow::is_group(rt, chat_id).await {
        return false;
    }
    // 只删提示，不碰成员：有「删除消息」就够，不要求「限制成员」。
    if !rt.td.am_i_admin(chat_id, &["can_delete_messages"]).await {
        log::debug(
            &rt.host,
            format!(
                "[入群提示] 群 {chat_id} 缺少「删除消息」权限，跳过（{}）",
                kind.key()
            ),
        );
        return true;
    }
    match rt.td.delete_messages(chat_id, &[message_id]).await {
        Ok(()) => {
            log::info(
                &rt.host,
                format!(
                    "[入群提示] 群 {chat_id} 已删除 {} 提示 msg={message_id}",
                    kind.key()
                ),
            );
            true
        }
        Err(error) => {
            log::debug(
                &rt.host,
                format!(
                    "[入群提示] 删除群 {chat_id} 的 {} 提示失败：{error}",
                    kind.key()
                ),
            );
            true
        }
    }
}

#[cfg(test)]
#[path = "../../tests/unit/flow__join.rs"]
mod tests;
