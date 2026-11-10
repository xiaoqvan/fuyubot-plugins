//! 本地化的 simplify 层（上游 `fuyubot-simplify` 的裁剪版）。
//!
//! 为什么放在这里：上游 `fuyubot-simplify` 是 `publish = false`，外部插件
//! 拿不到 crates.io 包。ABI v5 已经把 simplify 语义下沉成宿主的 op 表
//! （`simplify_invoke` / `simplify_invoke_result`，实现在宿主的
//! `fuyubot-simplify::ops`；v6 沿用这张表，只把**服务调用**改走宿主），所以本模块只保留两样东西：
//!
//! 1. 「人话参数 → TDLib 请求体」的构造器：`Text` / `Media` / `SendMessageParams`
//!    / `send_params` / `edit_*_params` / 按钮 / 命令解析；
//! 2. [`Simplify`] 发送门面：op 下发 + 按 `request_id` 接回 `simplifyResult`。
//!
//! 代码与上游 `fuyubot-simplify`（MIT OR Apache-2.0）保持一致，只裁掉本插件
//! 没用到的变体（音频 / 文档 / 动图 / 贴纸 / 位置 / 富消息 / 反应 / 连接状态）。

// 这是兼容层：保留上游的完整字段形状，允许暂时没有调用方的构造器存在。
#![allow(dead_code)]

use serde_json::{Map, Value, json};

/// 等发送结局的默认预算（与上游 `Simplify` 一致）。
pub const DEFAULT_TIMEOUT_MS: u64 = 30_000;

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

/// 消息落在哪个话题：TDLib 的 `MessageTopic`。
///
/// 本项目只用到论坛话题；保留显式类型是为了不在调用点传裸 `i32`，
/// 避免把 `forum_topic_id` 和 `message_thread_id` 混掉。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Topic {
    /// 论坛群组 / 与 bot 的私聊里的话题（`messageTopicForum`）。
    Forum(i32),
}

impl Topic {
    pub fn forum(forum_topic_id: i32) -> Self {
        Self::Forum(forum_topic_id)
    }

    pub fn to_json(&self) -> Value {
        match self {
            Self::Forum(id) => {
                json!({ "@type": "messageTopicForum", "forum_topic_id": id })
            }
        }
    }
}

/// 要发的文件：本地路径或已上传的远程 id。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputFile {
    Local { path: String },
    Remote { id: String },
}

impl InputFile {
    pub fn local(path: impl Into<String>) -> Self {
        Self::Local { path: path.into() }
    }

    pub fn remote(id: impl Into<String>) -> Self {
        Self::Remote { id: id.into() }
    }

