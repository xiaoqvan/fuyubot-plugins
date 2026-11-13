//! 本地化的 simplify 层（上游 `fuyubot-simplify` 的裁剪版，**不依赖那个 crate**）。
//!
//! 为什么放在这里：`fuyubot-simplify` 是 `publish = false`，外部插件拿不到
//! crates.io 包、只能 path 依赖——插件仓库不该背这个包袱。ABI v5 已经把 simplify
//! 语义下沉成宿主的 op 表（`simplify_invoke` / `simplify_invoke_result`，实现在宿主的
//! `fuyubot-simplify::ops`），所以本模块只保留两样东西：
//!
//! 1. 「人话参数 → TDLib 请求体」的构造器：`Text` / `Media` / `SendMessageParams` /
//!    `SendAlbumParams` / `send_params` / `album_params` / 命令解析；
//! 2. 「从已发消息里取媒体文件 id」的 `sent_media` / `sent_media_list`（缓存复用用）。
//!
//! 下发与等待**不在这里**：走 [`crate::runtime::Runtime`] 的 `invoke_op`
//! （`fuyubot_abi::ResponseRouter::invoke_op` + `tokio::time::timeout`）。
//!
//! 代码与上游 `fuyubot-simplify` / `fuyubot-plugin-adblock` 的同名模块保持一致，
//! 只裁掉本插件没用到的变体（音频 / 文档 / 动图 / 贴纸 / 位置 / 富消息 / 反应 / 键盘）。

// 这是兼容层：保留上游的字段形状，允许暂时没有调用方的构造器存在。
#![allow(dead_code)]

use std::ffi::{CStr, c_void};

use fuyubot_abi::{HostApi, SubscriptionHandle};
use serde_json::{Map, Value, json};

/// 等发送结局的默认预算（与上游 `Simplify` 一致）。
pub const DEFAULT_TIMEOUT_MS: u64 = 30_000;

// ── 文本 ──────────────────────────────────────────────────────────

/// 文本解析方式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ParseMode {
    /// 纯文本：不花 `parseTextEntities` 往返，`entities` 为空。
    #[default]
    Plain,
    Markdown,
    MarkdownV2,
    Html,
}

/// 一段文本 + 它的解析模式。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Text {
    pub text: String,
    pub parse_mode: ParseMode,
}

impl Text {
    pub fn new(text: impl Into<String>, parse_mode: ParseMode) -> Self {
        Self {
            text: text.into(),
            parse_mode,
        }
    }

    pub fn plain(text: impl Into<String>) -> Self {
        Self::new(text, ParseMode::Plain)
    }

    pub fn markdown(text: impl Into<String>) -> Self {
        Self::new(text, ParseMode::Markdown)
    }

    pub fn markdown_v2(text: impl Into<String>) -> Self {
        Self::new(text, ParseMode::MarkdownV2)
    }

    pub fn html(text: impl Into<String>) -> Self {
        Self::new(text, ParseMode::Html)
    }
}

impl From<&str> for Text {
    fn from(text: &str) -> Self {
        Self::plain(text)
    }
}

impl From<String> for Text {
    fn from(text: String) -> Self {
        Self::plain(text)
    }
}

/// `Text` 的 op 侧形态：`{"text":"…","parse_mode":"markdown"}`。
///
/// 宿主的 op 认这两种形状：已经是 `formattedText` 的原样放过，带 `parse_mode` 的
/// 替你走一次 `parseTextEntities`——所以插件里不用自己 defer 一次解析往返。
pub fn text_json(text: &Text) -> Value {
    let mode = match text.parse_mode {
        ParseMode::Plain => "plain",
        ParseMode::Markdown => "markdown",
        ParseMode::MarkdownV2 => "markdown_v2",
        ParseMode::Html => "html",
    };
    json!({ "text": text.text, "parse_mode": mode })
}

/// 纯文本 → `formattedText`（不解析，不花往返）。
pub fn plain_text(text: &Text) -> Value {
    json!({ "@type": "formattedText", "text": text.text, "entities": [] })
}

// ── 媒体 ──────────────────────────────────────────────────────────

/// 要发的文件（TDLib 的 `InputFile`）。
///
/// | 变体 | TDLib | 用途 |
/// | --- | --- | --- |
/// | [`Local`](Self::Local) | `inputFileLocal` | 本地路径（插件自己下载出来的文件） |
/// | [`Remote`](Self::Remote) | `inputFileRemote` | 远程 URL（TDLib 自己去拉） |
/// | [`Id`](Self::Id) | `inputFileId` | **已存在于 TDLib 的文件 id**：不重传、不重下，直接复用 |
///
/// ⚠️ `Remote` 的 `id` 是 URL；`Id` 的是 TDLib 的 `file.id`（int32，这里收 `i64`）。
/// 缓存复用（下次直接发已上传过的文件）走 [`Id`](Self::Id)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputFile {
    Local {
        path: String,
    },
    Remote {
        id: String,
    },
    /// TDLib 已知的文件 id（来自之前某条消息的 `file.id`）。
    Id {
        id: i64,
    },
}

impl InputFile {
    pub fn local(path: impl Into<String>) -> Self {
        Self::Local { path: path.into() }
    }

    pub fn remote(id: impl Into<String>) -> Self {
        Self::Remote { id: id.into() }
    }

    /// 复用一个 TDLib 已经知道的文件：不重新上传、不重新下载。
    pub fn id(id: i64) -> Self {
        Self::Id { id }
    }

    pub fn to_json(&self) -> Value {
        match self {
            Self::Local { path } => json!({ "@type": "inputFileLocal", "path": path }),
            Self::Remote { id } => json!({ "@type": "inputFileRemote", "id": id }),
            Self::Id { id } => json!({ "@type": "inputFileId", "id": id }),
        }
    }
}

