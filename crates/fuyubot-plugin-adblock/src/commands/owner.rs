//! `/admatch`、`/adaiadd`：Bot 主人排查用的两个工具。
//!
//! - `/admatch <文本>`：一次查五个黑名单库，看这段文本到底命中哪条规则；
//! - `/adaiadd <文本>`：让 AI 判一段文本，判定为广告且关键词 > 3 时入库并返回 ID。
//!
//! 两个都只在私聊/任意聊天里给主人用，结果用 Markdown 排版（排查时更好读）。

use crate::simplify::Command;

use crate::ai::{self, AiMeta, ModerationInput};
use crate::api::Sender;
use crate::flow;
use crate::log;
use crate::runtime::Runtime;
use crate::store::doc_id;

/// `/admatch <文本>`
pub async fn ad_match(rt: &Runtime, command: &Command, sender: Sender) {
    let chat_id = command.chat_id;
    let reply_to = Some(command.message_id);
    if !flow::ensure_owner(rt, chat_id, sender, reply_to).await {
        return;
    }
    let query = command.args.trim().to_owned();
    if query.is_empty() {
        flow::send_tip(
            rt,
            chat_id,
            "用法:\n/admatch <文本>\n\n示例:\n/admatch 兼职日赚500加微信\n\n同时查询以下库:\n- ads_blacklist（关键词组 AND 匹配）\n- bio_blacklist（全文精确匹配）\n- name_blacklist（名称精确）\n- username_blacklist（用户名包含）\n- ai_fulltext_blacklist（全文精确）",
            reply_to,
            60,
        ).await;
        return;
    }

    let ads = rt.db.match_ad_keywords(&query).await;
    let bio = rt.db.match_bio(&query).await;
    let name = rt.db.match_name(&query).await;
    let username = rt.db.match_username(&query).await;
    let fulltext = rt.db.match_ai_fulltext(&query).await;

    let mut lines: Vec<String> = Vec::new();
    let mut hits: Vec<&str> = Vec::new();

    match &ads {
        Some(record) => {
            hits.push("ads_blacklist");
            let keywords = record
                .get("keywords")
                .and_then(serde_json::Value::as_array)
                .cloned()
                .unwrap_or_default();
            let rendered = keywords
                .iter()
                .filter_map(serde_json::Value::as_str)
                .map(|keyword| format!("`{keyword}`"))
                .collect::<Vec<_>>()
                .join(", ");
            lines.push(format!(
                "✅ **ads_blacklist**\n**ID**: `{}`\n**Keywords**: {rendered}\n**Keyword count**: {}",
                doc_id(record),
                keywords.len()
            ));
        }
        None => lines.push("❌ **ads_blacklist**: 未命中".to_owned()),
    }

    match &bio {
        Some(record) => {
            hits.push("bio_blacklist");
            lines.push(format!(
                "✅ **bio_blacklist**\n**ID**: `{}`",
                doc_id(record)
            ));
        }
        None => lines.push("❌ **bio_blacklist**: 未命中（需全文一致）".to_owned()),
    }

    match &name {
        Some(record) => {
            hits.push("name_blacklist");
            lines.push(format!(
                "✅ **name_blacklist**\n**ID**: `{}`\n**说明**: 整段文本需与名称完全一致才命中原库；此处用 trim 后全文比对",
                doc_id(record)
            ));
        }
        None => lines.push("❌ **name_blacklist**: 未命中（需与名称完全一致）".to_owned()),
    }

    match &username {
        Some(record) => {
            hits.push("username_blacklist");
            lines.push(format!(
                "✅ **username_blacklist**\n**ID**: `{}`",
                doc_id(record)
            ));
        }
        None => lines.push("❌ **username_blacklist**: 未命中".to_owned()),
    }

    match &fulltext {
        Some(record) => {
            hits.push("ai_fulltext_blacklist");
            lines.push(format!(
                "✅ **ai_fulltext_blacklist**\n**ID**: `{}`\n**说明**: 与已封禁广告全文完全一致",
                doc_id(record)
            ));
        }
        None => lines.push("❌ **ai_fulltext_blacklist**: 未命中（需全文一致）".to_owned()),
    }

    let preview = crate::util::truncate(&query, 200);
    let summary = if hits.is_empty() {
        "所有库均未命中".to_owned()
    } else {
        format!("命中 {} 个库: {}", hits.len(), hits.join(", "))
    };
    let body = [
        "#AdMatch".to_owned(),
        format!("**输入**: {preview}"),
        format!("**结果**: {summary}"),
        String::new(),
        lines.join("\n\n"),
    ]
    .join("\n\n");
    flow::send_tip(rt, chat_id, &body, reply_to, 60).await;
    log::info(
        &rt.host,
        format!(
            "[admatch] owner 查询 hits={}",
            if hits.is_empty() {
                "none".to_owned()
            } else {
                hits.join(",")
            }
        ),
    );
}