    fn to_json(&self) -> Value {
        match self {
            Self::Local { path } => json!({ "@type": "inputFileLocal", "path": path }),
            Self::Remote { id } => json!({ "@type": "inputFileRemote", "id": id }),
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
    fn to_json(&self) -> Value {
        json!({
            "@type": "inputThumbnail",
            "thumbnail": self.file.to_json(),
            "width": self.width,
            "height": self.height,
        })
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

    /// 剧透遮罩（图 / 视频）。
    pub fn spoiler(self, has_spoiler: bool) -> Self {
        match self {
            Self::Photo {
                file,
                caption,
                thumbnail,
                width,
                height,
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
                width,
                height,
                supports_streaming,
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

    fn caption_ref(&self) -> Option<&Text> {
        match self {
            Self::Photo { caption, .. } | Self::Video { caption, .. } => caption.as_ref(),
        }
    }
}

/// 媒体 → `InputMessageContent`。
///
/// 形状照真 `td_api.tl`：`inputMessagePhoto.photo` 是 `inputPhoto`，
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

// ── 内联键盘 ──────────────────────────────────────────────────────

/// 按钮样式（TDLib `ButtonStyle`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ButtonStyle {
    #[default]
    Default,
    Primary,
    Danger,
    Success,
}

impl ButtonStyle {
    fn to_json(self) -> Value {
        let name = match self {
            Self::Default => "buttonStyleDefault",
            Self::Primary => "buttonStylePrimary",
            Self::Danger => "buttonStyleDanger",
            Self::Success => "buttonStyleSuccess",
        };
        json!({ "@type": name })
    }
}

/// 按钮行为。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ButtonAction {
    /// 回一条 callback query（`data` 会 base64 编码）。
    Callback { data: String },
    /// 打开链接。
    Url { url: String },
}

/// 一个内联按钮。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplyButton {
    pub text: String,
    pub action: ButtonAction,
    pub emoji_id: Option<i64>,
    pub style: ButtonStyle,
}

impl ReplyButton {
    pub fn callback(text: impl Into<String>, data: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            action: ButtonAction::Callback { data: data.into() },
            emoji_id: None,
            style: ButtonStyle::Default,
        }
    }

    pub fn url(text: impl Into<String>, url: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            action: ButtonAction::Url { url: url.into() },
            emoji_id: None,
            style: ButtonStyle::Default,
        }
    }

    /// 换按钮样式（构造后调整，链式调用）。
    pub fn style(mut self, style: ButtonStyle) -> Self {
        self.style = style;
        self
    }

    /// 按钮左侧的自定义 emoji（`icon_custom_emoji_id`），链式调用。
    pub fn emoji(mut self, custom_emoji_id: i64) -> Self {
        self.emoji_id = Some(custom_emoji_id);
        self
    }

    fn to_json(&self) -> Value {
        let kind = match &self.action {
            // 真 `td_api.tl`：`inlineKeyboardButtonTypeCallback data:bytes`（JSON 里是 base64）。
            ButtonAction::Callback { data } => {
                json!({ "@type": "inlineKeyboardButtonTypeCallback", "data": base64(data) })
            }
            ButtonAction::Url { url } => {
                json!({ "@type": "inlineKeyboardButtonTypeUrl", "url": url })
            }
        };
        json!({
            "@type": "inlineKeyboardButton",
            "text": &self.text,
            "icon_custom_emoji_id": self.emoji_id.unwrap_or(0),
            "style": self.style.to_json(),
            "type": kind,
        })
    }
}

/// 二维按钮 → `replyMarkupInlineKeyboard`；全空就是「不要键盘」。
pub fn reply_markup(rows: &[Vec<ReplyButton>]) -> Option<Value> {
    if rows.iter().all(|row| row.is_empty()) {
        return None;
    }
    Some(json!({
        "@type": "replyMarkupInlineKeyboard",
        "rows": rows
            .iter()
            .map(|row| Value::Array(row.iter().map(ReplyButton::to_json).collect()))
            .collect::<Vec<_>>(),
    }))
}

/// TDLib 的 callback data 是 base64 编码的字节串。
fn base64(data: &str) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let bytes = data.as_bytes();
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        out.push(ALPHABET[(n >> 18) as usize & 63] as char);
        out.push(ALPHABET[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            ALPHABET[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            ALPHABET[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

// ── 参数结构体 ────────────────────────────────────────────────────

/// `sendMessage` 的参数。`text` 与 `media` 二选一；都填就是「媒体 + 标题」。
#[derive(Debug, Clone)]
pub struct SendMessageParams {
    pub text: Option<Text>,
    pub media: Option<Media>,
    pub reply_to_message_id: Option<i64>,
    pub topic_id: Option<Topic>,
    pub reply_markup: Vec<Vec<ReplyButton>>,
    pub link_preview: Option<bool>,
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
            reply_to_message_id: None,
            topic_id: None,
            reply_markup: Vec::new(),
            link_preview: None,
            timeout_ms: DEFAULT_TIMEOUT_MS,
        }
    }
}

/// `sendMessageAlbum` 的参数（相册没有 `reply_markup`）。
#[derive(Debug, Clone)]
pub struct SendAlbumParams {
    pub medias: Vec<Media>,
    pub caption: Option<Text>,
    pub reply_to_message_id: Option<i64>,
    pub topic_id: Option<Topic>,
    pub timeout_ms: u64,
}

impl Default for SendAlbumParams {
    fn default() -> Self {
        Self {
            medias: Vec::new(),
            caption: None,
            reply_to_message_id: None,
            topic_id: None,
            timeout_ms: DEFAULT_TIMEOUT_MS,
        }
    }
}

/// `editMessageText` 的参数（`text` 为 `None` 时只改键盘）。
#[derive(Debug, Clone)]
pub struct EditTextParams {
    pub text: Option<Text>,
    pub link_preview: Option<bool>,
    pub reply_markup: Vec<Vec<ReplyButton>>,
    pub timeout_ms: u64,
}

impl Default for EditTextParams {
    fn default() -> Self {
        Self {
            text: None,
            link_preview: None,
            reply_markup: Vec::new(),
            timeout_ms: DEFAULT_TIMEOUT_MS,
        }
    }
}

/// `editMessageCaption` 的参数。
#[derive(Debug, Clone)]
pub struct EditCaptionParams {
    pub text: Option<Text>,
    pub reply_markup: Vec<Vec<ReplyButton>>,
    pub timeout_ms: u64,
}

impl Default for EditCaptionParams {
    fn default() -> Self {
        Self {
            text: None,
            reply_markup: Vec::new(),
            timeout_ms: DEFAULT_TIMEOUT_MS,
        }
    }
}

/// `editMessageMedia` 的参数（`text` 是新媒体内容的标题）。
#[derive(Debug, Clone)]
pub struct EditMediaParams {
    pub text: Option<Text>,
    pub media: Option<Media>,
    pub reply_markup: Vec<Vec<ReplyButton>>,
    pub timeout_ms: u64,
}

impl Default for EditMediaParams {
    fn default() -> Self {
        Self {
            text: None,
            media: None,
            reply_markup: Vec::new(),
            timeout_ms: DEFAULT_TIMEOUT_MS,
        }
    }
}

// ── 请求组装 ──────────────────────────────────────────────────────

/// 组装 `sendMessage` 的参数（`@type` 由宿主补）。
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
    put_common(&mut payload, &params.topic_id, &params.reply_to_message_id);
    if let Some(markup) = reply_markup(&params.reply_markup) {
        payload.insert("reply_markup".to_owned(), markup);
    }
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
            if params.link_preview == Some(false) {
                object.insert(
                    "link_preview_options".to_owned(),
                    json!({ "@type": "linkPreviewOptions", "is_disabled": true }),
                );
            }
            Value::Object(object)
        }
    }
}

