//! 发送：把 [`VideoInfo`] / 缓存文档变成 Telegram 消息。
//!
//! 对应 TS 版 `utils/videomsg.ts`。三处刻意的不同：
//!
//! 1. **不做 `json` 前缀调试模式**。TS 版的 `isJsonStart` 让用户在消息里写 `json`
//!    就能拿到原始 JSON——那是 TS 版特有的调试通道，这里没有对应的需求。
//! 2. **不发 ffprobe 元数据**。TS 版会用 `ffprobe` 补宽高时长；这里直接用抖音给的值
//!    （`bit_rate` 里就有），省掉一个外部进程依赖。缺就传 0，让 TDLib 自己读。
//! 3. **不再自己等响应**。发送是 `simplify op`（`send_message` / `send_message_album` /
//!    `edit_message_media`），下发与等待在 [`crate::runtime::Runtime::invoke_op`] 里，
//!    这里是 `async fn`，不占线程。
//!
//! ⚠️ **「先发远程直链、失败再下载后发」这条回退是保留的**（见 [`send_main_video`]）：
//! 抖音直链带防盗链，TDLib 按 `inputFileRemote` 去拉常常拿不到东西。
//! 下载那一侧走 [`crate::http::download`] 的流式写盘，大文件不受旧 10 MiB 上限限制。
//!
//! 剩下的两条主路径与 TS 版一致：**图集优先**（有图就发相册，相册里有视频就发视频），
//! 否则发主视频。缓存命中时走 [`send_cached`]：全 `inputFileId`，不解析、不下载。
//!
//! 频道里按 TS 的**编辑模式**：编辑前先 [`channel_rights`] 查
//! `getMessageProperties`；单条媒体 `edit_message_media` 编辑原帖；相册没有
//! 编辑形态，所以先 `delete_messages` 删原帖再发相册；解析前后的占位/报错文案
//! 都通过 [`replace_text`] 编辑原帖（不是新发消息）。没有编辑权限就不编辑。

use serde_json::{Map, Value, json};

use crate::cached::Cached;
use crate::model::VideoInfo;
use crate::runtime::Runtime;
use crate::simplify::{
    InputFile, Media, SendAlbumParams, SendMessageParams, Text, album_params, send_params,
    text_json,
};

/// 一次发送的预算。发视频比发文本慢得多（TDLib 要先把 bytes 拿到手再 useState）。
pub const TIMEOUT_MS: u64 = 120_000;

/// 相册每组的项数上限（TDLib 的硬上限，不是我们挑的）。
pub const ALBUM_LIMIT: usize = 10;

/// 发送失败的原因。文案会给用户看。
#[derive(Debug)]
pub struct SendError(pub String);

impl std::fmt::Display for SendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for SendError {}

impl From<crate::runtime::CallError> for SendError {
    fn from(error: crate::runtime::CallError) -> Self {
        Self(error.0)
    }
}

/// 把一条解析结果发进会话。返回**最终消息**（单条 = 1 个，相册 = N 个）。
///
/// **不引用原消息**（用户要求去掉引用行为，见 [`Target`]）：
/// 一律新发一条；只有频道帖子走「编辑已发的那条」（`editMessageMedia`）。
pub async fn send_video(
    rt: &Runtime,
    info: &VideoInfo,
    target: &Target,
) -> Result<Vec<Value>, SendError> {
    let caption = build_caption(info);

    // 图集优先：抖音的「图文」里常常是图片夹着视频，两条路径都在下面。
    match (&info.images, &info.image_videos) {
        (Some(images), Some(videos)) if !images.is_empty() && !videos.is_empty() => {
            // TS 版这里先试着发 video 相册，失败了退回图片相册；这里的组合更简单：
            // **有视频就发视频**（用户想看的是能动的那些页），图片留作兜底。
            //
            // 只有 1 个图集视频时 TS 走 `sendSingleImageVideo`：频道里是
            // `editMessageMedia`，不是「删原帖 + 发只有一个元素的相册」。
            if videos.len() == 1 {
                let video = &videos[0];
                let media = Media::video(InputFile::remote(&video.video_url))
                    .size(video.width as i32, video.height as i32)
                    .streaming(true);
                return deliver(rt, target, media, Text::markdown(&caption))
                    .await
                    .map(|message| vec![message]);
            }
            let medias = videos
                .iter()
                .map(|video| {
                    Media::video(InputFile::remote(&video.video_url))
                        .size(video.width as i32, video.height as i32)
                        .streaming(true)
                })
                .collect();
            send_albums(rt, medias, &caption, target).await
        }
        (Some(images), _) if !images.is_empty() => {
            let medias = images
                .iter()
                .map(|image| {
                    Media::photo(InputFile::remote(&image.url))
                        .size(image.width as i32, image.height as i32)
                })
                .collect();
            send_albums(rt, medias, &caption, target).await
        }
        _ => send_main_video(rt, info, &caption, target)
            .await
            .map(|message| vec![message]),
    }
}

