//! 发送：把已经落盘（必要时已解密）的音频 / 视频变成一条 Telegram 消息。
//!
//! 只做发送，不下载、不解密——那些在 [`crate::commands`] 里按平台分别做，
//! 这一层不认识网易云也不认识汽水。
//!
//! 三个刻意的决定：
//!
//! 1. **本地文件走 `inputFileLocal`**：两家的直链都有防盗链 / 时效签名 / 需要特定
//!    `User-Agent`，TDLib 按 `inputFileRemote` 去拉常常拿不到东西。
//! 2. **缓存命中走 `inputFileId`**：文件已经在 TDLib 里，直接用 `file.id` 发，
//!    不再上传、不再下载（见 [`InputFile::id`]）。
//! 3. **文案用纯文本**：歌名里的 `_`、`[`、`*` 在 Markdown 里都是语法字符，
//!    转义漏一个就整条发不出去；纯文本最稳，链接直接以原文出现。
//!
//! 新区别（与抖音插件一致）：普通会话 `Target::send` 新发一条；频道帖
//! `Target::edit` 走 `editMessageMedia` / `editMessageText` 编辑原帖，且编辑前先
//! [`channel_rights`] 查 `getMessageProperties`——没有权限就不编辑。
//!
//! 所有等待都走 [`Runtime::send_op`]（`ResponseRouter` + `tokio::time::timeout`）：
//! 先发后登记，响应比 `invoke_op` 返回还早也不会丢；`stop` 之后立刻返回。

use serde_json::{Map, Value, json};

use crate::runtime::Runtime;
use crate::simplify::{InputFile, Media, SendMessageParams, Text, Thumbnail, send_params};

/// 发送预算。无损音频几十 MB，TDLib 要先上传完才算发出去。
pub const TIMEOUT_MS: u64 = 300_000;

/// 纯文本消息的预算（错误提示 / 搜索结果 / 频道占位）。
const TEXT_TIMEOUT_MS: u64 = 60_000;

// ── 落点：普通会话新发 / 频道帖编辑原帖 ─────────────────────────────

/// 一次发送落到哪儿 + 频道帖的编辑权限。
///
/// 普通会话 **不引用原消息**（用户要求）；频道里命令 / 链接消息本身就是那条帖子，
/// 所以编辑它（与抖音插件同一套口径）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Target {
    pub chat_id: i64,
    /// `Some(message_id)` = 频道里编辑原帖；`None` = 新发一条。
    pub edit_message_id: Option<i64>,
    /// 频道帖的文本编辑权限（`getMessageProperties.can_be_edited`）。
    pub can_edit_text: bool,
    /// 频道帖的媒体编辑权限（`getMessageProperties.can_edit_media`）。
    pub can_edit_media: bool,
}

impl Target {
    /// 往这个会话新发一条消息（不引用）。
    pub fn send(chat_id: i64) -> Self {
        Self {
            chat_id,
            edit_message_id: None,
            can_edit_text: false,
            can_edit_media: false,
        }
    }

    /// 频道里编辑原帖。
    pub fn edit(chat_id: i64, message_id: i64) -> Self {
        Self {
            chat_id,
            edit_message_id: Some(message_id),
            can_edit_text: false,
            can_edit_media: false,
        }
    }

    /// 一条命令的落点：频道帖编辑原帖，其它新发。
    pub fn of(command: &crate::simplify::Command) -> Self {
        if command.is_channel_post {
            Self::edit(command.chat_id, command.message_id)
        } else {
            Self::send(command.chat_id)
        }
    }

    pub fn is_edit(&self) -> bool {
        self.edit_message_id.is_some()
    }

    /// 带上刚查到的频道权限。
    pub fn with_rights(mut self, rights: ChannelRights) -> Self {
        self.can_edit_text = rights.can_edit_text;
        self.can_edit_media = rights.can_edit_media;
        self
    }
}

/// 当前账号对一条频道帖的编辑权限（来自 TDLib `getMessageProperties`）。
///
/// ⚠️ 新版 TDLib 把 `can_be_edited` / `can_edit_media` 放在 `MessageProperties` 里
/// （`getMessageProperties` 方法），消息对象本身不再带这两个字段；编辑媒体要用
/// `can_edit_media`，只查 `can_be_edited` 不够。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ChannelRights {
    pub can_edit_text: bool,
    pub can_edit_media: bool,
}