impl From<&str> for InputFile {
    fn from(path: &str) -> Self {
        Self::local(path)
    }
}

impl From<String> for InputFile {
    fn from(path: String) -> Self {
        Self::local(path)
    }
}

/// 缩略图 / 封面（`inputThumbnail`）。
///
/// 真 `td_api.tl`：`inputThumbnail thumbnail:InputFile width:int32 height:int32`。
/// 没设置时**整个字段不发**——真库文档写的是「pass null to skip thumbnail uploading」。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Thumbnail {
    pub file: InputFile,
    pub width: i32,
    pub height: i32,
}

impl Thumbnail {
    /// 一张缩略图；尺寸交给 TDLib 自己判断。
    pub fn new(file: impl Into<InputFile>) -> Self {
        Self {
            file: file.into(),
            width: 0,
            height: 0,
        }
    }

    pub fn to_json(&self) -> Value {
        json!({
            "@type": "inputThumbnail",
            "thumbnail": self.file.to_json(),
            "width": self.width,
            "height": self.height,
        })
    }
}

impl From<InputFile> for Thumbnail {
    fn from(file: InputFile) -> Self {
        Self::new(file)
    }
}

/// 媒体内容（TDLib 的 `inputMessagePhoto` / `inputMessageVideo`）。
#[derive(Debug, Clone, PartialEq)]
pub enum Media {
    Photo {
        file: InputFile,
        caption: Option<Text>,
        thumbnail: Option<Thumbnail>,
        width: i32,
        height: i32,
        has_spoiler: bool,
    },
    Video {
        file: InputFile,
        caption: Option<Text>,
        thumbnail: Option<Thumbnail>,
        /// 视频封面（那一帧的画面文件）。
        cover: Option<InputFile>,
        /// 从视频的第几秒开始播。
        start_timestamp: Option<i32>,
        duration: i32,
        width: i32,
        height: i32,
        supports_streaming: bool,
        has_spoiler: bool,
    },
}

impl Media {
    pub fn photo(file: impl Into<InputFile>) -> Self {
        Self::Photo {
            file: file.into(),
            caption: None,
            thumbnail: None,
            width: 0,
            height: 0,
            has_spoiler: false,
        }
    }

    pub fn video(file: impl Into<InputFile>) -> Self {
        Self::Video {
            file: file.into(),
            caption: None,
            thumbnail: None,
            cover: None,
            start_timestamp: None,
            duration: 0,
            width: 0,
            height: 0,
            supports_streaming: false,
            has_spoiler: false,
        }
    }

    pub fn caption(self, text: impl Into<Text>) -> Self {
        let caption = Some(text.into());
        match self {
            Self::Photo {
                file,
                thumbnail,
                width,
                height,
                has_spoiler,
                ..
            } => Self::Photo {
                file,
                caption,
                thumbnail,
                width,
                height,
                has_spoiler,
            },
            Self::Video {
                file,
                thumbnail,
                cover,
                start_timestamp,
                duration,
                width,
                height,
                supports_streaming,
                has_spoiler,
                ..
            } => Self::Video {
                file,
                caption,
                thumbnail,
                cover,
                start_timestamp,
                duration,
                width,
                height,
                supports_streaming,
                has_spoiler,
            },
        }
    }

    /// 视频时长（秒）。非视频变体原样返回。
    pub fn duration(self, seconds: i32) -> Self {
        match self {
            Self::Video {
                file,
                caption,
                thumbnail,
                cover,
                start_timestamp,
                width,
                height,
                supports_streaming,
                has_spoiler,
                ..
            } => Self::Video {
                file,
                caption,
                thumbnail,
                cover,
                start_timestamp,
                duration: seconds,
                width,
                height,
                supports_streaming,
                has_spoiler,
            },
            other => other,
        }
    }

    /// 宽高。填 `0` 让 TDLib 自己读真实值。
    pub fn size(self, width: i32, height: i32) -> Self {
        match self {
            Self::Photo {
                file,
                caption,
                thumbnail,
                has_spoiler,
                ..
            } => Self::Photo {
                file,
                caption,
                thumbnail,
                width,
                height,
                has_spoiler,
            },
            Self::Video {
                file,
                caption,
                thumbnail,
                cover,
                start_timestamp,
                duration,
                supports_streaming,
                has_spoiler,
                ..
            } => Self::Video {
                file,
                caption,
                thumbnail,
                cover,
                start_timestamp,
                duration,
                width,
                height,
                supports_streaming,
                has_spoiler,
            },
        }
    }

    /// 视频封面 + 起始秒（只对视频有效）。
    pub fn cover(self, cover: impl Into<InputFile>, start_timestamp: i32) -> Self {
        match self {
            Self::Video {
                file,
                caption,
                thumbnail,
                duration,
                width,
                height,
                supports_streaming,
                has_spoiler,
                ..
            } => Self::Video {
                file,
                caption,
                thumbnail,
                cover: Some(cover.into()),
                start_timestamp: Some(start_timestamp),
                duration,
                width,
                height,
                supports_streaming,
                has_spoiler,
            },
            other => other,
        }
    }

    /// 是否边下边播（只对视频有效）。
    pub fn streaming(self, supports_streaming: bool) -> Self {
        match self {
            Self::Video {
                file,
                caption,
                thumbnail,
                cover,
                start_timestamp,
                duration,
                width,
                height,
                has_spoiler,
                ..
            } => Self::Video {
                file,
                caption,
                thumbnail,
                cover,
                start_timestamp,
                duration,
                width,
                height,
                supports_streaming,
                has_spoiler,
            },
            other => other,
        }
    }