/// 缓存命中：**不发任何网络请求**，直接用缓存的 TDLib 文件 id 重发。
///
/// 单视频按一条消息发（频道里走编辑）；图集 / 视频集按 10 个一组发相册。
/// 文件 id 失效（TDLib 回了错误）时由调用方删缓存并回落到完整解析流程。
pub async fn send_cached(
    rt: &Runtime,
    cached: &Cached,
    target: &Target,
) -> Result<Vec<Value>, SendError> {
    let caption = Text::markdown(cached.caption());
    let medias = cached.medias();
    if medias.is_empty() {
        return Err(SendError("缓存里没有可发送的媒体".to_owned()));
    }
    // 单视频：走 `send_message`（频道里还能走 `edit_message_media`）。
    if matches!(cached, Cached::Video { .. }) {
        let media = medias.into_iter().next().expect("上面已经判过非空");
        return Ok(vec![deliver(rt, target, media, caption).await?]);
    }
    send_albums(rt, medias, cached.caption(), target).await
}

/// 发送到哪儿 + 怎么发。
///
/// ⚠️ **没有「引用原消息」**：用户明确要求不引用/不回复原消息，所以连
/// `reply_to_message_id` 字段都不留——构造出来的请求里永远不会出现
/// `inputMessageReplyToMessage`。也因此**不需要 `getChat` 判群聊**
/// （那个请求当年只为决定「要不要引用」而存在）。
///
/// 唯一保留的定位是 `edit_message_id`：频道帖子里机器人通常没有发言权，
/// 只有编辑权，所以走 `editMessageMedia` / `editMessageText`——那是**编辑**，不是引用。
/// 图集例外：没有编辑形态，先删原帖再发相册（见 [`send_albums`]）。
#[derive(Debug, Clone, Copy)]
pub struct Target {
    pub chat_id: i64,
    /// 频道：改成**编辑**那条帖子（频道里机器人通常没有发帖权，只有编辑权）。
    pub edit_message_id: Option<i64>,
}

impl Target {
    /// 往这个会话发一条新消息（不引用）。
    pub fn send(chat_id: i64) -> Self {
        Self {
            chat_id,
            edit_message_id: None,
        }
    }

    /// 一条命令落在哪儿。
    ///
    /// 普通会话只取 `chat_id`（不引用原消息，所以不需要 `getChat`）；频道帖走编辑，
    /// 命令消息本身就是那条帖子。
    pub fn of(command: &crate::simplify::Command) -> Self {
        if command.is_channel_post {
            Self::edit(command.chat_id, command.message_id)
        } else {
            Self::send(command.chat_id)
        }
    }

    /// 这个目标是不是「编辑原帖」（频道）。
    pub fn is_edit(&self) -> bool {
        self.edit_message_id.is_some()
    }

    /// 频道里「编辑原帖」的目标。
    ///
    /// 机器人在频道里通常没有发帖权，只有编辑权——TS 版就是这么绕过去的。
    /// 相册**没有**对应的编辑形态（`editMessageMedia` 一次只换一条内容），
    /// 所以这个目标只适用于单条视频。
    pub fn edit(chat_id: i64, message_id: i64) -> Self {
        Self {
            chat_id,
            edit_message_id: Some(message_id),
        }
    }
}

// ── 频道：权限 / 占位 / 报错 / 删原帖（对齐 TS 的编辑流程）───────────