/// 问 TDLib 这条频道帖当前能不能编辑（一次 `getMessageProperties` 往返）。
pub async fn channel_rights(runtime: &Runtime, target: &Target) -> Result<ChannelRights, String> {
    let Some(message_id) = target.edit_message_id else {
        return Err("这个目标不是频道编辑模式".to_owned());
    };
    let args = json!({
        "request": {
            "@type": "getMessageProperties",
            "chat_id": target.chat_id,
            "message_id": message_id,
        },
        "timeout_ms": TEXT_TIMEOUT_MS,
    });
    let op = json!({ "op": "invoke", "args": args }).to_string();
    let properties = runtime.send_op(&op, TEXT_TIMEOUT_MS).await?;
    Ok(ChannelRights {
        can_edit_text: properties
            .get("can_be_edited")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        can_edit_media: properties
            .get("can_edit_media")
            .and_then(Value::as_bool)
            .unwrap_or(false),
    })
}

/// 频道里解析媒体前的占位提示。只有文本编辑权限时才会真的改原帖。
pub async fn announce_parsing(runtime: &Runtime, target: &Target) {
    if !target.is_edit() || !target.can_edit_text {
        return;
    }
    if let Err(error) = send_text(runtime, target, "正在获取音乐...").await {
        crate::log::warn(&format!("频道占位提示编辑失败（继续解析）：{error}"));
    }
}

/// 一条音频的展示信息。
#[derive(Debug, Clone, Default)]
pub struct AudioMeta {
    pub title: String,
    pub performer: String,
    pub album: String,
    pub link: String,
    /// 已带展示名的音质（如「极高（exhigh）」）。
    pub quality: String,
    pub bitrate: u64,
    pub size: u64,
    /// 秒。
    pub duration: i64,
    /// 只是试听片段。
    pub trial: bool,
}

impl AudioMeta {
    /// 音频消息的文案。
    pub fn caption(&self) -> String {
        let mut lines = vec![format!("{} - {}", self.title, self.performer)];
        if !self.album.trim().is_empty() {
            lines.push(format!("专辑：{}", self.album));
        }
        let mut detail = vec![self.quality.clone()];
        if self.bitrate > 0 {
            let kbps = if self.bitrate > 1000 {
                self.bitrate / 1000
            } else {
                self.bitrate
            };
            detail.push(format!("{kbps} kbps"));
        }
        if self.size > 0 {
            detail.push(format!("{:.1} MB", self.size as f64 / 1024.0 / 1024.0));
        }
        lines.push(format!(
            "音质：{}",
            detail
                .into_iter()
                .filter(|part| !part.is_empty())
                .collect::<Vec<_>>()
                .join(" · ")
        ));
        if !self.link.trim().is_empty() {
            lines.push(self.link.clone());
        }
        if self.trial {
            lines.push("⚠️ 只是试听片段（整曲需要会员 / 应用签名凭证）".to_owned());
        }
        lines.join("\n")
    }
}

/// 一条 UGC 音乐视频的展示信息。
#[derive(Debug, Clone, Default)]
pub struct VideoMeta {
    pub title: String,
    pub author: String,
    /// 秒。
    pub duration: i64,
    pub width: i64,
    pub height: i64,
    /// 只是试听片段。
    pub preview: bool,
}

impl VideoMeta {
    /// 文案照抖音插件：标题（相邻话题标签拆开）+ 空行 + `By @作者`。
    pub fn caption(&self) -> String {
        let title = if self.title.trim().is_empty() {
            "汽水音乐视频".to_owned()
        } else {
            sanitize_title(&self.title)
        };
        let author = if self.author.trim().is_empty() {
            "汽水音乐".to_owned()
        } else {
            self.author.trim().to_owned()
        };
        let mut caption = format!(
            "{title}

By @{author}"
        );
        if self.preview {
            caption.push_str(
                "

⚠️ 只是试听片段（完整视频需要会员 / 应用签名凭证）",
            );
        }
        caption
    }
}