    fn caption_ref(&self) -> Option<&Text> {
        match self {
            Self::Photo { caption, .. } | Self::Video { caption, .. } => caption.as_ref(),
        }
    }
}

/// 媒体 → `InputMessageContent`。
///
/// 形状照真 `td_api.tl`：`inputMessagePhoto.photo` 是 `inputPhoto`、
/// `inputMessageVideo.video` 是 `inputVideo`，文件在再内层；caption 在内容类型里，
/// 不是 `sendMessage` 的顶层参数。
pub fn content_json(media: &Media, render: &dyn Fn(&Text) -> Value) -> Value {
    let caption = media.caption_ref().map(render);
    match media {
        Media::Photo {
            file,
            thumbnail,
            width,
            height,
            has_spoiler,
            ..
        } => {
            let mut inner = Map::new();
            inner.insert("@type".to_owned(), json!("inputPhoto"));
            inner.insert("photo".to_owned(), file.to_json());
            put_some(
                &mut inner,
                "thumbnail",
                thumbnail.as_ref().map(Thumbnail::to_json),
            );
            inner.insert("width".to_owned(), json!(width));
            inner.insert("height".to_owned(), json!(height));

            let mut object = Map::new();
            object.insert("@type".to_owned(), json!("inputMessagePhoto"));
            object.insert("photo".to_owned(), Value::Object(inner));
            if let Some(caption) = caption {
                object.insert("caption".to_owned(), caption);
            }
            object.insert("has_spoiler".to_owned(), json!(has_spoiler));
            Value::Object(object)
        }
        Media::Video {
            file,
            thumbnail,
            cover,
            start_timestamp,
            duration,
            width,
            height,
            supports_streaming,
            has_spoiler,
            ..
        } => {
            let mut inner = Map::new();
            inner.insert("@type".to_owned(), json!("inputVideo"));
            inner.insert("video".to_owned(), file.to_json());
            put_some(
                &mut inner,
                "thumbnail",
                thumbnail.as_ref().map(Thumbnail::to_json),
            );
            put_some(&mut inner, "cover", cover.as_ref().map(InputFile::to_json));
            put_some(
                &mut inner,
                "start_timestamp",
                start_timestamp.map(|seconds| json!(seconds)),
            );
            inner.insert("duration".to_owned(), json!(duration));
            inner.insert("width".to_owned(), json!(width));
            inner.insert("height".to_owned(), json!(height));
            inner.insert("supports_streaming".to_owned(), json!(supports_streaming));

            let mut object = Map::new();
            object.insert("@type".to_owned(), json!("inputMessageVideo"));
            object.insert("video".to_owned(), Value::Object(inner));
            if let Some(caption) = caption {
                object.insert("caption".to_owned(), caption);
            }
            object.insert("has_spoiler".to_owned(), json!(has_spoiler));
            Value::Object(object)
        }
    }
}

/// 只在 `Some` 时写键：可空字段没设置就整个不发，而不是发 `null`。
fn put_some(object: &mut Map<String, Value>, key: &str, value: Option<Value>) {
    if let Some(value) = value {
        object.insert(key.to_owned(), value);
    }
}

// ── 参数结构体 ────────────────────────────────────────────────────

/// `sendMessage` 的参数。`text` 与 `media` 二选一；都填就是「媒体 + 标题」。
///
/// ⚠️ 刻意**不带 `reply_to`**：用户要求不引用/不回复原消息，所以这里连字段都不留，
/// 构造出来的请求里永远不会出现 `inputMessageReplyToMessage`。
#[derive(Debug, Clone, Default)]
pub struct SendMessageParams {
    pub text: Option<Text>,
    pub media: Option<Media>,
    pub timeout_ms: u64,
}

impl SendMessageParams {
    pub fn text(text: impl Into<Text>) -> Self {
        Self {
            text: Some(text.into()),
            timeout_ms: DEFAULT_TIMEOUT_MS,
            ..Self::default()
        }
    }

    pub fn media(media: impl Into<Media>) -> Self {
        Self {
            media: Some(media.into()),
            timeout_ms: DEFAULT_TIMEOUT_MS,
            ..Self::default()
        }
    }
}

/// `sendMessageAlbum` 的参数（2–10 项，只有音频/文档/图片/视频能组成相册）。
///
/// ⚠️ **相册没有 `reply_markup`**：真签名是
/// `sendMessageAlbum chat_id topic_id reply_to options input_message_contents`。
#[derive(Debug, Clone)]
pub struct SendAlbumParams {
    pub medias: Vec<Media>,
    /// 整组共用一段标题。
    pub caption: Option<Text>,
    /// 整组**共用**的等待预算。
    pub timeout_ms: u64,
}

impl Default for SendAlbumParams {
    fn default() -> Self {
        Self {
            medias: Vec::new(),
            caption: None,
            timeout_ms: DEFAULT_TIMEOUT_MS,
        }
    }
}

// ── 请求组装 ──────────────────────────────────────────────────────

/// 组装 `sendMessage` 的参数（不含 `@type`，那是 op 侧补的）。
///
/// 参数名以真 `td_api.tl` 为准（`sendMessage chat_id topic_id reply_to options
/// reply_markup input_message_content`）。这里只填 `chat_id` + `input_message_content`：
///
/// - **不发 `reply_to`**（不引用原消息，用户明确要求）；
/// - **没有顶层 `caption`**，它在内容类型里（见 [`content_json`]）。
pub fn send_params(
    chat_id: i64,
    params: &SendMessageParams,
    render: &dyn Fn(&Text) -> Value,
) -> Value {
    let mut payload = Map::new();
    payload.insert("chat_id".to_owned(), json!(chat_id));
    payload.insert(
        "input_message_content".to_owned(),
        message_content(params, render),
    );
    Value::Object(payload)
}