fn media_with_caption(media: &Media, text: Option<&Text>) -> Media {
    match text {
        Some(text) => media.clone().caption((*text).clone()),
        None => media.clone(),
    }
}

fn put_common(payload: &mut Map<String, Value>, topic_id: &Option<Topic>, reply_to: &Option<i64>) {
    if let Some(topic) = topic_id {
        payload.insert("topic_id".to_owned(), topic.to_json());
    }
    if let Some(message) = reply_to {
        payload.insert(
            "reply_to".to_owned(),
            json!({ "@type": "inputMessageReplyToMessage", "message_id": message }),
        );
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
                Some(caption) => media.clone().caption((*caption).clone()),
                None => media.clone(),
            };
            content_json(&media, render)
        })
        .collect();

    let mut payload = Map::new();
    payload.insert("chat_id".to_owned(), json!(chat_id));
    put_common(&mut payload, &params.topic_id, &params.reply_to_message_id);
    payload.insert("input_message_contents".to_owned(), Value::Array(contents));
    Value::Object(payload)
}

fn edit_params(chat_id: i64, message_id: i64, rows: &[Vec<ReplyButton>]) -> Map<String, Value> {
    let mut payload = Map::new();
    payload.insert("chat_id".to_owned(), json!(chat_id));
    payload.insert("message_id".to_owned(), json!(message_id));
    if let Some(markup) = reply_markup(rows) {
        payload.insert("reply_markup".to_owned(), markup);
    }
    payload
}

/// `editMessageText` 的参数。
pub fn edit_text_params(
    chat_id: i64,
    message_id: i64,
    params: &EditTextParams,
    render: &dyn Fn(&Text) -> Value,
) -> Value {
    let mut payload = edit_params(chat_id, message_id, &params.reply_markup);
    if let Some(text) = &params.text {
        let mut content = Map::new();
        content.insert("@type".to_owned(), json!("inputMessageText"));
        content.insert("text".to_owned(), render(text));
        if params.link_preview == Some(false) {
            content.insert(
                "link_preview_options".to_owned(),
                json!({ "@type": "linkPreviewOptions", "is_disabled": true }),
            );
        }
        payload.insert("input_message_content".to_owned(), Value::Object(content));
    }
    Value::Object(payload)
}

