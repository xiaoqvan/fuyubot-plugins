//! 新成员入群检查：名称 / 简介 / 主页挂载频道 查黑名单与 AI。
//!
//! 对应 TS 版 `utils/newMemberCheck.ts`。阈值保持原样：
//! - 黑名单命中 → 直接封禁；
//! - AI 命中且 `confidence >= 0.95` → 直接封禁；
//! - AI 命中但置信度不足 → **永久禁言 + 群内提示 + 「1 小时内复审否则封禁」**。
//!
//! 那条「禁言待复审」记录由 [`crate::flow::sweeper`] 定时兜底，`/重新审查` 按钮由
//! `callbacks::new_member` 处理。

use fuyubot_simplify::{ButtonStyle, ReplyButton};
use serde_json::Value;

use crate::ai::{self, AiMeta, ModerationInput};
use crate::api::{Sender, callback_row, unban_callback};
use crate::flow;
use crate::log;
use crate::runtime::Runtime;
use crate::store::doc_id;
use crate::text;
use crate::util::now_ms;

/// 「重新审查」按钮的自定义 emoji（与原版一致）。
const BAN_EMOJI_ID: i64 = 6_122_821_483_581_416_154;
/// 复审期限：1 小时。
const REVIEW_DEADLINE_MS: i64 = 60 * 60 * 1000;

