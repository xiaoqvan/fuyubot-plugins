//! 回调查询（日志频道 / 群内提示上的按钮）。
//!
//! TDLib 的 `callback_data` 是 **base64 编码的字节串**，所以这里先解码再按
//! `命令?k=v&…` 切分。按钮数据的形状与 TS 版一致（`unban?cid=…&u=…`、
//! `SpamSet?&u=…&set=…` …）。
//!
//! 每个入口都先查「点按钮的人是不是该群管理员」——日志频道是公开可读的，
//! 按钮却不是给所有人按的。
//!
//! 按钮处理要改消息、写日志、排自删，全是 `async`；`rt` 统一收 `&Runtime`
//! （排期任务要把它捕获进去）。

pub mod basic;
pub mod media;
pub mod review;

use serde_json::Value;

use crate::api::Sender;
use crate::runtime::Runtime;
use crate::util::{base64_decode, param_i64, parse_callback};

/// 一次回调的上下文。
#[derive(Debug, Clone)]
pub struct CallbackCtx {
    /// 按钮所在的消息属于哪个 chat（可能是日志频道）。
    pub chat_id: i64,
    pub sender_user_id: i64,
    pub message_id: i64,
    pub query_id: i64,
    pub params: Vec<(String, String)>,
}

impl CallbackCtx {
    fn group_id(&self) -> Option<i64> {
        param_i64(&self.params, "cid")
    }
}

/// 处理 `updateNewCallbackQuery`。
pub async fn handle(rt: &Runtime, update: &Value) {
    let Some(payload) = update.get("payload") else {
        return;
    };
    if payload.get("@type").and_then(Value::as_str) != Some("callbackQueryPayloadData") {
        return;
    }
    let Some(data) = payload.get("data").and_then(Value::as_str) else {
        return;
    };
    let Some(bytes) = base64_decode(data) else {
        return;
    };
    let Ok(raw) = String::from_utf8(bytes) else {
        return;
    };
    let (command, params) = parse_callback(&raw);
    let Some(chat_id) = update.get("chat_id").and_then(Value::as_i64) else {
        return;
    };
    let Some(sender_user_id) = update.get("sender_user_id").and_then(Value::as_i64) else {
        return;
    };
    let Some(message_id) = update.get("message_id").and_then(Value::as_i64) else {
        return;
    };
    let Some(query_id) = update.get("id").and_then(as_i64) else {
        return;
    };
    let ctx = CallbackCtx {
        chat_id,
        sender_user_id,
        message_id,
        query_id,
        params,
    };
    crate::log::debug(
        &rt.host,
        format!("回调查询 chat={chat_id} sender={sender_user_id} msg={message_id} cmd={command}"),
    );

    match command.as_str() {
        "unban" => basic::unban(rt, &ctx).await,
        "unmute" => basic::unmute(rt, &ctx).await,
        "ban" => basic::ban(rt, &ctx).await,
        "banCnt" => basic::not_implemented(rt, &ctx).await,
        "newMemberReview" => review::new_member_review(rt, &ctx).await,
        "SpamSet" => media::spam_set(rt, &ctx).await,
        "SpamFile" => media::spam_file(rt, &ctx).await,
        "SpamPhoto" => media::spam_photo(rt, &ctx).await,
        _ => {}
    }
}

/// JSON 里可能是数字也可能是字符串（int64 的两种编码都见过）。
fn as_i64(value: &Value) -> Option<i64> {
    value
        .as_i64()
        .or_else(|| value.as_str().and_then(|text| text.parse().ok()))
}

/// 回调里带的成员：`u=` 用户 / `c=` 频道。
pub(crate) fn member_of(params: &[(String, String)]) -> Option<Sender> {
    if let Some(chat_id) = param_i64(params, "c") {
        return Some(Sender::Chat(chat_id));
    }
    param_i64(params, "u").map(Sender::User)
}

/// 点按钮的人是不是该群管理员；不是就回一个弹窗并返回 `false`。
pub(crate) async fn require_admin(rt: &Runtime, ctx: &CallbackCtx, group_id: i64) -> bool {
    if rt
        .td
        .is_admin(group_id, Sender::User(ctx.sender_user_id))
        .await
    {
        return true;
    }
    answer(rt, ctx, "你没有权限使用该按钮", true, 300).await;
    false
}

/// 回一条 callback query。
pub(crate) async fn answer(
    rt: &Runtime,
    ctx: &CallbackCtx,
    text: &str,
    alert: bool,
    cache_time: u32,
) {
    rt.td
        .answer_callback(ctx.query_id, text, alert, cache_time)
        .await;
}

/// 往群绑定的日志频道发一条（没配就静默跳过）。
pub(crate) async fn send_group_log(rt: &Runtime, group_id: i64, body: &str) {
    let config = rt.db.config_or_default(group_id).await;
    let Some(channel) = config.log_channel else {
        return;
    };
    let _ = rt
        .td
        .send_text(
            channel,
            crate::text::markdown(body),
            None,
            Vec::new(),
            None,
            Some(true),
        )
        .await;
}

/// 往群绑定的日志频道发一条带按钮的。
pub(crate) async fn send_group_log_with_buttons(
    rt: &Runtime,
    group_id: i64,
    body: &str,
    buttons: Vec<Vec<crate::simplify::ReplyButton>>,
) {
    let config = rt.db.config_or_default(group_id).await;
    let Some(channel) = config.log_channel else {
        return;
    };
    let _ = rt
        .td
        .send_text(
            channel,
            crate::text::markdown(body),
            None,
            buttons,
            None,
            Some(true),
        )
        .await;
}
