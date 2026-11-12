//! 一条消息 → 一行日志。**纯函数**：不发请求、不碰 TDLib、不写日志。
//!
//! 宿主会在前面补上时间、插件名与级别，所以这里只拼正文：
//!
//! ```text
//! [开发群(id:-1001234567890)][张三(id:123456)][reply_chat:…][reply_msg:42]: 你好\n世界
//! ```
//!
//! 分段来源与 TS 版 `plugins/message_log.ts` 一致：聊天标签、发送者、附加信息
//! （话题 / 回复 / 引用 / 转发）、正文。差异都写在 README 的「与 TS 版的差异」一节。

use serde_json::Value;

// ── 标签 ──────────────────────────────────────────────────────────

/// 聊天标签。私聊沿用 TS 版的 `Private`；群 / 频道是 `标题(id:…)`。
///
/// 私聊按 `chat_id > 0` 判定（TDLib 里私聊的 chat id 就是对方 user id），
/// 这样连 `getChat` 都不用发。
pub fn chat_label(chat_id: i64, title: Option<&str>) -> String {
    if chat_id > 0 {
        return "Private".to_owned();
    }
    match clean_title(title) {
        Some(title) => format!("{title}(id:{chat_id})"),
        None => format!("未知聊天(id:{chat_id})"),
    }
}

/// 用户标签：`名字(id:…)`；名字取不到时用「未知用户」。
pub fn user_label(user_id: i64, name: Option<&str>) -> String {
    match clean_title(name) {
        Some(name) => format!("{name}(id:{user_id})"),
        None => format!("未知用户(id:{user_id})"),
    }
}

/// 以聊天身份发言（频道 / 匿名管理员）的标签。
pub fn chat_sender_label(chat_id: i64, title: Option<&str>) -> String {
    match clean_title(title) {
        Some(title) => format!("{title}(id:{chat_id})"),
        None => format!("未知频道(id:{chat_id})"),
    }
}

/// `message.sender_id` → 标签。未知 / 不认识的发送者也要出一条，不能整条吞掉。
pub fn sender_label(
    sender: Option<&Value>,
    user_name: Option<&str>,
    chat_title: Option<&str>,
) -> String {
    let Some(sender) = sender else {
        return "未知发送者".to_owned();
    };
    match kind(sender) {
        "messageSenderUser" => match int(sender, "user_id") {
            Some(user_id) => user_label(user_id, user_name),
            None => "未知发送者".to_owned(),
        },
        "messageSenderChat" => match int(sender, "chat_id") {
            Some(chat_id) => chat_sender_label(chat_id, chat_title),
            None => "未知发送者".to_owned(),
        },
        // 未来可能新增的发送者类型：保留一行，不 panic。
        _ => "未知发送者".to_owned(),
    }
}

fn clean_title(text: Option<&str>) -> Option<&str> {
    text.map(str::trim).filter(|text| !text.is_empty())
}

// ── 附加信息：话题 / 回复 / 引用 / 转发 ───────────────────────────

/// 拼在发送者后面、正文前面的方括号标签。
///
/// 顺序：话题 → 回复 → 转发（TS 版里话题也是插在最前面，但只在非默认分支；
/// 这里修掉了「认不出的话题会把回复 / 转发整段清空」的 bug）。
pub fn extra_tags(message: &Value) -> String {
    let mut out = String::new();
    if let Some(topic) = message.get("topic_id") {
        out.push_str(&topic_tags(topic));
    }
    if let Some(reply) = message.get("reply_to") {
        out.push_str(&reply_tags(reply));
    }
    if let Some(forward) = message.get("forward_info") {
        out.push_str(&forward_tags(forward));
    }
    out
}

fn topic_tags(topic: &Value) -> String {
    match kind(topic) {
        "messageTopicThread" => {
            format!("[topic:thread:{}]", tag_id(topic, "message_thread_id"))
        }
        "messageTopicForum" => {
            format!("[topic:forum:{}]", tag_id(topic, "forum_topic_id"))
        }
        "messageTopicDirectMessages" => format!(
            "[topic:direct:{}]",
            tag_id(topic, "direct_messages_chat_topic_id")
        ),
        "messageTopicSavedMessages" => {
            format!("[topic:saved:{}]", tag_id(topic, "saved_messages_topic_id"))
        }
        _ => String::new(),
    }
}

