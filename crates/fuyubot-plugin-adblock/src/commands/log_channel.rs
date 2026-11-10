//! `/setlogchannel`：把本群处置日志绑定到一个频道。
//!
//! 校验比 TS 版严格一点：原版先发测试消息、**无论成功失败**都在 `finally` 里写配置并
//! 回复「设置成功」——频道 ID 打错也会显示成功。这里只有测试消息真的发出去了才写配置。
//!
//! 所有回复都由**宿主**决定形态：普通的发新消息 + 回原命令；user 账户自己触发命令时
//! 编辑原消息；群里的 ephemeral 命令改走 `sendEphemeralMessage`（只对执行者可见）。
//! 插件只把 `reply_to` 指到命令消息，见 [`crate::api::Td::send`]。

use crate::simplify::Command;

use crate::api::Sender;
use crate::flow;
use crate::log;
use crate::runtime::Runtime;

pub async fn handle(rt: &Runtime, command: &Command, sender: Sender, _message: &serde_json::Value) {
    let chat_id = command.chat_id;
    let reply_to = Some(command.message_id);
    if !flow::is_group(rt, chat_id).await {
        reply(rt, chat_id, reply_to, "此命令只能在群组中使用。").await;
        return;
    }
    if !rt.td.is_admin(chat_id, sender).await {
        reply(rt, chat_id, reply_to, "只有群组管理员才能使用此命令。").await;
        return;
    }
    let words = super::words(command);
    let Some(raw) = words.first() else {
        reply(
            rt,
            chat_id,
            reply_to,
            "请提供日志频道ID，例如：/setlogchannel -1001234567890 \n请确保机器人已加入该频道并具有发送消息的权限。",
        ).await;
        return;
    };
    let Ok(channel) = raw.parse::<i64>() else {
        reply(
            rt,
            chat_id,
            reply_to,
            &format!("无效的频道 ID: {raw}，请提供 -100 开头的数字。"),
        )
        .await;
        return;
    };
    if !channel.to_string().starts_with("-100") {
        reply(
            rt,
            chat_id,
            reply_to,
            &format!("频道 ID 必须是 -100 开头，当前: {channel}"),
        )
        .await;
        return;
    }

    // 操作人必须是这个频道的管理员
    if !rt.td.is_admin(channel, sender).await {
        reply(
            rt,
            chat_id,
            reply_to,
            "请确保您是日志频道的管理员，并且机器人已加入该频道。",
        )
        .await;
        return;
    }

    // 发一条测试消息：能发出去才算绑定成功
    let sent = rt
        .td
        .send_plain(channel, "这是一个测试消息，用于验证日志频道设置成功。")
        .await;
    if sent.is_none() {
        reply(
            rt,
            chat_id,
            reply_to,
            "无法向日志频道发送消息。请确保机器人已加入该频道并具有发送消息的权限。",
        )
        .await;
        return;
    }

    rt.db
        .update_config(chat_id, |config| config.log_channel = Some(channel))
        .await;
    reply(
        rt,
        chat_id,
        reply_to,
        &format!("日志频道设置成功！ID: {channel}。"),
    )
    .await;
    log::info(&rt.host, format!("群 {chat_id} 日志频道设为 {channel}"));
}

/// 本命令的所有回复都指向触发命令的那条消息（15 秒后自删，宿主编辑原消息时不删）。
async fn reply(rt: &Runtime, chat_id: i64, reply_to: Option<i64>, body: &str) {
    flow::send_tip(rt, chat_id, body, reply_to, 15).await;
}
