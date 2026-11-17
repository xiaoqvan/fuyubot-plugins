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
use crate::store::{GroupConfig, LimitMode};

/// 一个 `/adconfig` 开关：名字、说明、开启 / 关闭回执，以及读 / 写函数。
///
/// 用函数指针而不是 `match`，是为了新增开关时只改这一张表——状态页、切换、
/// 「未知配置项」里的可用列表都从它来，不会再出现「加了开关忘了改 match」。
struct Toggle {
    name: &'static str,
    description: &'static str,
    on: &'static str,
    off: &'static str,
    get: fn(&GroupConfig) -> bool,
    set: fn(&mut GroupConfig, bool),
}

/// 全部开关（顺序就是 `/adconfig` 状态页的顺序）。
const TOGGLES: [Toggle; 8] = [
    Toggle {
        name: "ai_review_enabled",
        description: "AI深度审核：对未部分命中关键词的消息进行AI审查",
        on: "✅ AI深度审核已开启",
        off: "❌ AI深度审核已关闭",
        get: |config: &GroupConfig| config.ai_review_enabled,
        set: |config: &mut GroupConfig, value| config.ai_review_enabled = value,
    },
    Toggle {
        name: "check_new_member",
        description: "新用户检查：新成员加入时使用AI检查名称和简介是否含广告",
        on: "✅ 新用户检查已开启",
        off: "❌ 新用户检查已关闭",
        get: |config: &GroupConfig| config.check_new_member,
        set: |config: &mut GroupConfig, value| config.check_new_member = value,
    },
    Toggle {
        name: "spam_tips",
        description: "垃圾信息提示：检测到广告时在群内发送提示消息",
        on: "✅ 垃圾信息提示已开启",
        off: "❌ 垃圾信息提示已关闭",
        get: |config: &GroupConfig| config.spam_tips,
        set: |config: &mut GroupConfig, value| config.spam_tips = value,
    },
    Toggle {
        name: "like_guard_enabled",
        description: "自动点赞机器人：连续回应 5 条消息的账号会被封禁回应权限",
        on: "✅ 自动点赞机器人拦截已开启",
        off: "❌ 自动点赞机器人拦截已关闭",
        get: |config: &GroupConfig| config.like_guard_enabled,
        set: |config: &mut GroupConfig, value| config.like_guard_enabled = value,
    },
    Toggle {
        name: "delete_join_message",
        description: "入群提示·成员加入：删除「成员加入」的服务消息（被添加 / 审批通过）",
        on: "✅ 已开启：成员加入的提示会被删除",
        off: "❌ 已关闭：成员加入的提示会保留",
        get: |config: &GroupConfig| config.delete_join_message,
        set: |config: &mut GroupConfig, value| config.delete_join_message = value,
    },
    Toggle {
        name: "delete_join_community_message",
        description: "入群提示·社区加入：删除「成员通过社区加入」的服务消息",
        on: "✅ 已开启：通过社区加入的提示会被删除",
        off: "❌ 已关闭：通过社区加入的提示会保留",
        get: |config: &GroupConfig| config.delete_join_community_message,
        set: |config: &mut GroupConfig, value| config.delete_join_community_message = value,
    },
    Toggle {
        name: "delete_join_invite_message",
        description: "入群提示·邀请链接加入：删除「成员通过邀请链接加入」的服务消息",
        on: "✅ 已开启：通过邀请链接加入的提示会被删除",
        off: "❌ 已关闭：通过邀请链接加入的提示会保留",
        get: |config: &GroupConfig| config.delete_join_invite_message,
        set: |config: &mut GroupConfig, value| config.delete_join_invite_message = value,
    },
    Toggle {
        name: "unpin_channel_pins",
        description: "取消频道置顶：群绑定频道自动转发并被 Telegram 自动置顶时取消置顶",
        on: "✅ 已开启：绑定频道的置顶会被自动取消",
        off: "❌ 已关闭：绑定频道的置顶会保留",
        get: |config: &GroupConfig| config.unpin_channel_pins,
        set: |config: &mut GroupConfig, value| config.unpin_channel_pins = value,
    },
];

