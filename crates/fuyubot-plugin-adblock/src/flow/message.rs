//! 一条新消息的完整处理链（`updateNewMessage`）。
//!
//! 顺序对应 TS 版 `index.ts` 的 updateHandlers：
//!
//! ```text
//! 私聊 AI 配置流程（消费掉就结束）
//!   → 最近消息缓存 + 用户历史文本
//!   → 命令（注册表里的名字交给 commands::dispatch）
//!   → 消息索引（records）
//!   → GKY 封禁记录转发
//!   → 广告检测 / AI 审核（本文件下半部分）
//! ```
//!
//! ## 命令与检测是**两条独立路径**
//!
//! TS 里框架的 `handleCommand` 与插件的 `updateHandlers.updateNewMessage` 各跑各的：
//! 一条 `/xxx` 消息既可能被命令处理，也照样进 `isSpam` / `LogMesgIds`。
//! 这里保持同样的语义——**命令处理后不 return**，检测继续跑。
//!
//! 反过来，**不能因为「以 `/` 开头」就当成命令**：`command_of` 解析出的名字如果不在
//! 注册表里，它就是普通文本，必须继续检测（否则 `/接单 代收款` 就能绕过全部规则）。
//!
//! 整条链是 `async` 的：命令、检测、AI、处置都 `.await`，不占线程干等。

use serde_json::Value;

use crate::action;
use crate::ai::{self, AiMeta, Contact, ModerationInput, Profile};
use crate::api::{Sender, message_sender};
use crate::detect::{self, SpamHit};
use crate::flow::{self, SenderDetails};
use crate::log;
use crate::runtime::Runtime;
use crate::store::doc_id;
use crate::text;

/// 处理一条 `updateNewMessage`。
pub async fn handle(rt: &Runtime, update: &Value) {
    let Some(message) = update.get("message") else {
        return;
    };
    let Some(chat_id) = message.get("chat_id").and_then(Value::as_i64) else {
        return;
    };
    // 没有消息 id 的更新没法索引 / 处置，直接放过。
    if message.get("id").and_then(Value::as_i64).is_none() {
        return;
    }
    // `chat_allowlist`：不在列表里的群整条更新都忽略（私聊放行，AI 配置流程照常）。
    if !flow::chat_enabled(rt, chat_id) {
        return;
    }

    // 入群提示：群配置要求删除时直接撤回，不再进索引 / 命令 / 检测链路。
    if flow::join::handle(rt, message).await {
        return;
    }

    // 群绑定频道自动转发过来的帖子：Telegram 会自动置顶，这里撤掉。
    flow::pin::handle_channel_post(rt, message).await;

    // 置顶服务消息：绑定频道的置顶同样撤掉。
    if flow::pin::handle_pin_message(rt, message).await {
        return;
    }

    // 私聊里的 AI 配置流程：命中就消费掉这条消息。
    if flow::is_private(chat_id) && crate::commands::ai_config::consume_private(rt, message).await {
        return;
    }

    // 缓存：最近消息（`/spam` 引用兜底）。用户历史文本在 `records` 里落库。
    rt.cache.remember_message(chat_id, message);
    let sender = message_sender(message);

    // 命令：只认注册表里的名字，且**不 return**——命令消息同样要进索引、同样要被检测。
    // 注意传的是**整条 update**：`command_of` 会自己取 `update.message`。
    if let Some(sender) = sender {
        crate::commands::dispatch(rt, update, sender).await;
    }

    flow::records::log_message(rt, message).await;
    flow::gky::forward_ban_record(rt, message).await;
    check(rt, message).await;
}