/// 文本 + 媒体 → `InputMessageContent`（两者都空时按空文本处理，TDLib 会拒绝）。
fn message_content(params: &SendMessageParams, render: &dyn Fn(&Text) -> Value) -> Value {
    match &params.media {
        Some(media) => content_json(&media_with_caption(media, params.text.as_ref()), render),
        None => {
            let mut object = Map::new();
            object.insert("@type".to_owned(), json!("inputMessageText"));
            object.insert(
                "text".to_owned(),
                render(params.text.as_ref().unwrap_or(&Text::plain(""))),
            );
            Value::Object(object)
        }
    }
}

/// 媒体的标题：`params.text` 优先，其次是媒体自带的 `caption`。
fn media_with_caption(media: &Media, text: Option<&Text>) -> Media {
    match text {
        Some(text) => media.clone().caption(text.clone()),
        None => media.clone(),
    }
}

/// 组装 `sendMessageAlbum` 的参数。
pub fn album_params(
    chat_id: i64,
    params: &SendAlbumParams,
    render: &dyn Fn(&Text) -> Value,
) -> Value {
    let contents: Vec<Value> = params
        .medias
        .iter()
        .map(|media| {
            let media = match &params.caption {
                Some(caption) => media.clone().caption(caption.clone()),
                None => media.clone(),
            };
            content_json(&media, render)
        })
        .collect();

    let mut payload = Map::new();
    payload.insert("chat_id".to_owned(), json!(chat_id));
    payload.insert("input_message_contents".to_owned(), Value::Array(contents));
    Value::Object(payload)
}

// ── 已发送消息里的媒体文件 id（缓存复用那一半）───────────────────

/// 媒体种类（存缓存 / 打日志用的稳定字符串）。
pub const KIND_AUDIO: &str = "audio";
pub const KIND_VIDEO: &str = "video";
pub const KIND_PHOTO: &str = "photo";
pub const KIND_DOCUMENT: &str = "document";
pub const KIND_ANIMATION: &str = "animation";
pub const KIND_OTHER: &str = "other";

/// 一条已发送消息里的媒体文件 id。
///
/// `kind` 是 [`KIND_AUDIO`] 这一类稳定字符串；`file_id` / `cover_id` 都可能缺
/// （比如没封面的音频、`other` 类型的消息）。`width` / `height` / `duration`
/// 缺失时是 0（与 TDLib 的「0 = 让库自己判断」一致）。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SentMedia {
    pub kind: &'static str,
    /// 媒体本体的 TDLib `file.id`。
    pub file_id: Option<i64>,
    /// 封面 / 缩略图的 `file.id`（音频专辑封面、视频封面；没有就是 `None`）。
    pub cover_id: Option<i64>,
    pub width: i64,
    pub height: i64,
    /// 秒。
    pub duration: i64,
}

/// 从一条 `Message`（`sendMessage` / `editMessageMedia` 的结果）里取媒体 id。
///
/// 形状全按**真** `td_api.tl`（不是 Bot API）：
///
/// ```text
/// messageAudio      content.audio.audio.file.id
///                   content.audio.album_cover_thumbnail.thumbnail.file.id（封面）
/// messageVideo      content.video.video.file.id
///                   content.cover.sizes[].photo.file.id（封面，取最大一张）
///                   content.video.thumbnail.thumbnail.file.id（退路）
/// messagePhoto      content.photo.sizes[].photo.file.id（取最大一张）
/// messageDocument   content.document.document.file.id
/// messageAnimation  content.animation.animation.file.id
/// ```
///
/// `file.id` 在 TDLib 里是 **int32**；这里按 `i64` 收（SQLite / JSON 都更顺手），
/// 0 视作「没有」（空文件对象会带 0）。
pub fn sent_media(message: &Value) -> Option<SentMedia> {
    let content = message.get("content")?;
    let kind = content.get("@type")?.as_str()?;
    match kind {
        "messageAudio" => {
            let audio = content.get("audio")?;
            let cover = audio.get("album_cover_thumbnail");
            Some(SentMedia {
                kind: KIND_AUDIO,
                file_id: file_id(audio.get("audio")),
                cover_id: cover.and_then(thumbnail_file_id),
                width: number(cover, "width") as i64,
                height: number(cover, "height") as i64,
                duration: number(Some(audio), "duration") as i64,
            })
        }
        "messageVideo" => {
            let video = content.get("video")?;
            // 封面优先用 `cover`（发的时候 inputVideo.cover 传的那张原图），
            // 没有就退回 `thumbnail`（TDLib 抽的帧）。
            let cover = content
                .get("cover")
                .and_then(best_photo_file_id)
                .or_else(|| video.get("thumbnail").and_then(thumbnail_file_id));
            Some(SentMedia {
                kind: KIND_VIDEO,
                file_id: file_id(video.get("video")),
                cover_id: cover,
                width: number(Some(video), "width") as i64,
                height: number(Some(video), "height") as i64,
                duration: number(Some(video), "duration") as i64,
            })
        }
        "messagePhoto" => {
            let (file_id, width, height) = content
                .get("photo")
                .and_then(best_photo_size)
                .map(|size| (Some(size.0), size.1, size.2))
                .unwrap_or((None, 0, 0));
            Some(SentMedia {
                kind: KIND_PHOTO,
                file_id,
                cover_id: None,
                width,
                height,
                duration: 0,
            })
        }
        "messageDocument" => {
            let document = content.get("document")?;
            Some(SentMedia {
                kind: KIND_DOCUMENT,
                file_id: file_id(document.get("document")),
                cover_id: document.get("thumbnail").and_then(thumbnail_file_id),
                width: 0,
                height: 0,
                duration: 0,
            })
        }
        "messageAnimation" => {
            let animation = content.get("animation")?;
            Some(SentMedia {
                kind: KIND_ANIMATION,
                file_id: file_id(animation.get("animation")),
                cover_id: animation.get("thumbnail").and_then(thumbnail_file_id),
                width: number(Some(animation), "width") as i64,
                height: number(Some(animation), "height") as i64,
                duration: number(Some(animation), "duration") as i64,
            })
        }
        _ => None,
    }
}