/// 当前账号对一条频道帖的编辑权限（来自 TDLib `getMessageProperties`）。
///
/// ⚠️ 为什么不用 `message.can_be_edited`：新版 TDLib 已把 `can_be_edited` /
/// `can_edit_media` 移到 `MessageProperties`（`getMessageProperties` 方法），
/// 消息对象本身不再带这两个字段。编辑媒体要用 `can_edit_media`，只查
/// `can_be_edited` 不够（它只覆盖 `editMessageText` / `editMessageCaption`）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ChannelRights {
    /// 能否编辑文本（占位提示 / ❌ 报错）。
    pub can_edit_text: bool,
    /// 能否编辑媒体（`editMessageMedia`，频道里把原帖换成视频）。
    pub can_edit_media: bool,
}

/// 问 TDLib 这条频道帖当前能不能编辑（一次 `getMessageProperties` 往返）。
///
/// 走 simplify 的通用 `invoke` op：`args.request` 就是完整的 TDLib 请求 JSON。
pub async fn channel_rights(rt: &Runtime, target: &Target) -> Result<ChannelRights, SendError> {
    let Some(message_id) = target.edit_message_id else {
        return Err(SendError("这个目标不是频道编辑模式".to_owned()));
    };
    let args = json!({
        "request": {
            "@type": "getMessageProperties",
            "chat_id": target.chat_id,
            "message_id": message_id,
        },
        "timeout_ms": TIMEOUT_MS,
    });
    let properties = rt.invoke_op("invoke", args, TIMEOUT_MS).await?;
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

/// TS 版频道占位提示中间那条分隔线：`\-` 重复 27 次（Markdown v1 里转义的连字符）。
fn placeholder_separator() -> String {
    "\\-".repeat(27)
}

/// 频道里「正在解析」的占位文案：原文 + 分隔线 + 提示。
///
/// 与 TS 版 `parse/douyin/index.ts` 的 `editMessageText` 文案逐字一致。
fn parsing_placeholder(source_text: &str) -> String {
    format!(
        "{source_text}\n{}\n\n正在解析抖音视频...",
        placeholder_separator()
    )
}

/// 把原帖文本换成另一段文本（频道编辑模式）。
///
/// 走 simplify 的 `edit_message_text` op；`input_message_content` 的 `text` 交给
/// 宿主按 Markdown 解析，与发送媒体时的文案口径一致。
pub async fn replace_text(rt: &Runtime, target: &Target, text: &str) -> Result<Value, SendError> {
    let Some(message_id) = target.edit_message_id else {
        return Err(SendError("这个目标不是频道编辑模式".to_owned()));
    };
    let args = json!({
        "chat_id": target.chat_id,
        "message_id": message_id,
        "input_message_content": {
            "@type": "inputMessageText",
            "text": text_json(&Text::markdown(text)),
        },
        "timeout_ms": TIMEOUT_MS,
    });
    Ok(rt.invoke_op("edit_message_text", args, TIMEOUT_MS).await?)
}

/// 解析开始前先把原帖改成「正在解析抖音视频...」（频道专用）。
///
/// 失败只记日志、**继续解析**：占位提示是体验优化，不该因为它没权限之类的原因
/// 让整次解析失败；真正失败会由 [`announce_error`] 再试一次编辑。
pub async fn announce_parsing(rt: &Runtime, target: &Target, source_text: &str) {
    if let Err(error) = replace_text(rt, target, &parsing_placeholder(source_text)).await {
        crate::log::warn(&format!("频道占位提示编辑失败（继续解析）：{error}"));
    }
}

/// 频道里解析失败：把原帖编辑成 `❌ …`，而不是往频道另发一条错误消息。
pub async fn announce_error(rt: &Runtime, target: &Target, error: &str) {
    if let Err(failure) = replace_text(rt, target, &format!("❌ {error}")).await {
        crate::log::warn(&format!("频道错误提示编辑失败：{failure}"));
    }
}

/// 频道里图集要把原帖删掉再发相册：`editMessageMedia` 一次只能换一条内容，
/// 相册没有编辑形态（TS 版也是 `deleteMessage` 后 `sendMessageAlbum`）。
///
/// 删除失败只记日志、继续发相册：删不掉顶多留一条原帖，比整次解析失败好。
async fn remove_original(rt: &Runtime, target: &Target) {
    let Some(message_id) = target.edit_message_id else {
        return;
    };
    let args = json!({
        "chat_id": target.chat_id,
        "message_ids": [message_id],
        "revoke": true,
        "timeout_ms": TIMEOUT_MS,
    });
    if let Err(error) = rt.invoke_op("delete_messages", args, TIMEOUT_MS).await {
        crate::log::warn(&format!("频道里删原帖失败（继续发相册）：{error}"));
    }
}

// ── 文案 ────────────────────────────────────────────────────────────

/// 构造标题：照 TS 版 `buildCaption` 搬的（那些看起来别扭的判据都保留原样）。
///
/// ```text
/// <文案（#标签前补空格）>
///
/// [<音乐名>](<音乐地址>)        ← 只有「有音乐且有图集视频」时才有
///
/// By [@作者](主页) [@共创者(角色)](主页)…
/// ```
pub fn build_caption(info: &VideoInfo) -> String {
    // 主页链接只在 `sec_uid` 真的拼进去了时可用：`https://…/user/` 那种空尾巴
    // 说明作者没有 sec_uid，拿它当链接发出去是个指向 404 的 markdown。
    // 判据是「斜杠之后还剩不剩下东西」——先在**原串**上 trim 尾部斜杠，再看它是不是
    // 以 `/user` 收尾（反过来写会因为 trim 已经吃掉斜杠而永远不成立）。
    let usable_home = info
        .author
        .home_url
        .as_deref()
        .filter(|home| !home.trim_end_matches('/').ends_with("/user"));
    let mut author = match usable_home {
        Some(home) => format!("[@{}]({})", info.author.name, home),
        None => format!("@{}", info.author.name),
    };

    if !info.cooperation_info.is_empty() {
        for coauthor in &info.cooperation_info {
            let nickname = coauthor.nickname.as_deref().unwrap_or("?");
            let role = coauthor.role.as_deref().unwrap_or("共创");
            match coauthor
                .home_url
                .as_deref()
                .filter(|home| !home.trim_end_matches('/').ends_with("/user"))
            {
                Some(home) => author.push_str(&format!(" [@{nickname}({role})]({home})")),
                None => author.push_str(&format!(" @{nickname}({role})")),
            }
        }
    }

    let music = match (&info.music, &info.image_videos) {
        // ⚠️ TS 版的条件就是「有音乐 **且** 有图集视频」——纯视频作品不挂音乐，
        // 那个 strange 的判据原样保留，改了会让老用户对不上。
        (Some(music), Some(_)) if !music.url.is_empty() && !music.name.is_empty() => {
            format!("\n\n[{}]({})", music.name, music.url)
        }
        _ => String::new(),
    };

    format!("{}{}\n\nBy {}", sanitize_title(&info.title), music, author)
}

/// 抖音文案里的标签常常是连着的（`#视频#推荐`），直接发出去会变成一个超长 tag。
///
/// 在非空白前的 `#` 前插一个空格把它拆开；第一个字符是 `#` 时不插。
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

// ── 三种发送形态 ────────────────────────────────────────────────────

/// 发主视频（单个作品最常见的情况）。
///
/// 分两步：**先按远程直链发**，失败就下载到本地再发。
///
/// ⚠️ 那个回退不是多余的：抖音直链带防盗链，TDLib 按 `inputFileRemote`
/// 去拉常常拿不到东西（报 `send_failed: 发送失败（TDLib 0）`）。
/// 下载时带上解析用的 UA / cookie / Referer 才能拿到真文件。
async fn send_main_video(
    rt: &Runtime,
    info: &VideoInfo,
    caption: &str,
    target: &Target,
) -> Result<Value, SendError> {
    let Some(video) = info.video.as_ref() else {
        return Err(SendError("这个作品里没有可发送的视频".to_owned()));
    };

    let size = (video.width as i32, video.height as i32);
    let duration = video.duration.unwrap_or(0) as i32;

    let mut remote = Media::video(InputFile::remote(&video.video_url))
        .size(size.0, size.1)
        .duration(duration)
        .streaming(true);
    if !info.cover_url.is_empty() {
        remote = remote.cover(InputFile::remote(&info.cover_url), 0);
    }

    crate::log::debug("发送：先试远程直链");
    match deliver(rt, target, remote, Text::markdown(caption)).await {
        Ok(message) => {
            crate::log::debug("发送成功（远程直链）");
            return Ok(message);
        }
        Err(first) => {
            crate::log::warn(&format!("远程直链发送失败，改为下载后发送：{first}"));
        }
    }

    // 回退：下载到本地再发（封面也一并下载，远程封面同样可能拉不到）。
    let file = crate::http::download(
        &rt.http,
        &video.video_url,
        &rt.cookie,
        &rt.cache_dir,
        &rt.cancel,
    )
    .await
    .map_err(|error| SendError(format!("下载视频失败：{error}")))?;
    let cleanup = Cleanup(Some(file.clone()));

    let mut local = Media::video(InputFile::local(file.to_string_lossy()))
        .size(size.0, size.1)
        .duration(duration)
        .streaming(true);
    let cover = if info.cover_url.is_empty() {
        None
    } else {
        // 封面是图片：扩展名要给对，否则 TDLib 不认它是图。
        crate::http::download_as(
            &rt.http,
            &info.cover_url,
            &rt.cookie,
            crate::http::Kind::Image,
            &rt.cache_dir,
            &rt.cancel,
        )
        .await
        .ok()
    };
    if let Some(cover) = &cover {
        local = local.cover(InputFile::local(cover.to_string_lossy()), 0);
    }
    let _cover_cleanup = Cleanup(cover.clone());

    crate::log::debug("发送：已下载到本地，改用 inputFileLocal 再发");
    let outcome = match deliver(rt, target, local, Text::markdown(caption)).await {
        Ok(message) => {
            crate::log::debug("发送成功（下载后本地发送）");
            Ok(message)
        }
        Err(error) => {
            crate::log::error(&format!("本地发送也失败：{error}"));
            Err(error)
        }
    };
    // 无论成败都把临时文件删掉（drop 顺序：先取 outcome，再清文件）
    drop(cleanup);
    outcome
}

/// 临时文件的守卫：drop 就删。
///
/// 下载下来的视频几十 MB，用完必须删；发送失败时也要删（不然临时目录会堆满）。
struct Cleanup(Option<std::path::PathBuf>);

impl Drop for Cleanup {
    fn drop(&mut self) {
        if let Some(path) = self.0.take()
            && let Err(error) = std::fs::remove_file(&path)
        {
            // ⚠️ 不静默：删不掉通常意味着 TDLib 还开着它（Windows 上独占句柄），
            // 那正是下一次「写缓存文件失败：拒绝访问」的前兆。
            crate::log::warn(&format!("清理缓存文件失败 {}: {error}", path.display()));
        }
    }
}

/// 把一组媒体按 10 个一组发成相册，返回所有最终消息。
///
/// 相册**没有**编辑形态（`editMessageMedia` 一次只换一条内容），所以频道里先
/// 删掉原帖再按「新发一组」处理——与 TS 版 `sendPhotoAlbum` / 多视频相册一致。
async fn send_albums(
    rt: &Runtime,
    medias: Vec<Media>,
    caption: &str,
    target: &Target,
) -> Result<Vec<Value>, SendError> {
    if medias.is_empty() {
        return Err(SendError("这个作品里没有可发送的媒体".to_owned()));
    }
    // 频道里相册没有编辑形态：先删原帖，再把相册新发出去（TS 版同样处理）。
    if target.is_edit() {
        remove_original(rt, target).await;
    }
    let mut sent = Vec::new();
    for batch in medias.chunks(ALBUM_LIMIT) {
        let params = SendAlbumParams {
            medias: batch.to_vec(),
            caption: Some(Text::markdown(caption)),
            timeout_ms: TIMEOUT_MS,
        };
        let (op, args) = album_request(&params, target);
        let result = rt.invoke_op(op, args, TIMEOUT_MS).await?;
        match result {
            // 相册的结果是「最终消息」数组；万一宿主改了形状也照收单条。
            Value::Array(items) => sent.extend(items),
            other => sent.push(other),
        }
    }
    Ok(sent)
}

/// 单条媒体的两种落点：频道编辑 / 新发一条（不引用）。返回最终消息。
async fn deliver(
    rt: &Runtime,
    target: &Target,
    media: Media,
    caption: Text,
) -> Result<Value, SendError> {
    let params = SendMessageParams {
        text: Some(caption),
        media: Some(media),
        timeout_ms: TIMEOUT_MS,
    };
    let (op, args) = message_request(&params, target);
    Ok(rt.invoke_op(op, args, TIMEOUT_MS).await?)
}

/// 组单条 `sendMessage` / `editMessageMedia` 的 op + args。
fn message_request(params: &SendMessageParams, target: &Target) -> (&'static str, Value) {
    // 渲染器交给宿主的 op 自己做（`args.text` 支持 `{text, parse_mode}` 形态），
    // 所以这里**不**调用 `parseTextEntities`——那条路要额外往返一次 `td_execute`。
    let payload = send_params(target.chat_id, params, &text_json);
    let mut args = match payload {
        Value::Object(map) => map,
        other => json!({ "raw": other }).as_object().unwrap().clone(),
    };
    args.insert("timeout_ms".to_owned(), json!(params.timeout_ms));
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
            ("edit_message_media", Value::Object(edit))
        }
        None => ("send_message", Value::Object(args)),
    }
}

