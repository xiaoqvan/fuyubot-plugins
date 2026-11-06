//! `unban` / `unmute` / `ban` / `banCnt` 四个按钮。
//!
//! 三个真实动作都会：先回 callback（否则客户端一直转圈）→ 改原消息 → 写日志频道 →
//! 10 秒后删掉群里的提示。顺序是有意的：**先回弹窗，再做慢操作**，用户不会觉得按钮没反应。

use crate::simplify::ReplyButton;

use crate::action;
use crate::api::{Sender, callback_row, unban_callback};
use crate::callbacks::{
    CallbackCtx, answer, member_of, require_admin, send_group_log, send_group_log_with_buttons,
};
use crate::log;
use crate::runtime::Runtime;
use crate::text;

/// `unban?cid=…&u=…`：解除封禁（并给用户加白名单）。
pub async fn unban(rt: &Runtime, ctx: &CallbackCtx) {
    let Some(group) = ctx.group_id() else {
        return;
    };
    let Some(member) = member_of(&ctx.params) else {
        return;
    };
    if !require_admin(rt, ctx, group).await {
        return;
    }
    if let Err(error) = rt.td.unban(group, member).await {
        log::warn(&rt.host, format!("解除封禁失败：{error}"));
        answer(rt, ctx, "解除封禁失败", true, 300).await;
        return;
    }
    // 只有**用户**进入白名单；频道/群组身份只解封（TS `unbanWhitelist.ts` 同款）。
    // 否则解封一个广告频道会把它永久写进 `Whitelist`，之后它的消息全部跳过检测。
    let whitelisted = matches!(member, Sender::User(_));
    if whitelisted {
        rt.db.set_whitelisted(group, member.id(), true).await;
    }
    answer(
        rt,
        ctx,
        if whitelisted {
            "该用户已解除封禁，并加入群白名单"
        } else {
            "该用户已被解除封禁"
        },
        false,
        10,
    )
    .await;

    let chat_mention = rt.td.sender_mention(Sender::Chat(group), true).await;
    let title = rt.td.chat_title(group).await;
    let target = rt.td.sender_mention(member, true).await;
    let admin = rt
        .td
        .sender_mention(Sender::User(ctx.sender_user_id), true)
        .await;
    let body = format!(
        "#Unban\n**Group**: {chat_mention}({title})\n**User**: {target}({})\n**Admin**: {admin}({})\n**Action**: 已解除封禁{}",
        member.id(),
        ctx.sender_user_id,
        if whitelisted {
            " 并已加入群白名单"
        } else {
            ""
        }
    );
    let _ = rt
        .td
        .edit_text(
            ctx.chat_id,
            ctx.message_id,
            text::markdown(&body),
            Vec::new(),
            Some(true),
        )
        .await;
    send_group_log(rt, group, &body).await;
    action::schedule_delete(rt, ctx.chat_id, ctx.message_id, 10);
}

/// `unmute?cid=…&u=…`：解除限制（按当前成员状态给出不同提示）。
pub async fn unmute(rt: &Runtime, ctx: &CallbackCtx) {
    let Some(group) = ctx.group_id() else {
        return;
    };
    let Some(member) = member_of(&ctx.params) else {
        return;
    };
    if !require_admin(rt, ctx, group).await {
        return;
    }
    let status = rt.td.member_status(group, member).await;
    match status.as_deref() {
        Some("chatMemberStatusMember") => {
            answer(rt, ctx, "该用户已是普通成员", true, 10).await;
            return;
        }
        Some("chatMemberStatusBanned") => {
            answer(rt, ctx, "该用户已被封禁", true, 10).await;
            return;
        }
        Some("chatMemberStatusLeft") => {
            answer(rt, ctx, "该用户已离开群组", true, 10).await;
            return;
        }
        Some("chatMemberStatusRestricted") => {}
        _ => return,
    }

    if let Err(error) = rt.td.unban(group, member).await {
        log::warn(&rt.host, format!("解除限制失败：{error}"));
        answer(rt, ctx, "解除限制失败", true, 300).await;
        return;
    }
    answer(rt, ctx, "该用户已被解除限制", false, 10).await;
    let _ = rt
        .td
        .edit_text(
            ctx.chat_id,
            ctx.message_id,
            text::plain("该用户已被解除限制"),
            Vec::new(),
            Some(true),
        )
        .await;
    if group == ctx.chat_id {
        action::schedule_delete(rt, ctx.chat_id, ctx.message_id, 10);
    }

    let chat_mention = rt.td.sender_mention(Sender::Chat(group), true).await;
    let title = rt.td.chat_title(group).await;
    let target = rt.td.sender_mention(member, true).await;
    let admin = rt
        .td
        .sender_mention(Sender::User(ctx.sender_user_id), true)
        .await;
    let body = format!(
        "#Unmute\n**Group**: {chat_mention}({title})\n**User**: {target}({})\n**Admin**: {admin}({})",
        member.id(),
        ctx.sender_user_id
    );
    send_group_log(rt, group, &body).await;
}

/// `ban?cid=…&u=…`：管理员点「Ban」。
pub async fn ban(rt: &Runtime, ctx: &CallbackCtx) {
    let Some(group) = ctx.group_id() else {
        return;
    };
    let Some(member) = member_of(&ctx.params) else {
        return;
    };
    if !require_admin(rt, ctx, group).await {
        return;
    }
    if let Err(error) = rt.td.ban(group, member).await {
        log::warn(&rt.host, format!("封禁用户失败：{error}"));
        answer(rt, ctx, "封禁用户失败", true, 300).await;
        return;
    }
    rt.db
        .add_ban_record(
            group,
            member.id(),
            member.type_name(),
            "ban",
            "admin_ban_button",
            &format!("管理员 {} 点击 Ban 按钮", ctx.sender_user_id),
            None,
        )
        .await;
    answer(rt, ctx, "该用户已被封禁", false, 10).await;

    let chat_mention = rt.td.sender_mention(Sender::Chat(group), true).await;
    let title = rt.td.chat_title(group).await;
    let target = rt.td.sender_mention(member, true).await;
    let admin = rt
        .td
        .sender_mention(Sender::User(ctx.sender_user_id), true)
        .await;
    let body = format!(
        "#Ban\n**Group**: {chat_mention}({title})\n**User**: {target}({})\n**Admin**: {admin}({})\n**Reason**: Spam detected",
        member.id(),
        ctx.sender_user_id
    );
    let buttons: Vec<Vec<ReplyButton>> =
        vec![callback_row("unban+白名单", unban_callback(group, member))];
    let _ = rt
        .td
        .edit_text(
            ctx.chat_id,
            ctx.message_id,
            text::markdown(&body),
            buttons.clone(),
            Some(true),
        )
        .await;
    send_group_log_with_buttons(rt, group, &body, buttons).await;
    action::schedule_delete(rt, ctx.chat_id, ctx.message_id, 10);
}

/// `banCnt?…`：原版就没实现（按钮数据里带联系人 ID，但处理函数是空的）。
pub async fn not_implemented(rt: &Runtime, ctx: &CallbackCtx) {
    answer(rt, ctx, "功能暂未实现", true, 0).await;
}
