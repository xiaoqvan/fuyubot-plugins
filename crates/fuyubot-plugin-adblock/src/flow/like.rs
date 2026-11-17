//! 自动点赞机器人：检测 + 处置（默认只封禁「回应权限」，可改为跟随广告的禁言 / 封禁）。
//!
//! 数据来源是 TDLib 的 updateMessageInteractionInfo：消息的反应变化会带着
//! messageReaction.recent_sender_ids（最多 3 个最近回应者）一起下发。每次把这些
//! 发送者交给 LikeGuard，由它按「最近消息里是否连续回应 5 条」判定。
//!
//! 最近消息不另存一份：cache 本来就有每群最近 20 条（recent_ids），被删除的消息
//! 也会从那里摘掉，所以删掉的消息不会把连续段隔开。
//!
//! 处置默认只关掉「回应」权限（`can_react_to_messages = false`，见
//! `api::Td::restrict_reactions`）：发言、链接预览这些照常，只是不能再点表情；
//! 群管理员可用 `/adconfig like_guard_mode ad` 改成跟随广告——按群配置的
//! `limit_mode` 禁言（`api::Td::restrict`）或封禁（`api::Td::ban`）。
//! 两种方式都会把这个已经点出去的回应一起清掉
//! （`deleteAllRecentMessageReactionsFromSender`）。管理员点提示上的「解除限制」
//! 可以恢复权限，被删掉的回应不会回来。
//!
//! 默认关闭：只有群管理员在 /adconfig 里打开 like_guard_enabled 之后才会统计与处置。

use serde_json::Value;

use crate::action;
use crate::api::{Sender, callback_row, member_param, unban_callback};
use crate::log;
use crate::runtime::Runtime;
use crate::simplify::Topic;
use crate::store::{GroupConfig, LikeGuardMode, LimitMode};
use crate::text;
use crate::util::now_ms;

/// 一次更新里最多看多少种反应类型（防御异常大的负载）。
const MAX_REACTION_TYPES: usize = 64;

/// 从 updateMessageInteractionInfo 里取出点过赞的用户。
///
/// 只认 messageSenderUser：频道身份没法「限制成员」，匿名反应本来也不带发送者。
pub fn reactors_of(update: &Value) -> Vec<i64> {
    let Some(reactions) = update
        .pointer("/interaction_info/reactions/reactions")
        .and_then(Value::as_array)
    else {
        return Vec::new();
    };
    let mut users = Vec::new();
    for reaction in reactions.iter().take(MAX_REACTION_TYPES) {
        let Some(senders) = reaction.get("recent_sender_ids").and_then(Value::as_array) else {
            continue;
        };
        for sender in senders {
            if sender.get("@type").and_then(Value::as_str) != Some("messageSenderUser") {
                continue;
            }
            let Some(user_id) = sender.get("user_id").and_then(Value::as_i64) else {
                continue;
            };
            // 0 是 TDLib 的「未知用户」；同一次更新里同一个人只记一次。
            if user_id != 0 && !users.contains(&user_id) {
                users.push(user_id);
            }
        }
    }
    users
}

/// 处理一条 updateMessageInteractionInfo。
pub async fn handle(rt: &Runtime, update: &Value) {
    let Some(chat_id) = update.get("chat_id").and_then(Value::as_i64) else {
        return;
    };
    let Some(message_id) = update.get("message_id").and_then(Value::as_i64) else {
        return;
    };
    let reactors = reactors_of(update);
    if reactors.is_empty() {
        return;
    }
    // 群配置里由管理员开启；没开的群连统计都不做（默认关闭）。
    let config = rt.db.config_or_default(chat_id).await;
    if !config.like_guard_enabled {
        return;
    }
    if !crate::flow::is_group(rt, chat_id).await {
        return;
    }
    // 没有「限制成员」权限就什么都做不了，先挡掉，省下每条反应的库查询。
    if !crate::flow::am_i_admin(rt, chat_id).await {
        return;
    }
    // 最近 20 条消息（cache 维护；被删除的已经摘掉）。
    let recent = rt.cache.recent_ids(chat_id);
    let me = rt.td.my_id().await;
    let now = now_ms();
    for user_id in reactors {
        // 自己（AI 审核时会给消息加 👀）不是机器人。
        if Some(user_id) == me {
            continue;
        }
        let Some(hit) = rt.like.observe(chat_id, user_id, message_id, &recent, now) else {
            continue;
        };
        punish(rt, chat_id, user_id, message_id, &hit, &config).await;
    }
}