/// `editMessageCaption` 的参数（caption 在顶层）。
pub fn edit_caption_params(
    chat_id: i64,
    message_id: i64,
    params: &EditCaptionParams,
    render: &dyn Fn(&Text) -> Value,
) -> Value {
    let mut payload = edit_params(chat_id, message_id, &params.reply_markup);
    if let Some(text) = &params.text {
        payload.insert("caption".to_owned(), render(text));
    }
    Value::Object(payload)
}

/// `editMessageMedia` 的参数。
pub fn edit_media_params(
    chat_id: i64,
    message_id: i64,
    params: &EditMediaParams,
    render: &dyn Fn(&Text) -> Value,
) -> Value {
    let mut payload = edit_params(chat_id, message_id, &params.reply_markup);
    if let Some(media) = &params.media {
        let media = media_with_caption(media, params.text.as_ref());
        payload.insert(
            "input_message_content".to_owned(),
            content_json(&media, render),
        );
    }
    Value::Object(payload)
}

// ── 命令解析 ──────────────────────────────────────────────────────

/// 一条 `updateNewMessage` 里的命令：`/start`、`/help@SomeBot 参数`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Command {
    pub chat_id: i64,
    pub message_id: i64,
    /// 命令名：去掉 `/` 与 `@BotName` 后缀，已转小写。
    pub name: String,
    /// `/cmd` 后面那一段（去首尾空白，可能为空）。
    pub args: String,
    /// `/help@SomeBot` 里点名的那个 bot；裸命令是 `None`。
    pub addressed_to: Option<String>,
    /// 整条消息文本。
    pub text: String,
}

impl Command {
    /// 是这条命令吗（不分大小写，`/Start` 也算）。
    pub fn is(&self, name: &str) -> bool {
        self.name.eq_ignore_ascii_case(name)
    }

    /// 这条命令点名的是不是 `username`（不分大小写）。裸命令不算点名。
    pub fn is_addressed_to(&self, username: &str) -> bool {
        self.addressed_to
            .as_deref()
            .is_some_and(|target| target.eq_ignore_ascii_case(username))
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

/// 宿主身份服务的名字（登录后由 `getMe` 填好）。
pub const IDENTITY_SERVICE: &str = "fuyubot.host.identity";

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

// ── 发送门面（ABI v5：op 下发 + response 通道接回结果） ────────────

use fuyubot_abi::{FILTER_EVENT_TYPE_KEY, HostApi, LogLevel, RESPONSE_EVENT, WaitHandle};

/// simplify 调用的统一错误。插件侧一般只需要 `format!("{error}")` 记日志。
#[derive(Debug)]
pub enum SimplifyError {
    /// `simplify_invoke` 自身调用宿主失败（拿不到请求号）。
    Host(String),
    /// op 跑完但 `ok:false`：`error.code` / `error.message`。
    Op { code: String, message: String },
    /// `wait_for_update` 超时（宿主侧 op 还没投回结果）。
    NoMatch,
    /// 结果 JSON 解析失败。
    BadJson(String),
}

impl std::fmt::Display for SimplifyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SimplifyError::Host(message) => write!(f, "宿主调用失败：{message}"),
            SimplifyError::Op { code, message } => write!(f, "{code}: {message}"),
            SimplifyError::NoMatch => write!(f, "等不到 simplify 结果（超时/未订阅）"),
            SimplifyError::BadJson(message) => write!(f, "结果 JSON 解析失败：{message}"),
        }
    }
}

impl std::error::Error for SimplifyError {}

/// simplify 门面：`Copy`，随手传。
#[derive(Debug, Clone, Copy)]
pub struct Simplify {
    host: HostApi,
}

impl Simplify {
    pub fn new(host: HostApi) -> Self {
        Self { host }
    }

    /// 业务文案 → `formattedText`（同步 op `parse_text_entities`）。
    ///
    /// Plain 直接退回纯文本 `formattedText`；其余走同步 op（宿主内部即
    /// `td_execute(parseTextEntities)`），拿不到就退回纯文本——不会因为一条
    /// Markdown 写错把整条消息发废。
    pub fn formatted(&self, text: &Text) -> Value {
        if text.parse_mode == ParseMode::Plain {
            return plain_text(text);
        }
        let mode = match text.parse_mode {
            ParseMode::Markdown => "markdown",
            ParseMode::MarkdownV2 => "markdown_v2",
            ParseMode::Html => "html",
            ParseMode::Plain => "plain",
        };
        let op = json!({
            "op": "parse_text_entities",
            "args": { "text": text.text.clone(), "parse_mode": mode },
        });
        let response = self.host.simplify_invoke_result(&op.to_string());
        if response.is_empty() {
            self.host.log(
                LogLevel::Warn,
                &format!(
                    "宿主没接 TDLib（或解析失败），Markdown 按纯文本发：{}",
                    text.text
                ),
            );
            return plain_text(text);
        }
        match serde_json::from_str::<Value>(&response) {
            Ok(payload) if payload.get("ok").and_then(Value::as_bool) == Some(true) => payload
                .get("result")
                .cloned()
                .unwrap_or_else(|| plain_text(text)),
            _ => plain_text(text),
        }
    }

