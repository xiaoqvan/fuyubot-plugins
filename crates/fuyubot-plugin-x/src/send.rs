//! 发送：X 风格卡片 / 相册（每组最多 10 个）/ 单条媒体 / 文字消息。
//!
//! 有图片或视频的帖子优先出卡片（最多两张预览，视频只画封面，配文只放原链接）；
//! 卡片关闭、没有 html2img 或出图失败时，才走下面的相册 / 单条媒体流程。
//!
//! ## 相册的分组规则
//!
//! Telegram 的 sendMessageAlbum 只接受 **2..=10** 项（1 项根本不算相册，0 项是错），
//! 所以一条媒体再多的文章也只能按 10 个一组切开，一组一条相册消息。规则就是：
//!
//! - 媒体总数 <= 1：发单条 sendMessage；
//! - 媒体总数 2..=10：一条相册；
//! - 媒体总数 > 10：每 10 个一条相册，最后那组不足 2 个时走单条。
//!
//! 例：23 个图片 → 10 + 10 + 3，三条相册消息。
//!
//! ## 为什么正文和相册分开
//!
//! 相册只有**第一条**能带 caption（上限 1024 字符），而文章正文动辄几千字。
//! 所以正文单独发（config.send_text，默认开），相册标题只留「标题 + 作者 + 原链接」；
//! 普通推文的正文短，直接当标题，不额外发一条。

use std::sync::Mutex;

use base64::Engine as _;
use serde_json::Value;

use crate::model::{Media as PostMedia, Post};
use crate::render;
use crate::runtime::{CallError, Runtime};
use crate::simplify::{self, InputFile, Media, SendAlbumParams, SendMessageParams, Text};

/// Telegram sendMessageAlbum 的硬上限。
pub const ALBUM_HARD_LIMIT: usize = 10;

/// 发送失败的原因。文案会给用户看。
#[derive(Debug)]
pub struct SendError(pub String);

impl std::fmt::Display for SendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for SendError {}

impl From<CallError> for SendError {
    fn from(error: CallError) -> Self {
        Self(error.0)
    }
}

/// 一条链接发完之后的结果，用来打一行总结日志。
#[derive(Debug, Default)]
pub struct Stats {
    /// 文字消息条数。
    pub texts: usize,
    /// 相册消息组数。
    pub albums: usize,
    /// 成功发出的媒体个数。
    pub media: usize,
    /// 失败的条数（文字 + 媒体）。
    pub failed: usize,
    /// 是否发了 X 风格卡片。
    pub card: bool,
}

impl Stats {
    pub fn summary(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        if self.card {
            parts.push("卡片 1 张".to_owned());
        }
        if self.texts > 0 {
            parts.push(format!("文字 {} 条", self.texts));
        }
        if self.albums > 0 {
            parts.push(format!("相册 {} 组", self.albums));
        }
        if self.media > 0 && self.albums == 0 && !self.card {
            parts.push(format!("单条媒体 {} 个", self.media));
        }
        if self.failed > 0 {
            parts.push(format!("失败 {}", self.failed));
        }
        if parts.is_empty() {
            "没有发出任何内容".to_owned()
        } else {
            parts.join("，")
        }
    }
}

/// 把一条解析结果发进会话（一律新发，不引用原消息）。
pub async fn deliver(rt: &Runtime, post: &Post, chat_id: i64) -> Result<Stats, SendError> {
    let config = &rt.config;
    let mut media = post.media_for_send();

    // 有图 / 视频的帖子也出卡片：图上最多两张，视频只画封面，配文只留原链接。
    // 没有可画的封面（比如视频没有缩略图）时保持原来的发送方式；渲染失败也回退。
    let has_preview = media.iter().take(2).any(|item| {
        if item.is_video() || item.gif {
            item.thumbnail.as_deref().is_some_and(|url| !url.is_empty())
        } else {
            true
        }
    });
    if config.image_card && has_preview && rt.image_service_available() {
        match deliver_card(rt, post, chat_id).await {
            Ok(stats) => return Ok(stats),
            Err(error) => crate::log::warn(&format!("发卡片失败：{error}；改发原帖内容")),
        }
    }

    if media.len() > config.max_media {
        crate::log::warn(&format!(
            "这条有 {} 个媒体，超过上限 {}，只发前 {} 个",
            media.len(),
            config.max_media,
            config.max_media
        ));
        media.truncate(config.max_media);
    }
    if media.is_empty() {
        return deliver_text(rt, post, chat_id).await;
    }

    let mut stats = Stats::default();
    let (caption_html, caption_truncated) = render::caption(post, render::CAPTION_LIMIT);
    let long_form = post.article.is_some();

    // 正文：长文总是单独发；普通推文只在正文装不进标题（1024）时才单独发。
    if config.send_text && (long_form || caption_truncated) {
        for message in render::messages(post, config) {
            match rt.send_html(chat_id, &message).await {
                Ok(_) => stats.texts += 1,
                Err(error) => {
                    crate::log::warn(&format!("发正文失败：{error}"));
                    stats.failed += 1;
                }
            }
        }
    }

    let limit = config.album_limit.clamp(2, ALBUM_HARD_LIMIT);
    let groups = media.len().div_ceil(limit);
    for (index, chunk) in media.chunks(limit).enumerate() {
        let caption = if index == 0 {
            Some(caption_html.as_str())
        } else {
            None
        };
        crate::log::debug(&format!(
            "发第 {}/{} 组：{} 个媒体",
            index + 1,
            groups,
            chunk.len()
        ));
        let (sent, failed, album) = send_chunk(rt, chat_id, chunk, caption).await;
        stats.media += sent;
        stats.failed += failed;
        if album {
            stats.albums += 1;
        }
    }

    if stats.media == 0 && stats.texts == 0 && stats.failed == 0 {
        return Err(SendError("没有可发送的内容".to_owned()));
    }
    if stats.media == 0 {
        return Err(SendError("媒体全部发送失败".to_owned()));
    }
    Ok(stats)
}