fn reply_tags(reply: &Value) -> String {
    match kind(reply) {
        "messageReplyToMessage" => {
            let mut out = format!(
                "[reply_chat:{}][reply_msg:{}]",
                tag_id(reply, "chat_id"),
                tag_id(reply, "message_id")
            );
            // `quote` 在 TDLib 里可能是 null：只有真的有引用才打 [quote]。
            if reply.get("quote").is_some_and(|quote| !quote.is_null()) {
                out.push_str("[quote]");
                out.push_str(&origin_tags(reply.get("origin"), "quote_from"));
            } else {
                out.push_str(&origin_tags(reply.get("origin"), "reply_to"));
            }
            out
        }
        "messageReplyToStory" => format!(
            "[reply_story_chat:{}][reply_story:{}]",
            tag_id(reply, "story_poster_chat_id"),
            tag_id(reply, "story_id")
        ),
        _ => String::new(),
    }
}

fn forward_tags(forward: &Value) -> String {
    let Some(origin) = forward.get("origin") else {
        return String::new();
    };
    match kind(origin) {
        "messageOriginUser" => {
            format!("[forward_from_user:{}]", tag_id(origin, "sender_user_id"))
        }
        "messageOriginChannel" => format!(
            "[forward_from_channel:{}][forward_msg_id:{}]",
            tag_id(origin, "chat_id"),
            tag_id(origin, "message_id")
        ),
        "messageOriginChat" => {
            format!("[forward_from_chat:{}]", tag_id(origin, "sender_chat_id"))
        }
        "messageOriginHiddenUser" => format!(
            "[forward_from_hidden_user:{}]",
            sanitize(
                origin
                    .get("sender_name")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown"),
                64
            )
        ),
        _ => String::new(),
    }
}

/// 回复 / 引用消息的来源（`reply_to_user:`、`quote_from_channel:` …）。
fn origin_tags(origin: Option<&Value>, prefix: &str) -> String {
    let Some(origin) = origin else {
        return String::new();
    };
    match kind(origin) {
        "messageOriginUser" => format!("[{prefix}_user:{}]", tag_id(origin, "sender_user_id")),
        "messageOriginChannel" => {
            format!("[{prefix}_channel:{}]", tag_id(origin, "chat_id"))
        }
        "messageOriginChat" => {
            let signature = origin
                .get("author_signature")
                .and_then(Value::as_str)
                .filter(|signature| !signature.is_empty())
                .map(|signature| format!("({})", sanitize(signature, 64)))
                .unwrap_or_default();
            format!(
                "[{prefix}_chat:{}{signature}]",
                tag_id(origin, "sender_chat_id")
            )
        }
        "messageOriginHiddenUser" => format!(
            "[{prefix}_hidden_user:{}]",
            sanitize(
                origin
                    .get("sender_name")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown"),
                64
            )
        ),
        _ => String::new(),
    }
}

// ── 正文 ──────────────────────────────────────────────────────────

