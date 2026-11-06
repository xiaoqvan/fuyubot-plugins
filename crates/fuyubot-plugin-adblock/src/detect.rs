//! 内容检测：一条消息「像不像广告」。
//!
//! 对应 TS 版 `Ads/message*.ts` 六个检查器，判定口径保持一致：
//!
//! | 内容类型 | 检查顺序 |
//! | --- | --- |
//! | 文本 / 富消息 | 广告关键词组（AND）→ 用户名黑名单 |
//! | 图片 | 标题 → 群文件规则 → 全局文件 ID |
//! | 视频 | 标题 → 群文件规则 → 全局文件 ID |
//! | 联系人 | 姓名 + vCard → 关键词 → 用户名 |
//! | 贴纸 | 群贴纸集规则 → 全局贴纸集 → 全局贴纸 |
//!
//! 命中结果是 [`SpamHit`]：`kind` 会原样进日志与封禁记录（`Text` / `Username` /
//! `group_FileRule` …），排查时能分清是哪一条规则干的。
//!
//! 检查器要查库（`rt.db` 是 async），所以它们都是 `async fn`；纯字符串处理
//! （[`text_of_field`]）保持同步。

use serde_json::Value;

use crate::runtime::Runtime;
use crate::store::doc_id;
use crate::text;

/// 一次内容检测的结果。
#[derive(Debug, Clone, Default)]
pub struct SpamHit {
    pub matched: bool,
    /// 规则名（写进日志的 `Type`）。
    pub kind: String,
    /// 命中的库记录 `_id`（没有就是 `None`）。
    pub id: Option<String>,
    pub details: String,
}

impl SpamHit {
    pub fn hit(kind: impl Into<String>, id: Option<String>, details: impl Into<String>) -> Self {
        Self {
            matched: true,
            kind: kind.into(),
            id,
            details: details.into(),
        }
    }

    pub fn none() -> Self {
        Self::default()
    }
}

/// 按内容类型分发。
pub async fn detect(rt: &Runtime, message: &Value) -> SpamHit {
    let Some(content) = message.get("content") else {
        return SpamHit::none();
    };
    let kind = content.get("@type").and_then(Value::as_str).unwrap_or("");
    match kind {
        "messageText" => check_text(rt, &text::message_text(message)).await,
        "messageRichMessage" => check_text(rt, &text::rich_message_text(content)).await,
        "messagePhoto" => check_photo(rt, message).await,
        "messageVideo" => check_video(rt, message).await,
        "messageContact" => check_contact(rt, content).await,
        "messageSticker" => check_sticker(rt, message).await,
        _ => SpamHit::none(),
    }
}

/// 文本：关键词组（全部词都在文本里）→ 用户名黑名单。
pub async fn check_text(rt: &Runtime, text: &str) -> SpamHit {
    if text.trim().is_empty() {
        return SpamHit::none();
    }
    if let Some(group) = rt.db.match_ad_keywords(text).await {
        return SpamHit::hit("Text", Some(doc_id(&group)), "hasAdKeyword");
    }
    if let Some(record) = rt.db.match_username(text).await {
        return SpamHit::hit("Username", Some(doc_id(&record)), "hasUsername");
    }
    SpamHit::none()
}

/// 图片：标题 → 群规则 → 全局文件 ID。
async fn check_photo(rt: &Runtime, message: &Value) -> SpamHit {
    let content = message.get("content").unwrap_or(&Value::Null);
    let caption = text_of_field(content, "caption");
    if !caption.is_empty() {
        let hit = check_text(rt, &caption).await;
        if hit.matched {
            return hit;
        }
    }

    let ids = text::photo_unique_ids(content);
    if ids.is_empty() {
        return SpamHit::none();
    }

    let chat_id = message.get("chat_id").and_then(Value::as_i64).unwrap_or(0);
    let config = rt.db.config_or_default(chat_id).await;
    if config
        .spam_file_id
        .iter()
        .any(|allowed| ids.iter().any(|id| id == allowed))
    {
        return SpamHit::hit(
            "group_FileRule",
            Some(format!("photo_ids:{}", ids.join(","))),
            "groupRule",
        );
    }

    if let Some(record) = rt.db.match_file_ids(&ids).await {
        return SpamHit::hit("Photo", Some(doc_id(&record)), "hasSpamFileid");
    }
    SpamHit::none()
}