/// 卡片底部署名：当前 bot 的用户名 + 插件标识；没登录时只写插件。
const FOOTER_PLUGIN: &str = "fuyubot_plugin.xiaoqvan.x";

static AVATAR_CACHE: Mutex<Option<(i64, String)>> = Mutex::new(None);

fn cached_avatar(file_id: i64) -> Option<String> {
    let cache = AVATAR_CACHE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    cache
        .as_ref()
        .filter(|(id, _)| *id == file_id)
        .map(|(_, svg)| svg.clone())
}

fn store_avatar(file_id: i64, svg: &str) {
    let mut cache = AVATAR_CACHE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    *cache = Some((file_id, svg.to_owned()));
}

/// getMe 里的 small 头像文件；没有头像就 None，卡片也就不画头像。
fn photo_file(me: &Value) -> Option<(i64, Option<String>, bool)> {
    let photo = me.get("profile_photo")?;
    let file = photo.get("small").or_else(|| photo.get("big"))?;
    let file_id = file.get("id")?.as_i64()?;
    let path = file
        .pointer("/local/path")
        .and_then(Value::as_str)
        .filter(|path| !path.is_empty())
        .map(str::to_owned);
    let downloaded = file
        .pointer("/local/is_downloading_completed")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    Some((file_id, path, downloaded))
}

async fn avatar_bytes(
    rt: &Runtime,
    file_id: i64,
    path: Option<&str>,
    downloaded: bool,
) -> Option<Vec<u8>> {
    if downloaded
        && let Some(path) = path
        && let Ok(bytes) = tokio::fs::read(path).await
    {
        return Some(bytes);
    }
    let file = rt
        .invoke_op(
            "download_file",
            serde_json::json!({
                "file_id": file_id,
                "priority": 1,
                "timeout_ms": 20_000,
            }),
            25_000,
        )
        .await
        .ok()?;
    let path = file.pointer("/local/path").and_then(Value::as_str)?;
    tokio::fs::read(path).await.ok()
}

fn image_mime(bytes: &[u8]) -> &'static str {
    if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        "image/jpeg"
    } else if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        "image/png"
    } else if bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        "image/webp"
    } else {
        "image/jpeg"
    }
}

fn inline_avatar(bytes: &[u8]) -> String {
    let mime = image_mime(bytes);
    let data = base64::engine::general_purpose::STANDARD.encode(bytes);
    format!(
        "<svg class=\"footer-avatar\" xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 64 64\"><defs><clipPath id=\"avatar-clip\"><circle cx=\"32\" cy=\"32\" r=\"32\"/></clipPath></defs><image width=\"64\" height=\"64\" preserveAspectRatio=\"xMidYMid slice\" clip-path=\"url(#avatar-clip)\" href=\"data:{mime};base64,{data}\"/></svg>"
    )
}

async fn avatar_svg(
    rt: &Runtime,
    file_id: i64,
    path: Option<&str>,
    downloaded: bool,
) -> Option<String> {
    if let Some(svg) = cached_avatar(file_id) {
        return Some(svg);
    }
    let bytes = avatar_bytes(rt, file_id, path, downloaded).await?;
    let svg = inline_avatar(&bytes);
    store_avatar(file_id, &svg);
    Some(svg)
}