/// 处理 `updateChatMember`。
pub fn handle_chat_member(rt: &Runtime, update: &Value) {
    let Some(member) = update.get("new_chat_member") else {
        return;
    };
    let status = member
        .get("status")
        .and_then(|status| status.get("@type"))
        .and_then(Value::as_str);
    if status != Some("chatMemberStatusMember") {
        return;
    }
    let Some(chat_id) = update.get("chat_id").and_then(Value::as_i64) else {
        return;
    };
    if !flow::is_group(rt, chat_id) {
        return;
    }
    let config = rt.db.config_or_default(chat_id);
    if !config.check_new_member {
        return;
    }
    // 只管「能不能限制成员」：删消息不参与入群检查。
    if !rt.td.am_i_admin(chat_id, &["can_restrict_members"]) {
        return;
    }
    let Some(user_id) = member
        .get("member_id")
        .and_then(|sender| sender.get("user_id"))
        .and_then(Value::as_i64)
    else {
        return;
    };
    let Ok(user) = rt.td.user(user_id) else {
        return;
    };
    let full = rt.td.user_full(user_id).ok();
    let name = format!(
        "{}{}",
        user.get("first_name").and_then(Value::as_str).unwrap_or(""),
        user.get("last_name").and_then(Value::as_str).unwrap_or("")
    );
    let bio = full
        .as_ref()
        .and_then(|full| full.get("bio"))
        .and_then(|bio| bio.get("text"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();

    // ① 黑名单优先
    if let Some(record) = rt.db.match_name(&name) {
        ban_new_member(
            rt,
            chat_id,
            user_id,
            &format!("名称匹配黑名单 (ID: {})", doc_id(&record)),
            1.0,
        );
        return;
    }
    if !bio.is_empty()
        && let Some(record) = rt.db.match_bio(&bio)
    {
        ban_new_member(
            rt,
            chat_id,
            user_id,
            &format!("简介匹配黑名单关键词 (ID: {})", doc_id(&record)),
            1.0,
        );
        return;
    }

    // ② 登记 24h 监视：窗口内改资料 → AI 复检；改过资料后再发消息强制文本审
    rt.db.upsert_watch(chat_id, user_id, &name, &bio);

    // ③ AI 检查资料
    let channel = flow::personal_channel(rt, user_id);
    if bio.trim().is_empty() && channel.is_none() {
        log::info(
            &rt.host,
            format!(
                "新成员({user_id}) 在群({chat_id}) bio为空且无主页频道，跳过AI检查，仅登记24h监视"
            ),
        );
        return;
    }
    let mut input = ModerationInput::profile(name.clone(), bio.clone());
    if let Some((channel_name, description)) = &channel {
        input = input.with_personal_channel(channel_name.clone(), description.clone());
    }
    let result = ai::review(
        rt,
        &input,
        None,
        Some(chat_id),
        &AiMeta {
            scene: Some("new_member"),
            message_type: None,
            force_default: false,
        },
    );

    if result.profile_hit() && result.confidence > 0.0 {
        let reason = if result.reason.is_empty() {
            "AI检测到资料含广告".to_owned()
        } else {
            result.reason.clone()
        };
        if result.confidence >= 0.95 {
            ban_new_member(rt, chat_id, user_id, &reason, result.confidence);
        } else {
            mute_new_member(rt, chat_id, user_id, &name, &reason, result.confidence);
        }
    }
}

/// 永久禁言 + 群内提示 + 复审记录。
pub fn mute_new_member(
    rt: &Runtime,
    chat_id: i64,
    user_id: i64,
    name: &str,
    reason: &str,
    confidence: f64,
) {
    let sender = Sender::User(user_id);
    if let Err(error) = rt.td.restrict(chat_id, sender) {
        log::warn(&rt.host, format!("禁言新成员 {user_id} 失败：{error}"));
        return;
    }
    let user_mention = format!(
        "[{}](tg://user?id={user_id})",
        if name.trim().is_empty() {
            user_id.to_string()
        } else {
            name.to_owned()
        }
    );
    let body = format!(
        "{user_mention}，你的个人信息中有疑似广告内容，请更改你的个人信息。\n\n**原因**: {reason}\n**置信度**: {}\n\n📝 修改后请点击下方按钮重新审查，**1小时内**未修改并通过复审将被封禁。",
        text::confidence_percent(confidence)
    );
    let review_data = format!("newMemberReview?cid={chat_id}&u={user_id}");
    let ban_data = format!("ban?cid={chat_id}&u={user_id}");
    let buttons: Vec<Vec<ReplyButton>> = vec![
        callback_row("🔄 重新审查", review_data),
        vec![
            ReplyButton::callback("Ban", ban_data)
                .style(ButtonStyle::Danger)
                .emoji(BAN_EMOJI_ID),
        ],
    ];
    let message = rt.td.send_text(
        chat_id,
        text::markdown(&body),
        None,
        buttons,
        None,
        Some(false),
    );
    let message_id = message
        .as_ref()
        .and_then(|message| message.get("id"))
        .and_then(Value::as_i64);

    // 复审记录：1 小时没通过就封（sweeper 兜底）
    rt.db.upsert_mute_review(
        chat_id,
        user_id,
        message_id,
        reason,
        confidence,
        now_ms() + REVIEW_DEADLINE_MS,
    );

    // 群绑定的日志频道
    let config = rt.db.config_or_default(chat_id);
    if let Some(channel) = config.log_channel {
        let chat_mention = rt.td.sender_mention(Sender::Chat(chat_id), true);
        let body = format!(
            "#Mute #welcome\n**User**: {user_mention}({user_id})\n**Chat**: {chat_mention}({chat_id})\n**Action**: 资料疑似广告，已禁言待复审\n**Confidence**: {}\n**Reason**: {reason}\n**Deadline**: 1小时内需修改并通过复审",
            text::confidence_percent(confidence)
        );
        let _ = rt.td.send_text(
            channel,
            text::markdown(&body),
            None,
            Vec::new(),
            None,
            Some(true),
        );
    }
    log::info(
        &rt.host,
        format!("新成员({user_id}) 因资料含广告被永久禁言 (confidence: {confidence})"),
    );
}

/// 封禁新成员并写审计 + 日志频道。
pub fn ban_new_member(rt: &Runtime, chat_id: i64, user_id: i64, reason: &str, confidence: f64) {
    let sender = Sender::User(user_id);
    if let Err(error) = rt.td.ban(chat_id, sender) {
        // 封禁没成功就不写记录、也不发「已封禁」日志：`/qban` 与日志频道不能谎报。
        log::warn(&rt.host, format!("封禁新成员 {user_id} 失败：{error}"));
        return;
    }
    rt.db.add_ban_record(
        chat_id,
        user_id,
        sender.type_name(),
        "ban",
        "profile_ad",
        reason,
        None,
    );
    log::info(
        &rt.host,
        format!("新成员({user_id}) 因资料含广告被自动封禁 (confidence: {confidence})"),
    );

    let config = rt.db.config_or_default(chat_id);
    if let Some(channel) = config.log_channel {
        let chat_mention = rt.td.sender_mention(Sender::Chat(chat_id), true);
        let user_mention = rt.td.sender_mention(sender, true);
        let body = format!(
            "#Ban #welcome\n**ID**: {user_mention}({user_id})\n**Chat**: {chat_mention}({chat_id})\n**Reason**: Spam detected\n**Detail**:\n - Type: profile_ad\n - Reason: {reason} ({})",
            text::confidence_percent(confidence)
        );
        let buttons: Vec<Vec<ReplyButton>> = vec![callback_row(
            "unban+白名单",
            unban_callback(chat_id, sender),
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
}
