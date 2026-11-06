//! 文本提取与 Markdown 渲染。
//!
//! 两件事挤在一个模块里，是因为它们服务同一个问题：**把 TDLib 的 JSON 变成人看的一段话**。
//!
//! ## 消息文本
//!
//! TS 版散落在 `utils/richText.ts` / `utils/messageCache.ts` / `Ads/*.ts` 里，
//! 这里收敛成 [`message_text`]（按内容类型分发）+ [`rich_message_text`]（富消息递归）。
//!
//! ## Markdown
//!
//! TS 版把 GFM 经 `remark` 转成 MarkdownV2 再交给 `parseTextEntities`。Rust 侧不拖
//! 一个 CommonMark 解析器（插件是 cdylib，依赖越少越好），而是**只认三种标记**：
//!
//! | 写法 | 渲染成 |
//! | --- | --- |
//! | `**粗体**` | MarkdownV2 的 `*粗体*` |
//! | `` `代码` `` | 行内代码 |
//! | `[文字](url)` | 链接（`tg://user?id=…` 的提及靠它） |
//!
//! 其余字符一律按 MarkdownV2 的规则转义。这样源码里可以继续写 `**Reason**: …`
//! 这种熟悉的写法，而 TDLib 那边看到的是合法 MarkdownV2。
//!
//! ⚠️ 真库实测（见 `fuyubot-simplify` 的 `Text::markdown_v2` 注释）：MarkdownV2 的粗体是
//! `*x*`，**不是** `**x**`；`**x**` 会被吞掉。转换是必须的，不是可选的。

use fuyubot_simplify::Text;
use serde_json::Value;

// ── 消息文本 ──────────────────────────────────────────────────────

/// 内容对象本身。
pub fn content(message: &Value) -> Option<&Value> {
    message.get("content")
}

/// 一条消息里「能拿去审的文本」。
///
/// 与 TS 版 `extractMessageText` 对齐，并多覆盖了文档 / 音频 / 动画的 caption
/// （原版漏了这几种，导致带广告文案的文件消息完全绕过 AI 审核）。
pub fn message_text(message: &Value) -> String {
    let Some(content) = content(message) else {
        return String::new();
    };
    match content.get("@type").and_then(Value::as_str).unwrap_or("") {
        "messageText" => text_of(content.get("text")),
        "messageRichMessage" => rich_message_text(content),
        "messagePhoto" | "messageVideo" | "messageAnimation" | "messageDocument"
        | "messageAudio" | "messageVoiceNote" | "messageVideoNote" => {
            text_of(content.get("caption"))
        }
        "messageSticker" => content
            .get("sticker")
            .and_then(|sticker| sticker.get("emoji"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned(),
        "messageContact" => contact_name(content),
        _ => String::new(),
    }
}

/// `formattedText` / `richText` / 普通字符串 → 纯文本。
fn text_of(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(text)) => text.clone(),
        Some(value) => value
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned(),
        None => String::new(),
    }
}