/// 组一条 `sendMessageAlbum` 的 op + args。
fn album_request(params: &SendAlbumParams, target: &Target) -> (&'static str, Value) {
    let payload = album_params(target.chat_id, params, &text_json);
    let mut args = match payload {
        Value::Object(map) => map,
        other => json!({ "raw": other }).as_object().unwrap().clone(),
    };
    args.insert("timeout_ms".to_owned(), json!(params.timeout_ms));
    ("send_message_album", Value::Object(args))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Author, Coauthor, Music, Video};

    fn info() -> VideoInfo {
        VideoInfo {
            detail_id: "1".to_owned(),
            title: "标题 #A#B".to_owned(),
            cover_url: "https://cover/1".to_owned(),
            video: Some(Video {
                video_url: "https://play/1".to_owned(),
                fps: Some(30),
                duration: Some(12),
                height: 1920,
                width: 1080,
            }),
            images: None,
            image_videos: None,
            author: Author {
                name: "作者".to_owned(),
                home_url: Some("https://www.douyin.com/user/MS4w".to_owned()),
                ..Author::default()
            },
            music: None,
            cooperation_info: Vec::new(),
        }
    }

    /// 频道占位文案对齐 TS：原文 + 27 个 `\-` + 「正在解析抖音视频...」。
    #[test]
    fn channel_parsing_placeholder_matches_ts() {
        let text = parsing_placeholder("https://v.douyin.com/x/");
        assert!(text.starts_with("https://v.douyin.com/x/\n"), "{text}");
        assert_eq!(text.matches("\\-").count(), 27, "{text}");
        assert!(text.ends_with("\n\n正在解析抖音视频..."), "{text}");
    }

    #[test]
    fn adjacent_hashtags_are_separated() {
        assert_eq!(sanitize_title("#开头"), "#开头", "首字符不插空格");
        assert_eq!(sanitize_title("a#b"), "a #b");
        assert_eq!(
            sanitize_title("已经 #好 的"),
            "已经 #好 的",
            "本来有空格就不动"
        );
        assert_eq!(sanitize_title(""), "");
    }

    #[test]
    fn caption_ends_with_a_markdown_author_link() {
        let caption = build_caption(&info());
        assert!(caption.starts_with("标题 #A #B"), "{caption}");
        assert!(
            caption.ends_with("By [@作者](https://www.douyin.com/user/MS4w)"),
            "{caption}"
        );
    }

    #[test]
    fn an_author_without_home_url_falls_back_to_plain_text() {
        let mut value = info();
        value.author.home_url = Some("https://www.douyin.com/user/".to_owned());
        assert!(build_caption(&value).ends_with("By @作者"));

        let mut value = info();
        value.author.home_url = None;
        assert!(build_caption(&value).ends_with("By @作者"));
    }

    #[test]
    fn coauthors_are_appended_after_the_main_author() {
        let mut value = info();
        value.cooperation_info = vec![Coauthor {
            nickname: Some("摄影君".to_owned()),
            role: Some("摄影".to_owned()),
            home_url: Some("https://www.douyin.com/user/abc".to_owned()),
            ..coauthor()
        }];
        let caption = build_caption(&value);
        assert!(
            caption.ends_with("[@摄影君(摄影)](https://www.douyin.com/user/abc)"),
            "{caption}"
        );
    }

    /// TS 版那个奇怪的判据：只有「有音乐 **且** 有图集视频」时才挂音乐链接。
    #[test]
    fn music_only_shows_when_image_videos_exist() {
        let mut value = info();
        value.music = Some(music());
        assert!(
            !build_caption(&value).contains("歌名"),
            "纯视频作品不挂音乐"
        );

        value.image_videos = Some(vec![Video {
            video_url: "https://iv/1".to_owned(),
            ..info().video.unwrap()
        }]);
        let caption = build_caption(&value);
        assert!(caption.contains("[歌名](https://m/1)"), "{caption}");
    }

    /// 单条 `sendMessage` 的 op 形状：op 名 + 超时 + 内容，且**不带 `reply_to`**。
    #[test]
    fn message_request_builds_a_send_message_op_without_reply() {
        let target = Target::send(42);
        let params = SendMessageParams {
            text: Some(Text::markdown("hi")),
            media: Some(Media::video(InputFile::id(5))),
            timeout_ms: 1234,
        };
        let (op, args) = message_request(&params, &target);
        assert_eq!(op, "send_message");
        assert_eq!(args["chat_id"], 42);
        assert_eq!(args["timeout_ms"], 1234);
        assert!(args.get("reply_to").is_none(), "不该引用原消息：{args}");
        assert_eq!(
            args["input_message_content"]["video"]["video"],
            json!({"@type": "inputFileId", "id": 5})
        );
    }

    /// 频道：同一个媒体改成 `edit_message_media`，键是 `chat_id` + `message_id`。
    #[test]
    fn message_request_in_channel_becomes_edit_media() {
        let target = Target::edit(42, 99);
        let params = SendMessageParams {
            text: Some(Text::markdown("hi")),
            media: Some(Media::video(InputFile::id(5))),
            timeout_ms: 10,
        };
        let (op, args) = message_request(&params, &target);
        assert_eq!(op, "edit_message_media");
        assert_eq!(args["chat_id"], 42);
        assert_eq!(args["message_id"], 99);
        assert!(args.get("chat_id").is_some());
        assert!(args.get("reply_to").is_none());
    }

    /// 相册：op 名与 `input_message_contents` 数组，且**不带 `reply_to`**。
    #[test]
    fn album_request_builds_a_send_message_album_op_without_reply() {
        let target = Target::send(42);
        let params = SendAlbumParams {
            medias: vec![
                Media::photo(InputFile::id(1)),
                Media::photo(InputFile::id(2)),
            ],
            caption: Some(Text::markdown("c")),
            timeout_ms: 5,
        };
        let (op, args) = album_request(&params, &target);
        assert_eq!(op, "send_message_album");
        assert_eq!(args["timeout_ms"], 5);
        assert!(args.get("reply_to").is_none(), "不该引用原消息：{args}");
        assert_eq!(args["input_message_contents"].as_array().unwrap().len(), 2);
        assert_eq!(
            args["input_message_contents"][0]["photo"]["photo"],
            json!({"@type": "inputFileId", "id": 1})
        );
    }

    fn music() -> Music {
        Music {
            id: 1,
            id_str: "m".to_owned(),
            name: "歌名".to_owned(),
            url: "https://m/1".to_owned(),
            author: "演唱者".to_owned(),
            sec_uid: String::new(),
            avatar: String::new(),
            cover: String::new(),
        }
    }

    fn coauthor() -> Coauthor {
        Coauthor {
            avatar: None,
            nickname: None,
            role: None,
            uid: None,
            sec_uid: None,
            home_url: None,
        }
    }
}