/// 广告判定 + 处置。
async fn check(rt: &Runtime, message: &Value) {
    let Some(chat_id) = message.get("chat_id").and_then(Value::as_i64) else {
        return;
    };
    let Some(message_id) = message.get("id").and_then(Value::as_i64) else {
        return;
    };
    // 下面每一处 return 都**必须**留痕：这条链路没有任何别的「检查过/没检查」日志，
    // 静默 return 就是「消息进了日志插件、广告插件毫无动静」的唯一来源。
    if !flow::is_group(rt, chat_id).await {
        log::skip(
            &rt.host,
            chat_id,
            "not_group",
            &format!("[检查跳过] 群 {chat_id} 不是群 / 取不到 chat 信息，msg={message_id}"),
        );
        return;
    }
    if !flow::am_i_admin(rt, chat_id).await {
        log::skip(
            &rt.host,
            chat_id,
            "no_rights",
            &format!(
                "[检查跳过] 群 {chat_id} Bot 缺「删除消息 + 封禁用户」权限（或拿不到自身 id），msg={message_id}"
            ),
        );
        return;
    }
    let Some(sender) = message_sender(message) else {
        log::debug(
            &rt.host,
            format!("[检查跳过] 群 {chat_id} 取不到发送者，msg={message_id}"),
        );
        return;
    };
    let details = flow::sender_details(rt, chat_id, sender).await;
    let is_white = rt.db.is_whitelisted(chat_id, sender.id()).await;
    if details.is_admin || is_white {
        log::debug(
            &rt.host,
            format!(
                "[检查跳过] 群 {chat_id} 发送者 {} 是{}",
                sender.id(),
                if details.is_admin {
                    "群管理员"
                } else {
                    "白名单用户"
                }
            ),
        );
        return;
    }

    let content_hit = detect::detect(rt, message).await;

    // 访客机器人：直接处置，并连带处理「谁把它拉进来的」那条消息。
    if details.is_bot && !details.is_admin && !details.is_member {
        let hit = SpamHit::hit(
            "bot",
            None,
            "机器人访客模式在本群中禁用，如果需要请将机器人加入群组并赋予管理权限",
        );
        action::punish(rt, chat_id, sender, &hit, Some(message_id)).await;
        punish_trigger(rt, chat_id, message, &content_hit).await;
        return;
    }

    // 转发 / 引用自黑名单频道
    if let Some(origin) = origin_chat_id(message)
        && let Some(record) = rt.db.match_forward(origin).await
    {
        let hit = SpamHit::hit("forward", Some(doc_id(&record)), "from blacklisted channel");
        action::punish(rt, chat_id, sender, &hit, Some(message_id)).await;
        return;
    }

    let mut hit = content_hit;
    let config = rt.db.config_or_default(chat_id).await;
    if !hit.matched && !config.ai_review_enabled {
        log::debug(
            &rt.host,
            format!(
                "[检查跳过] 群 {chat_id} 未开启 ai_review_enabled，msg={message_id} 只走本地规则"
            ),
        );
    }
    if !hit.matched
        && config.ai_review_enabled
        && let Some(ai_hit) =
            review_with_ai(rt, message, sender, &details, chat_id, message_id).await
    {
        hit = ai_hit;
    }

    if !hit.matched {
        // 24h 监视：正常发言攒够数就提前解除监视（被处置的消息不算）。
        // 没开「新用户检查」的群根本不会建监视记录，直接跳过这次查询。
        if config.check_new_member
            && let Sender::User(user_id) = sender
        {
            note_watch_clean_message(rt, chat_id, user_id).await;
        }
        return;
    }
    // TS 版在处置前又查了一次管理员权限：中间可能刚好被摘了权限。
    if !flow::am_i_admin(rt, chat_id).await {
        return;
    }
    action::punish(rt, chat_id, sender, &hit, Some(message_id)).await;
}

/// 被访客机器人回复的那条消息的发送者：一起处置（「连带封禁」）。
async fn punish_trigger(rt: &Runtime, chat_id: i64, message: &Value, content_hit: &SpamHit) {
    let Some(reply) = message.get("reply_to").filter(|reply| {
        reply.get("@type").and_then(Value::as_str) == Some("messageReplyToMessage")
    }) else {
        return;
    };
    let Some(reply_id) = reply.get("message_id").and_then(Value::as_i64) else {
        return;
    };
    let trigger = match rt.cache.recent_message(chat_id, reply_id) {
        Some(cached) => Some(cached),
        None => rt.td.message(chat_id, reply_id).await,
    };
    let Some(trigger) = trigger else {
        return;
    };
    let Some(trigger_sender) = message_sender(&trigger) else {
        return;
    };
    let trigger_details = flow::sender_details(rt, chat_id, trigger_sender).await;
    let trigger_white = rt.db.is_whitelisted(chat_id, trigger_sender.id()).await;
    if trigger_details.is_admin || trigger_white {
        log::debug(
            &rt.host,
            format!(
                "连带删除被跳过: 原始发送者是{}",
                if trigger_details.is_admin {
                    "管理员"
                } else {
                    "白名单用户"
                }
            ),
        );
        return;
    }
    let hit = SpamHit::hit(
        if content_hit.matched {
            content_hit.kind.clone()
        } else {
            "bot".to_owned()
        },
        content_hit.id.clone(),
        if content_hit.matched {
            "连带封禁：原始消息触发垃圾bot，且命中广告关键词"
        } else {
            "连带封禁：原始消息触发垃圾bot"
        },
    );
    action::punish(rt, chat_id, trigger_sender, &hit, Some(reply_id)).await;
}