/// 消息正文：文本原样（转义后），媒体给一个带文件 id / 时长的标签，
/// 再加 caption。认不出的类型也至少留一个 `[messageXxx]`。
pub fn content_text(content: Option<&Value>, max_chars: usize) -> String {
    let Some(content) = content else {
        return "[空内容]".to_owned();
    };
    let caption = sanitize(text_value(content.get("caption")), max_chars);

    match kind(content) {
        "messageText" => {
            let text = sanitize(text_value(content.get("text")), max_chars);
            if text.is_empty() {
                "[空文本]".to_owned()
            } else {
                text
            }
        }
        "messageRichMessage" => {
            let text = sanitize(&rich_message_text(content), max_chars);
            if text.is_empty() {
                "[富消息]".to_owned()
            } else {
                text
            }
        }
        "messagePhoto" => {
            let id = photo_id(content).unwrap_or_else(|| "unknown".to_owned());
            with_caption(format!("[图片 file_id:{id}]"), &caption)
        }
        "messageVideo" => {
            let id = file_id_of(content, "video", "video").unwrap_or_else(|| "unknown".to_owned());
            with_caption(
                media_marker("视频", Some(id), duration_of(content, "video")),
                &caption,
            )
        }
        "messageAnimation" => {
            let id = file_id_of(content, "animation", "animation")
                .unwrap_or_else(|| "unknown".to_owned());
            with_caption(
                media_marker("动画", Some(id), duration_of(content, "animation")),
                &caption,
            )
        }
        "messageDocument" => {
            let id =
                file_id_of(content, "document", "document").unwrap_or_else(|| "unknown".to_owned());
            let name = clean_field(
                content.get("document").and_then(|doc| doc.get("file_name")),
                120,
            );
            let marker = if name.is_empty() {
                format!("[文件 file_id:{id}]")
            } else {
                format!("[文件 {name} file_id:{id}]")
            };
            with_caption(marker, &caption)
        }
        "messageAudio" => {
            let audio = content.get("audio");
            let id = audio
                .and_then(|audio| audio.get("audio"))
                .and_then(unique_id)
                .unwrap_or_else(|| "unknown".to_owned());
            let title = clean_field(audio.and_then(|audio| audio.get("title")), 120);
            let performer = clean_field(audio.and_then(|audio| audio.get("performer")), 120);
            let duration = duration_of(content, "audio");
            let mut parts = vec!["音频".to_owned()];
            if !title.is_empty() {
                parts.push(title);
            }
            if !performer.is_empty() {
                parts.push(format!("- {performer}"));
            }
            if duration > 0 {
                parts.push(clock(duration));
            }
            parts.push(format!("file_id:{id}"));
            with_caption(format!("[{}]", parts.join(" ")), &caption)
        }
        "messageVoiceNote" => {
            let id =
                file_id_of(content, "voice_note", "voice").unwrap_or_else(|| "unknown".to_owned());
            with_caption(
                media_marker("语音", Some(id), duration_of(content, "voice_note")),
                &caption,
            )
        }
        "messageVideoNote" => {
            let id =
                file_id_of(content, "video_note", "video").unwrap_or_else(|| "unknown".to_owned());
            with_caption(
                media_marker("视频消息", Some(id), duration_of(content, "video_note")),
                &caption,
            )
        }
        "messageSticker" => {
            let sticker = content.get("sticker");
            let emoji = clean_field(sticker.and_then(|sticker| sticker.get("emoji")), 16);
            let id = tag_id(content.get("sticker").unwrap_or(&Value::Null), "id");
            let set_id = tag_id(content.get("sticker").unwrap_or(&Value::Null), "set_id");
            let file_id = sticker
                .and_then(|sticker| sticker.get("sticker"))
                .and_then(unique_id)
                .unwrap_or_else(|| "unknown".to_owned());
            let mut parts = vec!["贴纸".to_owned()];
            if !emoji.is_empty() {
                parts.push(emoji);
            }
            parts.push(format!("id:{id}"));
            parts.push(format!("set_id:{set_id}"));
            parts.push(format!("file_id:{file_id}"));
            with_caption(format!("[{}]", parts.join(" ")), &caption)
        }
        "messageAnimatedEmoji" => {
            let emoji = clean_field(
                content
                    .get("animated_emoji")
                    .and_then(|emoji| emoji.get("emoji")),
                16,
            );
            if emoji.is_empty() {
                "[动态表情]".to_owned()
            } else {
                format!("[动态表情 {emoji}]")
            }
        }
        "messageContact" => {
            let contact = content.get("contact");
            let name = join_name(
                contact.and_then(|contact| contact.get("first_name")),
                contact.and_then(|contact| contact.get("last_name")),
            );
            let phone = clean_field(contact.and_then(|contact| contact.get("phone_number")), 64);
            let mut parts = vec!["联系人".to_owned()];
            if !name.is_empty() {
                parts.push(name);
            }
            if !phone.is_empty() {
                parts.push(phone);
            }
            format!("[{}]", parts.join(" "))
        }
        "messageLocation" => match content.get("location") {
            Some(location) => format!(
                "[位置 {:.5},{:.5}]",
                float(location, "latitude"),
                float(location, "longitude")
            ),
            None => "[位置]".to_owned(),
        },
        "messageVenue" => {
            let venue = content.get("venue");
            let title = clean_field(venue.and_then(|venue| venue.get("title")), 120);
            let address = clean_field(venue.and_then(|venue| venue.get("address")), 160);
            let mut parts = vec!["地点".to_owned()];
            if !title.is_empty() {
                parts.push(title);
            }
            if !address.is_empty() {
                parts.push(address);
            }
            format!("[{}]", parts.join(" "))
        }
        "messagePoll" => {
            let question = clean_field(
                content.get("poll").and_then(|poll| poll.get("question")),
                300,
            );
            if question.is_empty() {
                "[投票]".to_owned()
            } else {
                format!("[投票 {question}]")
            }
        }
        "messageDice" => {
            let dice = content.get("dice");
            let emoji = clean_field(dice.and_then(|dice| dice.get("emoji")), 16);
            let value = int(dice.unwrap_or(&Value::Null), "value").unwrap_or(0);
            let mut parts = vec!["骰子".to_owned()];
            if !emoji.is_empty() {
                parts.push(emoji);
            }
            parts.push(value.to_string());
            format!("[{}]", parts.join(" "))
        }
        "messageGame" => {
            let title = clean_field(content.get("game").and_then(|game| game.get("title")), 160);
            if title.is_empty() {
                "[游戏]".to_owned()
            } else {
                format!("[游戏 {title}]")
            }
        }
        "messageStory" => {
            let story = content.get("story");
            format!(
                "[故事 id:{} 发布者:{}]",
                tag_id(story.unwrap_or(&Value::Null), "story_id"),
                tag_id(story.unwrap_or(&Value::Null), "story_poster_chat_id")
            )
        }
        "messageCall" => {
            let call = content.get("call");
            let duration = int(call.unwrap_or(&Value::Null), "duration").unwrap_or(0);
            let video = call
                .and_then(|call| call.get("is_video"))
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let mut parts = vec!["通话".to_owned()];
            if video {
                parts.push("视频".to_owned());
            }
            if duration > 0 {
                parts.push(clock(duration));
            }
            format!("[{}]", parts.join(" "))
        }
        "messageExpiredPhoto" => "[图片已过期]".to_owned(),
        "messageExpiredVideo" => "[视频已过期]".to_owned(),
        "messageUnsupported" => "[不支持的消息类型]".to_owned(),
        // 服务消息（入群 / 改标题 / 置顶…）以及未来新增的类型：
        // 没有专门的展示也比整条丢掉强，至少留下类型名。
        other if !other.is_empty() => format!("[{other}]"),
        _ => "[未知内容]".to_owned(),
    }
}

