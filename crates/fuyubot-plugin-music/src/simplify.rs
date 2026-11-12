//! 本地化的 simplify 层（上游 `fuyubot-simplify` 的裁剪版）。
//!
//! 为什么放在这里：上游 `fuyubot-simplify` 是 `publish = false`，外部插件拿不到
//! crates.io 包。ABI v5 已经把 simplify 语义下沉成宿主的 op 表（`simplify_invoke` /
//! `simplify_invoke_result`），所以本模块只保留两样东西：
//!
//! 1. 「人话参数 → TDLib 请求体」的构造器：`Text` / `Media` / `SendMessageParams`
//!    / `send_params` / 命令解析；
//! 2. 从**已发送消息**里取媒体 `file.id`（缓存复用那一半）。
//!
//! 发送本身不在这里：插件走 `ResponseRouter::invoke_op(op_json)` +
//! `tokio::time::timeout`（见 [`crate::runtime`]），不 import `fuyubot-simplify`。
//!
//! 代码与上游 `fuyubot-simplify`（MIT OR Apache-2.0）保持一致，只裁掉本插件没用到的
//! 变体（文档 / 动图 / 贴纸 / 位置 / 富消息 / 反应 / 连接状态 / 编辑 / 相册 / 键盘）。

// 这是兼容层：保留上游的字段形状，允许暂时没有调用方的构造器存在。
#![allow(dead_code)]

use std::ffi::{CStr, c_char, c_void};

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

/// 纯文本 → `formattedText`（不解析，不花往返）。
pub fn plain_text(text: &Text) -> Value {
    json!({ "@type": "formattedText", "text": text.text, "entities": [] })
}

/// TDLib 是否真的把文本解析成了 `formattedText`。
pub fn is_formatted_text(response: Option<&Value>) -> bool {
    response
        .and_then(|value| value.get("@type"))
        .and_then(Value::as_str)
        == Some("formattedText")
}

// ── 媒体 ──────────────────────────────────────────────────────────

