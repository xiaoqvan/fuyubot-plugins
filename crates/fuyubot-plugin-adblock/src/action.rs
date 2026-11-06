//! 处置动作：删除 + 禁言/封禁 + 三处通知（全局审计、群日志频道、群内提示）。
//!
//! 对应 TS 版 `Ads/index.ts` 的 `spamMegMuteuser`。行为逐条对齐：
//!
//! 1. 撤回触发消息；
//! 2. 把这个人在本群**所有**消息一起撤掉（索引来自 `message_records`，48 小时窗口）；
//! 3. 按群配置的 `limit_mode` 禁言（默认）或封禁；**频道/群组身份发的一律封禁**——
//!    禁言对一个频道没有任何意义；
//! 4. 写 `ban_records`（`/qban` 靠它追溯）；
//! 5. 发全局审计频道（可选）、群绑定的日志频道（带按钮）、群内提示（带按钮 + 自删）。
//!
//! 提示消息的自删走 `Runtime::after`（tokio 定时任务）而不是 `sleep`：处置流程还在
//! tokio 任务里，就地睡会白占一个 worker。
//!
//! 整个模块是 `async` 的：TDLib 往返与 Mongo 调用都 `.await`，不占线程干等。

use std::time::Duration;

use crate::simplify::Topic;
use serde_json::Value;

use crate::api::{Sender, callback_row, member_param, unban_callback};
use crate::detect::SpamHit;
use crate::log;
use crate::runtime::Runtime;
use crate::store::LimitMode;
use crate::text;

/// 处置一个命中广告的发送者。
///
/// `delete_message_id` 是要撤回的触发消息；`None` 表示只处置人不删消息
/// （比如 `/spam` 举报里消息已经被删过）。
pub async fn punish(
    rt: &Runtime,
    chat_id: i64,
    sender: Sender,
    hit: &SpamHit,
    delete_message_id: Option<i64>,
) {
    let config = rt.db.config_or_default(chat_id).await;

    // ① 触发消息
    if let Some(message_id) = delete_message_id
        && rt.td.message(chat_id, message_id).await.is_some()
    {
        let _ = rt.td.delete_messages(chat_id, &[message_id]).await;
    }

    // ② 这个人在本群的历史消息（48 小时内的索引）
    let ids: Vec<i64> = rt
        .db
        .user_message_ids(chat_id, sender.id())
        .await
        .into_iter()
        .filter(|id| Some(*id) != delete_message_id)
        .collect();
    if !ids.is_empty() {
        for chunk in ids.chunks(100) {
            if let Err(error) = rt.td.delete_messages(chat_id, chunk).await {
                log::debug(&rt.host, format!("批量撤回历史消息失败：{error}"));
            }
        }
        rt.db.remove_message_ids(chat_id, &ids).await;
    }

    // ③ 禁言还是封禁：频道/群组身份没有「禁言」这一说，统一封禁。
    let is_chat_sender = matches!(sender, Sender::Chat(_));
    let action_is_ban = config.limit_mode == LimitMode::Ban || is_chat_sender;
    let result = if action_is_ban {
        rt.td.ban(chat_id, sender).await
    } else {
        rt.td.restrict(chat_id, sender).await
    };
    if let Err(error) = result {
        log::warn(
            &rt.host,
            format!(
                "{} {} 失败：{error}",
                if action_is_ban { "封禁" } else { "禁言" },
                sender.id()
            ),
        );
    }

    // ④ 审计记录
    rt.db
        .add_ban_record(
            chat_id,
            sender.id(),
            sender.type_name(),
            if action_is_ban { "ban" } else { "mute" },
            &hit.kind,
            &hit.details,
            hit.id.as_deref(),
        )
        .await;

    // ⑤ 文案与按钮
    let member = rt.td.sender_mention(sender, true).await;
    let chat_mention = rt.td.sender_mention(Sender::Chat(chat_id), true).await;
    let title = rt.td.chat_title(chat_id).await;
    let detail = match &hit.id {
        Some(id) => format!("\n - Type: {}\n - ID: `{id}`", hit.kind),
        None => format!("\n - Type: {}\n - Reason: {}", hit.kind, hit.details),
    };
    let log_text = format!(
        "#{}\n**ID**: {member}({})\n**Chat ID**: {chat_mention}({title})\n**Reason**: Spam detected\n**Detail**:{detail}",
        if action_is_ban { "Ban" } else { "Mute" },
        sender.id()
    );
    let member_query = member_param(sender);
    let buttons = if action_is_ban {
        vec![callback_row(
            "unban+白名单",
            unban_callback(chat_id, sender),
        )]
    } else {
        vec![
            callback_row("unmute", format!("unmute?cid={chat_id}&{member_query}")),
            callback_row("Ban", format!("ban?cid={chat_id}&{member_query}")),
        ]
    };

    // ⑥ 全局审计频道（可配；原版是写死的频道 + 话题）
    if let Some(channel) = rt.cfg.log_channel {
        send_log(
            rt,
            channel,
            rt.cfg.log_topic.map(Topic::forum),
            &log_text,
            Vec::new(),
        )
        .await;
    }

    // ⑦ 群绑定的日志频道（带按钮，之后提示消息会链到这条）
    let mut log_message_id = None;
    if let Some(channel) = config.log_channel {
        if let Some(message) = send_log(rt, channel, None, &log_text, buttons.clone()).await {
            log_message_id = message.get("id").and_then(Value::as_i64);
        }
    }

    // ⑧ 群内提示（默认开，默认 30 秒后自删）。
    // `spam_tips=false` 的语义是「提示常驻不自动删」，**不是「不发提示」**——
    // TS 版永远发提示，只把自删挂在这个开关上。
    let mut tips = format!(
        "**{} ID**:{member}\n**Reason**: {}{}",
        if action_is_ban { "Ban" } else { "Mute" },
        hit.kind,
        if hit.details.is_empty() {
            String::new()
        } else {
            format!(" {}", hit.details)
        }
    );
    if let (Some(channel), Some(id)) = (config.log_channel, log_message_id)
        && let Some(link) = rt.td.message_link(channel, id).await
    {
        tips.push_str(&format!("\n**LOG CHANNEL**: [Link]({link})"));
    }
    if let Some(message) = send_log(rt, chat_id, None, &tips, buttons).await {
        let message_id = message.get("id").and_then(Value::as_i64).unwrap_or(0);
        if message_id != 0 && config.spam_tips && config.spam_tips_meg_delay > 0 {
            schedule_delete(rt, chat_id, message_id, config.spam_tips_meg_delay);
        }
    }
}