/// 抖音的文案习惯：相邻的 `#话题#` 之间补空格，否则会连成一个超长话题。
fn sanitize_title(title: &str) -> String {
    let mut out = String::with_capacity(title.len() + 4);
    let mut previous: Option<char> = None;
    for ch in title.chars() {
        if ch == '#' && previous.is_some_and(|prev| !prev.is_whitespace()) {
            out.push(' ');
        }
        out.push(ch);
        previous = Some(ch);
    }
    out
}

/// 发一条视频消息。封面可选（下载失败就不带，不影响视频本身）。
///
/// `file` / `cover` 既可以是本地路径（`InputFile::local`），也可以是 TDLib 已知的
/// 文件 id（`InputFile::id`，缓存命中）。普通会话**不引用原消息**；频道里编辑原帖。
pub async fn send_video(
    runtime: &Runtime,
    target: &Target,
    file: InputFile,
    cover: Option<InputFile>,
    meta: &VideoMeta,
) -> Result<Value, String> {
    if target.is_edit() && !target.can_edit_media {
        return Err("没有编辑频道帖媒体的权限".to_owned());
    }
    let media = Media::Video {
        file,
        caption: None,
        thumbnail: cover.map(Thumbnail::new),
        cover: None,
        start_timestamp: None,
        duration: meta.duration.max(0) as i32,
        width: meta.width.max(0) as i32,
        height: meta.height.max(0) as i32,
        supports_streaming: true,
        has_spoiler: false,
    };
    let params = SendMessageParams {
        text: Some(Text::plain(meta.caption())),
        media: Some(media),
        timeout_ms: TIMEOUT_MS,
    };
    let op = message_op(target, &params);
    runtime.send_op(&op, TIMEOUT_MS).await
}

/// 发一条音频消息。封面可选（下载失败就不带，不影响音频本身）。
/// 普通会话**不引用原消息**；频道里编辑原帖。
pub async fn send_audio(
    runtime: &Runtime,
    target: &Target,
    file: InputFile,
    cover: Option<InputFile>,
    meta: &AudioMeta,
) -> Result<Value, String> {
    if target.is_edit() && !target.can_edit_media {
        return Err("没有编辑频道帖媒体的权限".to_owned());
    }
    let media = Media::Audio {
        file,
        caption: None,
        album_cover_thumbnail: cover.map(Thumbnail::new),
        duration: meta.duration.max(0) as i32,
        title: meta.title.clone(),
        performer: meta.performer.clone(),
    };
    let params = SendMessageParams {
        text: Some(Text::plain(meta.caption())),
        media: Some(media),
        timeout_ms: TIMEOUT_MS,
    };
    let op = message_op(target, &params);
    runtime.send_op(&op, TIMEOUT_MS).await
}

/// 发一条纯文本（错误 / 搜索结果 / 频道占位）。普通会话**不引用原消息**。
pub async fn send_text(runtime: &Runtime, target: &Target, text: &str) -> Result<(), String> {
    if target.is_edit() {
        if !target.can_edit_text {
            return Err("没有编辑频道帖文本的权限".to_owned());
        }
        let Some(message_id) = target.edit_message_id else {
            return Err("这个目标不是频道编辑模式".to_owned());
        };
        let args = json!({
            "chat_id": target.chat_id,
            "message_id": message_id,
            "input_message_content": {
                "@type": "inputMessageText",
                "text": text_to_json(&Text::plain(text)),
            },
            "timeout_ms": TEXT_TIMEOUT_MS,
        });
        let op = json!({ "op": "edit_message_text", "args": args }).to_string();
        return runtime.send_op(&op, TEXT_TIMEOUT_MS).await.map(|_| ());
    }
    let params = SendMessageParams {
        text: Some(Text::plain(text)),
        ..SendMessageParams::default()
    };
    let payload = send_params(target.chat_id, &params, &|text: &Text| text_to_json(text));
    let op = json!({ "op": "send_message", "args": payload }).to_string();
    runtime.send_op(&op, TEXT_TIMEOUT_MS).await.map(|_| ())
}