/// 把 `/adconfig` 的参数归一化成配置键。
///
/// 短别名（`like_guard`、`join` / `join_community` / `join_invite` …）在这里
/// 归一化成完整配置键，其余参数原样交给下面的匹配。
fn config_key(word: &str) -> &str {
    match word {
        "like_guard" => "like_guard_enabled",
        // 入群提示的三个开关：短别名，少敲几个字符。
        "join" => "delete_join_message",
        "join_community" => "delete_join_community_message",
        "join_invite" => "delete_join_invite_message",
        // 取消频道置顶。
        "unpin" | "unpin_channel" => "unpin_channel_pins",
        other => other,
    }
}

pub async fn handle(rt: &Runtime, command: &Command, sender: Sender, message: &Value) {
    let chat_id = command.chat_id;
    let reply_to = Some(command.message_id);
    if !flow::ensure_group(rt, chat_id, reply_to).await {
        return;
    }
    if !flow::ensure_admin(rt, chat_id, sender, reply_to).await {
        return;
    }

    let words = super::words(command);
    let key = config_key(words.first().map(String::as_str).unwrap_or(""));
    if key.is_empty() || key == "status" {
        show_status(rt, chat_id, sender, message).await;
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

    // 开关：`TOGGLES` 里没有的名字就是未知配置项。
    let Some(toggle) = TOGGLES.iter().find(|toggle| toggle.name == key) else {
        let available = TOGGLES
            .iter()
            .map(|toggle| toggle.name)
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
    let next = !(toggle.get)(&config);
    rt.db
        .update_config(chat_id, |config| (toggle.set)(config, next))
        .await;
    flow::send_tip(
        rt,
        chat_id,
        if next { toggle.on } else { toggle.off },
        reply_to,
        10,
    )
    .await;
    log::info(
        &rt.host,
        format!("群 {chat_id} {} 已切换为 {next}", toggle.name),
    );
}

/// 状态页。
async fn show_status(rt: &Runtime, chat_id: i64, _sender: Sender, message: &Value) {
    let config = rt.db.config_or_default(chat_id).await;
    let mut body = String::from("📋 **当前广告屏蔽配置：**\n\n");
    for toggle in &TOGGLES {
        let value = (toggle.get)(&config);
        body.push_str(&format!(
            "{} `{}`\n  {}\n  → 当前: {}\n\n",
            if value { "✅" } else { "⬜" },
            toggle.name,
            toggle.description,
            if value { "开启" } else { "关闭" }
        ));
    }
    body.push_str("ℹ️ **自动点赞判定：**\n  连续回应 5 条消息的账号会被封禁回应权限，并清理它已经点出去的回应（被删除的消息不计；同一账号 30 分钟内只处置一次）\n\n");
    body.push_str("🤖 **AI 接口配置：**\n");
    body.push_str(&ai::ai_status_line(rt, chat_id, Some(&config)).await);
    body.push_str("\n\n🛡️ **广告处置模式：**\n");
    body.push_str(&format!(
        "  → 模式: `{}`（/adconfig limit ban|mute）\n  → 提示删除延时: `{}` 秒（/adconfig delay <秒数>）\n\n",
        config.limit_mode.as_str(),
        config.spam_tips_meg_delay
    ));
    body.push_str("**开关管理：**\n发送 `/adconfig <配置项>` 切换开关\n可用命令：/adconfig limit ban|mute、/adconfig delay <秒数>、/adconfig like_guard、/adconfig join|join_community|join_invite、/adconfig unpin");

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

    // 回复走宿主：普通的发新消息 + 回原命令（60 秒后自删）；user 账户自己触发命令时
    // 编辑原消息（不删，那是用户自己发的）；群里的 ephemeral 命令由宿主改走
    // `sendEphemeralMessage`（只对执行者可见）。
    let reply_to = message.get("id").and_then(Value::as_i64);
    flow::send_with(rt, chat_id, &body, reply_to, buttons, 60).await;
}

#[cfg(test)]
#[path = "../../tests/unit/commands__config.rs"]
mod tests;