/// 视频：标题 → 群规则 → 全局文件 ID。
async fn check_video(rt: &Runtime, message: &Value) -> SpamHit {
    let content = message.get("content").unwrap_or(&Value::Null);
    let caption = text_of_field(content, "caption");
    if !caption.is_empty() {
        let hit = check_text(rt, &caption).await;
        if hit.matched {
            return hit;
        }
    }

    let unique_id = content
        .get("video")
        .and_then(|video| video.get("video"))
        .and_then(text::remote_unique_id);
    let Some(unique_id) = unique_id else {
        return SpamHit::none();
    };

    let chat_id = message.get("chat_id").and_then(Value::as_i64).unwrap_or(0);
    let config = rt.db.config_or_default(chat_id).await;
    if config.spam_file_id.iter().any(|id| id == &unique_id) {
        return SpamHit::hit("group_rule", Some(format!("uq:{unique_id}")), "groupRule");
    }
    if let Some(record) = rt.db.match_file_ids(&[unique_id]).await {
        return SpamHit::hit("Video", Some(doc_id(&record)), "hasSpamFileid");
    }
    SpamHit::none()
}

/// 联系人：姓名 + vCard 查关键词 / 用户名。
async fn check_contact(rt: &Runtime, content: &Value) -> SpamHit {
    let name = text::contact_name(content);
    let vcard = text::contact_vcard(content);
    let search = [name.as_str(), vcard.as_str()]
        .iter()
        .filter(|part| !part.trim().is_empty())
        .copied()
        .collect::<Vec<_>>()
        .join(" ");
    if search.trim().is_empty() {
        return SpamHit::none();
    }
    let hit = check_text(rt, &search).await;
    if hit.matched {
        return SpamHit::hit(
            if hit.kind == "Username" {
                "Contact_Username"
            } else {
                "Contact"
            },
            hit.id,
            if hit.kind == "Username" {
                "contact_hasUsername"
            } else {
                "contact_hasAdKeyword"
            },
        );
    }
    SpamHit::none()
}

/// 贴纸：群贴纸集规则 → 全局贴纸集 → 全局贴纸。
async fn check_sticker(rt: &Runtime, message: &Value) -> SpamHit {
    let content = message.get("content").unwrap_or(&Value::Null);
    let sticker = content.get("sticker").unwrap_or(&Value::Null);
    let set_id = sticker.get("set_id").and_then(Value::as_i64);
    let unique_id = sticker.get("sticker").and_then(text::remote_unique_id);

    let chat_id = message.get("chat_id").and_then(Value::as_i64).unwrap_or(0);
    let config = rt.db.config_or_default(chat_id).await;
    if let Some(set_id) = set_id
        && config
            .spam_sticker
            .iter()
            .any(|allowed| allowed == &set_id.to_string())
    {
        return SpamHit::hit(
            "group_StickerRule",
            Some(format!(
                "uq:{}_set:{set_id}",
                unique_id.as_deref().unwrap_or("-")
            )),
            "groupRule",
        );
    }

    if let Some(set_id) = set_id
        && let Some(record) = rt.db.match_sticker_set(&set_id.to_string()).await
    {
        return SpamHit::hit("sticker_set", Some(doc_id(&record)), "hasSpamSet");
    }

    if let Some(unique_id) = unique_id
        && let Some(record) = rt.db.match_sticker(&unique_id).await
    {
        return SpamHit::hit("sticker", Some(doc_id(&record)), "hasSpamFileid");
    }
    SpamHit::none()
}

/// `content.caption.text` / `content.text.text` 这类字段。
pub fn text_of_field(content: &Value, field: &str) -> String {
    content
        .get(field)
        .and_then(|value| value.get("text"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned()
}
