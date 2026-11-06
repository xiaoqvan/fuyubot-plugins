//! 业务流：消息处理、入群检查、资料复检、超时封禁、消息索引、GKY 转发。
//!
//! 这里的函数都跑在**工作线程**上（`flow::message::handle` 由事件回调排队进来），
//! 所以可以放心阻塞、发请求、调 AI。回调本身只做一件事：把 `Value` 丢进队列。

pub mod gky;
pub mod message;
pub mod new_member;
pub mod profile;
pub mod records;
pub mod sweeper;

use crate::simplify::ReplyButton;
use serde_json::Value;

use crate::action;
use crate::api::{Sender, message_sender};
use crate::runtime::Runtime;
use crate::text;

/// 发送者画像：一次消息处理要用四五次它的字段，所以一次查齐。
#[derive(Debug, Clone, Default)]
pub struct SenderDetails {
    pub user: Option<Value>,
    pub bio: String,
    pub is_bot: bool,
    pub is_admin: bool,
    pub is_member: bool,
}

/// 私聊（chat_id 为正）。
pub fn is_private(chat_id: i64) -> bool {
    chat_id > 0
}

/// 这个 chat 是不是群（带 60 秒缓存）。
pub fn is_group(rt: &Runtime, chat_id: i64) -> bool {
    if let Some(value) = rt.cache.chat_is_group(chat_id) {
        return value;
    }
    let value = rt.td.chat_is_group(chat_id);
    rt.cache.store_chat_is_group(chat_id, value);
    value
}

/// Bot 在这个群有没有「删消息 + 限制成员」（带 60 秒缓存）。
pub fn am_i_admin(rt: &Runtime, chat_id: i64) -> bool {
    if let Some(value) = rt.cache.admin_ok(chat_id) {
        return value;
    }
    let value = rt
        .td
        .am_i_admin(chat_id, &["can_delete_messages", "can_restrict_members"]);
    rt.cache.store_admin_ok(chat_id, value);
    value
}

/// 查一次发送者画像。
pub fn sender_details(rt: &Runtime, chat_id: i64, sender: Sender) -> SenderDetails {
    match sender {
        Sender::User(user_id) => {
            let user = rt.td.user(user_id).ok();
            let is_bot = user
                .as_ref()
                .and_then(|user| user.get("type"))
                .and_then(|kind| kind.get("@type"))
                .and_then(Value::as_str)
                == Some("userTypeBot");
            let status = rt.td.member_status(chat_id, sender);
            let is_admin = matches!(
                status.as_deref(),
                Some("chatMemberStatusCreator") | Some("chatMemberStatusAdministrator")
            );
            let is_member = matches!(
                status.as_deref(),
                Some("chatMemberStatusMember")
                    | Some("chatMemberStatusAdministrator")
                    | Some("chatMemberStatusCreator")
                    | Some("chatMemberStatusRestricted")
            );
            let bio = rt
                .td
                .user_full(user_id)
                .ok()
                .and_then(|full| {
                    full.get("bio")
                        .and_then(|bio| bio.get("text"))
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                })
                .unwrap_or_default();
            SenderDetails {
                user,
                bio,
                is_bot,
                is_admin,
                is_member,
            }
        }
        Sender::Chat(_) => {
            let status = rt.td.member_status(chat_id, sender);
            let is_admin = sender == Sender::Chat(chat_id)
                || matches!(
                    status.as_deref(),
                    Some("chatMemberStatusCreator") | Some("chatMemberStatusAdministrator")
                );
            let is_member = matches!(
                status.as_deref(),
                Some("chatMemberStatusMember")
                    | Some("chatMemberStatusAdministrator")
                    | Some("chatMemberStatusCreator")
                    | Some("chatMemberStatusRestricted")
            );
            SenderDetails {
                is_admin,
                is_member,
                ..SenderDetails::default()
            }
        }
    }
}

