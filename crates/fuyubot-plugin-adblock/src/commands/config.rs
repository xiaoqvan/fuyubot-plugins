//! `/adconfig`：查看与切换本群的广告拦截配置。
//!
//! 状态页与 TS 版同构，但把「用内置 AI」的判定换成新逻辑（`/adaiset` 授权的群 +
//! 插件配置里配了默认接口），不再有写死的群名单。

use crate::simplify::Command;
use serde_json::Value;

use crate::ai;
use crate::api::{Sender, url_row};
use crate::flow;
use crate::log;
use crate::runtime::Runtime;
use crate::store::LimitMode;

/// 三个开关的展示文案。
const TOGGLES: [(&str, &str, &str, &str); 3] = [
    (
        "ai_review_enabled",
        "AI深度审核：对未部分命中关键词的消息进行AI审查",
        "✅ AI深度审核已开启",
        "❌ AI深度审核已关闭",
    ),
    (
        "check_new_member",
        "新用户检查：新成员加入时使用AI检查名称和简介是否含广告",
        "✅ 新用户检查已开启",
        "❌ 新用户检查已关闭",
    ),
    (
        "spam_tips",
        "垃圾信息提示：检测到广告时在群内发送提示消息",
        "✅ 垃圾信息提示已开启",
        "❌ 垃圾信息提示已关闭",
    ),
];

pub async fn handle(rt: &Runtime, command: &Command, sender: Sender, _message: &Value) {
    let chat_id = command.chat_id;
    let reply_to = Some(command.message_id);
    if !flow::ensure_group(rt, chat_id, reply_to).await {
        return;
    }
    if !flow::ensure_admin(rt, chat_id, sender, reply_to).await {
        return;
    }

    let words = super::words(command);
    let key = words.first().map(String::as_str).unwrap_or("");
    if key.is_empty() || key == "status" {
        show_status(rt, chat_id).await;
        return;
    }

    let config = rt.db.config_or_default(chat_id).await;

    // 处置模式
    if key == "limit_mode" || key == "limit" {
        let mode = words.get(1).map(|word| word.to_lowercase());
        let mode = match mode.as_deref() {
            Some("ban") => LimitMode::Ban,
            Some("mute") => LimitMode::Mute,
            _ => {
                flow::send_tip(
                    rt,
                    chat_id,
                    &format!(
                        "用法: /adconfig limit ban|mute\n当前模式: {}",
                        config.limit_mode.as_str()
                    ),
                    reply_to,
                    10,
                )
                .await;
                return;
            }
        };
        rt.db
            .update_config(chat_id, |config| config.limit_mode = mode)
            .await;
        flow::send_tip(
            rt,
            chat_id,
            match mode {
                LimitMode::Ban => "✅ 广告处置模式已切换为「封禁」",
                LimitMode::Mute => "✅ 广告处置模式已切换为「禁言」",
            },
            reply_to,
            10,
        )
        .await;
        log::info(
            &rt.host,
            format!("群 {chat_id} limit_mode 已切换为 {}", mode.as_str()),
        );
        return;
    }

    // 提示消息自删延时
    if key == "spam_tips_meg_delay" || key == "delay" {
        let delay = words.get(1).and_then(|word| word.parse::<u32>().ok());
        let Some(delay) = delay.filter(|delay| *delay <= 3600) else {
            flow::send_tip(
                rt,
                chat_id,
                &format!(
                    "用法: /adconfig delay <秒数>（0-3600）\n当前延时: {} 秒",
                    config.spam_tips_meg_delay
                ),
                reply_to,
                10,
            )
            .await;
            return;
        };
        rt.db
            .update_config(chat_id, |config| config.spam_tips_meg_delay = delay)
            .await;
        flow::send_tip(
            rt,
            chat_id,
            &format!("✅ 垃圾信息提示删除延时已设置为 {delay} 秒"),
            reply_to,
            10,
        )
        .await;
        log::info(
            &rt.host,
            format!("群 {chat_id} spam_tips_meg_delay 已设置为 {delay}"),
        );
        return;
    }

    // 三个开关
    let Some((name, _, on, off)) = TOGGLES.iter().find(|(name, _, _, _)| *name == key) else {
        let available = TOGGLES
            .iter()
            .map(|(name, _, _, _)| *name)
            .collect::<Vec<_>>()
            .join("、");
        flow::send_tip(
            rt,
            chat_id,
            &format!(
                "未知配置项：{}\n可用配置：{available}、limit_mode、delay",
                words.first().map(String::as_str).unwrap_or("")
            ),
            reply_to,
            10,
        )
        .await;
        return;
    };
    let current = match *name {
        "ai_review_enabled" => config.ai_review_enabled,
        "check_new_member" => config.check_new_member,
        _ => config.spam_tips,
    };
    let next = !current;
    rt.db
        .update_config(chat_id, |config| match *name {
            "ai_review_enabled" => config.ai_review_enabled = next,
            "check_new_member" => config.check_new_member = next,
            _ => config.spam_tips = next,
        })
        .await;
    flow::send_tip(rt, chat_id, if next { on } else { off }, reply_to, 10).await;
    log::info(&rt.host, format!("群 {chat_id} {name} 已切换为 {next}"));
}

/// 状态页。
async fn show_status(rt: &Runtime, chat_id: i64) {
    let config = rt.db.config_or_default(chat_id).await;
    let mut body = String::from("📋 **当前广告屏蔽配置：**\n\n");
    for (name, description, _, _) in TOGGLES {
        let value = match name {
            "ai_review_enabled" => config.ai_review_enabled,
            "check_new_member" => config.check_new_member,
            _ => config.spam_tips,
        };
        body.push_str(&format!(
            "{} `{name}`\n  {description}\n  → 当前: {}\n\n",
            if value { "✅" } else { "⬜" },
            if value { "开启" } else { "关闭" }
        ));
    }
    body.push_str("🤖 **AI 接口配置：**\n");
    body.push_str(&ai::ai_status_line(rt, chat_id, Some(&config)).await);
    body.push_str("\n\n🛡️ **广告处置模式：**\n");
    body.push_str(&format!(
        "  → 模式: `{}`（/adconfig limit ban|mute）\n  → 提示删除延时: `{}` 秒（/adconfig delay <秒数>）\n\n",
        config.limit_mode.as_str(),
        config.spam_tips_meg_delay
    ));
    body.push_str("**开关管理：**\n发送 `/adconfig <配置项>` 切换开关\n可用命令：/adconfig limit ban|mute、/adconfig delay <秒数>");

    // 私聊配置链接：需要 Bot 自己的用户名
    let username = crate::simplify::my_usernames(&rt.host)
        .into_iter()
        .next()
        .unwrap_or_default();
    let buttons = if username.is_empty() {
        Vec::new()
    } else {
        vec![url_row(
            "🔑 配置AI接口",
            format!("https://t.me/{username}?start=aiConfig_{chat_id}"),
        )]
    };
    flow::send_with(rt, chat_id, &body, None, buttons, 0).await;
}