/// 要发的文件（对方的 `toTdInputFile`）。
///
/// | 变体 | TDLib | 用途 |
/// | --- | --- | --- |
/// | [`Local`](Self::Local) | `inputFileLocal` | 本地路径（插件自己下载 / 解密出来的文件） |
/// | [`Remote`](Self::Remote) | `inputFileRemote` | 远程 URL（TDLib 自己去拉） |
/// | [`Id`](Self::Id) | `inputFileId` | **已存在于 TDLib 的文件 id**：不再上传、不再下载，直接复用 |
///
/// ⚠️ `Remote` 的 `id` 是 URL；`Id` 的是 TDLib 的 `file.id`（int32）。名字很像，
/// 别混——缓存复用（下次直接发已上传过的文件）走 [`Id`](Self::Id)。
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

    fn to_json(&self) -> Value {
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
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Thumbnail {
    pub file: InputFile,
    pub width: i32,
    pub height: i32,
}

impl Thumbnail {
    /// 一张缩略图；尺寸交给 TDLib 自己判断（填 0）。
    pub fn new(file: impl Into<InputFile>) -> Self {
        Self {
            file: file.into(),
            width: 0,
            height: 0,
        }
    }

    fn to_json(&self) -> Value {
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

/// 媒体内容（TDLib 的 `inputMessageAudio` / `inputMessageVideo`）。
///
/// 尺寸/时长这些填 `0` 也没关系（TDLib 会自己读真实值），要精确控制再填。
/// `thumbnail` 同理：不给就不发这个字段（TDLib 自己抽，或者干脆没有）。
#[derive(Debug, Clone, PartialEq)]
pub enum Media {
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
    Audio {
        file: InputFile,
        caption: Option<Text>,
        /// 专辑封面（真 `inputAudio` 里叫 `album_cover_thumbnail`）。
        album_cover_thumbnail: Option<Thumbnail>,
        duration: i32,
        title: String,
        performer: String,
    },
}

impl Media {
    /// 一段视频（尺寸/时长交给 TDLib 自己判断）。
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

    /// 挂一张缩略图 / 封面。
    pub fn thumbnail(self, thumbnail: impl Into<Thumbnail>) -> Self {
        let thumbnail = Some(thumbnail.into());
        match self {
            Self::Video {
                file,
                caption,
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
            Self::Audio {
                file,
                caption,
                duration,
                title,
                performer,
                ..
            } => Self::Audio {
                file,
                caption,
                album_cover_thumbnail: thumbnail,
                duration,
                title,
                performer,
            },
        }
    }

    fn caption_ref(&self) -> Option<&Text> {
        match self {
            Self::Video { caption, .. } | Self::Audio { caption, .. } => caption.as_ref(),
        }
    }

    fn with_caption(self, text: Text) -> Self {
        let caption = Some(text);
        match self {
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
            Self::Audio {
                file,
                album_cover_thumbnail,
                duration,
                title,
                performer,
                ..
            } => Self::Audio {
                file,
                caption,
                album_cover_thumbnail,
                duration,
                title,
                performer,
            },
        }
    }
}

/// 媒体 → `InputMessageContent`。
///
/// 形状照真 `td_api.tl`：外层内容 → 中层 `inputX` → 内层 `InputFile`；
/// **caption 在内容类型里**，不是 `sendMessage` 的顶层参数。
pub fn content_json(media: &Media, render: &dyn Fn(&Text) -> Value) -> Value {
    let caption = media.caption_ref().map(render);
    match media {
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
        Media::Audio {
            file,
            album_cover_thumbnail,
            duration,
            title,
            performer,
            ..
        } => {
            let mut inner = Map::new();
            inner.insert("@type".to_owned(), json!("inputAudio"));
            inner.insert("audio".to_owned(), file.to_json());
            put_some(
                &mut inner,
                "album_cover_thumbnail",
                album_cover_thumbnail.as_ref().map(Thumbnail::to_json),
            );
            inner.insert("duration".to_owned(), json!(duration));
            inner.insert("title".to_owned(), json!(title));
            inner.insert("performer".to_owned(), json!(performer));

            let mut object = Map::new();
            object.insert("@type".to_owned(), json!("inputMessageAudio"));
            object.insert("audio".to_owned(), Value::Object(inner));
            if let Some(caption) = caption {
                object.insert("caption".to_owned(), caption);
            }
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
/// 刻意**没有** `reply_to`：本插件不引用 / 回复原消息（用户要求）。
#[derive(Debug, Clone)]
pub struct SendMessageParams {
    pub text: Option<Text>,
    pub media: Option<Media>,
    pub timeout_ms: u64,
}

impl SendMessageParams {
    pub fn text(text: impl Into<Text>) -> Self {
        Self {
            text: Some(text.into()),
            ..Self::default()
        }
    }

    pub fn media(media: impl Into<Media>) -> Self {
        Self {
            media: Some(media.into()),
            ..Self::default()
        }
    }
}

impl Default for SendMessageParams {
    fn default() -> Self {
        Self {
            text: None,
            media: None,
            timeout_ms: DEFAULT_TIMEOUT_MS,
        }
    }
}

/// 组装 `sendMessage` 的参数（`@type` 由宿主补）。
///
/// 参数名以真 `td_api.tl` 为准（`sendMessage chat_id options reply_markup
/// input_message_content`）；**没有顶层 `caption`**，它在内容类型里（见
/// [`content_json`]）。**不发 `reply_to`**：本插件不引用原消息。
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

fn media_with_caption(media: &Media, text: Option<&Text>) -> Media {
    match text {
        Some(text) => media.clone().with_caption(text.clone()),
        None => media.clone(),
    }
}

// ── 从已发送消息里取文件 id（缓存复用）────────────────────────────

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
/// 形状全部按**真** `td_api.tl`（不是 Bot API）：
///
/// ```text
/// messageAudio      content.audio.audio.file.id
///                   content.audio.album_cover_thumbnail.thumbnail.file.id
/// messageVideo      content.video.video.file.id
///                   content.video.cover.sizes[].photo.file.id（优先）
///                   content.video.thumbnail.thumbnail.file.id（退路）
/// messagePhoto      content.photo.sizes[].photo.file.id（取最大一张）
/// messageDocument   content.document.document.file.id
/// messageAnimation  content.animation.animation.file.id
/// ```
///
/// 不是媒体消息（文本、贴纸……）返回 `None`；**不 panic**：上游 JSON 形状不对
/// 就当作没有。`file.id` 在 TDLib 里是 int32，这里按 `i64` 收（SQLite / JSON 更顺手），
/// 0 视作「没有」。
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
                width: number(cover, "width"),
                height: number(cover, "height"),
                duration: number(Some(audio), "duration"),
            })
        }
        "messageVideo" => {
            let video = content.get("video")?;
            // 封面优先用 `cover`（发的时候 inputVideo.cover 传的那张原图），
            // 没有就退回 `thumbnail`（TDLib 抽的帧）。
            let cover = content
                .get("cover")
                .and_then(best_photo_file_id)
                .or_else(|| {
                    content
                        .get("video")
                        .and_then(|video| video.get("thumbnail"))
                        .and_then(thumbnail_file_id)
                });
            Some(SentMedia {
                kind: KIND_VIDEO,
                file_id: file_id(video.get("video")),
                cover_id: cover,
                width: number(Some(video), "width"),
                height: number(Some(video), "height"),
                duration: number(Some(video), "duration"),
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
                width: number(Some(animation), "width"),
                height: number(Some(animation), "height"),
                duration: number(Some(animation), "duration"),
            })
        }
        _ => Some(SentMedia {
            kind: KIND_OTHER,
            ..SentMedia::default()
        }),
    }
}

/// 相册（`sendMessageAlbum` 的 `result` 是 `[message, …]`）→ 逐条 [`sent_media`]。
pub fn sent_media_list(messages: &Value) -> Vec<SentMedia> {
    messages
        .as_array()
        .map(|items| items.iter().filter_map(sent_media).collect())
        .unwrap_or_default()
}

/// 一个 `file` 对象（`{"@type":"file","id":123,…}`）的 id；0 / 非数字 → `None`。
fn file_id(value: Option<&Value>) -> Option<i64> {
    let id = value?.get("id")?.as_i64()?;
    (id != 0).then_some(id)
}

/// 一个 `thumbnail` 对象（`{"format":…,"width":…,"height":…,"file":{…}}`）的文件 id。
fn thumbnail_file_id(thumbnail: &Value) -> Option<i64> {
    file_id(thumbnail.get("file"))
}

/// `photo` 对象里最大的一张：`sizes` 里按面积挑，面积相同取宽的那张。
fn best_photo_size(photo: &Value) -> Option<(i64, i64, i64)> {
    let sizes = photo.get("sizes")?.as_array()?;
    sizes
        .iter()
        .filter_map(|size| {
            let id = file_id(size.get("photo"))?;
            let width = number(Some(size), "width");
            let height = number(Some(size), "height");
            Some((id, width, height))
        })
        .max_by_key(|(_, width, height)| (width.saturating_mul(*height), *width))
}

/// `photo` 对象里最大那张的文件 id。
fn best_photo_file_id(photo: &Value) -> Option<i64> {
    best_photo_size(photo).map(|size| size.0)
}

/// 取一个整数字段，缺省 / 非数字 → 0。
fn number(value: Option<&Value>, key: &str) -> i64 {
    value
        .and_then(|value| value.get(key))
        .and_then(Value::as_i64)
        .unwrap_or(0)
}

// ── 命令（插件最常见的入口）──────────────────────────────────────

/// 宿主「我是谁」那条 service 的名字（由 `fuyubot-loader` 提供）。
///
/// 方法 `usernames`（无参）返回自身全部活跃用户名的 JSON 数组，比如 `["FuyuBot"]`；
/// 还没登录 / 没接 TDLib 时返回 `[]`（= 还不知道自己是谁）。[`on_command`] 靠它
/// 判断「这条命令点名的是不是我」。
pub const IDENTITY_SERVICE: &str = "fuyubot.host.identity";

/// 一条 `updateNewMessage` 里的命令：`/start`、`/help@SomeBot 参数`。
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
}

impl Command {
    /// 是这条命令吗（不分大小写，`/Start` 也算）。
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

/// 从 `updateNewMessage` 的负载里认出一条命令；不是命令就 `None`。
///
/// 字段路径照真 `td_api.tl`：`updateNewMessage.message` →
/// `content:messageText` → `text:formattedText` → `text:string`。
pub fn command_of(update: &Value) -> Option<Command> {
    let message = update.get("message")?;
    if message
        .get("is_outgoing")
        .and_then(Value::as_bool)
        .unwrap_or(false)
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
    })
}