    /// 发消息并等到发送结局：成功返回最终 `Message`，失败 / 超时返回错误。
    pub async fn send_message(
        &self,
        chat_id: i64,
        params: &SendMessageParams,
    ) -> Result<Value, SimplifyError> {
        let payload = send_params(chat_id, params, &|text: &Text| self.formatted(text));
        self.invoke_async("send_message", payload, params.timeout_ms)
            .await
    }

    /// 改一条消息的文本（可同时换键盘），等到改完的 `Message`。
    pub async fn edit_message_text(
        &self,
        chat_id: i64,
        message_id: i64,
        params: &EditTextParams,
    ) -> Result<Value, SimplifyError> {
        let payload = edit_text_params(chat_id, message_id, params, &|text: &Text| {
            self.formatted(text)
        });
        self.invoke_async("edit_message_text", payload, params.timeout_ms)
            .await
    }

    /// 删消息（`deleteMessages`），`revoke = true` 表示对所有人撤回。
    ///
    /// 成功是 `Ok`（TDLib 的 `ok`，没有内容），失败（权限不够、消息太老）进错误。
    pub async fn delete_message(
        &self,
        chat_id: i64,
        message_ids: impl IntoIterator<Item = i64>,
        revoke: bool,
        timeout_ms: u64,
    ) -> Result<(), SimplifyError> {
        let args = json!({
            "chat_id": chat_id,
            "message_ids": message_ids.into_iter().collect::<Vec<_>>(),
            "revoke": revoke,
        });
        self.invoke_async("delete_messages", args, timeout_ms)
            .await
            .map(|_| ())
    }

    // ── 内部：下发一条异步 op 并等回 `response` 通道的 `simplifyResult` ──────────

    /// `payload` 已经是该 TDLib 方法的请求体（构造器输出）；这里包成 op JSON 下发，
    /// 再按 `request_id` 在 `response` 通道等回
    /// `{"@type":"simplifyResult","ok","result"|"error"}`。
    ///
    /// 「发 + 等」这一段是宿主的**同步阻塞** API（`wait_for_update` 干等回调），
    /// 整体丢进 `spawn_blocking`：async 任务里不能占着 tokio worker 干等。
    async fn invoke_async(
        &self,
        op: &str,
        payload: Value,
        timeout_ms: u64,
    ) -> Result<Value, SimplifyError> {
        let mut args = match payload {
            Value::Object(map) => map,
            _ => {
                return Err(SimplifyError::BadJson(format!(
                    "op `{op}` 的请求体不是对象：{payload}"
                )));
            }
        };
        args.insert("timeout_ms".to_owned(), json!(timeout_ms));
        let op_json = json!({ "op": op, "args": Value::Object(args) }).to_string();

        let host = self.host;
        let op = op.to_owned();
        tokio::task::spawn_blocking(move || invoke_blocking(host, &op, &op_json, timeout_ms))
            .await
            .map_err(|join| SimplifyError::Host(format!("阻塞任务被打断：{join}")))?
    }
}

