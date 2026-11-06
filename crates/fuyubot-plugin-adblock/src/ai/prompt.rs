//! System prompt 的动态拼装。
//!
//! 规则来自 TS 版 `function/buildSystemPrompt.ts`：永远先给 `core.md`，再按输入里
//! **实际存在**的字段追加对应的上下文规则，最后按媒体类型追加媒体规则。
//!
//! 提示词文件用 `include_str!` 嵌进动态库——插件不该在运行时依赖源码目录布局
//! （它是 cdylib，算不出自己旁边有什么）。改提示词要重新编译，这是有意的：
//! 版本化的提示词才能解释「昨天和今天的判定为什么不一样」。
//!
//! 段落之间用 `\n\n---\n\n` 分隔，与 TS 版一致。

use super::ModerationInput;

/// 段与段之间的分隔（TS 版同款）。
const SEPARATOR: &str = "\n\n---\n\n";

const CORE: &str = include_str!("prompts/core.md");
const CONTEXT_MESSAGE: &str = include_str!("prompts/context/message.md");
const CONTEXT_QUOTE: &str = include_str!("prompts/context/quote.md");
const CONTEXT_PROFILE: &str = include_str!("prompts/context/profile.md");
const CONTEXT_MENTION: &str = include_str!("prompts/context/mention.md");
const CONTEXT_CONTACT: &str = include_str!("prompts/context/contact.md");
const CONTEXT_PERSONAL_CHANNEL: &str = include_str!("prompts/context/personalChannel.md");
const CONTEXT_LINK_PREVIEW: &str = include_str!("prompts/context/linkPreview.md");
const CONTEXT_HISTORY: &str = include_str!("prompts/context/history.md");
const MEDIA_PHOTO_VIDEO: &str = include_str!("prompts/media/photoVideo.md");
const MEDIA_STICKER: &str = include_str!("prompts/media/sticker.md");

fn has_text(value: Option<&String>) -> bool {
    value.is_some_and(|text| !text.trim().is_empty())
}

fn any_text<const N: usize>(values: [Option<&String>; N]) -> bool {
    values.into_iter().any(has_text)
}

/// 正文 / 引用 / 链接预览 / 图片 / 图片视频类消息都和「消息」上下文相关。
fn needs_message_rules(input: &ModerationInput, has_image: bool) -> bool {
    has_text(input.message.as_ref())
        || has_text(input.quote_message.as_ref())
        || any_text([
            input
                .link_preview
                .as_ref()
                .and_then(|preview| preview.title.as_ref()),
            input
                .link_preview
                .as_ref()
                .and_then(|preview| preview.description.as_ref()),
        ])
        || has_image
        || input.kind == "photo"
        || input.kind == "video"
        || any_text([
            input
                .personal_channel
                .as_ref()
                .and_then(|channel| channel.name.as_ref()),
            input
                .personal_channel
                .as_ref()
                .and_then(|channel| channel.description.as_ref()),
        ])
}

/// 按输入字段拼出完整 system prompt。
pub fn build(input: &ModerationInput, has_image: bool) -> String {
    let mut segments: Vec<&str> = vec![CORE.trim()];

    if needs_message_rules(input, has_image) {
        segments.push(CONTEXT_MESSAGE.trim());
    }
    if has_text(input.quote_message.as_ref()) {
        segments.push(CONTEXT_QUOTE.trim());
    }
    if any_text([
        input
            .profile
            .as_ref()
            .and_then(|profile| profile.name.as_ref()),
        input
            .profile
            .as_ref()
            .and_then(|profile| profile.bio.as_ref()),
    ]) {
        segments.push(CONTEXT_PROFILE.trim());
    }
    if has_text(
        input
            .mentioned_username
            .as_ref()
            .map(|mention| &mention.username),
    ) {
        segments.push(CONTEXT_MENTION.trim());
    }
    if any_text([
        input
            .contact
            .as_ref()
            .and_then(|contact| contact.name.as_ref()),
        input
            .contact
            .as_ref()
            .and_then(|contact| contact.phone.as_ref()),
        input
            .contact
            .as_ref()
            .and_then(|contact| contact.username.as_ref()),
    ]) {
        segments.push(CONTEXT_CONTACT.trim());
    }
    if any_text([
        input
            .personal_channel
            .as_ref()
            .and_then(|channel| channel.name.as_ref()),
        input
            .personal_channel
            .as_ref()
            .and_then(|channel| channel.description.as_ref()),
    ]) {
        segments.push(CONTEXT_PERSONAL_CHANNEL.trim());
    }
    if any_text([
        input
            .link_preview
            .as_ref()
            .and_then(|preview| preview.title.as_ref()),
        input
            .link_preview
            .as_ref()
            .and_then(|preview| preview.description.as_ref()),
    ]) {
        segments.push(CONTEXT_LINK_PREVIEW.trim());
    }
    if input
        .history_messages
        .as_ref()
        .is_some_and(|messages| messages.iter().any(|text| !text.trim().is_empty()))
    {
        segments.push(CONTEXT_HISTORY.trim());
    }

    if has_image || input.kind == "photo" || input.kind == "video" {
        segments.push(MEDIA_PHOTO_VIDEO.trim());
    }
    if input.kind == "sticker"
        || input
            .sticker
            .as_ref()
            .and_then(|sticker| sticker.name.as_ref())
            .is_some_and(|name| !name.trim().is_empty())
    {
        segments.push(MEDIA_STICKER.trim());
    }

    segments.join(SEPARATOR)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn core_always_present() {
        let prompt = build(&ModerationInput::default(), false);
        assert!(prompt.contains("广告"), "{prompt}");
    }

    #[test]
    fn quote_adds_quote_rules() {
        let input = ModerationInput {
            quote_message: Some("引用内容".to_owned()),
            ..ModerationInput::default()
        };
        let prompt = build(&input, false);
        assert!(prompt.contains("引用"), "{prompt}");
    }

    #[test]
    fn photo_adds_media_rules() {
        let input = ModerationInput {
            kind: "photo".to_owned(),
            ..ModerationInput::default()
        };
        let prompt = build(&input, false);
        assert!(
            prompt.contains("图片") || prompt.contains("照片"),
            "{prompt}"
        );
    }
}
