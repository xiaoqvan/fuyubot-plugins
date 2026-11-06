//! `/spam`：人工举报入口 —— 对一条消息、一个用户或一个 `@用户名` 做一次 AI 判定。
//!
//! 对应 TS 版 `cmd/spam.ts`（1200 行）。它是整份插件里最复杂的一条命令，三种入口：
//!
//! | 用法 | 被审对象 | 处置 |
//! | --- | --- | --- |
//! | `/spam`（回复某条消息） | 被回复消息的发送者 | 命中即禁言/封禁 + 撤回 |
//! | `/spam <user_id>` | 该用户的资料 | 命中即处置（不删消息） |
//! | `/spam @username` | 该用户/频道 | 同上；用户还会被写进用户名黑名单 |
//!
//! 判定阈值保持原样：`confidence > 0.8` 处置；`< 0.75` 给管理员一个「Ban」按钮；
//! 中间那段按「存疑」处理。命中后还会把关键词 / 全文 / 名称 / 简介 / 转发来源 /
//! 贴纸集写进对应的黑名单库——这是原版最有价值的一半：一次人工举报，之后自动拦。

use fuyubot_simplify::{Command, ReplyButton};
use serde_json::Value;

use crate::ai::{self, AiMeta, Contact, ModerationInput, ModerationResult, Profile, Sticker};
use crate::api::{Sender, callback_row, member_param, message_sender};
use crate::detect::SpamHit;
use crate::flow;
use crate::log;
use crate::runtime::Runtime;
use crate::text;
use crate::util::base64_encode;

/// 举报对象的画像。
#[derive(Debug, Clone)]
struct TargetInfo {
    sender: Sender,
    name: String,
    bio: String,
    personal_chat_id: Option<i64>,
    /// `@username`（不带 `@`），用于判定广告后写黑名单。
    username: Option<String>,
}

/// `/spam`
pub fn handle(rt: &std::sync::Arc<Runtime>, command: &Command, _sender: Sender, message: &Value) {
    let chat_id = command.chat_id;
    if !flow::is_group(rt, chat_id) {
        flow::send_tip(
            rt,
            chat_id,
            "此功能仅在群组中有效，个人聊天无法使用。请在群组中使用此功能。",
            Some(command.message_id),
            10,
        );
        return;
    }
    if !flow::am_i_admin(rt, chat_id) {
        return;
    }
    // 命令本身不需要留在群里
    let _ = rt.td.delete_messages(chat_id, &[command.message_id]);

    let words = super::words(command);
    let param = words.first().cloned();

    if let Some(param) = param {
        let target = if param.chars().all(|c| c.is_ascii_digit()) {
            param
                .parse::<i64>()
                .ok()
                .and_then(|user_id| target_from_user(rt, user_id))
        } else if let Some(username) = param.strip_prefix('@') {
            if username.is_empty() {
                None
            } else {
                target_from_username(rt, username)
            }
        } else {
            None
        };
        match target {
            Some(info) => return report_user(rt, message, chat_id, info),
            None => {
                flow::send_tip(
                    rt,
                    chat_id,
                    "参数格式不正确或找不到目标\n示例 /spam <user_id> 或 /spam @username",
                    None,
                    6,
                );
                return;
            }
        }
    }

    // ── 回复模式 ──────────────────────────────────────────────
    let Some(reply_id) = message
        .get("reply_to")
        .filter(|reply| reply.get("@type").and_then(Value::as_str) == Some("messageReplyToMessage"))
        .and_then(|reply| reply.get("message_id"))
        .and_then(Value::as_i64)
    else {
        flow::send_tip(
            rt,
            chat_id,
            "回复的消息格式不正确，无法进行广告屏蔽。",
            None,
            10,
        );
        return;
    };
    let Some(spam_message) = rt
        .td
        .message(chat_id, reply_id)
        .or_else(|| rt.cache.recent_message(chat_id, reply_id))
    else {
        flow::send_tip(rt, chat_id, "获取回复的消息失败，可以稍后再试。", None, 10);
        return;
    };
    let Some(target) = message_sender(&spam_message) else {
        return;
    };
    if rt.td.is_admin(chat_id, target) {
        flow::send_tip(rt, chat_id, "你不能举报管理员的消息！", None, 10);
        return;
    }
    if rt.db.is_whitelisted(chat_id, target.id()) {
        let mention = rt.td.sender_mention(target, true);
        flow::send_tip(
            rt,
            chat_id,
            &format!(
                "{mention} 是白名单用户，不能被举报！\n 你可以先将其移出白名单。\n id: `{}`",
                target.id()
            ),
            None,
            15,
        );
        return;
    }
    report_message(rt, message, chat_id, &spam_message, target);
}

