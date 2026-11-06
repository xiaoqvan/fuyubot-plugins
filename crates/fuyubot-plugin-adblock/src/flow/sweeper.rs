//! 禁言超时扫描：被禁言后 1 小时内没通过复审 → 封禁。
//!
//! 对应 TS 版 `utils/muteReviewSweeper.ts`，由插件的周期任务每 5 分钟调一次。
//!
//! 两条竞态规则（原样保留）：
//! - 记录已经不是 `pending`（用户刚点了复审通过）→ **不封**；
//! - 用户已经不在「受限」状态（自己退群 / 已被解禁 / 已被封）→ 标记通过，不再处理。

use crate::simplify::ReplyButton;

use crate::action;
use crate::api::{Sender, callback_row, unban_callback};
use crate::log;
use crate::runtime::Runtime;
use crate::text;
use crate::util::now_ms;

/// 扫一遍到期的复审记录。
pub fn sweep(rt: &Runtime) {
    let records = rt.db.expired_mute_reviews(now_ms());
    if records.is_empty() {
        return;
    }
    log::debug(&rt.host, format!("复审扫描：{} 条到期", records.len()));

    for record in records {
        let sender = Sender::User(record.user_id);
        let status = rt.td.member_status(record.chat_id, sender);
        let status = status.as_deref();

        // 不在群里 / 已经解禁 / 已经封禁 / 查不到 → 无从封禁，标记通过收尾。
        if status.is_none()
            || matches!(
                status,
                Some("chatMemberStatusMember")
                    | Some("chatMemberStatusLeft")
                    | Some("chatMemberStatusBanned")
            )
        {
            rt.db
                .mark_mute_review_approved(record.chat_id, record.user_id);
            continue;
        }

        // `pending → banned` 的原子转移：转移失败说明刚复审通过，别抢着封。
        if !rt
            .db
            .mark_mute_review_banned(record.chat_id, record.user_id)
        {
            log::info(
                &rt.host,
                format!(
                    "用户({}) 在群({}) 到期扫描时记录已非 pending，跳过封禁",
                    record.user_id, record.chat_id
                ),
            );
            continue;
        }

        if let Err(error) = rt.td.ban(record.chat_id, sender) {
            // 与 TS 的 try/catch 一致：封禁失败就跳过这条的审计与通知，别谎报。
            log::warn(
                &rt.host,
                format!("复审超时封禁 {} 失败：{error}", record.user_id),
            );
            continue;
        }
        rt.db.add_ban_record(
            record.chat_id,
            record.user_id,
            sender.type_name(),
            "ban",
            "mute_review_timeout",
            &format!("禁言1小时内未通过复审: {}", record.reason),
            None,
        );
        log::info(
            &rt.host,
            format!(
                "用户({}) 在群({}) 禁言超时未通过复审，已封禁",
                record.user_id, record.chat_id
            ),
        );

        // 改原提示：让群里看到「最终结果」
        if let Some(message_id) = record.message_id {
            let _ = rt.td.edit_text(
                record.chat_id,
                message_id,
                text::markdown(&format!(
                    "🚫 {} 因 1 小时内未修改个人信息并通过复审，已被封禁。",
                    record.user_id
                )),
                Vec::new(),
                Some(false),
            );
        }

        // 日志频道
        let config = rt.db.config_or_default(record.chat_id);
        if let Some(channel) = config.log_channel {
            let chat_mention = rt.td.sender_mention(Sender::Chat(record.chat_id), true);
            let user_mention = rt.td.sender_mention(sender, true);
            let body = format!(
                "#Ban #welcome #timeout\n**ID**: {user_mention}({})\n**Chat**: {chat_mention}({})\n**Reason**: 禁言1小时内未通过复审\n**Detail**:\n - Type: profile_ad\n - Reason: {} ({})",
                record.user_id,
                record.chat_id,
                record.reason,
                text::confidence_percent(record.confidence)
            );
            let buttons: Vec<Vec<ReplyButton>> = vec![callback_row(
                "unban+白名单",
                unban_callback(record.chat_id, sender),
            )];
            let _ = rt.td.send_text(
                channel,
                text::markdown(&body),
                None,
                buttons,
                None,
                Some(true),
            );
        }

        // 10 秒后删掉群里的提示
        if let Some(message_id) = record.message_id {
            action::schedule_delete(rt, record.chat_id, message_id, 10);
        }
    }
}

/// 供周期任务调用：吞掉异常，扫描失败不该打断后续扫描。
pub fn sweep_safely(rt: &Runtime) {
    if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| sweep(rt))).is_err() {
        log::warn(&rt.host, "复审扫描 panic，已跳过本轮");
    }
}