/// 一组消息（相册的结果数组）里的媒体 id；单条消息也接受。
pub fn sent_media_list(messages: &Value) -> Vec<SentMedia> {
    match messages {
        Value::Array(items) => items.iter().filter_map(sent_media).collect(),
        other => sent_media(other).into_iter().collect(),
    }
}

/// `file` 对象里的 id；0 当成没有。
fn file_id(node: Option<&Value>) -> Option<i64> {
    let id = node?.get("id")?.as_i64()?;
    (id != 0).then_some(id)
}

/// `thumbnail` 对象里的 `file.id`。
///
/// 真形状是 `node.thumbnail = {file: {id: …}}`（`file` 还套一层），
/// 不是 `thumbnail.id`。
fn thumbnail_file_id(node: &Value) -> Option<i64> {
    file_id(node.get("thumbnail")?.get("file"))
}

/// `photo` 里最大那张的 `(file_id, width, height)`。
fn best_photo_size(photo: &Value) -> Option<(i64, i64, i64)> {
    let sizes = photo.get("sizes")?.as_array()?;
    sizes
        .iter()
        .filter_map(|size| {
            let file_id = file_id(size.get("photo"))?;
            let width = size.get("width").and_then(Value::as_i64).unwrap_or(0);
            let height = size.get("height").and_then(Value::as_i64).unwrap_or(0);
            Some((file_id, width, height))
        })
        .max_by_key(|(_, width, height)| width * height)
}

/// `photo` 里最大那张的 `file.id`。
fn best_photo_file_id(photo: &Value) -> Option<i64> {
    best_photo_size(photo).map(|(file_id, _, _)| file_id)
}

/// 取一个整数字段（缺字段 / 非数字都当 0）。
fn number(node: Option<&Value>, key: &str) -> i32 {
    node.and_then(|value| value.get(key))
        .and_then(Value::as_i64)
        .unwrap_or(0) as i32
}

// ── 命令（插件最常见的那种入口）──────────────────────────────────

/// 宿主「我是谁」那条 service 的名字（由 `fuyubot-loader` 提供）。
///
/// 方法 `usernames`（无参）返回自身全部活跃用户名的 JSON 数组，比如 `["FuyuBot"]`；
/// 还没登录 / 没接 TDLib 时返回 `[]`（= 还不知道自己是谁）。[`on_command`] 靠它
/// 判断「这条命令点名的是不是我」。
pub const IDENTITY_SERVICE: &str = "fuyubot.host.identity";

/// 一条 `updateNewMessage` 里的命令：`/dy`、`/dy@SomeBot 链接`。
///
/// 只认**文本消息**，并跳过自己发的（`is_outgoing`）——机器人不该响应自己。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Command {
    pub chat_id: i64,
    pub message_id: i64,
    /// 命令名：去掉 `/` 与 `@BotName` 后缀，已转小写。
    pub name: String,
    /// `/cmd` 后面那一段（去首尾空白，可能为空）。
    pub args: String,
    /// `@` 后面那个名字（原样保留大小写，比较时不分大小写）；裸命令是 `None`。
    pub addressed_to: Option<String>,
    /// 整条消息文本（命令 + 参数）。
    pub text: String,
    /// 这条命令是不是**频道帖子**（见 [`channel_post_of`]）——频道里要编辑原帖，
    /// 不能新发一条。
    pub is_channel_post: bool,
}

impl Command {
    /// 是这条命令吗（不分大小写，`/Dy` 也算）。
    pub fn is(&self, name: &str) -> bool {
        self.name.eq_ignore_ascii_case(name)
    }

    /// 该不该由「用户名是这些」的账号响应：裸命令谁都可以，点名命令只有被点到的那个。
    ///
    /// `usernames` 为空表示**还不知道自己是谁**（没登录 / 没接 TDLib）——这时一律算
    /// 「该响应」：宁可多回一句，也别在身份没就绪时把命令吞掉。
    pub fn is_for(&self, usernames: &[String]) -> bool {
        match &self.addressed_to {
            None => true,
            Some(_) if usernames.is_empty() => true,
            Some(target) => usernames
                .iter()
                .any(|name| name.eq_ignore_ascii_case(target)),
        }
    }
}