/// `/adaiadd <文本>`
pub async fn ad_ai_add(rt: &Runtime, command: &Command, sender: Sender) {
    let chat_id = command.chat_id;
    let reply_to = Some(command.message_id);
    if !flow::ensure_owner(rt, chat_id, sender, reply_to).await {
        return;
    }
    let ad_text = command.args.trim().to_owned();
    if ad_text.is_empty() {
        flow::send_tip(
            rt,
            chat_id,
            "用法:\n/adaiadd <广告文本>\n\n示例:\n/adaiadd 兼职日赚800，有微信的来\n\n说明：仅对消息文本做 AI 判断；判定为广告且关键词>3 时写入 ads_blacklist 并返回 ID。",
            reply_to,
            60,
        ).await;
        return;
    }

    flow::send_tip(rt, chat_id, "正在 AI 判断，请稍候…", reply_to, 0).await;
    let result = ai::review(
        rt,
        &ModerationInput::text(ad_text),
        None,
        None,
        &AiMeta {
            scene: Some("adaiadd"),
            message_type: None,
            force_default: true,
        },
    )
    .await;

    if ai::is_failure_reason(&result.reason) || result.confidence <= 0.0 {
        flow::send_tip(
            rt,
            chat_id,
            &format!(
                "❌ AI 调用失败/异常\n\n**原因**: {}",
                if result.reason.is_empty() {
                    "未知错误"
                } else {
                    &result.reason
                }
            ),
            reply_to,
            60,
        )
        .await;
        return;
    }
    if !result.ad_detected {
        flow::send_tip(
            rt,
            chat_id,
            &format!(
                "ℹ️ **AI 未判定为广告，未写入数据库**\n\n**置信度**: {}\n**理由**: {}",
                crate::text::confidence_percent(result.confidence),
                if result.reason.is_empty() {
                    "无"
                } else {
                    &result.reason
                }
            ),
            reply_to,
            60,
        )
        .await;
        return;
    }
    let keywords = &result.message.keywords;
    if keywords.len() <= 3 {
        let rendered = keywords
            .iter()
            .map(|keyword| format!("`{keyword}`"))
            .collect::<Vec<_>>()
            .join(", ");
        flow::send_tip(
            rt,
            chat_id,
            &format!(
                "ℹ️ **判定为广告，但关键词 ≤ 3，未写入数据库**\n\n**置信度**: {}\n**关键词数**: {}\n**Keywords**: {}\n**理由**: {}",
                crate::text::confidence_percent(result.confidence),
                keywords.len(),
                if rendered.is_empty() {
                    "-".to_owned()
                } else {
                    rendered
                },
                if result.reason.is_empty() {
                    "无"
                } else {
                    &result.reason
                }
            ),
            reply_to,
            60,
        ).await;
        return;
    }
    match rt.db.add_ad_keywords(keywords, result.r18).await {
        Ok(id) => {
            let rendered = keywords
                .iter()
                .map(|keyword| format!("`{keyword}`"))
                .collect::<Vec<_>>()
                .join(", ");
            flow::send_tip(
                rt,
                chat_id,
                &format!(
                    "✅ **已写入 ads_blacklist**\n\n**ID**: `{id}`\n**ad_detected**: true\n**confidence**: {}\n**r18**: {}\n**关键词数**: {}\n**Keywords**: {rendered}\n**理由**: {}",
                    crate::text::confidence_percent(result.confidence),
                    if result.r18 { "true" } else { "false" },
                    keywords.len(),
                    if result.reason.is_empty() {
                        "无"
                    } else {
                        &result.reason
                    }
                ),
                reply_to,
                60,
            ).await;
            log::info(
                &rt.host,
                format!(
                    "[adaiadd] owner 入库成功 id={id} keywords={} conf={}",
                    keywords.len(),
                    result.confidence
                ),
            );
        }
        Err(error) => {
            flow::send_tip(
                rt,
                chat_id,
                &format!(
                    "❌ 判定为广告且关键词 {} 个，但写入数据库失败。\n**错误**: {error}",
                    keywords.len()
                ),
                reply_to,
                60,
            )
            .await;
        }
    }
}