/// 转发的来源频道 / 被引用消息的聊天。
fn origin_chat_id(message: &Value) -> Option<i64> {
    if let Some(forward) = message.get("forward_info")
        && let Some(origin) = forward.get("origin")
        && origin.get("@type").and_then(Value::as_str) == Some("messageOriginChannel")
    {
        return origin.get("chat_id").and_then(Value::as_i64);
    }
    let reply = message.get("reply_to")?;
    if reply.get("@type").and_then(Value::as_str) == Some("messageReplyToMessage") {
        return reply.get("chat_id").and_then(Value::as_i64);
    }
    None
}

/// 取引用文本与来源 chat（只认 `textQuote`）。
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

/// AI 深度审核。返回 `Some(命中)` 才处置；未命中 / 未触发返回 `None`。
///
/// 全文精确比对（[`fulltext_hit`]）是**本地黑名单**，先于 AI 执行：命中就直接返回，
/// 既不发 AI 请求，也不贴 👀。
async fn review_with_ai(
    rt: &Runtime,
    message: &Value,
    sender: Sender,
    details: &SenderDetails,
    chat_id: i64,
    message_id: i64,
) -> Option<SpamHit> {
    let content = message.get("content")?;
    let content_type = content.get("@type").and_then(Value::as_str).unwrap_or("");
    let mut input = ModerationInput::of_kind(match content_type {
        "messagePhoto" => "photo",
        "messageVideo" => "video",
        "messageSticker" => "sticker",
        "messageContact" => "contact",
        _ => "text",
    });

    let mut message_text = String::new();
    match content_type {
        "messageText" => {
            message_text = content
                .get("text")
                .and_then(|text| text.get("text"))
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned();
            input.message = Some(message_text.clone());
        }
        "messageRichMessage" => {
            message_text = text::rich_message_text(content);
            input.message = Some(message_text.clone());
        }
        "messagePhoto" | "messageVideo" => {
            message_text = content
                .get("caption")
                .and_then(|caption| caption.get("text"))
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned();
            input.message = Some(message_text.clone());
        }
        "messageContact" => {
            let contact_name = text::contact_name(content);
            let vcard = text::contact_vcard(content);
            input.contact = Some(Contact {
                name: Some(contact_name),
                phone: content
                    .get("contact")
                    .and_then(|contact| contact.get("phone_number"))
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                username: content
                    .get("contact")
                    .and_then(|contact| contact.get("username"))
                    .and_then(Value::as_str)
                    .map(str::to_owned),
            });
            input.message = Some(vcard);
        }
        _ => {}
    }

    // 自定义 emoji：把 [E1] 占位文本塞进 message；像「用 emoji 当字」时再出联系表。
    let prepared = crate::custom_emoji::prepare(rt, content, &mut input).await;

    let quote = quote_of(message);
    if let Some((_, quote_text)) = &quote {
        input = input.with_quote(quote_text.clone());
    }

    // ── 要不要触发 AI ─────────────────────────────────────────
    let keywords = rt.db.global_ai_keywords().await;
    let lower = message_text.to_lowercase();
    let keyword_hit = !message_text.is_empty()
        && keywords
            .iter()
            .any(|keyword| !keyword.is_empty() && lower.contains(&keyword.to_lowercase()));

    let mut watch_hit = false;
    if !keyword_hit
        && !message_text.is_empty()
        && matches!(
            content_type,
            "messageText" | "messageRichMessage" | "messagePhoto" | "messageVideo"
        )
        && let Sender::User(user_id) = sender
    {
        watch_hit = rt.db.is_profile_changed_watch_user(chat_id, user_id).await;
        if watch_hit {
            log::info(
                &rt.host,
                format!(
                    "[入群监视] 用户({user_id}) 群({chat_id}) 入群24h内改过资料，消息强制AI审查 msg={message_id}"
                ),
            );
        }
    }

    // 「用 emoji 当字」的消息文本里一个关键词都没有，必须靠这条把它送进 AI。
    let emoji_art = prepared
        .as_ref()
        .is_some_and(|prepared| prepared.report.is_glyph_art());
    let should_review = content_type == "messageContact"
        || quote.is_some()
        || keyword_hit
        || watch_hit
        || emoji_art;
    if !should_review {
        return None;
    }

    // ── 全文精确比对（与已封禁广告完全一致） ─────────────────────
    // 本地判定，排在 AI 之前：命中就不调 AI，也不贴 👀。
    if let Some(hit) = fulltext_hit(
        rt,
        &message_text,
        quote.as_ref().map(|(_, text)| text.as_str()),
    )
    .await
    {
        log::info(
            &rt.host,
            format!("[AI_FullText] 命中全文黑名单，跳过 AI 审核 msg={message_id}"),
        );
        // 联系表已经出图的话顺手删掉，别在临时目录里留垃圾。
        if let Some(prepared) = prepared {
            prepared.cleanup();
        }
        return Some(hit);
    }

    // 资料 + 主页挂载频道：只有真进 AI 才取，且共用同一次 `getUserFullInfo`。
    let full = match sender {
        Sender::User(user_id) => flow::user_profile(rt, user_id).await,
        Sender::Chat(_) => None,
    };
    input.profile = Some(Profile {
        name: user_first_name(details),
        bio: Some(full.as_ref().map(flow::bio_of).unwrap_or_default()),
    });

    // 最近历史（只对用户发送者）
    if let Sender::User(user_id) = sender {
        if let Some(full) = &full
            && let Some((name, description)) = flow::personal_channel_of(rt, full).await
        {
            input = input.with_personal_channel(name, description);
        }
        let history = rt
            .db
            .recent_texts(chat_id, user_id, Some(message_id), 5)
            .await;
        input = input.with_history(history);
    }

    // 给被审的消息加 👀（Bot 账号必须用 setMessageReactions）。
    // 只在**真的会调 AI** 时贴：没配接口的群贴了也只是空等，不该出现“正在审核”的假象。
    if ai::has_backend(rt, Some(chat_id), false).await {
        if let Err(error) = rt.td.react(chat_id, message_id, "👀").await {
            log::debug(&rt.host, format!("加 👀 回应失败：{error}"));
        }
    } else {
        log::debug(
            &rt.host,
            format!("未配置可用 AI 接口，跳过 👀 msg={message_id}"),
        );
    }

    let meta = AiMeta {
        scene: Some(if content_type == "messageContact" {
            "message_contact"
        } else {
            "message"
        }),
        message_type: Some(content_type.to_owned()),
        force_default: false,
    };
    let image = prepared
        .as_ref()
        .and_then(|prepared| prepared.image.as_deref())
        .and_then(std::path::Path::to_str);
    let result = ai::review(rt, &input, image, Some(chat_id), &meta).await;
    if let Some(prepared) = prepared {
        prepared.cleanup();
    }

    // ── 结果落库 + 判定 ───────────────────────────────────────
    let confidence_high = result.confidence > 0.9;
    // 联系人广告只置 `contact_ad` 也算命中（见 `ModerationResult::is_ad`）。
    let is_ad = result.is_ad();
    let mut hit = None;

    if is_ad {
        let mut reason = if result.reason.is_empty() {
            if result.contact.contact_ad && !result.ad_detected {
                "联系人名称/号码含广告信息".to_owned()
            } else {
                "检测到广告内容".to_owned()
            }
        } else {
            result.reason.clone()
        };
        if !result.ad_detected && result.quoted_message.has_ad {
            reason = format!("引用消息含广告: {reason}");
        }

        if result.ad_detected && confidence_high {
            persist_keywords(
                rt,
                &result.message.keywords,
                &message_text,
                "保存正文关键词/全文失败",
            )
            .await;
        }
        if let Some((quoted_chat, quote_text)) = &quote
            && *quoted_chat != chat_id
        {
            if rt.db.match_forward(*quoted_chat).await.is_none()
                && rt.db.add_forward(*quoted_chat).await.is_ok()
            {
                log::info(
                    &rt.host,
                    format!("AI自动添加引用来源 {quoted_chat} 到转发黑名单（引用消息含广告）"),
                );
            }
            if result.quoted_message.has_ad && confidence_high {
                persist_keywords(
                    rt,
                    &result.quoted_message.keywords,
                    quote_text,
                    "保存引用关键词/全文失败",
                )
                .await;
            }
        }
        if content_type == "messageContact" && result.contact.contact_ad {
            let contact_name = text::contact_name(content);
            if !contact_name.is_empty()
                && let Err(error) = rt.db.add_name(&contact_name).await
            {
                log::warn(&rt.host, format!("保存联系人名称失败：{error}"));
            }
            let vcard = text::contact_vcard(content);
            if !vcard.is_empty()
                && let Err(error) = rt.db.add_bio(&[vcard]).await
            {
                log::warn(&rt.host, format!("保存联系人 bio 失败：{error}"));
            }
        }
        hit = Some(SpamHit::hit("AI_Text", None, format!("AI检测: {reason}")));
    }

    hit
}