/// [`Simplify::invoke_async`] 的阻塞半截：下发 op + 等 `simplifyResult`。
/// 只在 `spawn_blocking` 线程里跑（会占住调用线程干等宿主回调）。
fn invoke_blocking(
    host: HostApi,
    op: &str,
    op_json: &str,
    timeout_ms: u64,
) -> Result<Value, SimplifyError> {
    let request = host
        .simplify_invoke(op_json)
        .map_err(|error| SimplifyError::Host(error.to_string()))?;
    if !request.is_valid() {
        return Err(SimplifyError::Host(format!(
            "simplify_invoke(`{op}`) 返回无效请求号"
        )));
    }

    // 比宿主侧 op 自己的超时再多等一点，确保能接住超时 / 错误结果。
    let wait_ms = timeout_ms.saturating_add(2_000);
    let filter =
        json!({ FILTER_EVENT_TYPE_KEY: RESPONSE_EVENT, "request_id": request.id }).to_string();
    let raw = wait_raw(host.wait_for_update(&filter, wait_ms)).ok_or(SimplifyError::NoMatch)?;
    let payload: Value =
        serde_json::from_str(&raw).map_err(|error| SimplifyError::BadJson(error.to_string()))?;
    match payload.get("ok").and_then(Value::as_bool) {
        Some(true) => Ok(payload.get("result").cloned().unwrap_or(Value::Null)),
        _ => {
            let code = payload
                .pointer("/error/code")
                .and_then(Value::as_str)
                .unwrap_or("unknown")
                .to_owned();
            let message = payload
                .pointer("/error/message")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned();
            Err(SimplifyError::Op { code, message })
        }
    }
}

/// 把 `WaitHandle` 里的响应 JSON 立刻拷出来（宿主缓冲只在下一次宿主调用前有效）。
fn wait_raw(handle: WaitHandle) -> Option<String> {
    if !handle.did_match() || handle.payload_json.is_null() {
        return None;
    }
    // SAFETY: 宿主保证 `payload_json` 是 NUL 结尾的合法 UTF-8，且在下一次宿主调用前有效；
    // 这里在读任何其它宿主调用之前就拷走，不会越界。
    let payload = unsafe { std::ffi::CStr::from_ptr(handle.payload_json) };
    Some(payload.to_string_lossy().into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_message_uses_input_message_text() {
        let params = SendMessageParams::text(Text::plain("你好"));
        let payload = send_params(42, &params, &|text| plain_text(text));
        assert_eq!(payload["chat_id"], 42);
        assert_eq!(
            payload["input_message_content"]["@type"],
            "inputMessageText"
        );
        assert_eq!(payload["input_message_content"]["text"]["text"], "你好");
    }

    #[test]
    fn video_content_nests_input_video() {
        let media = Media::video(InputFile::remote("file-1"))
            .duration(24)
            .size(1920, 1080)
            .streaming(true)
            .caption(Text::plain("第 1 集"));
        let params = SendMessageParams::media(media);
        let payload = send_params(7, &params, &|text| plain_text(text));
        let content = &payload["input_message_content"];
        assert_eq!(content["@type"], "inputMessageVideo");
        assert_eq!(content["video"]["@type"], "inputVideo");
        assert_eq!(content["video"]["video"]["@type"], "inputFileRemote");
        assert_eq!(content["video"]["duration"], 24);
        assert_eq!(content["video"]["supports_streaming"], true);
        assert_eq!(content["caption"]["text"], "第 1 集");
    }

    #[test]
    fn reply_markup_base64_encodes_callback_data() {
        let params = SendMessageParams {
            reply_markup: vec![vec![ReplyButton::callback("好", "ok")]],
            ..SendMessageParams::default()
        };
        let payload = send_params(1, &params, &|text| plain_text(text));
        assert_eq!(
            payload["reply_markup"]["rows"][0][0]["type"]["data"],
            "b2s="
        );
    }

    #[test]
    fn command_of_parses_mention_and_args() {
        let update = json!({
            "message": {
                "chat_id": -100,
                "id": 9,
                "is_outgoing": false,
                "content": {
                    "@type": "messageText",
                    "text": { "text": "/Help@SomeBot  参数 " }
                }
            }
        });
        let command = command_of(&update).expect("是命令");
        assert_eq!(command.name, "help");
        assert_eq!(command.args, "参数");
        assert_eq!(command.chat_id, -100);
    }

    #[test]
    fn outgoing_or_plain_messages_are_not_commands() {
        let outgoing = json!({
            "message": {
                "chat_id": 1, "id": 2, "is_outgoing": true,
                "content": { "@type": "messageText", "text": { "text": "/help" } }
            }
        });
        assert!(command_of(&outgoing).is_none());

        let plain = json!({
            "message": {
                "chat_id": 1, "id": 2,
                "content": { "@type": "messageText", "text": { "text": "hello" } }
            }
        });
        assert!(command_of(&plain).is_none());
    }
}
