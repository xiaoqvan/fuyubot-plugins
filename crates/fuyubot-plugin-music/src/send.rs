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
//! 所有等待都走 [`Runtime::send_op`]（`ResponseRouter` + `tokio::time::timeout`）：
//! 先发后登记，响应比 `invoke_op` 返回还早也不会丢；`stop` 之后立刻返回。

use serde_json::{Value, json};

use crate::runtime::Runtime;
use crate::simplify::{InputFile, Media, SendMessageParams, Text, Thumbnail, send_params};

/// 发送预算。无损音频几十 MB，TDLib 要先上传完才算发出去。
pub const TIMEOUT_MS: u64 = 300_000;

/// 纯文本消息的预算（错误提示 / 搜索结果）。
const TEXT_TIMEOUT_MS: u64 = 60_000;

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
/// 文件 id（`InputFile::id`，缓存命中）。**不引用原消息**（用户要求）。
pub async fn send_video(
    runtime: &Runtime,
    chat_id: i64,
    file: InputFile,
    cover: Option<InputFile>,
    meta: &VideoMeta,
) -> Result<Value, String> {
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
    let op = send_message_op(chat_id, &params);
    runtime.send_op(&op, TIMEOUT_MS).await
}

/// 发一条音频消息。封面可选（下载失败就不带，不影响音频本身）。**不引用原消息**。
pub async fn send_audio(
    runtime: &Runtime,
    chat_id: i64,
    file: InputFile,
    cover: Option<InputFile>,
    meta: &AudioMeta,
) -> Result<Value, String> {
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
    let op = send_message_op(chat_id, &params);
    runtime.send_op(&op, TIMEOUT_MS).await
}

/// 发一条纯文本（错误 / 搜索结果）。**不引用原消息**。
pub async fn send_text(runtime: &Runtime, chat_id: i64, text: &str) -> Result<(), String> {
    let params = SendMessageParams {
        text: Some(Text::plain(text)),
        ..SendMessageParams::default()
    };
    let payload = send_params(chat_id, &params, &|text: &Text| text_to_json(text));
    let op = json!({ "op": "send_message", "args": payload }).to_string();
    runtime.send_op(&op, TEXT_TIMEOUT_MS).await.map(|_| ())
}

/// 组装 `sendMessage` 的 simplify op（`send_params` 只当 JSON 构造器用）。
fn send_message_op(chat_id: i64, params: &SendMessageParams) -> String {
    let payload = send_params(chat_id, params, &|text: &Text| text_to_json(text));
    let mut args = match payload {
        Value::Object(map) => map,
        other => json!({ "raw": other })
            .as_object()
            .cloned()
            .unwrap_or_default(),
    };
    args.insert("timeout_ms".to_owned(), json!(TIMEOUT_MS));
    json!({ "op": "send_message", "args": Value::Object(args) }).to_string()
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