/// 发一条格式化消息；失败只记 debug（处置已经做完了，通知失败不该回滚）。
async fn send_log(
    rt: &Runtime,
    chat_id: i64,
    topic: Option<Topic>,
    text_body: &str,
    buttons: Vec<Vec<crate::simplify::ReplyButton>>,
) -> Option<Value> {
    let message = rt
        .td
        .send_text(
            chat_id,
            text::markdown(text_body),
            None,
            buttons,
            topic,
            Some(false),
        )
        .await;
    if message.is_none() {
        log::debug(&rt.host, format!("通知 {chat_id} 发送失败"));
    }
    message
}

/// 到点删一条消息（提示自删、复审提示自删）。
///
/// 排期本身就是 `Runtime::after`（tokio 定时任务）；`rt` 得是 `Arc` 才能被任务捕获。
pub fn schedule_delete(rt: &Runtime, chat_id: i64, message_id: i64, delay_seconds: u32) {
    rt.after(
        Duration::from_secs(delay_seconds as u64),
        move |rt| async move {
            if let Err(error) = rt.td.delete_messages(chat_id, &[message_id]).await {
                // 消息可能已经被管理员手工删掉：这不是错误。
                log::debug(&rt.host, format!("延时删除 {message_id} 失败：{error}"));
            }
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detail_prefers_record_id() {
        let hit = SpamHit::hit("Text", Some("abc".to_owned()), "hasAdKeyword");
        let detail = match &hit.id {
            Some(id) => format!("\n - Type: {}\n - ID: `{id}`", hit.kind),
            None => format!("\n - Type: {}\n - Reason: {}", hit.kind, hit.details),
        };
        assert!(detail.contains("ID: `abc`"));
        assert!(!detail.contains("Reason"));
    }
}