/// 组装 `sendMessage` / `editMessageMedia` 的 simplify op（`send_params` 只当 JSON 构造器用）。
fn message_op(target: &Target, params: &SendMessageParams) -> String {
    let payload = send_params(target.chat_id, params, &|text: &Text| text_to_json(text));
    let mut args = match payload {
        Value::Object(map) => map,
        other => json!({ "raw": other })
            .as_object()
            .cloned()
            .unwrap_or_default(),
    };
    args.insert("timeout_ms".to_owned(), json!(TIMEOUT_MS));
    match target.edit_message_id {
        Some(message_id) => {
            args.remove("chat_id");
            let mut edit = Map::new();
            edit.insert("chat_id".to_owned(), json!(target.chat_id));
            edit.insert("message_id".to_owned(), json!(message_id));
            edit.insert(
                "input_message_content".to_owned(),
                args.remove("input_message_content").unwrap_or(Value::Null),
            );
            json!({ "op": "edit_message_media", "args": Value::Object(edit) }).to_string()
        }
        None => json!({ "op": "send_message", "args": Value::Object(args) }).to_string(),
    }
}

/// 宿主 `ops` 认的 `Text` 形状：`{text, parse_mode}`。
fn text_to_json(text: &Text) -> Value {
    let mode = match text.parse_mode {
        crate::simplify::ParseMode::Plain => "plain",
        crate::simplify::ParseMode::Markdown => "markdown",
        crate::simplify::ParseMode::MarkdownV2 => "markdown_v2",
        crate::simplify::ParseMode::Html => "html",
    };
    json!({ "text": text.text, "parse_mode": mode })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta() -> AudioMeta {
        AudioMeta {
            title: "晴天".to_owned(),
            performer: "周杰伦".to_owned(),
            album: "叶惠美".to_owned(),
            link: "https://music.163.com/song?id=186016".to_owned(),
            quality: "极高（exhigh）".to_owned(),
            bitrate: 320_000,
            size: 10 * 1024 * 1024,
            duration: 269,
            trial: false,
        }
    }

    #[test]
    fn caption_carries_title_quality_and_link() {
        let caption = meta().caption();
        assert!(caption.starts_with("晴天 - 周杰伦"), "{caption}");
        assert!(caption.contains("专辑：叶惠美"), "{caption}");
        assert!(caption.contains("320 kbps"), "{caption}");
        assert!(caption.contains("10.0 MB"), "{caption}");
        assert!(
            caption.contains("https://music.163.com/song?id=186016"),
            "{caption}"
        );
        assert!(!caption.contains("试听"), "{caption}");
    }

    #[test]
    fn trial_streams_say_so() {
        let mut value = meta();
        value.trial = true;
        assert!(value.caption().contains("试听片段"));
    }

    #[test]
    fn empty_optional_fields_do_not_leave_empty_lines() {
        let mut value = meta();
        value.album.clear();
        value.link.clear();
        value.bitrate = 0;
        value.size = 0;
        let caption = value.caption();
        assert!(!caption.contains("专辑："), "{caption}");
        assert!(!caption.ends_with('\n'), "{caption}");
        assert!(caption.contains("音质：极高（exhigh）"), "{caption}");
    }

    fn video_meta() -> VideoMeta {
        VideoMeta {
            title: "塔不灭 #永雏塔菲 #重金属 #关注塔菲谢谢喵关注#塔菲 谢谢喵！".to_owned(),
            author: "永川塔菲".to_owned(),
            duration: 33,
            width: 1920,
            height: 1080,
            preview: false,
        }
    }

    #[test]
    fn video_caption_follows_the_douyin_shape() {
        let caption = video_meta().caption();
        assert!(
            caption.contains("#关注塔菲谢谢喵关注 #塔菲"),
            "相邻话题要拆开：{caption}"
        );
        assert!(caption.ends_with("By @永川塔菲"), "{caption}");
        assert!(caption.starts_with("塔不灭 #永雏塔菲"), "{caption}");
        assert!(!caption.contains("试听"), "{caption}");
    }

    #[test]
    fn preview_videos_say_so_and_empty_fields_have_fallbacks() {
        let mut value = video_meta();
        value.preview = true;
        assert!(value.caption().contains("试听片段"));
        value.preview = false;
        value.author.clear();
        value.title.clear();
        let caption = value.caption();
        assert!(caption.starts_with("汽水音乐视频"), "{caption}");
        assert!(caption.contains("By @汽水音乐"), "{caption}");
    }
}