/// 消息是不是**频道帖子**。
///
/// 用 TDLib 自己的 `message.is_channel_post`（「All messages to channels are channel
/// posts, all other messages are not channel posts」），不要只看 `sender_id` 的类型：
/// 群里「匿名频道马甲」的发送者也是 `messageSenderChat`，甚至可能就是这个群本身；
/// 靠发送者猜会把群消息误判成频道帖，接着去编辑一条不属于自己的消息（TDLib 直接报错）。
pub fn channel_post_of(message: &Value) -> bool {
    message
        .get("is_channel_post")
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

/// 从 `updateNewMessage` 的负载里认出一条命令；不是命令就 `None`。
///
/// 字段路径照真 `td_api.tl`：`updateNewMessage.message` →
/// `content:messageText` → `text:formattedText` → `text:string`。
pub fn command_of(update: &Value) -> Option<Command> {
    let message = update.get("message")?;
    // 自己发的不当命令：除非它是频道帖（在自家频道里敲 `/dy` 要能把原帖编辑成视频）。
    let is_channel_post = channel_post_of(message);
    if message
        .get("is_outgoing")
        .and_then(Value::as_bool)
        .unwrap_or(false)
        && !is_channel_post
    {
        return None;
    }
    let content = message.get("content")?;
    if content.get("@type").and_then(Value::as_str) != Some("messageText") {
        return None;
    }
    let text = content.get("text")?.get("text")?.as_str()?;
    let trimmed = text.trim_start();
    let token = trimmed.split_whitespace().next()?;
    // `/help@SomeBot args…` → `help` + `SomeBot`；`@` 之后是「发给哪个 bot」，
    // 不是命令名的一部分，但**要留下来**——是不是自己，得有人判断。
    let rest = token.strip_prefix('/')?;
    let (name, addressed_to) = match rest.split_once('@') {
        Some((name, target)) => (name, Some(target)),
        None => (rest, None),
    };
    if name.is_empty() {
        return None;
    }
    Some(Command {
        chat_id: message.get("chat_id")?.as_i64()?,
        message_id: message.get("id")?.as_i64()?,
        name: name.to_ascii_lowercase(),
        args: trimmed[token.len()..].trim().to_owned(),
        // 空 `@`（`/start@`）不算点名别人：按裸命令处理。
        addressed_to: addressed_to
            .filter(|target| !target.is_empty())
            .map(str::to_owned),
        text: text.to_owned(),
        is_channel_post,
    })
}

/// 问宿主「我是谁」：返回自身全部活跃用户名；拿不到（老宿主 / 还没登录 / 没接 TDLib）
/// 返回空表。
///
/// 走的是 [`IDENTITY_SERVICE`] 这条 **service**，不是 TDLib 请求——所以在事件回调里
/// 调它**不会**卡住接收循环。
pub fn my_usernames(host: &HostApi) -> Vec<String> {
    let Ok(reply) = host.call_service(IDENTITY_SERVICE, "usernames", "{}") else {
        return Vec::new();
    };
    serde_json::from_str(&reply).unwrap_or_default()
}

/// 命令订阅的持有者：**drop 就退订**（和 `Effect` 一个语义）。
///
/// 回调闭包由它自己持有——递给 ABI 的 `user_data` 指的就是这里那个 Box，
/// 退订之后一起释放。
pub struct CommandSubscription {
    host: HostApi,
    handle: SubscriptionHandle,
    /// 只负责**持有**回调与宿主句柄；退订之前它的地址必须稳定。
    _handler: Box<CommandHandler>,
}

impl Drop for CommandSubscription {
    fn drop(&mut self) {
        if self.handle.is_valid() {
            self.host.off(self.handle);
        }
    }
}

/// 回调 + 「我是谁」的判定。
struct CommandHandler {
    host: HostApi,
    handler: Box<dyn Fn(&Command) + Send + Sync>,
}

impl CommandHandler {
    /// 这条命令该不该交给插件：点名别人的不交（裸命令、点名自己的都交）。
    fn accepts(&self, command: &Command) -> bool {
        if command.addressed_to.is_none() {
            return true;
        }
        // 每次现问「我是谁」：用户名会变（`updateUser`），缓存住就会一直按旧名字判断。
        // 这是纯内存的 service 读，不碰 TDLib，命令频率也不高。
        command.is_for(&my_usernames(&self.host))
    }
}

/// 订阅 `updateNewMessage`，把**该由本插件响应的**命令交给 `handler`。
///
/// 不打扰它的：非命令、自己发的、以及**点名给别人的**（`/dy@别的Bot`）。
/// 交给它的：裸命令（`/dy`，Telegram 的约定是所有 bot 都响应）与点名给自己的。
///
/// 插件在自己的 `start` 里调一次，把返回的守卫**存进插件状态**（drop 就退订）。
/// 宿主拒绝订阅时返回 `None`。
pub fn on_command(
    host: HostApi,
    handler: impl Fn(&Command) + Send + Sync + 'static,
) -> Option<CommandSubscription> {
    let mut owned: Box<CommandHandler> = Box::new(CommandHandler {
        host,
        handler: Box::new(handler),
    });
    // 递给 ABI 的是这个 Box 的地址：它活着，指针就稳定。
    let user_data = (&raw mut *owned).cast::<c_void>();
    let handle = host.on("updateNewMessage", None, command_trampoline, user_data);
    handle.is_valid().then_some(CommandSubscription {
        host,
        handle,
        _handler: owned,
    })
}

/// 命令订阅的 C 回调：把负载解成 [`Command`]，过掉点名给别人的，再交给闭包。
extern "C-unwind" fn command_trampoline(
    user_data: *mut c_void,
    payload: *const std::ffi::c_char,
) -> i32 {
    if user_data.is_null() || payload.is_null() {
        return -1;
    }
    // SAFETY: `user_data` 是 `on_command` 里那个 Box，订阅活多久它就活多久。
    let state = unsafe { &*(user_data as *const CommandHandler) };
    // SAFETY: 宿主保证 `payload` 是 NUL 结尾的临时字符串。
    let text = unsafe { CStr::from_ptr(payload) }.to_string_lossy();
    let Ok(update) = serde_json::from_str::<Value>(&text) else {
        return 0;
    };
    if let Some(command) = command_of(&update)
        && state.accepts(&command)
    {
        (state.handler)(&command);
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 纯文本 / Markdown 的 op 形状：宿主 `format_fields` 认这两个键。
    #[test]
    fn text_serializes_to_the_op_shape() {
        assert_eq!(
            text_json(&Text::markdown("你好")),
            json!({ "text": "你好", "parse_mode": "markdown" })
        );
        assert_eq!(text_json(&Text::plain("x"))["parse_mode"], "plain");
    }

    /// 三种输入文件的 JSON 构造器各是各的：`inputFileId` 与 `inputFileRemote` 别混。
    #[test]
    fn input_files_serialize_to_the_right_constructors() {
        assert_eq!(
            InputFile::local("D:/cache/a.mp4").to_json(),
            json!({"@type": "inputFileLocal", "path": "D:/cache/a.mp4"})
        );
        assert_eq!(
            InputFile::remote("https://x/a.mp4").to_json(),
            json!({"@type": "inputFileRemote", "id": "https://x/a.mp4"})
        );
        assert_eq!(
            InputFile::id(123456).to_json(),
            json!({"@type": "inputFileId", "id": 123456})
        );
    }

    /// 视频：`inputMessageVideo` 里 `video:inputVideo`，封面在 `inputVideo.cover`。
    #[test]
    fn video_content_matches_the_real_schema() {
        let media = Media::video(InputFile::remote("https://x/v.mp4"))
            .size(1080, 1920)
            .duration(12)
            .streaming(true)
            .cover(InputFile::remote("https://x/c.jpg"), 0);
        let content = content_json(&media, &text_json);
        assert_eq!(content["@type"], "inputMessageVideo");
        assert_eq!(
            content["video"]["video"],
            json!({"@type": "inputFileRemote", "id": "https://x/v.mp4"})
        );
        assert_eq!(content["video"]["width"], 1080);
        assert_eq!(content["video"]["height"], 1920);
        assert_eq!(content["video"]["duration"], 12);
        assert_eq!(content["video"]["supports_streaming"], true);
        assert_eq!(
            content["video"]["cover"],
            json!({"@type": "inputFileRemote", "id": "https://x/c.jpg"})
        );
    }

    /// 相册：每个内容各自带 caption；**不带 `reply_to`**（不引用原消息）。
    #[test]
    fn album_params_carry_caption_without_reply() {
        let params = SendAlbumParams {
            medias: vec![
                Media::photo(InputFile::remote("https://x/1.jpg")).size(1, 2),
                Media::photo(InputFile::remote("https://x/2.jpg")).size(3, 4),
            ],
            caption: Some(Text::markdown("标题")),
            timeout_ms: 1000,
        };
        let payload = album_params(42, &params, &text_json);
        assert_eq!(payload["chat_id"], 42);
        assert!(
            payload.get("reply_to").is_none(),
            "不该引用原消息：{payload}"
        );
        let contents = payload["input_message_contents"].as_array().unwrap();
        assert_eq!(contents.len(), 2);
        assert_eq!(contents[0]["@type"], "inputMessagePhoto");
        assert_eq!(contents[0]["caption"]["text"], "标题");
        assert_eq!(contents[1]["photo"]["width"], 3);
    }

    /// 单条 `sendMessage`：文本与媒体二选一；带媒体时 text 变 caption。
    #[test]
    fn send_params_text_and_media_shapes() {
        let text = send_params(
            1,
            &SendMessageParams::text(Text::markdown("hi")),
            &text_json,
        );
        assert_eq!(text["input_message_content"]["@type"], "inputMessageText");
        assert_eq!(text["input_message_content"]["text"]["text"], "hi");

        let media = SendMessageParams {
            text: Some(Text::markdown("标题")),
            media: Some(Media::video(InputFile::id(9))),
            timeout_ms: 1000,
        };
        let payload = send_params(1, &media, &text_json);
        assert_eq!(
            payload["input_message_content"]["video"]["video"],
            json!({"@type": "inputFileId", "id": 9})
        );
        assert_eq!(payload["input_message_content"]["caption"]["text"], "标题");
    }

    // ── sent_media：字段路径照真 td_api.tl ──────────────────────

    #[test]
    fn video_message_yields_file_and_cover_ids() {
        let message = json!({
            "content": {
                "@type": "messageVideo",
                "video": {
                    "video": { "id": 111 },
                    "width": 1080, "height": 1920, "duration": 15,
                    "thumbnail": { "thumbnail": { "file": { "id": 222 } } },
                },
                "cover": { "sizes": [
                    { "photo": { "id": 333 }, "width": 100, "height": 100 },
                    { "photo": { "id": 444 }, "width": 400, "height": 400 },
                ]},
            },
        });
        let media = sent_media(&message).expect("视频消息应当取到 id");
        assert_eq!(media.kind, KIND_VIDEO);
        assert_eq!(media.file_id, Some(111));
        assert_eq!(media.cover_id, Some(444), "封面取最大一张");
        assert_eq!(
            (media.width, media.height, media.duration),
            (1080, 1920, 15)
        );
    }

    #[test]
    fn photo_message_takes_the_largest_size() {
        let message = json!({
            "content": {
                "@type": "messagePhoto",
                "photo": { "sizes": [
                    { "photo": { "id": 1 }, "width": 90, "height": 90 },
                    { "photo": { "id": 2 }, "width": 800, "height": 600 },
                ]},
            },
        });
        let media = sent_media(&message).unwrap();
        assert_eq!(media.kind, KIND_PHOTO);
        assert_eq!(media.file_id, Some(2));
        assert_eq!((media.width, media.height), (800, 600));
    }

    #[test]
    fn audio_message_reads_album_cover() {
        let message = json!({
            "content": {
                "@type": "messageAudio",
                "audio": {
                    "audio": { "id": 10 }, "duration": 30,
                    "album_cover_thumbnail": {
                        "width": 320, "height": 320,
                        "thumbnail": { "file": { "id": 11 } },
                    },
                },
            },
        });
        let media = sent_media(&message).unwrap();
        assert_eq!(media.kind, KIND_AUDIO);
        assert_eq!(media.file_id, Some(10));
        assert_eq!(media.cover_id, Some(11));
        assert_eq!(media.duration, 30);
    }

    /// 0 是「空文件对象」的占位，不能当成一个可复用的 id。
    #[test]
    fn zero_file_ids_mean_none() {
        let message = json!({
            "content": { "@type": "messageVideo", "video": { "video": { "id": 0 } } },
        });
        let media = sent_media(&message).unwrap();
        assert_eq!(media.file_id, None);
    }

    /// 非媒体消息 / 形状不对的 JSON 一律 `None`，不 panic。
    #[test]
    fn non_media_messages_yield_nothing() {
        assert!(sent_media(&json!({"content": {"@type": "messageText"}})).is_none());
        assert!(sent_media(&json!({})).is_none());
        assert!(sent_media_list(&json!({})).is_empty());
    }

    /// 相册结果是消息**数组**；单条结果也接受。
    #[test]
    fn sent_media_list_accepts_album_arrays_and_single_messages() {
        let album = json!([
            { "content": { "@type": "messagePhoto", "photo": { "sizes": [
                { "photo": { "id": 7 }, "width": 10, "height": 10 }]}}},
            { "content": { "@type": "messagePhoto", "photo": { "sizes": [
                { "photo": { "id": 8 }, "width": 10, "height": 10 }]}}},
        ]);
        let ids: Vec<i64> = sent_media_list(&album)
            .iter()
            .filter_map(|media| media.file_id)
            .collect();
        assert_eq!(ids, vec![7, 8]);

        let single = json!({ "content": { "@type": "messagePhoto", "photo": { "sizes": [
            { "photo": { "id": 9 }, "width": 10, "height": 10 }]}}});
        assert_eq!(sent_media_list(&single).len(), 1);
    }

    // ── 命令解析 ────────────────────────────────────────────────

    fn update_with(text: &str) -> Value {
        json!({
            "@type": "updateNewMessage",
            "message": {
                "@type": "message",
                "id": 22,
                "chat_id": 42,
                "is_outgoing": false,
                "content": {
                    "@type": "messageText",
                    "text": { "@type": "formattedText", "text": text, "entities": [] },
                },
            },
        })
    }

    #[test]
    fn commands_are_parsed_with_name_and_args() {
        let command = command_of(&update_with("/dy https://v.douyin.com/abc/")).unwrap();
        assert_eq!(command.name, "dy");
        assert_eq!(command.args, "https://v.douyin.com/abc/");
        assert_eq!(command.addressed_to, None);
        assert!(command.is("DY"), "命令名不分大小写");
    }

    #[test]
    fn addressed_commands_are_only_for_the_named_bot() {
        let command = command_of(&update_with("/dy@SomeBot x")).unwrap();
        assert_eq!(command.addressed_to.as_deref(), Some("SomeBot"));
        assert!(command.is_for(&["somebot".to_owned()]));
        assert!(!command.is_for(&["otherbot".to_owned()]));
        // 身份未知（空表）时不过滤：宁可多回一句也别吞命令。
        assert!(command.is_for(&[]));
    }

    #[test]
    fn plain_messages_and_outgoing_messages_are_not_commands() {
        assert!(command_of(&update_with("https://v.douyin.com/abc/")).is_none());
        let mut outgoing = update_with("/dy x");
        outgoing["message"]["is_outgoing"] = json!(true);
        assert!(command_of(&outgoing).is_none());

        // 频道帖例外：自己的频道里发的 `/dy` 是命令，解析后要编辑这条原帖。
        let mut outgoing_post = update_with("/dy x");
        outgoing_post["message"]["chat_id"] = json!(-100777_i64);
        outgoing_post["message"]["is_outgoing"] = json!(true);
        outgoing_post["message"]["is_channel_post"] = json!(true);
        outgoing_post["message"]["sender_id"] =
            json!({ "@type": "messageSenderChat", "chat_id": -100777_i64 });
        assert!(
            command_of(&outgoing_post).is_some_and(|command| command.is_channel_post),
            "自家频道的 /dy 不能因为 is_outgoing 被吞掉"
        );
    }

    /// 频道判定只看 TDLib 的 `is_channel_post`：群里匿名频道马甲（发送者也是
    /// `messageSenderChat`）不能被当成频道帖，否则 /dy 会去编辑不属于自己的群消息。
    #[test]
    fn only_channel_posts_use_the_channel_identity() {
        let mut update = update_with("/dy https://v.douyin.com/abc/");
        update["message"]["chat_id"] = json!(-100777_i64);
        update["message"]["is_channel_post"] = json!(true);
        update["message"]["sender_id"] =
            json!({ "@type": "messageSenderChat", "chat_id": -100777_i64 });
        assert!(
            command_of(&update).unwrap().is_channel_post,
            "频道自己发的帖子要认出来"
        );

        update["message"]["chat_id"] = json!(-100888_i64);
        update["message"]["is_channel_post"] = json!(false);
        update["message"]["sender_id"] =
            json!({ "@type": "messageSenderChat", "chat_id": -100999_i64 });
        assert!(
            !command_of(&update).unwrap().is_channel_post,
            "群里的匿名频道马甲不是频道帖"
        );

        update["message"]["sender_id"] = json!({ "@type": "messageSenderUser", "user_id": 1 });
        assert!(!command_of(&update).unwrap().is_channel_post);
    }
}