/// 整行：`[聊天][发送者]附加信息: 正文`。
pub fn render(message: &Value, chat: &str, sender: &str, max_chars: usize) -> String {
    format!(
        "[{chat}][{sender}]{}: {}",
        extra_tags(message),
        content_text(message.get("content"), max_chars)
    )
}

// ── 文本处理 ──────────────────────────────────────────────────────

/// 把一段文本变成**一行**可落盘的内容：
///
/// - 换行 / 回车 / 制表符转成字面量 `\n` `\r` `\t`（不是真换行）；
/// - 其余控制字符按 Rust 转义（`\u{1}`），避免 ANSI 转义序列污染终端 / 日志页；
/// - `max_chars > 0` 时按**字符**截断（中文不会被切成乱码），并注明原长度。
pub fn sanitize(text: &str, max_chars: usize) -> String {
    let mut out = String::with_capacity(text.len());
    for (count, ch) in text.chars().enumerate() {
        if max_chars > 0 && count >= max_chars {
            let total = text.chars().count();
            out.push('…');
            out.push_str(&format!("（共 {total} 字符）"));
            break;
        }
        match ch {
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            ch if ch.is_control() => out.extend(ch.escape_default()),
            ch => out.push(ch),
        }
    }
    out
}

// ── 小工具 ────────────────────────────────────────────────────────