/// 用户的「主页挂载频道」名称与简介（广告载体之一）。
pub fn personal_channel(rt: &Runtime, user_id: i64) -> Option<(String, String)> {
    let full = rt.td.user_full(user_id).ok()?;
    let personal_chat_id = full
        .get("personal_chat_id")
        .and_then(Value::as_i64)
        .filter(|id| *id != 0)?;
    let chat = rt.td.chat(personal_chat_id).ok()?;
    let kind = chat.get("type")?;
    if kind.get("@type").and_then(Value::as_str) != Some("chatTypeSupergroup")
        || !kind
            .get("is_channel")
            .and_then(Value::as_bool)
            .unwrap_or(false)
    {
        return None;
    }
    let supergroup_id = kind.get("supergroup_id").and_then(Value::as_i64)?;
    let description = rt
        .td
        .supergroup_full(supergroup_id)
        .ok()
        .and_then(|full| {
            full.get("description")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .unwrap_or_default();
    Some((
        chat.get("title")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned(),
        description,
    ))
}

/// 命令的「只能在群里用」检查；不满足时回一句并返回 `false`。
pub fn ensure_group(rt: &Runtime, chat_id: i64, reply_to: Option<i64>) -> bool {
    if is_group(rt, chat_id) {
        return true;
    }
    send_tip(rt, chat_id, "此命令只能在群组中使用。", reply_to, 10);
    false
}

/// 命令的「只有管理员能用」检查。
pub fn ensure_admin(rt: &Runtime, chat_id: i64, sender: Sender, reply_to: Option<i64>) -> bool {
    if !is_group(rt, chat_id) {
        send_tip(rt, chat_id, "此命令只能在群组中使用。", reply_to, 10);
        return false;
    }
    if rt.td.is_admin(chat_id, sender) {
        return true;
    }
    send_tip(rt, chat_id, "只有群组管理员才能使用此命令。", reply_to, 10);
    false
}

/// 「仅 Bot 主人」检查。
pub fn ensure_owner(rt: &Runtime, chat_id: i64, sender: Sender, reply_to: Option<i64>) -> bool {
    let Some(user_id) = sender.user_id() else {
        send_tip(rt, chat_id, "该命令仅限 Bot 主人使用。", reply_to, 10);
        return false;
    };
    if rt.cfg.is_owner(user_id) {
        return true;
    }
    let hint = if rt.cfg.owner_ids.is_empty() {
        "该命令仅限 Bot 主人使用（`owner_ids` 还没配置）。"
    } else {
        "该命令仅限 Bot 主人使用。"
    };
    send_tip(rt, chat_id, hint, reply_to, 10);
    false
}

/// 「Bot 主人 **或** Bot 管理员」检查（对应 TS 的 `permission:"admin"` 档）。
///
/// 注册表（[`crate::commands::spec`]）已经先判过一次；这里保留是给「直接调 handler」
/// 的路径兜底，也保证权限语义只写在一处判断里。
pub fn ensure_bot_admin(rt: &Runtime, chat_id: i64, sender: Sender, reply_to: Option<i64>) -> bool {
    let Some(user_id) = sender.user_id() else {
        send_tip(
            rt,
            chat_id,
            "该命令仅限 Bot 主人或管理员使用。",
            reply_to,
            10,
        );
        return false;
    };
    if rt.cfg.is_bot_admin(user_id) {
        return true;
    }
    let hint = if rt.cfg.owner_ids.is_empty() && rt.cfg.admin_ids.is_empty() {
        "该命令仅限 Bot 主人或管理员使用（`owner_ids` / `admin_ids` 都还没配置）。"
    } else {
        "该命令仅限 Bot 主人或管理员使用。"
    };
    send_tip(rt, chat_id, hint, reply_to, 10);
    false
}

/// 发一条提示并安排自删（`seconds = 0` 表示不删）。
pub fn send_tip(
    rt: &Runtime,
    chat_id: i64,
    body: &str,
    reply_to: Option<i64>,
    seconds: u32,
) -> Option<Value> {
    send_with(rt, chat_id, body, reply_to, Vec::new(), seconds)
}

/// 发一条消息（可带按钮），并在 `seconds` 后自删。
pub fn send_with(
    rt: &Runtime,
    chat_id: i64,
    body: &str,
    reply_to: Option<i64>,
    buttons: Vec<Vec<ReplyButton>>,
    seconds: u32,
) -> Option<Value> {
    let message = rt.td.send_text(
        chat_id,
        text::markdown(body),
        reply_to,
        buttons,
        None,
        Some(false),
    );
    if let Some(message) = &message
        && seconds > 0
        && let Some(id) = message.get("id").and_then(Value::as_i64)
    {
        action::schedule_delete(rt, chat_id, id, seconds);
    }
    message
}

/// 消息的发送者（解析不出来时 `None`）。
pub fn sender_of(message: &Value) -> Option<Sender> {
    message_sender(message)
}