async fn card_footer(rt: &Runtime) -> (String, Option<String>) {
    let me = rt
        .host
        .call_service("fuyubot.host.identity", "me", "{}")
        .ok()
        .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
        .unwrap_or(Value::Null);
    let username = me
        .pointer("/usernames/active_usernames/0")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .or_else(|| simplify::my_usernames(&rt.host).into_iter().next());
    let text = match &username {
        Some(name) => format!("@{name} · {FOOTER_PLUGIN}"),
        None => FOOTER_PLUGIN.to_owned(),
    };
    let avatar = match photo_file(&me) {
        Some((file_id, path, downloaded)) => {
            avatar_svg(rt, file_id, path.as_deref(), downloaded).await
        }
        None => None,
    };
    (text, avatar)
}

/// 渲染卡片前按需机翻；谷歌不通就不带译文，不影响出图。
async fn card_html(rt: &Runtime, post: &Post) -> String {
    let translation = if crate::translate::needs(post) {
        match crate::translate::to_chinese(&rt.http, post.translation_text()).await {
            Ok(text) => Some(text),
            Err(error) => {
                crate::log::debug(&format!("谷歌翻译失败：{error}"));
                None
            }
        }
    } else {
        None
    };
    let (footer, avatar) = card_footer(rt).await;
    render::card_html_with_footer(
        post,
        &rt.config,
        translation.as_deref(),
        Some(footer.as_str()),
        None,
        avatar.as_deref(),
    )
}

/// 整条帖子渲染成一张卡片发出去；配文只有原文链接。
async fn deliver_card(rt: &Runtime, post: &Post, chat_id: i64) -> Result<Stats, SendError> {
    let config = &rt.config;
    let html = card_html(rt, post).await;
    let path = rt.render_card(html).await.map_err(SendError)?;
    let params = SendMessageParams {
        media: Some(Media::photo(InputFile::local(path.as_str())).size(0, 0)),
        text: Some(Text::html(render::card_caption(post))),
        timeout_ms: config.send_timeout_ms,
    };
    let render = |text: &Text| simplify::text_json(text);
    let args = simplify::send_params(chat_id, &params, &render);
    rt.invoke_op("send_message", args, config.send_timeout_ms)
        .await?;
    Ok(Stats {
        card: true,
        media: 1,
        ..Stats::default()
    })
}

/// 发一组：1 个走单条消息，2..=10 个走相册。
///
/// 返回 (成功个数, 失败个数, 是不是相册消息)。
async fn send_chunk(
    rt: &Runtime,
    chat_id: i64,
    chunk: &[PostMedia],
    caption: Option<&str>,
) -> (usize, usize, bool) {
    if chunk.is_empty() {
        return (0, 0, false);
    }
    if chunk.len() == 1 {
        let (sent, failed) = send_one(rt, chat_id, &chunk[0], caption).await;
        return (sent, failed, false);
    }

    let timeout = rt.config.send_timeout_ms;
    let params = SendAlbumParams {
        medias: chunk.iter().map(remote_media).collect(),
        caption: caption.map(Text::html),
        timeout_ms: timeout,
    };
    let render = |text: &Text| simplify::text_json(text);
    let args = simplify::album_params(chat_id, &params, &render);
    match rt.invoke_op("send_message_album", args, timeout).await {
        Ok(_) => {
            crate::log::debug(&format!("相册已发送（{} 项）", chunk.len()));
            (chunk.len(), 0, true)
        }
        Err(error) => {
            // 一项坏掉不该让整组消失：退回逐个发，尽量把能发的发出去。
            crate::log::warn(&format!(
                "发相册（{} 项）失败：{error}；改为逐个发送",
                chunk.len()
            ));
            let mut sent = 0;
            let mut failed = 0;
            for item in chunk {
                let (one_sent, one_failed) = send_one(rt, chat_id, item, None).await;
                sent += one_sent;
                failed += one_failed;
            }
            (sent, failed, false)
        }
    }
}

/// 发单个媒体（sendMessage）：先按远程直链发，失败就下载后发本地文件。
async fn send_one(
    rt: &Runtime,
    chat_id: i64,
    media: &PostMedia,
    caption: Option<&str>,
) -> (usize, usize) {
    match send_single(rt, chat_id, remote_media(media), caption).await {
        Ok(()) => return (1, 0),
        Err(error) => crate::log::warn(&format!(
            "直链发送失败：{error}（{}），改为下载后发送",
            crate::log::redact_url(&media.url)
        )),
    }
    let path = match download_media(rt, media).await {
        Ok(path) => path,
        Err(error) => {
            crate::log::warn(&format!("下载也失败：{error}"));
            return (0, 1);
        }
    };
    let result = send_single(rt, chat_id, local_media(media, &path), caption).await;
    // 发完就删：这只是一次性的中转文件。
    let _ = tokio::fs::remove_file(&path).await;
    match result {
        Ok(()) => (1, 0),
        Err(error) => {
            crate::log::warn(&format!("发本地文件失败：{error}"));
            (0, 1)
        }
    }
}