fn kind(value: &Value) -> &str {
    value.get("@type").and_then(Value::as_str).unwrap_or("")
}

fn int(value: &Value, key: &str) -> Option<i64> {
    value.get(key).and_then(Value::as_i64)
}

fn float(value: &Value, key: &str) -> f64 {
    value.get(key).and_then(Value::as_f64).unwrap_or(0.0)
}

/// 日志标签里的 id：缺字段 / 类型不对时写 `unknown`，不整条丢。
fn tag_id(value: &Value, key: &str) -> String {
    int(value, key)
        .map(|id| id.to_string())
        .unwrap_or_else(|| "unknown".to_owned())
}

/// `formattedText` / 普通字符串 → 文本。
fn text_value(value: Option<&Value>) -> &str {
    match value {
        Some(Value::String(text)) => text,
        Some(value) => value.get("text").and_then(Value::as_str).unwrap_or(""),
        None => "",
    }
}

fn clean_field(value: Option<&Value>, max_chars: usize) -> String {
    sanitize(
        value.and_then(Value::as_str).unwrap_or("").trim(),
        max_chars,
    )
}

fn join_name(first: Option<&Value>, last: Option<&Value>) -> String {
    [first, last]
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// 文件对象的 `remote.unique_id`（跨重启稳定）；没有就退回数字 `id`。
fn unique_id(file: &Value) -> Option<String> {
    if let Some(unique) = file
        .get("remote")
        .and_then(|remote| remote.get("unique_id"))
        .and_then(Value::as_str)
        .filter(|unique| !unique.is_empty())
    {
        return Some(unique.to_owned());
    }
    file.get("id")
        .and_then(Value::as_i64)
        .map(|id| id.to_string())
}

/// `content.<wrapper>.<file_field>` → 文件 id。
fn file_id_of(content: &Value, wrapper: &str, file_field: &str) -> Option<String> {
    content
        .get(wrapper)
        .and_then(|wrapper| wrapper.get(file_field))
        .and_then(unique_id)
}

fn photo_id(content: &Value) -> Option<String> {
    // sizes 按从小到大排；取最后一档（最大图）的 remote.unique_id。
    content
        .get("photo")
        .and_then(|photo| photo.get("sizes"))
        .and_then(Value::as_array)?
        .iter()
        .rev()
        .find_map(|size| size.get("photo").and_then(unique_id))
}

fn duration_of(content: &Value, wrapper: &str) -> i64 {
    int(content.get(wrapper).unwrap_or(&Value::Null), "duration").unwrap_or(0)
}

fn media_marker(label: &str, file_id: Option<String>, duration: i64) -> String {
    let mut parts = vec![label.to_owned()];
    if duration > 0 {
        parts.push(clock(duration));
    }
    if let Some(file_id) = file_id {
        parts.push(format!("file_id:{file_id}"));
    }
    format!("[{}]", parts.join(" "))
}

fn with_caption(marker: String, caption: &str) -> String {
    if caption.is_empty() {
        marker
    } else {
        format!("{marker} {caption}")
    }
}

/// 秒 → `mm:ss`（超过一小时才带小时）。
fn clock(seconds: i64) -> String {
    let seconds = seconds.max(0);
    let (hours, minutes, seconds) = (seconds / 3600, (seconds % 3600) / 60, seconds % 60);
    if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes:02}:{seconds:02}")
    }
}