/// 全文精确比对：正文优先，其次引用文本。命中返回 `AI_FullText`。
///
/// 这是本地黑名单判定，**不调 AI**——调用方必须在贴 👀、发 AI 请求之前先查它。
async fn fulltext_hit(rt: &Runtime, body: &str, quote: Option<&str>) -> Option<SpamHit> {
    if !body.is_empty()
        && let Some(record) = rt.db.match_ai_fulltext(body).await
    {
        return Some(SpamHit::hit(
            "AI_FullText",
            Some(doc_id(&record)),
            "与已封禁广告全文完全一致",
        ));
    }
    if let Some(quote_text) = quote
        && let Some(record) = rt.db.match_ai_fulltext(quote_text).await
    {
        return Some(SpamHit::hit(
            "AI_FullText",
            Some(doc_id(&record)),
            "引用与已封禁广告全文完全一致",
        ));
    }
    None
}

/// 24h 监视期内「未判为广告」的消息上限：**超过**这个数就解除监视。
///
/// 需求是「超过 4 条正常消息」→ 第 5 条正常消息落地时删除监视记录。
const WATCH_CLEAN_MESSAGE_LIMIT: u32 = 4;

/// 记一条「未判为广告」的消息；攒够数就把用户从 24h 监视里移除。
///
/// 只对仍在监视期内的用户生效；移除后该用户不再被强制 AI 审查，也不再因
/// 入群 24h 内改资料触发复检。
async fn note_watch_clean_message(rt: &Runtime, chat_id: i64, user_id: i64) {
    let Some(watch) = rt.db.active_watch(chat_id, user_id).await else {
        return;
    };
    let count = watch.clean_message_count.saturating_add(1);
    if count > WATCH_CLEAN_MESSAGE_LIMIT {
        rt.db.remove_watch(chat_id, user_id).await;
        log::info(
            &rt.host,
            format!(
                "[入群监视] 用户({user_id}) 群({chat_id}) 已发 {count} 条正常消息，提前解除 24h 监视"
            ),
        );
    } else {
        rt.db
            .set_watch_clean_messages(chat_id, user_id, count)
            .await;
    }
}

/// 关键词 > 3 存关键词组；1–2 个存全文；正好 3 个**什么都不存**
/// （与 TS `Ads/index.ts`:326-330 的三段分支一致——否则一次 3 关键词的判定会把整段
/// 正文写进 `ai_fulltext_blacklist`，之后同样的话会被无 AI 直接封）。
async fn persist_keywords(rt: &Runtime, keywords: &[String], body: &str, context: &str) {
    if keywords.len() > 3 {
        if let Err(error) = rt.db.add_ad_keywords(keywords, false).await {
            log::warn(&rt.host, format!("{context}：{error}"));
        }
    } else if !keywords.is_empty() && keywords.len() < 3 && !body.trim().is_empty() {
        if let Err(error) = rt.db.add_ai_fulltext(body).await {
            log::warn(&rt.host, format!("{context}：{error}"));
        }
    }
}

/// 发送者的名字（AI 输入的 `profile.name`，TS 版只取 `first_name`）。
fn user_first_name(details: &SenderDetails) -> Option<String> {
    details.user.as_ref().and_then(|user| {
        user.get("first_name")
            .and_then(Value::as_str)
            .map(str::to_owned)
    })
}
