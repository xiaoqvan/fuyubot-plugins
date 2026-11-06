//! `SpamSet` / `SpamFile` / `SpamPhoto`：管理员在「存疑」提示上补一刀。
//!
//! 三个按钮的共同点：**先把对象拉黑，再封人**。这样即使封禁失败（权限/网络），
//! 下一次同类内容还是会被关键词/文件/贴纸规则拦住。

use crate::simplify::ReplyButton;
use serde_json::Value;

use crate::action;
use crate::api::{Sender, callback_row, unban_callback};
use crate::callbacks::{
    CallbackCtx, answer, member_of, require_admin, send_group_log_with_buttons,
};
use crate::log;
use crate::runtime::Runtime;
use crate::text;
use crate::util::{base64_decode, param_str};

/// `SpamSet?&u=…&set=<set_id>`：封贴纸集。
pub async fn spam_set(rt: &Runtime, ctx: &CallbackCtx) {
    let group = ctx.chat_id;
    let Some(set_id) = param_str(&ctx.params, "set").map(str::to_owned) else {
        answer(rt, ctx, "出现错误", true, 300).await;
        return;
    };
    let Some(member) = member_of(&ctx.params) else {
        return;
    };
    if !require_admin(rt, ctx, group).await {
        return;
    }
    rt.db.set_spam_sticker(group, &set_id, true).await;
    finish(
        rt,
        ctx,
        group,
        member,
        "spam_sticker_set",
        &format!("贴纸集举报封禁 set={set_id} admin={}", ctx.sender_user_id),
        "该用户和贴纸已在本群被封禁",
    )
    .await;
}

/// `SpamFile?u=…&f=<unique_id>`：封文件。
pub async fn spam_file(rt: &Runtime, ctx: &CallbackCtx) {
    let group = ctx.chat_id;
    let Some(file_id) = param_str(&ctx.params, "f").map(str::to_owned) else {
        answer(rt, ctx, "出现错误", true, 300).await;
        return;
    };
    let Some(member) = member_of(&ctx.params) else {
        return;
    };
    if !require_admin(rt, ctx, group).await {
        return;
    }
    rt.db.set_spam_file(group, &file_id, true).await;
    finish(
        rt,
        ctx,
        group,
        member,
        "spam_file",
        &format!("文件举报封禁 file={file_id} admin={}", ctx.sender_user_id),
        "该用户和文件已在本群被封禁",
    )
    .await;
}

/// `SpamPhoto?u=…`：从提示消息的 `data:` 里取出图片 unique_id 列表再封。
pub async fn spam_photo(rt: &Runtime, ctx: &CallbackCtx) {
    let group = ctx.chat_id;
    let Some(member) = member_of(&ctx.params) else {
        return;
    };
    if !require_admin(rt, ctx, group).await {
        return;
    }
    let Some(message) = rt.td.message(ctx.chat_id, ctx.message_id).await else {
        answer(rt, ctx, "出现错误", false, 10).await;
        return;
    };
    let body = message
        .get("content")
        .and_then(|content| content.get("text"))
        .and_then(|text| text.get("text"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let Some(ids) = body
        .split("data:")
        .nth(1)
        .and_then(|rest| rest.split_whitespace().next())
        .and_then(base64_decode)
        .and_then(|bytes| String::from_utf8(bytes).ok())
        .and_then(|json| serde_json::from_str::<Vec<String>>(&json).ok())
    else {
        log::warn(&rt.host, "SpamPhoto: 无法从消息里提取图片数据");
        answer(rt, ctx, "出现错误", false, 10).await;
        return;
    };
    for id in &ids {
        rt.db.set_spam_file(group, id, true).await;
    }
    finish(
        rt,
        ctx,
        group,
        member,
        "spam_photo",
        &format!("图片举报封禁 admin={}", ctx.sender_user_id),
        "该用户和图片已在本群被封禁",
    )
    .await;
}

/// 三个动作共用的收尾：封禁 + 审计 + 改消息 + 日志频道 + 删除。
async fn finish(
    rt: &Runtime,
    ctx: &CallbackCtx,
    group: i64,
    member: Sender,
    spam_type: &str,
    spam_detail: &str,
    answer_text: &str,
) {
    if let Err(error) = rt.td.ban(group, member).await {
        log::warn(&rt.host, format!("封禁用户失败：{error}"));
        answer(rt, ctx, "封禁失败", true, 300).await;
        return;
    }
    rt.db
        .add_ban_record(
            group,
            member.id(),
            member.type_name(),
            "ban",
            spam_type,
            spam_detail,
            None,
        )
        .await;
    answer(rt, ctx, answer_text, false, 10).await;

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
            group,
            ctx.message_id,
            text::markdown(&body),
            buttons.clone(),
            Some(true),
        )
        .await;
    send_group_log_with_buttons(rt, group, &body, buttons).await;
    action::schedule_delete(rt, group, ctx.message_id, 10);
}