/// 一个活着的「命令订阅」：**drop 就是退订**。
///
/// 回调闭包由它自己持有——递给 ABI 的 `user_data` 指的就是这里那个 Box，
/// 退订之后一起释放。
pub struct CommandSubscription {
    host: HostApi,
    handle: SubscriptionHandle,
    /// 只负责**持有**回调与身份缓存；退订之前它的地址必须稳定。
    _handler: Box<CommandHandler>,
}

impl Drop for CommandSubscription {
    fn drop(&mut self) {
        if self.handle.is_valid() {
            self.host.off(self.handle);
        }
    }
}

/// `on_command` 递给 C 回调的那份状态：插件自己的处理函数。
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

/// 问宿主「我是谁」：返回自身全部活跃用户名；拿不到（老宿主 / 还没登录 / 没接 TDLib）
/// 返回空表。
pub fn my_usernames(host: &HostApi) -> Vec<String> {
    let Ok(reply) = host.call_service(IDENTITY_SERVICE, "usernames", "{}") else {
        return Vec::new();
    };
    serde_json::from_str(&reply).unwrap_or_default()
}

/// 订阅 `updateNewMessage`，把**该由本插件响应的**命令交给 `handler`。
///
/// 不打扰它的：非命令、自己发的、以及**点名给别人的**（`/start@别的Bot`）。
/// 交给它的：裸命令（`/start`，Telegram 的约定是所有 bot 都响应）与点名给自己的。
///
/// 「自己是谁」来自宿主 [`IDENTITY_SERVICE`]：遇到点名命令时现问一次（纯内存读，
/// 不碰 TDLib）。还没登录 / 没接 TDLib（`--no-tdlib` / `--replay`）时问不到，
/// 这时**不过滤**——宁可多回一句也不吞命令。
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
extern "C-unwind" fn command_trampoline(user_data: *mut c_void, payload: *const c_char) -> i32 {
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

    #[test]
    fn cached_files_serialise_as_input_file_id() {
        let media = Media::Audio {
            file: InputFile::id(1234),
            caption: None,
            album_cover_thumbnail: Some(Thumbnail::new(InputFile::id(5678))),
            duration: 269,
            title: "晴天".to_owned(),
            performer: "周杰伦".to_owned(),
        };
        let params = SendMessageParams {
            text: Some(Text::plain("x")),
            media: Some(media),
            timeout_ms: DEFAULT_TIMEOUT_MS,
        };
        let payload = send_params(1, &params, &|text: &Text| plain_text(text));
        assert_eq!(
            payload.pointer("/input_message_content/@type"),
            Some(&json!("inputMessageAudio"))
        );
        assert_eq!(
            payload.pointer("/input_message_content/audio/@type"),
            Some(&json!("inputAudio"))
        );
        assert_eq!(
            payload.pointer("/input_message_content/audio/audio/@type"),
            Some(&json!("inputFileId"))
        );
        assert_eq!(
            payload.pointer("/input_message_content/audio/audio/id"),
            Some(&json!(1234))
        );
        assert_eq!(
            payload.pointer("/input_message_content/audio/album_cover_thumbnail/thumbnail/@type"),
            Some(&json!("inputFileId"))
        );
        assert_eq!(
            payload.pointer("/input_message_content/audio/album_cover_thumbnail/thumbnail/id"),
            Some(&json!(5678))
        );
        // 不引用原消息：`sendMessage` 参数里不该有 `reply_to`。
        assert!(
            payload.get("reply_to").is_none(),
            "本插件不引用原消息：{payload}"
        );
    }

    #[test]
    fn video_files_serialise_as_input_file_id_too() {
        let media = Media::Video {
            file: InputFile::id(11),
            caption: None,
            thumbnail: Some(Thumbnail::new(InputFile::id(12))),
            cover: None,
            start_timestamp: None,
            duration: 33,
            width: 1920,
            height: 1080,
            supports_streaming: true,
            has_spoiler: false,
        };
        let params = SendMessageParams {
            text: Some(Text::plain("t")),
            media: Some(media),
            timeout_ms: DEFAULT_TIMEOUT_MS,
        };
        let payload = send_params(1, &params, &|text: &Text| plain_text(text));
        assert_eq!(
            payload.pointer("/input_message_content/video/video/@type"),
            Some(&json!("inputFileId"))
        );
        assert_eq!(
            payload.pointer("/input_message_content/video/video/id"),
            Some(&json!(11))
        );
        assert_eq!(
            payload.pointer("/input_message_content/video/supports_streaming"),
            Some(&json!(true))
        );
    }

    #[test]
    fn sent_media_reads_audio_and_its_cover() {
        let message = json!({
            "content": {
                "@type": "messageAudio",
                "audio": {
                    "duration": 269,
                    "audio": { "@type": "file", "id": 42 },
                    "album_cover_thumbnail": {
                        "width": 320,
                        "height": 320,
                        "file": { "@type": "file", "id": 43 }
                    }
                }
            }
        });
        let media = sent_media(&message).expect("音频消息应当取得到");
        assert_eq!(media.kind, KIND_AUDIO);
        assert_eq!(media.file_id, Some(42));
        assert_eq!(media.cover_id, Some(43));
        assert_eq!(media.duration, 269);
    }

    #[test]
    fn sent_media_reads_video_cover_from_the_largest_photo() {
        let message = json!({
            "content": {
                "@type": "messageVideo",
                "video": {
                    "duration": 33,
                    "width": 1920,
                    "height": 1080,
                    "video": { "@type": "file", "id": 7 },
                    "thumbnail": { "file": { "@type": "file", "id": 8 } }
                },
                "cover": {
                    "sizes": [
                        { "width": 100, "height": 100, "photo": { "@type": "file", "id": 100 } },
                        { "width": 200, "height": 200, "photo": { "@type": "file", "id": 200 } }
                    ]
                }
            }
        });
        let media = sent_media(&message).expect("视频消息应当取得到");
        assert_eq!(media.kind, KIND_VIDEO);
        assert_eq!(media.file_id, Some(7));
        assert_eq!(media.cover_id, Some(200), "取最大的一张");
        assert_eq!(media.width, 1920);
        assert_eq!(media.height, 1080);
    }

    #[test]
    fn zero_file_ids_are_treated_as_absent() {
        let message = json!({
            "content": {
                "@type": "messageAudio",
                "audio": { "audio": { "@type": "file", "id": 0 } }
            }
        });
        let media = sent_media(&message).expect("还是媒体消息");
        assert_eq!(media.file_id, None);
        assert!(sent_media(&json!({})).is_none());
        // 非媒体内容（文本 / 贴纸）是 `other`：没有文件 id，调用方按「不能缓存」处理。
        let text = sent_media(&json!({"content": {"@type": "messageText"}})).expect("other");
        assert_eq!(text.kind, KIND_OTHER);
        assert_eq!(text.file_id, None);
    }

    #[test]
    fn commands_are_parsed_from_real_update_shapes() {
        let update = json!({
            "message": {
                "id": 7,
                "chat_id": -100123,
                "is_outgoing": false,
                "content": {
                    "@type": "messageText",
                    "text": { "text": "/music163@SomeBot 186016 lossless" }
                }
            }
        });
        let command = command_of(&update).expect("应当认得命令");
        assert_eq!(command.name, "music163");
        assert_eq!(command.args, "186016 lossless");
        assert_eq!(command.addressed_to.as_deref(), Some("SomeBot"));
        assert!(command.is_for(&["somebot".to_owned()]));
        assert!(!command.is_for(&["other".to_owned()]));
        assert!(command.is("MUSIC163"));

        let plain = json!({
            "message": {
                "id": 8,
                "chat_id": 1,
                "content": { "@type": "messageText", "text": { "text": "/soda" } }
            }
        });
        let command = command_of(&plain).expect("裸命令");
        assert!(command.args.is_empty());
        assert!(command.addressed_to.is_none());
        assert!(command.is_for(&[]), "不知道自己是谁时不过滤");

        // 自己发的 / 非文本都不算命令。
        let outgoing = json!({
            "message": {
                "id": 9,
                "chat_id": 1,
                "is_outgoing": true,
                "content": { "@type": "messageText", "text": { "text": "/soda" } }
            }
        });
        assert!(command_of(&outgoing).is_none());
        assert!(
            command_of(&json!({"message": {"id": 1, "content": {"@type": "messagePhoto"}}}))
                .is_none()
        );
    }
}
