//! `newMemberReview`：被禁言用户点「🔄 重新审查」。
//!
//! 通过条件与 TS 版一致：`confidence < 0.8` 或不再命中资料广告。
//! 通过后做三件事——解除禁言、标记复审已通过（sweeper 不会再来封）、标记入群监视
//! 已过审（之后改资料也不再因弱信号被禁言）。

use crate::simplify::{ButtonStyle, ReplyButton};
use serde_json::Value;

use crate::ai::{self, AiMeta, ModerationInput};
use crate::api::Sender;
use crate::callbacks::{CallbackCtx, answer};
use crate::flow;
use crate::log;
use crate::runtime::Runtime;
use crate::text;
use crate::util::param_i64;

const BAN_EMOJI_ID: i64 = 6_122_821_483_581_416_154;

pub fn new_member_review(rt: &Runtime, ctx: &CallbackCtx) {
    let Some(group) = param_i64(&ctx.params, "cid") else {
        return;
    };
    let Some(user_id) = param_i64(&ctx.params, "u") else {
        return;
    };

    let is_target = ctx.sender_user_id == user_id;
    let is_admin = rt.td.is_admin(group, Sender::User(ctx.sender_user_id));
    if !is_target && !is_admin {
        answer(rt, ctx, "你没有权限使用该按钮", true, 3600);
        return;
    }
    let operator_label = if is_admin && !is_target {
        "管理员代点"
    } else {
        "本人点击"
    };

    let Some(user) = rt.td.user(user_id).ok() else {
        answer(rt, ctx, "无法获取用户信息", true, 60);
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
    let user_mention = format!(
        "[{}](tg://user?id={user_id})",
        if name.trim().is_empty() {
            user_id.to_string()
        } else {
            name.clone()
        }
    );

    let channel = flow::personal_channel(rt, user_id);
    let mut input = ModerationInput::profile(name.clone(), bio.clone());
    if let Some((channel_name, description)) = &channel {
        input = input.with_personal_channel(channel_name.clone(), description.clone());
    }
    let result = ai::review(
        rt,
        &input,
        None,
        Some(group),
        &AiMeta {
            scene: Some("new_member_review"),
            message_type: None,
            force_default: false,
        },
    );

    let still_ad = result.profile_hit() && result.confidence >= 0.8;
    let confidence_pct = text::confidence_percent(result.confidence);
    let reason = if result.reason.is_empty() {
        "AI检测到资料含广告".to_owned()
    } else {
        result.reason.clone()
    };

    let chat_mention = rt.td.sender_mention(Sender::Chat(group), true);
    let chat_name = rt.td.chat_title(group);

    if still_ad {
        let fail_text = format!(
            "❌ {user_mention} 的复审未通过，仍疑似含广告，请修改资料后重试。\n\n**原因**: {reason}\n**置信度**: {confidence_pct}\n**操作**: {operator_label}({})",
            ctx.sender_user_id
        );
        let buttons: Vec<Vec<ReplyButton>> = vec![
            vec![ReplyButton::callback(
                "🔄 重新审查",
                format!("newMemberReview?cid={group}&u={user_id}"),
            )],
            vec![
                ReplyButton::callback("Ban", format!("ban?cid={group}&u={user_id}"))
                    .style(ButtonStyle::Danger)
                    .emoji(BAN_EMOJI_ID),
            ],
        ];
        let edited = rt.td.edit_text(
            ctx.chat_id,
            ctx.message_id,
            text::markdown(
                &format!(
                    "{user_mention}，你的个人信息中有疑似广告内容，请更改你的个人信息。\n\n**最近一次复审**: ❌ 未通过 ({confidence_pct})\n**原因**: {reason}\n\n📝 修改后请点击下方按钮重新审查，**1小时内**未修改并通过复审将被封禁。"
                ),
            ),
            buttons,
            Some(true),
        );
        if edited.is_none() {
            // 原消息可能已经被删：退回群里新发一条
            let _ = rt.td.send_text(
                group,
                text::markdown(&fail_text),
                None,
                Vec::new(),
                None,
                Some(true),
            );
        }
        send_review_log(
            rt,
            group,
            vec![
                "#Mute #welcome #reviewFail".to_owned(),
                format!("**User**: {user_mention}({user_id})"),
                format!("**Chat**: {chat_mention}({group})"),
                "**Action**: 复审未通过，保持禁言".to_owned(),
                format!("**Operator**: {operator_label}({})", ctx.sender_user_id),
                format!("**Confidence**: {confidence_pct}"),
                format!("**Reason**: {reason}"),
            ],
        );
        answer(
            rt,
            ctx,
            &format!(
                "❌ 复审未通过：仍疑似含广告，请修改后再次点击\"重新审查\"（5分钟内无法重复点击）。（{confidence_pct}）"
            ),
            true,
            300,
        );
        log::info(
            &rt.host,
            format!(
                "用户({user_id}) 在群({group}) 点击重新审查，复审未通过 (confidence: {}) operator={}",
                result.confidence, ctx.sender_user_id
            ),
        );
        return;
    }

    // ── 通过：解除禁言 ────────────────────────────────────────
    let member = Sender::User(user_id);
    if let Err(error) = rt.td.unban(group, member) {
        log::warn(&rt.host, format!("复审通过但解除禁言失败：{error}"));
    }
    rt.db.mark_mute_review_approved(group, user_id);
    rt.db.mark_watch_review_approved(group, user_id);

    let _ = rt.td.edit_text(
        ctx.chat_id,
        ctx.message_id,
        text::markdown(&format!(
            "✅ {user_mention} 的个人信息复审已通过，已解除禁言。\n**操作**: {operator_label}({})",
            ctx.sender_user_id
        )),
        Vec::new(),
        Some(true),
    );
    answer(rt, ctx, "✅ 复审通过，已解除禁言", false, 10);
    crate::action::schedule_delete(rt, ctx.chat_id, ctx.message_id, 10);

    send_review_log(
        rt,
        group,
        vec![
            "#Unmute #welcome #reviewPass".to_owned(),
            format!("**User**: {user_mention}({user_id})"),
            format!("**Chat**: {chat_mention}({group})"),
            "**Action**: 复审通过，已解除禁言".to_owned(),
            format!("**Operator**: {operator_label}({})", ctx.sender_user_id),
            format!("**Confidence**: {confidence_pct}"),
            "**Reason**: 资料已修改，AI复审未检出广告".to_owned(),
        ],
    );
    log::info(
        &rt.host,
        format!(
            "用户({user_id}) 在群({group}) 复审通过，已解除禁言 operator={}",
            ctx.sender_user_id
        ),
    );
    let _ = chat_name;
}

/// 往群绑定的日志频道写复审记录。
fn send_review_log(rt: &Runtime, group: i64, lines: Vec<String>) {
    let config = rt.db.config_or_default(group);
    let Some(channel) = config.log_channel else {
        return;
    };
    let _ = rt.td.send_text(
        channel,
        text::markdown(&lines.join("\n")),
        None,
        Vec::new(),
        None,
        Some(true),
    );
}