/// 富消息：递归收集所有 `richTextPlain` 的文本。
///
/// 不实现完整的 pageBlock 语义（日志不需要段落结构），只保证「文字能出来」；
/// 图片 / 表格之类没有纯文本的块自然贡献空串。
fn rich_message_text(content: &Value) -> String {
    let mut parts = Vec::new();
    if let Some(blocks) = content
        .get("message")
        .and_then(|message| message.get("blocks"))
        .and_then(Value::as_array)
    {
        for block in blocks {
            collect_plain_text(block, &mut parts);
        }
    }
    parts.join(" ")
}

fn collect_plain_text(value: &Value, out: &mut Vec<String>) {
    match value {
        Value::Object(map) => {
            if map.get("@type").and_then(Value::as_str) == Some("richTextPlain") {
                if let Some(text) = map.get("text").and_then(Value::as_str)
                    && !text.is_empty()
                {
                    out.push(text.to_owned());
                }
                return;
            }
            for child in map.values() {
                collect_plain_text(child, out);
            }
        }
        Value::Array(items) => {
            for item in items {
                collect_plain_text(item, out);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn text_message_renders_chat_sender_and_body() {
        let message = json!({
            "chat_id": -1001234567890i64,
            "sender_id": {"@type": "messageSenderUser", "user_id": 123},
            "content": {
                "@type": "messageText",
                "text": {"@type": "formattedText", "text": "你好\n世界"}
            }
        });
        let chat = chat_label(-1001234567890, Some("开发群"));
        let sender = sender_label(message.get("sender_id"), Some("张三"), None);
        assert_eq!(
            render(&message, &chat, &sender, 0),
            "[开发群(id:-1001234567890)][张三(id:123)]: 你好\\n世界"
        );
    }

    #[test]
    fn private_chat_uses_the_reference_tag_and_no_title_lookup() {
        assert_eq!(chat_label(123, Some("张三")), "Private");
        assert_eq!(chat_label(123, None), "Private");
        assert_eq!(
            user_label(123, Some("张三")),
            "张三(id:123)",
            "名字和 id 用同一个括号"
        );
        assert_eq!(user_label(123, None), "未知用户(id:123)");
        assert_eq!(
            chat_sender_label(-1001, Some("公告频道")),
            "公告频道(id:-1001)"
        );
        assert_eq!(chat_sender_label(-1001, None), "未知频道(id:-1001)");
    }

    #[test]
    fn unknown_sender_still_gets_a_line() {
        assert_eq!(sender_label(None, None, None), "未知发送者");
        assert_eq!(
            sender_label(Some(&json!({"@type": "messageSenderHidden"})), None, None),
            "未知发送者"
        );
    }

    #[test]
    fn channel_sender_uses_the_chat_title() {
        let sender = json!({"@type": "messageSenderChat", "chat_id": -1001});
        assert_eq!(
            sender_label(Some(&sender), None, Some("公告频道")),
            "公告频道(id:-1001)"
        );
        assert_eq!(
            sender_label(Some(&sender), None, None),
            "未知频道(id:-1001)"
        );
    }

    #[test]
    fn extra_tags_follow_the_reference_order_and_include_quote_origin() {
        let message = json!({
            "topic_id": {"@type": "messageTopicForum", "forum_topic_id": 7},
            "reply_to": {
                "@type": "messageReplyToMessage",
                "chat_id": -1001,
                "message_id": 42,
                "quote": {"text": {"text": "旧消息"}},
                "origin": {"@type": "messageOriginChannel", "chat_id": -1001, "message_id": 1}
            },
            "forward_info": {
                "origin": {"@type": "messageOriginUser", "sender_user_id": 9}
            }
        });
        assert_eq!(
            extra_tags(&message),
            "[topic:forum:7][reply_chat:-1001][reply_msg:42][quote][quote_from_channel:-1001][forward_from_user:9]"
        );
    }

    #[test]
    fn reply_without_quote_uses_reply_to_origin() {
        let reply = json!({
            "@type": "messageReplyToMessage",
            "chat_id": -1001,
            "message_id": 42,
            "origin": {
                "@type": "messageOriginChat",
                "sender_chat_id": -1002,
                "author_signature": "匿名管理员"
            }
        });
        assert_eq!(
            reply_tags(&reply),
            "[reply_chat:-1001][reply_msg:42][reply_to_chat:-1002(匿名管理员)]"
        );
    }

    #[test]
    fn forwarded_channel_keeps_message_id() {
        let forward = json!({
            "origin": {
                "@type": "messageOriginChannel",
                "chat_id": -1001,
                "message_id": 99
            }
        });
        assert_eq!(
            forward_tags(&forward),
            "[forward_from_channel:-1001][forward_msg_id:99]"
        );
    }

    #[test]
    fn photo_has_file_id_and_caption() {
        let content = json!({
            "@type": "messagePhoto",
            "photo": {"sizes": [
                {"photo": {"remote": {"unique_id": "small"}}},
                {"photo": {"remote": {"unique_id": "large"}}}
            ]},
            "caption": {"text": "风景"}
        });
        assert_eq!(
            content_text(Some(&content), 0),
            "[图片 file_id:large] 风景",
            "取最大一档的 unique_id"
        );
    }

    #[test]
    fn sticker_keeps_all_ids_the_reference_prints() {
        let content = json!({
            "@type": "messageSticker",
            "sticker": {
                "id": 11,
                "set_id": 22,
                "emoji": "🙂",
                "sticker": {"remote": {"unique_id": "file-33"}}
            }
        });
        assert_eq!(
            content_text(Some(&content), 0),
            "[贴纸 🙂 id:11 set_id:22 file_id:file-33]"
        );
    }

    #[test]
    fn document_and_audio_markers_are_readable() {
        let document = json!({
            "@type": "messageDocument",
            "document": {
                "file_name": "报表.pdf",
                "document": {"remote": {"unique_id": "doc-1"}}
            },
            "caption": {"text": "请查收"}
        });
        assert_eq!(
            content_text(Some(&document), 0),
            "[文件 报表.pdf file_id:doc-1] 请查收"
        );

        let audio = json!({
            "@type": "messageAudio",
            "audio": {
                "duration": 225,
                "title": "晴天",
                "performer": "周杰伦",
                "audio": {"remote": {"unique_id": "audio-1"}}
            }
        });
        assert_eq!(
            content_text(Some(&audio), 0),
            "[音频 晴天 - 周杰伦 03:45 file_id:audio-1]"
        );
    }

    #[test]
    fn unknown_content_still_has_a_marker() {
        assert_eq!(content_text(None, 0), "[空内容]");
        assert_eq!(
            content_text(Some(&json!({"@type": "messageFutureThing"})), 0),
            "[messageFutureThing]"
        );
        assert_eq!(
            content_text(Some(&json!({"@type": "messageUnsupported"})), 0),
            "[不支持的消息类型]"
        );
    }

    #[test]
    fn newlines_are_escaped_and_control_characters_neutralized() {
        assert_eq!(sanitize("a\nb\r\tc", 0), "a\\nb\\r\\tc");
        assert_eq!(sanitize("\u{1b}[31mred", 0), "\\u{1b}[31mred");
    }

    #[test]
    fn truncation_is_by_chars_and_reports_the_original_length() {
        assert_eq!(sanitize("一二三四五", 3), "一二三…（共 5 字符）");
        assert_eq!(sanitize("abc", 10), "abc", "没超就不动");
        assert_eq!(sanitize("abc", 0), "abc", "0 = 不截断");
    }

    #[test]
    fn rich_message_collects_plain_text_blocks() {
        let content = json!({
            "@type": "messageRichMessage",
            "message": {"blocks": [
                {"@type": "pageBlockParagraph", "text": {
                    "@type": "richTexts",
                    "texts": [
                        {"@type": "richTextPlain", "text": "第一段"},
                        {"@type": "richTextBold", "text": {"@type": "richTextPlain", "text": "加粗"}}
                    ]
                }},
                {"@type": "pageBlockDivider"}
            ]}
        });
        assert_eq!(content_text(Some(&content), 0), "第一段 加粗");
    }
}