// ── 目标解析 ────────────────────────────────────────────────────

fn target_from_user(rt: &Runtime, user_id: i64) -> Option<TargetInfo> {
    let user = rt.td.user(user_id).ok()?;
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
    let personal_chat_id = full
        .as_ref()
        .and_then(|full| full.get("personal_chat_id"))
        .and_then(Value::as_i64);
    Some(TargetInfo {
        sender: Sender::User(user_id),
        name,
        bio,
        personal_chat_id,
        username: None,
    })
}

fn target_from_username(rt: &Runtime, username: &str) -> Option<TargetInfo> {
    let chat = rt.td.search_public_chat(username).ok()?;
    let kind = chat.get("type")?;
    match kind.get("@type").and_then(Value::as_str) {
        Some("chatTypePrivate") => {
            let user_id = kind.get("user_id").and_then(Value::as_i64)?;
            let mut info = target_from_user(rt, user_id)?;
            info.username = Some(username.to_lowercase());
            Some(info)
        }
        Some("chatTypeSupergroup") => {
            let supergroup_id = kind.get("supergroup_id").and_then(Value::as_i64)?;
            let description = rt
                .td
                .supergroup_full(supergroup_id)
                .ok()
                .and_then(|full| {
                    full.get("description")
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                })
                .unwrap_or_default();
            Some(TargetInfo {
                sender: Sender::Chat(chat.get("id").and_then(Value::as_i64)?),
                name: chat
                    .get("title")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned(),
                bio: description,
                personal_chat_id: None,
                username: Some(username.to_lowercase()),
            })
        }
        _ => None,
    }
}

// ── 用户举报（按 ID / @username） ──────────────────────────────

fn report_user(rt: &std::sync::Arc<Runtime>, _message: &Value, chat_id: i64, info: TargetInfo) {
    let is_white = rt.db.is_whitelisted(chat_id, info.sender.id());
    if is_white {
        let mention = rt.td.sender_mention(info.sender, true);
        flow::send_tip(
            rt,
            chat_id,
            &format!(
                "{mention} 是白名单用户，不能被举报！\n 你可以先将其移出白名单。\n id: `{}`",
                info.sender.id()
            ),
            // 命令消息已经在入口删掉了，不能再拿它当 reply_to——那样发不出去。
            None,
            15,
        );
        return;
    }

    let channel = info
        .personal_chat_id
        .and_then(|id| personal_channel_by_chat(rt, id));
    let mut input = ModerationInput::profile(info.name.clone(), info.bio.clone());
    if let Some((name, description)) = &channel {
        input = input.with_personal_channel(name.clone(), description.clone());
    }
    let result = ai::review(
        rt,
        &input,
        None,
        Some(chat_id),
        &AiMeta {
            scene: Some("spam_user"),
            message_type: None,
            force_default: true,
        },
    );

    let user_target = rt.td.sender_mention(info.sender, true);
    if result.ad_detected {
        let hit = SpamHit::hit(
            "user",
            None,
            if result.reason.is_empty() {
                "AI检测为广告信息".to_owned()
            } else {
                result.reason.clone()
            },
        );
        crate::action::punish(rt, chat_id, info.sender, &hit, None);
        // 高置信度 → 用户名进黑名单（之后任何消息提到它都会触发）
        if let Some(username) = &info.username
            && result.confidence > 0.8
        {
            match rt.db.add_username(username) {
                Ok((_, existed)) => log::info(
                    &rt.host,
                    format!(
                        "用户名 @{username} 已加入 username_blacklist ({})",
                        if existed { "已存在" } else { "新增" }
                    ),
                ),
                Err(error) => log::warn(&rt.host, format!("写 username_blacklist 失败：{error}")),
            }
        }
    } else {
        let is_fail = ai::is_failure_reason(&result.reason);
        let reason = if result.reason.is_empty() {
            format!("AI未检测到广告内容 (confidence: {})", result.confidence)
        } else {
            result.reason.clone()
        };
        let body = if is_fail {
            format!("对 {user_target} 的 AI 检查未能完成\n\n**原因**: {reason}")
        } else {
            format!(
                "对 {user_target} 的 AI 检查已完成\n\n**结果**: 未判定为广告\n**理由**: {reason}\n**置信度**: {}",
                text::confidence_percent(result.confidence)
            )
        };
        flow::send_tip(rt, chat_id, &body, None, 60);
    }
}