/// 下发一条 sendMessage。
async fn send_single(
    rt: &Runtime,
    chat_id: i64,
    media: Media,
    caption: Option<&str>,
) -> Result<(), String> {
    let timeout = rt.config.send_timeout_ms;
    let params = SendMessageParams {
        media: Some(media),
        text: caption.map(Text::html),
        timeout_ms: timeout,
    };
    let render = |text: &Text| simplify::text_json(text);
    let args = simplify::send_params(chat_id, &params, &render);
    rt.invoke_op("send_message", args, timeout)
        .await
        .map(|_| ())
        .map_err(|error| error.to_string())
}

/// 下载到临时目录，返回本地路径。
async fn download_media(rt: &Runtime, media: &PostMedia) -> Result<std::path::PathBuf, String> {
    let path = rt.temp_dir.join(file_name(media));
    rt.http
        .download(&media.url, &path, &rt.cancel)
        .await
        .map_err(|error| error.to_string())?;
    Ok(path)
}

/// 临时文件名：URL 的稳定哈希 + 扩展名（同一张图重复下载覆盖，不堆垃圾）。
fn file_name(media: &PostMedia) -> String {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    media.url.hash(&mut hasher);
    let extension = if media.is_video() { "mp4" } else { "jpg" };
    format!("{:016x}.{extension}", hasher.finish())
}

/// 解析结果里的媒体 → 远程直链的 InputMessageContent。
///
/// 优先用 inputFileRemote：X 的图片 / 视频直链是公开 CDN，不需要 Referer 或
/// cookie，让 TDLib 自己去拉最省事。发不出去时才走本地下载那条路。
fn remote_media(media: &PostMedia) -> Media {
    build_media(media, InputFile::remote(media.url.as_str()))
}

fn local_media(media: &PostMedia, path: &std::path::Path) -> Media {
    build_media(media, InputFile::local(path.to_string_lossy()))
}

fn build_media(media: &PostMedia, file: InputFile) -> Media {
    if media.is_video() {
        Media::video(file)
            .size(media.width, media.height)
            .duration(media.duration)
            .streaming(true)
    } else {
        Media::photo(file).size(media.width, media.height)
    }
}

/// 没有媒体时：纯文本文章出卡片，其余（以及出图失败）发文字。
async fn deliver_text(rt: &Runtime, post: &Post, chat_id: i64) -> Result<Stats, SendError> {
    let config = &rt.config;
    let mut stats = Stats::default();

    // 只有「文章 + 一个媒体都没有」才出卡片：这是需求里的纯文本分支。
    if config.image_card && post.article.is_some() {
        if !rt.image_service_available() {
            crate::log::info(&format!(
                "没有 {} 服务，纯文本文章改发纯文本",
                crate::config::HTML2IMG_SERVICE
            ));
        } else {
            let html = card_html(rt, post).await;
            match rt.render_card(html).await {
                Ok(path) => {
                    let (caption, _) = render::caption(post, render::CAPTION_LIMIT);
                    let params = SendMessageParams {
                        media: Some(Media::photo(InputFile::local(path.as_str())).size(0, 0)),
                        text: Some(Text::html(caption)),
                        timeout_ms: config.send_timeout_ms,
                    };
                    let render = |text: &Text| simplify::text_json(text);
                    let args = simplify::send_params(chat_id, &params, &render);
                    match rt
                        .invoke_op("send_message", args, config.send_timeout_ms)
                        .await
                    {
                        Ok(_) => {
                            crate::log::info("纯文本文章已渲染成 X 风格卡片");
                            stats.card = true;
                            stats.media = 1;
                            return Ok(stats);
                        }
                        Err(error) => {
                            crate::log::warn(&format!("发卡片失败：{error}"));
                            stats.failed += 1;
                        }
                    }
                }
                Err(error) => {
                    crate::log::warn(&format!(
                        "出图失败：{error}；{}",
                        if config.fallback_text {
                            "改为发纯文本"
                        } else {
                            "按配置不发纯文本"
                        }
                    ));
                }
            }
            if !config.fallback_text {
                return Err(SendError(format!(
                    "图片生成失败（{}），且配置里 fallback_text = false",
                    crate::config::HTML2IMG_SERVICE
                )));
            }
        }
    }

    for message in render::messages(post, config) {
        match rt.send_html(chat_id, &message).await {
            Ok(_) => stats.texts += 1,
            Err(error) => {
                crate::log::warn(&format!("发文字失败：{error}"));
                stats.failed += 1;
            }
        }
    }

    if stats.texts == 0 && stats.failed == 0 {
        return Err(SendError("这条没有可发送的文本".to_owned()));
    }
    if stats.texts == 0 {
        return Err(SendError("文字全部发送失败".to_owned()));
    }
    Ok(stats)
}

#[cfg(test)]
#[path = "../tests/unit/send.rs"]
mod tests;