/// 处置一个自动点赞账号：按群配置封禁回应权限 / 禁言 / 封禁 + 审计 + 三处通知。
async fn punish(
    rt: &Runtime,
    chat_id: i64,
    user_id: i64,
    message_id: i64,
    hit: &crate::like::Detection,
    config: &GroupConfig,
) {
    let sender = Sender::User(user_id);

    // 管理员与白名单成员不动：检测只看行为，处置要看身份。
    if config.whitelist.contains(&user_id) {
        return;
    }
    if rt.td.is_admin(chat_id, sender).await {
        log::debug(
            &rt.host,
            format!("[自动点赞] 群 {chat_id} 的 {user_id} 是管理员，跳过"),
        );
        return;
    }

    // 处置方式跟群配置走：默认只关「回应」权限（发言照旧）；改成「跟随广告」后
    // 按 limit_mode 禁言或封禁——与 action::punish 同一套语义。
    let (record_action, action_label, result) = match config.like_guard_mode {
        LikeGuardMode::Reactions => (
            "restrict",
            "已封禁回应权限（仍可发言）",
            rt.td.restrict_reactions(chat_id, sender).await,
        ),
        LikeGuardMode::Ad => match config.limit_mode {
            LimitMode::Ban => ("ban", "已封禁", rt.td.ban(chat_id, sender).await),
            LimitMode::Mute => ("mute", "已禁言", rt.td.restrict(chat_id, sender).await),
        },
    };
    if let Err(error) = result {
        log::warn(
            &rt.host,
            format!("[自动点赞] 处置 {user_id} 失败（群 {chat_id}，{action_label}）：{error}"),
        );
        return;
    }

    // 处置只是「以后不能再回应」；已经点出去的表情还挂在别人的消息上，
    // 一起清掉。TDLib `deleteAllRecentMessageReactionsFromSender`（1.8.69+）是
    // 管理员能力（要「删除消息」权限），只支持基础群 / 超级群；清理失败不回滚
    // 处置，记一行 debug 就够了。
    let reactions_cleared = match rt.td.delete_reactions_from_sender(chat_id, sender).await {
        Ok(()) => true,
        Err(error) => {
            log::debug(
                &rt.host,
                format!("[自动点赞] 清理 {user_id} 的回应失败（群 {chat_id}）：{error}"),
            );
            false
        }
    };

    let detail = hit.detail();
    rt.db
        .add_ban_record(
            chat_id,
            user_id,
            sender.type_name(),
            record_action,
            "reaction_bot",
            &detail,
            None,
        )
        .await;
    log::info(
        &rt.host,
        format!(
            "[自动点赞] 群 {chat_id} 用户 {user_id} {action_label}（{detail}）{}",
            if reactions_cleared {
                "，并已清理其回应"
            } else {
                ""
            }
        ),
    );

    let member = rt.td.sender_mention(sender, true).await;
    let chat_mention = rt.td.sender_mention(Sender::Chat(chat_id), true).await;
    let user_label = text::with_id(&member, user_id);
    let chat_label = text::with_id(&chat_mention, chat_id);
    let mut body = format!(
        "#ReactionBot\n**Group**: {chat_label}\n**User**: {user_label}\n**Reason**: 自动点赞（{detail}）\n**Action**: {action_label}"
    );
    if reactions_cleared {
        body.push_str("\n**Cleanup**: 已删除该用户在本群的回应");
    }
    let member_query = member_param(sender);
    // 封禁只能「解除封禁」；禁言 / 只封回应用「解除限制」，两种都能再改成 Ban。
    let buttons = if record_action == "ban" {
        vec![callback_row(
            "unban+白名单",
            unban_callback(chat_id, sender),
        )]
    } else {
        vec![
            callback_row("解除限制", format!("unmute?cid={chat_id}&{member_query}")),
            callback_row("Ban", format!("ban?cid={chat_id}&{member_query}")),
        ]
    };

    // 全局审计（可选）与群绑定的日志频道（带按钮，之后提示会链到这条）。
    if let Some(channel) = rt.cfg.log_channel {
        action::send_log(
            rt,
            channel,
            rt.cfg.log_topic.map(Topic::forum),
            &body,
            Vec::new(),
        )
        .await;
    }
    let mut log_message_id = None;
    if let Some(channel) = config.log_channel
        && let Some(message) = action::send_log(rt, channel, None, &body, buttons.clone()).await
    {
        log_message_id = message.get("id").and_then(Value::as_i64);
    }

    // 群内提示：与广告处置的 Ban/Mute 提示同款两行格式。
    let mut tips = tip_text(&member, &detail);
    if let (Some(channel), Some(id)) = (config.log_channel, log_message_id)
        && let Some(link) = rt.td.message_link(channel, id).await
    {
        tips.push_str(&format!("\n**LOG CHANNEL**: [Link]({link})"));
    }
    if reactions_cleared {
        tips.push_str("\n**Cleanup**: 已删除该用户在本群的回应");
    }
    let delete_after =
        (config.spam_tips && config.spam_tips_meg_delay > 0).then_some(config.spam_tips_meg_delay);
    let topic = rt.cache.topic_of(chat_id, message_id);
    let sent = rt
        .td
        .send_text(
            chat_id,
            text::markdown(&tips),
            None,
            buttons,
            topic,
            Some(false),
        )
        .await;
    if let (Some(message), Some(seconds)) = (sent, delete_after)
        && let Some(id) = message.get("id").and_then(Value::as_i64)
    {
        action::schedule_delete(rt, chat_id, id, seconds);
    }
}

/// 群内的回应封禁提示：`#ReactionBot` 标签 + 与广告处置同款的两行正文。
///
/// `member` 是 `sender_mention` 的产物（可点击的名字）；数字 ID 只出现在
/// 日志频道的 `ID:` 行里。删除时间与其它处置提示共用 spam_tips_meg_delay。
fn tip_text(member: &str, detail: &str) -> String {
    format!("#ReactionBot\n**User ID**:{member}\n**Reason**: reaction_bot {detail}")
}

#[cfg(test)]
#[path = "../../tests/unit/flow__like.rs"]
mod tests;