/// 按 chat_id 取「主页挂载频道」（只认频道形态的超级群）。
fn personal_channel_by_chat(rt: &Runtime, chat_id: i64) -> Option<(String, String)> {
    let chat = rt.td.chat(chat_id).ok()?;
    let kind = chat.get("type")?;
    if kind.get("@type").and_then(Value::as_str) != Some("chatTypeSupergroup")
        || !kind
            .get("is_channel")
            .and_then(Value::as_bool)
            .unwrap_or(false)
    {
        return None;
    }
    let supergroup_id = kind.get("supergroup_id").and_then(Value::as_i64)?;
    let description = rt
        .td
        .supergroup_full(supergroup_id)
        .ok()
        .and_then(|full| {
            full.get("description")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .unwrap_or_default();
    Some((
        chat.get("title")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned(),
        description,
    ))
}

// ── 消息举报（回复模式） ────────────────────────────────────────

fn report_message(
    rt: &std::sync::Arc<Runtime>,
    command_message: &Value,
    chat_id: i64,
    spam_message: &Value,
    target: Sender,
) {
    let Some(spam_message_id) = spam_message.get("id").and_then(Value::as_i64) else {
        return;
    };
    let content_type = spam_message
        .get("content")
        .and_then(|content| content.get("@type"))
        .and_then(Value::as_str)
        .unwrap_or("");
    if matches!(
        content_type,
        "messagePhoto" | "messageSticker" | "messageContact"
    ) {
        let _ = rt.td.delete_messages(chat_id, &[spam_message_id]);
    }

    let target_mention = rt.td.sender_mention(target, true);
    let tips = rt.td.send_text(
        chat_id,
        text::markdown(&format!("正在处理对 {target_mention} 的广告举报...")),
        None,
        Vec::new(),
        None,
        Some(true),
    );
    let tips_id = tips
        .as_ref()
        .and_then(|message| message.get("id"))
        .and_then(Value::as_i64);

    send_report_log(rt, chat_id, command_message, target);

    // 引用 / 转发来源
    let quote = quote_of(spam_message);
    let forward_chat = forward_origin(spam_message);

    // 发送者资料
    let (name, bio) = sender_profile(rt, target);
    let personal_channel = match target {
        Sender::User(user_id) => flow::personal_channel(rt, user_id),
        _ => None,
    };

    // 按类型组装 AI 输入
    let mut image_path: Option<String> = None;
    let mut input = match content_type {
        "messageText" => ModerationInput::text(body_of(spam_message)),
        "messageRichMessage" => ModerationInput::text(
            spam_message
                .get("content")
                .map(text::rich_message_text)
                .unwrap_or_default(),
        ),
        "messagePhoto" => {
            let content = spam_message.get("content").cloned().unwrap_or(Value::Null);
            let mut input = ModerationInput::of_kind("photo");
            input.message = Some(caption_of(&content));
            if let Some(remote_id) = content
                .get("photo")
                .and_then(|photo| photo.get("sizes"))
                .and_then(Value::as_array)
                .and_then(|sizes| sizes.last())
                .and_then(|size| size.get("photo"))
                .and_then(remote_file_id)
            {
                image_path = rt.td.download_remote(&remote_id, "fileTypePhoto");
            }
            input
        }
        "messageVideo" => {
            let content = spam_message.get("content").cloned().unwrap_or(Value::Null);
            let mut input = ModerationInput::of_kind("video");
            input.message = Some(caption_of(&content));
            input
        }
        "messageContact" => {
            let content = spam_message.get("content").cloned().unwrap_or(Value::Null);
            let contact_name = text::contact_name(&content);
            let contact = content.get("contact").cloned().unwrap_or(Value::Null);
            let mut input = ModerationInput::of_kind("contact");
            input.contact = Some(Contact {
                name: Some(contact_name),
                phone: contact
                    .get("phone_number")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                username: contact
                    .get("username")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
            });
            input.profile = Some(Profile {
                name: Some(name.clone()),
                bio: Some(bio.clone()),
            });
            input
        }
        "messageSticker" => {
            let content = spam_message.get("content").cloned().unwrap_or(Value::Null);
            let sticker = content.get("sticker").cloned().unwrap_or(Value::Null);
            let set_id = sticker.get("set_id").and_then(Value::as_i64);
            let title = set_id
                .and_then(|set_id| rt.td.sticker_set(set_id))
                .and_then(|set| set.get("title").and_then(Value::as_str).map(str::to_owned))
                .unwrap_or_default();
            let mut input = ModerationInput::of_kind("sticker");
            input.sticker = Some(Sticker { name: Some(title) });
            if let Some(remote_id) = sticker.get("sticker").and_then(remote_file_id) {
                image_path = rt.td.download_remote(&remote_id, "fileTypeSticker");
            }
            input
        }
        other => {
            if let Some(tips_id) = tips_id {
                let target_mention = rt.td.sender_mention(target, true);
                let _ = rt.td.edit_text(
                    chat_id,
                    tips_id,
                    text::markdown(&format!(
                        "对 {target_mention} 的举报已记录，但该消息类型 ({other}) 目前不支持自动处理。请手动审核该消息。"
                    )),
                    Vec::new(),
                    Some(true),
                );
                crate::action::schedule_delete(rt, chat_id, tips_id, 10);
            }
            return;
        }
    };

    input.profile = Some(Profile {
        name: Some(name.clone()),
        bio: Some(bio.clone()),
    });
    if let Some((quoted_chat, quoted_text)) = &quote {
        input = input.with_quote(quoted_text.clone());
        let _ = quoted_chat;
    }
    if let Some((channel_name, description)) = &personal_channel {
        input = input.with_personal_channel(channel_name.clone(), description.clone());
    }
    // 链接预览
    if content_type == "messageText"
        && let Some(url) = spam_message
            .get("content")
            .and_then(|content| content.get("link_preview_options"))
            .and_then(|options| options.get("url"))
            .and_then(Value::as_str)
        && let Some((title, description)) = fetch_link_preview(rt, url)
    {
        input.link_preview = Some(ai::LinkPreview {
            title: Some(title),
            description: Some(description),
        });
    }

    // @username 提及检查放到独立任务里：它会发多次 AI 请求，不该拖住举报主流程。
    let mention_text = mention_source(spam_message, content_type);
    if !mention_text.is_empty() {
        let rt = std::sync::Arc::clone(rt);
        rt.submit(move |rt| check_mentioned_usernames(&rt, chat_id, &mention_text));
    }

    let meta = AiMeta {
        scene: Some("spam_message"),
        message_type: Some(content_type.to_owned()),
        force_default: true,
    };
    let result = ai::review(rt, &input, image_path.as_deref(), Some(chat_id), &meta);

    // ── 分支 ─────────────────────────────────────────────────
    let target_id = target.id();
    if result.ad_detected && result.confidence > 0.8 {
        if let Some(tips_id) = tips_id {
            let _ = rt.td.edit_text(
                chat_id,
                tips_id,
                text::markdown(&format!(
                    "经过 ChatGPT 的判断 {target_mention}(ID: {target_id}) 是已受到限制\n\n理由: {}",
                    if result.reason.is_empty() {
                        "AI检测为广告信息"
                    } else {
                        &result.reason
                    }
                )),
                Vec::new(),
                Some(true),
            );
        }
        persist_ai_result(
            rt,
            &result,
            &forward_chat,
            &quote,
            &name,
            &bio,
            spam_message,
        );
        let hit = SpamHit::hit(
            "AI_Spam",
            None,
            if result.reason.is_empty() {
                "AI检测为广告信息".to_owned()
            } else {
                result.reason.clone()
            },
        );
        crate::action::punish(rt, chat_id, target, &hit, Some(spam_message_id));
        // 提示与命令消息按群配置延时删除；`0` 表示不删（TS 的 `if (delay > 0)`）。
        let delay = rt.db.config_or_default(chat_id).spam_tips_meg_delay;
        if delay > 0
            && let Some(tips_id) = tips_id
        {
            crate::action::schedule_delete(rt, chat_id, tips_id, delay);
        }
        return;
    }

    if result.ad_detected && result.confidence < 0.75 {
        if let Some(tips_id) = tips_id {
            let member = member_param(target);
            let buttons: Vec<Vec<ReplyButton>> =
                vec![callback_row("Ban", format!("ban?cid={chat_id}&{member}"))];
            let _ = rt.td.edit_text(
                chat_id,
                tips_id,
                text::markdown(&format!(
                    "经过 ChatGPT 的判断 {target_mention}(ID: {target_id}) \n可能是 广告 消息，但是存疑\n\n请管理员手动确认，如果不是请忽略\n\nAI判断理由: {}",
                    if result.reason.is_empty() {
                        "AI检测可能为广告信息"
                    } else {
                        &result.reason
                    }
                )),
                buttons,
                Some(true),
            );
        }
        return;
    }

    // 未判定为广告 / 调用失败：必须在群里回传理由
    let Some(tips_id) = tips_id else {
        return;
    };
    let reason_line = if ai::is_failure_reason(&result.reason) {
        format!("AI调用失败/异常: {}", result.reason)
    } else {
        format!(
            "AI判断理由: {}",
            if result.reason.is_empty() {
                format!("AI未检测到广告内容 (confidence: {})", result.confidence)
            } else {
                result.reason.clone()
            }
        )
    };
    error_edit_tips(
        rt,
        chat_id,
        command_message,
        tips_id,
        spam_message,
        target,
        content_type,
        &reason_line,
    );
}

/// 举报流水：写库 + 发群绑定的日志频道。
fn send_report_log(rt: &Runtime, chat_id: i64, command_message: &Value, target: Sender) {
    let reporter = message_sender(command_message)
        .map(|sender| sender.id())
        .unwrap_or(0);
    rt.db.log_spam_report(chat_id, reporter, command_message);
    let config = rt.db.config_or_default(chat_id);
    let Some(channel) = config.log_channel else {
        return;
    };
    let chat_mention = rt.td.sender_mention(Sender::Chat(chat_id), true);
    let title = rt.td.chat_title(chat_id);
    let user_mention = rt.td.sender_mention(target, true);
    let reporter_mention = message_sender(command_message)
        .map(|sender| rt.td.sender_mention(sender, true))
        .unwrap_or_else(|| "-".to_owned());
    let body = format!(
        "#SPAM\n**Group**:{chat_mention}({title}) \n**User**:{reporter_mention}(ID: {reporter})\n**Reported User**:{user_mention}(ID: {})",
        target.id()
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

/// AI 命中后的落库（关键词 / 全文 / 名称 / 简介 / 转发来源 / 贴纸集）。
fn persist_ai_result(
    rt: &Runtime,
    result: &ModerationResult,
    forward_chat: &Option<i64>,
    quote: &Option<(i64, String)>,
    name: &str,
    bio: &str,
    spam_message: &Value,
) {
    let mut inserted: Vec<(String, String)> = Vec::new();

    if !result.message.keywords.is_empty() {
        if let Ok(id) = rt.db.add_ad_keywords(&result.message.keywords, result.r18) {
            inserted.push(("ads_blacklist".to_owned(), id));
        }
    }
    if result.quoted_message.has_ad
        && !result.quoted_message.keywords.is_empty()
        && let Ok(id) = rt
            .db
            .add_ad_keywords(&result.quoted_message.keywords, result.r18)
    {
        inserted.push(("ads_blacklist".to_owned(), id));
    }
    if result.profile.bio_ad
        && !bio.trim().is_empty()
        && let Ok(id) = rt.db.add_bio(&[bio.to_owned()])
    {
        inserted.push(("bio_blacklist".to_owned(), id));
    }
    if result.profile.name_ad
        && !name.trim().is_empty()
        && let Ok(id) = rt.db.add_name(name)
    {
        inserted.push(("name_blacklist".to_owned(), id));
    }
    if result.ad_detected {
        if let Some(chat) = forward_chat
            && let Ok(id) = rt.db.add_forward(*chat)
        {
            inserted.push(("forward_blacklist".to_owned(), id));
        }
        if let Some((chat, _)) = quote
            && let Ok(id) = rt.db.add_forward(*chat)
        {
            inserted.push(("forward_blacklist".to_owned(), id));
        }
    }
    if result.ad_detected
        && result.confidence > 0.9
        && spam_message
            .get("content")
            .and_then(|content| content.get("@type"))
            .and_then(Value::as_str)
            == Some("messageSticker")
    {
        let content = spam_message.get("content").cloned().unwrap_or(Value::Null);
        let sticker = content.get("sticker").cloned().unwrap_or(Value::Null);
        let unique_id = sticker.get("sticker").and_then(unique_id_of);
        let set_id = sticker.get("set_id").and_then(Value::as_i64);
        if let Some(unique_id) = &unique_id {
            if let Ok(id) = rt.db.add_sticker(unique_id) {
                inserted.push(("sticker_blacklist".to_owned(), id));
            }
            if let Ok(id) = rt.db.add_file(unique_id) {
                inserted.push(("file_unique_id_blacklist".to_owned(), id));
            }
        }
        if let Some(set_id) = set_id {
            let title = rt
                .td
                .sticker_set(set_id)
                .and_then(|set| set.get("title").and_then(Value::as_str).map(str::to_owned))
                .unwrap_or_default();
            if let Ok(id) = rt.db.add_sticker_set(&set_id.to_string(), &title) {
                inserted.push(("sticker_set_blacklist".to_owned(), id));
            }
        }
    }

    if inserted.is_empty() {
        return;
    }
    let notify = serde_json::json!({
        "aiResult": result,
        "sendInfo": { "name": name, "bio": bio },
        "quote": quote,
        "forward": forward_chat,
        "insertedIds": inserted,
    });
    if let Some(channel) = rt.cfg.ai_db_channel {
        let topic = rt.cfg.ai_db_topic.map(fuyubot_simplify::Topic::forum);
        let _ = rt.td.send_text(
            channel,
            text::plain(
                serde_json::to_string_pretty(&notify).unwrap_or_else(|_| notify.to_string()),
            ),
            None,
            Vec::new(),
            topic,
            None,
        );
    }
}

/// 未判定 / 存疑时的提示 + 按类型的补刀按钮。
#[allow(clippy::too_many_arguments)]
fn error_edit_tips(
    rt: &Runtime,
    chat_id: i64,
    command_message: &Value,
    tips_id: i64,
    spam_message: &Value,
    target: Sender,
    content_type: &str,
    reason_line: &str,
) {
    let member = member_param(target);
    let content = spam_message.get("content").cloned().unwrap_or(Value::Null);
    let mut body = format!(
        "{reason_line}\n\n群管理员可通过下方回调按钮进一步操作\n管理员可以在群操作记录中查看已被删除的消息\n"
    );
    let buttons: Vec<Vec<ReplyButton>> = match content_type {
        "messageContact" => {
            let contact_user_id = content
                .get("contact")
                .and_then(|contact| contact.get("user_id"))
                .and_then(Value::as_i64)
                .unwrap_or(0);
            body.push_str("封禁该用户和提及的用户");
            vec![callback_row(
                "ban",
                format!("banCnt?{member}&@={contact_user_id}"),
            )]
        }
        "messageSticker" => {
            let set_id = content
                .get("sticker")
                .and_then(|sticker| sticker.get("set_id"))
                .and_then(Value::as_i64)
                .unwrap_or(0);
            body.push_str("封禁该贴纸包");
            vec![callback_row(
                "Ban Sticker Set",
                format!("SpamSet?&{member}&set={set_id}"),
            )]
        }
        "messageVideo" | "messageDocument" => {
            let file_id = if content_type == "messageVideo" {
                content
                    .get("video")
                    .and_then(|video| video.get("video"))
                    .and_then(unique_id_of)
            } else {
                content
                    .get("document")
                    .and_then(|document| document.get("document"))
                    .and_then(unique_id_of)
            }
            .unwrap_or_default();
            body.push_str("封禁该文件");
            vec![callback_row(
                "Ban File",
                format!("SpamFile?{member}&f={file_id}"),
            )]
        }
        "messagePhoto" => {
            let ids = text::photo_unique_ids(&content);
            let ids_base64 =
                base64_encode(serde_json::to_string(&ids).unwrap_or_default().as_bytes());
            body.push_str(&format!("封禁该图片id\ndata: {ids_base64}"));
            vec![callback_row("Ban Photo", format!("SpamPhoto?{member}"))]
        }
        _ => {
            body.push_str("封禁该用户");
            vec![callback_row("Ban", format!("ban?cid={chat_id}&{member}"))]
        }
    };

    let _ = rt
        .td
        .edit_text(chat_id, tips_id, text::markdown(&body), buttons, Some(true));
    // 25 秒后把命令、提示、被举报消息一起删掉
    let mut ids = vec![tips_id];
    if let Some(id) = command_message.get("id").and_then(Value::as_i64) {
        ids.push(id);
    }
    if let Some(id) = spam_message.get("id").and_then(Value::as_i64) {
        ids.push(id);
    }
    let host = rt.host;
    rt.scheduler.after(
        std::time::Duration::from_secs(25),
        Box::new(move || {
            let _ = crate::api::Td::new(host).delete_messages(chat_id, &ids);
        }),
    );
}

// ── @username 提及检查 ──────────────────────────────────────────

/// 从消息里取「可能含 @username」的文本。
fn mention_source(message: &Value, content_type: &str) -> String {
    let content = message.get("content");
    match content_type {
        "messageText" => content
            .and_then(|content| content.get("text"))
            .and_then(|text| text.get("text"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned(),
        "messageRichMessage" => content.map(text::rich_message_text).unwrap_or_default(),
        "messagePhoto" | "messageVideo" => content
            .and_then(|content| content.get("caption"))
            .and_then(|caption| caption.get("text"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned(),
        _ => String::new(),
    }
}

/// `@([a-zA-Z_]\w{3,})` 的等价实现（不拖 regex 依赖）。
fn extract_mentions(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let chars: Vec<char> = text.chars().collect();
    let mut index = 0;
    while index < chars.len() {
        if chars[index] != '@' {
            index += 1;
            continue;
        }
        let start = index + 1;
        if start >= chars.len() || !(chars[start].is_ascii_alphabetic() || chars[start] == '_') {
            index += 1;
            continue;
        }
        let mut end = start + 1;
        while end < chars.len() && (chars[end].is_ascii_alphanumeric() || chars[end] == '_') {
            end += 1;
        }
        if end - start >= 4 {
            out.push(chars[start..end].iter().collect::<String>().to_lowercase());
        }
        index = end;
    }
    out.sort();
    out.dedup();
    out
}

/// 逐个解析 @username 并让 AI 判定它本身是不是广告。
fn check_mentioned_usernames(rt: &Runtime, chat_id: i64, text: &str) {
    let mentions = extract_mentions(text);
    if mentions.is_empty() {
        return;
    }
    for username in mentions {
        let Some(chat) = rt.td.search_public_chat(&username).ok() else {
            continue;
        };
        let kind = chat
            .get("type")
            .and_then(|kind| kind.get("@type"))
            .and_then(Value::as_str);
        let (name, bio, personal_channel, mention_type) = match kind {
            Some("chatTypePrivate") => {
                let Some(user_id) = chat
                    .get("type")
                    .and_then(|kind| kind.get("user_id"))
                    .and_then(Value::as_i64)
                else {
                    continue;
                };
                let user = rt.td.user(user_id).ok();
                let full = rt.td.user_full(user_id).ok();
                let name = user
                    .as_ref()
                    .map(|user| {
                        format!(
                            "{}{}",
                            user.get("first_name").and_then(Value::as_str).unwrap_or(""),
                            user.get("last_name").and_then(Value::as_str).unwrap_or("")
                        )
                    })
                    .unwrap_or_default();
                let bio = full
                    .as_ref()
                    .and_then(|full| full.get("bio"))
                    .and_then(|bio| bio.get("text"))
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned();
                let channel = full
                    .as_ref()
                    .and_then(|full| full.get("personal_chat_id"))
                    .and_then(Value::as_i64)
                    .and_then(|id| personal_channel_by_chat(rt, id));
                (name, bio, channel, "user")
            }
            Some("chatTypeSupergroup") => {
                let Some(supergroup_id) = chat
                    .get("type")
                    .and_then(|kind| kind.get("supergroup_id"))
                    .and_then(Value::as_i64)
                else {
                    continue;
                };
                let bio = rt
                    .td
                    .supergroup_full(supergroup_id)
                    .ok()
                    .and_then(|full| {
                        full.get("description")
                            .and_then(Value::as_str)
                            .map(str::to_owned)
                    })
                    .unwrap_or_default();
                (
                    chat.get("title")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_owned(),
                    bio,
                    None,
                    "channel",
                )
            }
            _ => continue,
        };

        let mut input = ModerationInput::profile(name.clone(), bio.clone());
        if let Some((channel_name, description)) = &personal_channel {
            input = input.with_personal_channel(channel_name.clone(), description.clone());
        }
        input.mentioned_username = Some(ai::Mention {
            username: username.clone(),
            name,
            bio,
            kind: mention_type.to_owned(),
        });
        let result = ai::review(
            rt,
            &input,
            None,
            Some(chat_id),
            &AiMeta {
                scene: Some("spam_mention"),
                message_type: None,
                force_default: true,
            },
        );
        if result.mentioned_username.username_ad && result.confidence > 0.8 {
            match rt.db.add_username(&username) {
                Ok((_, existed)) => log::info(
                    &rt.host,
                    format!(
                        "[提及检查] 用户名 @{username} 已加入 username_blacklist（{}）| 理由: {}",
                        if existed { "已存在" } else { "新增" },
                        result.reason
                    ),
                ),
                Err(error) => log::warn(&rt.host, format!("写 username_blacklist 失败：{error}")),
            }
        }
    }
}

// ── 小工具 ──────────────────────────────────────────────────────

fn body_of(message: &Value) -> String {
    message
        .get("content")
        .and_then(|content| content.get("text"))
        .and_then(|text| text.get("text"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned()
}

fn caption_of(content: &Value) -> String {
    content
        .get("caption")
        .and_then(|caption| caption.get("text"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned()
}

fn sender_profile(rt: &Runtime, sender: Sender) -> (String, String) {
    match sender {
        Sender::User(user_id) => {
            let user = rt.td.user(user_id).ok();
            let full = rt.td.user_full(user_id).ok();
            let name = user
                .as_ref()
                .map(|user| {
                    format!(
                        "{}{}",
                        user.get("first_name").and_then(Value::as_str).unwrap_or(""),
                        user.get("last_name").and_then(Value::as_str).unwrap_or("")
                    )
                })
                .unwrap_or_default();
            let bio = full
                .and_then(|full| {
                    full.get("bio")
                        .and_then(|bio| bio.get("text"))
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                })
                .unwrap_or_default();
            (name, bio)
        }
        Sender::Chat(chat_id) => {
            let title = rt.td.chat_title(chat_id);
            let bio = rt
                .td
                .chat(chat_id)
                .ok()
                .and_then(|chat| chat.get("type").cloned())
                .and_then(|kind| kind.get("supergroup_id").and_then(Value::as_i64))
                .and_then(|supergroup_id| rt.td.supergroup_full(supergroup_id).ok())
                .and_then(|full| {
                    full.get("description")
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                })
                .unwrap_or_default();
            (title, bio)
        }
    }
}

fn quote_of(message: &Value) -> Option<(i64, String)> {
    let reply = message.get("reply_to").filter(|reply| {
        reply.get("@type").and_then(Value::as_str) == Some("messageReplyToMessage")
    })?;
    let quote = reply
        .get("quote")
        .filter(|quote| quote.get("@type").and_then(Value::as_str) == Some("textQuote"))?;
    let body = quote
        .get("text")
        .and_then(|text| text.get("text"))
        .and_then(Value::as_str)?
        .to_owned();
    if body.trim().is_empty() {
        return None;
    }
    Some((
        reply.get("chat_id").and_then(Value::as_i64).unwrap_or(0),
        body,
    ))
}

fn forward_origin(message: &Value) -> Option<i64> {
    let origin = message.get("forward_info")?.get("origin")?;
    match origin.get("@type").and_then(Value::as_str) {
        Some("messageOriginChannel") => origin.get("chat_id").and_then(Value::as_i64),
        Some("messageOriginChat") => origin.get("sender_chat_id").and_then(Value::as_i64),
        _ => None,
    }
}

fn remote_file_id(file: &Value) -> Option<String> {
    file.get("remote")
        .and_then(|remote| remote.get("id"))
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .map(str::to_owned)
}

fn unique_id_of(file: &Value) -> Option<String> {
    file.get("remote")
        .and_then(|remote| remote.get("unique_id"))
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .map(str::to_owned)
}

/// 拉一次链接元数据（标题 + 描述）。失败返回 `None`，不影响主流程。
fn fetch_link_preview(rt: &Runtime, url: &str) -> Option<(String, String)> {
    let response = rt
        .http
        .get(url)
        .header(
            "User-Agent",
            "Mozilla/5.0 (compatible; FuyuBot-Adblock/0.1)",
        )
        .header("Accept", "text/html,application/xhtml+xml")
        .call()
        .ok()?;
    if !(200..400).contains(&response.status().as_u16()) {
        return None;
    }
    let mut response = response;
    let body = response.body_mut().read_to_string().ok()?;
    let body = crate::util::truncate(&body, 200_000);
    let title = between(&body, "<title", "</title>")
        .map(|tag| strip_html(tag))
        .unwrap_or_default();
    let description = meta_content(&body, "description")
        .or_else(|| meta_content(&body, "og:description"))
        .unwrap_or_default();
    if title.is_empty() && description.is_empty() {
        return None;
    }
    Some((title, description))
}

fn between<'a>(text: &'a str, start: &str, end: &str) -> Option<&'a str> {
    let from = text.find(start)? + start.len();
    let from = text[from..].find('>')? + from + 1;
    let to = text[from..].find(end)? + from;
    Some(&text[from..to])
}

fn meta_content(body: &str, name: &str) -> Option<String> {
    let needle = format!("name=\"{name}\"");
    let alt = format!("property=\"{name}\"");
    let index = body.find(&needle).or_else(|| body.find(&alt))?;
    let rest = &body[index..];
    let content_index = rest.find("content=\"")?;
    let start = index + content_index + "content=\"".len();
    let end = body[start..].find('"')? + start;
    Some(strip_html(&body[start..end]))
}

fn strip_html(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_tag = false;
    for c in text.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// 让编译器知道这些类型被用到（`GroupConfig` / `LimitMode` 在阈值说明里提到）。
#[allow(dead_code)]
fn _config_types(_: crate::store::GroupConfig, _: crate::store::LimitMode) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_mentions() {
        let text = "@SpamUser1 你好 @ab 与 @_ok_name 和 @long_name2";
        let mentions = extract_mentions(text);
        assert!(mentions.contains(&"spamuser1".to_owned()));
        assert!(!mentions.contains(&"ab".to_owned()));
        assert!(mentions.contains(&"long_name2".to_owned()));
    }

    #[test]
    fn strips_html_tags() {
        assert_eq!(strip_html("<b>你好</b> 世界"), "你好 世界");
    }
}