/// 联系人消息的展示名（`first_name + last_name`，与 TS 版一致——中间不加空格）。
pub fn contact_name(content: &Value) -> String {
    let contact = content.get("contact");
    let first = contact
        .and_then(|contact| contact.get("first_name"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let last = contact
        .and_then(|contact| contact.get("last_name"))
        .and_then(Value::as_str)
        .unwrap_or("");
    format!("{first}{last}")
}

/// 联系人消息的 vCard（广告载体之一）。
pub fn contact_vcard(content: &Value) -> String {
    content
        .get("contact")
        .and_then(|contact| contact.get("vcard"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned()
}

/// `video` 的缩略图 / 文件级 `remote.unique_id`（黑名单按它比对）。
pub fn remote_unique_id(value: &Value) -> Option<String> {
    value
        .get("remote")
        .and_then(|remote| remote.get("unique_id"))
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .map(str::to_owned)
}

/// `messagePhoto` 所有尺寸的 `unique_id`（TS 版逐个查黑名单）。
pub fn photo_unique_ids(content: &Value) -> Vec<String> {
    content
        .get("photo")
        .and_then(|photo| photo.get("sizes"))
        .and_then(Value::as_array)
        .map(|sizes| {
            sizes
                .iter()
                .filter_map(|size| size.get("photo").and_then(remote_unique_id))
                .collect()
        })
        .unwrap_or_default()
}

// ── 富消息 ────────────────────────────────────────────────────────

/// 从 `messageRichMessage` 里递归抽纯文本（对应 TS 版 `richText.ts`）。
pub fn rich_message_text(content: &Value) -> String {
    let blocks = content
        .get("message")
        .and_then(|message| message.get("blocks"))
        .and_then(Value::as_array);
    let Some(blocks) = blocks else {
        return String::new();
    };
    let mut parts: Vec<String> = Vec::new();
    for block in blocks {
        let text = page_block_text(block);
        if !text.is_empty() {
            parts.push(text);
        }
    }
    parts.join("\n")
}

/// RichText 的纯文本（递归；认不出的类型按空串处理，不 panic）。
pub fn rich_text(value: &Value) -> String {
    let Some(kind) = value.get("@type").and_then(Value::as_str) else {
        return String::new();
    };
    match kind {
        "richTextPlain" => value
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned(),
        "richTexts" => value
            .get("texts")
            .and_then(Value::as_array)
            .map(|texts| {
                texts
                    .iter()
                    .map(rich_text)
                    .filter(|text| !text.is_empty())
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .unwrap_or_default(),
        "richTextMention" | "richTextHashtag" | "richTextCashtag" | "richTextBotCommand" => value
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned(),
        // 这些要么是「包一层」的样式节点，要么带一个 text 子节点；统一递归。
        _ => value.get("text").map(rich_text).unwrap_or_default(),
    }
}

/// PageBlock 的纯文本（对应 TS 版 `extractTextFromPageBlock`）。
fn page_block_text(block: &Value) -> String {
    const TEXT_FIELDS: [&str; 14] = [
        "title",
        "subtitle",
        "header",
        "subheader",
        "footer",
        "text",
        "author",
        "kicker",
        "caption",
        "heading",
        "description",
        "value",
        "label",
        "key",
    ];
    let mut out = String::new();
    for field in TEXT_FIELDS {
        if let Some(value) = block.get(field)
            && value
                .get("@type")
                .and_then(Value::as_str)
                .is_some_and(|kind| kind.starts_with("richText"))
        {
            let text = rich_text(value);
            if !text.is_empty() {
                out.push(' ');
                out.push_str(&text);
            }
        }
    }

    let kind = block.get("@type").and_then(Value::as_str).unwrap_or("");
    match kind {
        "pageBlockList" => {
            if let Some(items) = block.get("items").and_then(Value::as_array) {
                for item in items {
                    if let Some(cells) = item.as_array() {
                        let text = cells
                            .iter()
                            .map(page_block_text)
                            .filter(|text| !text.is_empty())
                            .collect::<Vec<_>>()
                            .join(" ");
                        if !text.is_empty() {
                            out.push(' ');
                            out.push_str(&text);
                        }
                    }
                }
            }
        }
        "pageBlockTable" => {
            if let Some(rows) = block.get("rows").and_then(Value::as_array) {
                for row in rows {
                    let Some(cells) = row.get("cells").and_then(Value::as_array) else {
                        continue;
                    };
                    for cell in cells {
                        let Some(cells) = cell.as_array() else {
                            continue;
                        };
                        let text = cells
                            .iter()
                            .map(page_block_text)
                            .filter(|text| !text.is_empty())
                            .collect::<Vec<_>>()
                            .join(" ");
                        if !text.is_empty() {
                            out.push(' ');
                            out.push_str(&text);
                        }
                    }
                }
            }
        }
        "pageBlockDetails" => {
            if let Some(header) = block.get("header") {
                let text = page_block_text(header);
                if !text.is_empty() {
                    out.push(' ');
                    out.push_str(&text);
                }
            }
        }
        "pageBlockCollage" | "pageBlockSlideshow" => {
            if let Some(blocks) = block.get("page_blocks").and_then(Value::as_array) {
                let text = blocks
                    .iter()
                    .map(page_block_text)
                    .filter(|text| !text.is_empty())
                    .collect::<Vec<_>>()
                    .join(" ");
                if !text.is_empty() {
                    out.push(' ');
                    out.push_str(&text);
                }
            }
        }
        "pageBlockRelatedArticles" => {
            if let Some(articles) = block.get("articles").and_then(Value::as_array) {
                for article in articles {
                    for field in ["title", "description"] {
                        if let Some(value) = article.get(field) {
                            let text = rich_text(value);
                            if !text.is_empty() {
                                out.push(' ');
                                out.push_str(&text);
                            }
                        }
                    }
                }
            }
        }
        _ => {}
    }
    out.trim().to_owned()
}

// ── Markdown ─────────────────────────────────────────────────────

/// 把 `**粗体**` / `` `代码` `` / `[文字](url)` 转成 MarkdownV2，其余字符转义。
pub fn markdown(input: &str) -> Text {
    Text::markdown_v2(render_markdown_v2(input))
}

/// 纯文本（不解析任何标记）。
pub fn plain(input: impl Into<String>) -> Text {
    Text::plain(input.into())
}

/// 只有真的需要 `String` 时用（`Text` 本身能直接进 `SendMessageParams`）。
pub fn render_markdown_v2(input: &str) -> String {
    let chars: Vec<char> = input.chars().collect();
    let mut out = String::with_capacity(input.len() + input.len() / 4);
    let mut index = 0;
    while index < chars.len() {
        let current = chars[index];
        // **粗体**
        if current == '*'
            && chars.get(index + 1) == Some(&'*')
            && let Some(end) = find_seq(&chars, index + 2, &['*', '*'])
        {
            out.push('*');
            escape_into(&chars[index + 2..end], &mut out);
            out.push('*');
            index = end + 2;
            continue;
        }
        // `代码`
        if current == '`'
            && let Some(end) = chars[index + 1..]
                .iter()
                .position(|&c| c == '`')
                .map(|offset| index + 1 + offset)
        {
            out.push('`');
            for &c in &chars[index + 1..end] {
                if c == '`' || c == '\\' {
                    out.push('\\');
                }
                out.push(c);
            }
            out.push('`');
            index = end + 1;
            continue;
        }
        // [文字](url)
        if current == '['
            && let Some(close_label) = chars[index + 1..]
                .iter()
                .position(|&c| c == ']')
                .map(|offset| index + 1 + offset)
            && chars.get(close_label + 1) == Some(&'(')
            && let Some(close_url) = chars[close_label + 2..]
                .iter()
                .position(|&c| c == ')')
                .map(|offset| close_label + 2 + offset)
        {
            out.push('[');
            escape_into(&chars[index + 1..close_label], &mut out);
            out.push_str("](");
            for &c in &chars[close_label + 2..close_url] {
                if c == ')' || c == '\\' {
                    out.push('\\');
                }
                out.push(c);
            }
            out.push(')');
            index = close_url + 1;
            continue;
        }
        escape_char(current, &mut out);
        index += 1;
    }
    out
}

/// 转义一段普通文本（粗体 / 链接文字内部用）。
fn escape_into(chars: &[char], out: &mut String) {
    for &c in chars {
        escape_char(c, out);
    }
}

/// MarkdownV2 的保留字符：出现在普通文本里必须转义。
fn escape_char(c: char, out: &mut String) {
    const RESERVED: &[char] = &[
        '_', '*', '[', ']', '(', ')', '~', '`', '>', '#', '+', '-', '=', '|', '{', '}', '.', '!',
        '\\',
    ];
    if RESERVED.contains(&c) {
        out.push('\\');
    }
    out.push(c);
}

/// 在 `from` 之后找第一个「两个连续字符」的位置。
fn find_seq(chars: &[char], from: usize, seq: &[char; 2]) -> Option<usize> {
    let mut index = from;
    while index + 1 < chars.len() {
        if chars[index] == seq[0] && chars[index + 1] == seq[1] {
            return Some(index);
        }
        index += 1;
    }
    None
}

/// 置信度 → `87%`。
pub fn confidence_percent(confidence: f64) -> String {
    format!("{:.0}%", (confidence * 100.0).clamp(0.0, 100.0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn extracts_text_and_captions() {
        let text = json!({
            "content": {"@type": "messageText", "text": {"text": "你好"}}
        });
        assert_eq!(message_text(&text), "你好");

        let photo = json!({
            "content": {"@type": "messagePhoto", "caption": {"text": "图说"}}
        });
        assert_eq!(message_text(&photo), "图说");
    }

    #[test]
    fn rich_message_text_walks_blocks() {
        let content = json!({
            "@type": "messageRichMessage",
            "message": {
                "blocks": [
                    {"@type": "pageBlockTitle", "title": {"@type": "richTextPlain", "text": "标题"}},
                    {"@type": "pageBlockParagraph", "text": {"@type": "richTexts", "texts": [
                        {"@type": "richTextPlain", "text": "第一段"},
                        {"@type": "richTextBold", "text": {"@type": "richTextPlain", "text": "加粗"}}
                    ]}}
                ]
            }
        });
        let text = rich_message_text(&content);
        assert!(text.contains("标题"), "{text}");
        assert!(text.contains("第一段"), "{text}");
        assert!(text.contains("加粗"), "{text}");
    }

    #[test]
    fn bold_is_converted_to_markdown_v2() {
        assert_eq!(render_markdown_v2("**粗体**"), "*粗体*");
        assert_eq!(render_markdown_v2("**a.b**"), "*a\\.b*");
    }

    #[test]
    fn reserved_characters_are_escaped() {
        assert_eq!(render_markdown_v2("a.b-c!"), "a\\.b\\-c\\!");
        assert_eq!(render_markdown_v2("#Tag"), "\\#Tag");
    }

    #[test]
    fn inline_code_keeps_dots() {
        assert_eq!(render_markdown_v2("`a.b`"), "`a.b`");
        assert_eq!(render_markdown_v2("`a\\b`"), "`a\\\\b`");
    }

    #[test]
    fn links_keep_url_unescaped() {
        let rendered = render_markdown_v2("[我](tg://user?id=1)");
        assert_eq!(rendered, "[我](tg://user?id=1)");
    }

    #[test]
    fn unmatched_bold_marker_is_escaped() {
        assert_eq!(render_markdown_v2("**"), "\\*\\*");
        assert_eq!(render_markdown_v2("a*b"), "a\\*b");
    }

    #[test]
    fn confidence_is_percentage() {
        assert_eq!(confidence_percent(0.876), "88%");
        assert_eq!(confidence_percent(0.0), "0%");
    }

    #[test]
    fn photo_ids_are_collected() {
        let content = json!({
            "photo": {"sizes": [
                {"photo": {"remote": {"unique_id": "a"}}},
                {"photo": {"remote": {"unique_id": "b"}}},
                {"photo": {}}
            ]}
        });
        assert_eq!(photo_unique_ids(&content), vec!["a", "b"]);
    }
}
