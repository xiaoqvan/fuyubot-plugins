//! 命令、自动识别与异步处理链。
//!
//! 两个入口共用同一条处理链：
//!
//! ```text
//! /music163 <关键词|链接|ID> [音质]   ──┐
//! /soda     <关键词|链接|ID> [音质]   ──┤
//! 消息里出现分享链接（非命令）         ──┴─▶ tokio 任务 ─▶ 搜索 / 解析 ─▶ 发音频
//! ```
//!
//! ## 线程模型（这里最容易写出事故）
//!
//! `updateNewMessage` / 命令回调跑在**宿主的接收循环线程**上。搜索要走网络、取流要
//! 下载几十 MB、发送还要等 TDLib 应答——在回调里同步做就把整个宿主堵死了。
//!
//! 所以回调里只做「解析命令参数 + `Runtime::submit`」，真正的活在插件自己的 tokio
//! 多线程 runtime 里 `.await`（HTTP 是 reqwest async、下载是流式 chunk、TDLib 等待是
//! `ResponseRouter` + `tokio::time::timeout`）。**没有固定 worker 线程 / 任务队列**。
//!
//! 停机顺序见 [`crate::runtime`]：退订 → 置取消标志 → 关闸并等在飞归零 → 关 runtime。
//!
//! ## 落点：不引用原消息 / 频道编辑原帖
//!
//! 普通会话的发送都**不引用 / 不回复原消息**（`sendMessage` 不带 `reply_to`）。
//! **频道帖**（TDLib `is_channel_post`）例外：命令 / 链接消息本身就是那条帖子，
//! 所以编辑它——先查 `getMessageProperties` 的编辑权限，再写占位 / 换媒体；
//! 没有权限就整段跳过（不解析、不编辑、不新发）。
//!
//! 群聊里「发送人是频道身份（`messageSenderChat`）且这条是转发」的消息在自动识别
//! 入口直接跳过；频道帖不看 `is_outgoing`（自家频道里发链接 / 命令也要生效）。
//!
//! ## SQL 缓存（软依赖）
//!
//! 装了 `fuyubot.sqlite` 时，发送成功后把该次音质对应的 **TDLib 文件 id / 封面 id +
//! 文本信息**写进缓存；下次同一个「音乐 id + 音质」命中就直接用 `inputFileId` 发送，
//! **不再请求接口、不再下载**。命中但发送失败（文件被 TDLib 清掉等）会删缓存并自动
//! 回落到完整流程。没装 / SQL 插件坏掉时所有缓存操作都是 no-op，一切照常。

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::Ordering;

use fuyubot_abi::{HostApi, Plugin, SubscriptionHandle};
use serde_json::{Value, json};

use crate::auth;
use crate::cache;
use crate::http;
use crate::netease;
use crate::runtime::Runtime;
use crate::send::{self, AudioMeta, Target, VideoMeta};
use crate::simplify::{
    Command, CommandSubscription, InputFile, SentMedia, channel_post_of, on_command, sent_media,
};
use crate::soda;
use crate::soda_crypto;
use crate::soda_video;

/// 网易云命令名。
pub const NETEASE_COMMAND: &str = "music163";
/// 汽水命令名。
pub const SODA_COMMAND: &str = "soda";

const NETEASE_DESCRIPTION: &str = "网易云音乐：搜索 / 解析链接或 ID，发音频";
const SODA_DESCRIPTION: &str = "汽水音乐：搜索 / 解析链接或 ID，发音频";

/// 宿主预置的**命令能力表**（`register` / `list`）。
///
/// 从 FuyuBot 0.1.5 起表归宿主；旧的 `fuyubot.help.commands` 只是兼容别名。
const HOST_COMMANDS: &str = "fuyubot.host.commands";

/// 搜索结果最多展示几条。
const SEARCH_LIMIT: usize = 8;

/// 网易云音频 CDN 的 Referer。
const NETEASE_REFERER: &str = "https://music.163.com/";
/// 汽水 CDN 的 Referer。
const SODA_REFERER: &str = "https://www.qishui.com/";

/// Telegram 单个文件的上限（普通账号 2GB）。
const MAX_VIDEO_BYTES: u64 = 2 * 1024 * 1024 * 1024;

/// 一条命令 / 一条链接要解析的东西。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Platform {
    Netease,
    Soda,
}

impl Platform {
    fn name(self) -> &'static str {
        match self {
            Self::Netease => "网易云音乐",
            Self::Soda => "汽水音乐",
        }
    }

    fn default_quality(self) -> &'static str {
        match self {
            Self::Netease => netease::DEFAULT_QUALITY,
            Self::Soda => soda::DEFAULT_QUALITY,
        }
    }

    fn quality_help(self) -> String {
        match self {
            Self::Netease => netease::quality_help(),
            Self::Soda => soda::quality_help(),
        }
    }

    fn parse_quality(self, name: &str) -> Option<&'static str> {
        match self {
            Self::Netease => netease::parse_quality(name),
            Self::Soda => soda::parse_quality(name),
        }
    }
}

/// 插件状态。
pub struct MusicPlugin {
    host: HostApi,
    runtime: Option<Arc<Runtime>>,
    /// 命令订阅守卫：`drop` 就退订。
    subscription: Option<CommandSubscription>,
    /// 扫链接订阅守卫。
    scanning: Option<ScanSubscription>,
}

impl Plugin for MusicPlugin {
    fn init(host: HostApi) -> Result<Self, String> {
        // 日志槽要尽早装：auth / http 两层的日志都靠它。
        crate::log::install(host);
        Ok(Self {
            host,
            runtime: None,
            subscription: None,
            scanning: None,
        })
    }

    fn start(&mut self, config_json: &str) -> Result<(), String> {
        let runtime = Runtime::new(self.host, config_json)?;

        // ① `/music163 …` 与 `/soda …`：回调里只解析参数 + submit。
        let host = self.host;
        let command_runtime = Arc::clone(&runtime);
        let subscription = on_command(host, move |command: &Command| {
            let platform = if command.is(NETEASE_COMMAND) {
                Platform::Netease
            } else if command.is(SODA_COMMAND) {
                Platform::Soda
            } else {
                return;
            };
            let (query, quality) = split_args(platform, &command.args);
            let job = Job {
                platform,
                target: Target::of(command),
                query,
                quality,
            };
            command_runtime.submit(move |rt| async move { process(rt, job).await });
        });
        self.subscription =
            Some(subscription.ok_or_else(|| "订阅 updateNewMessage 失败".to_owned())?);

        // ② 消息里的分享链接（非命令）。
        self.scanning = Some(scan_links(host, Arc::clone(&runtime)));

        // ③ 登记进 `/help`（软依赖：没装 help 也能用）。
        register_help(host, NETEASE_COMMAND, NETEASE_DESCRIPTION);
        register_help(host, SODA_COMMAND, SODA_DESCRIPTION);

        // ④ SQL 缓存异步初始化（`spawn_blocking`；失败绝不影响 start）。
        //    第一条命令若比这更快到，`cache_get` 里还会补一次 init（幂等、带锁）。
        runtime.submit(|rt| async move {
            let _ = rt.cache_init().await;
        });

        self.runtime = Some(runtime);
        crate::log::info("已就绪：/music163、/soda 与分享链接自动识别都已挂上（tokio 异步执行）");
        Ok(())
    }

    /// 软依赖：没有 sqlite 插件时两个命令也必须能用；命令表由宿主预置。
    fn soft_inject() -> Vec<String> {
        vec![cache::SERVICE.to_owned()]
    }

    fn stop(&mut self) {
        // 先断输入，再停执行引擎：反过来会留下「任务在跑、事件又进来」的窗口。
        self.subscription = None;
        self.scanning = None;
        if let Some(runtime) = self.runtime.take() {
            // 下载 / 解密循环按块看这个标志，尽快退出。
            runtime.cancel.store(true, Ordering::SeqCst);
            // 关闸 → 等在飞任务归零 → 关 tokio runtime（见 runtime 模块文档）。
            runtime.shutdown();
        }
    }
}

/// 一条待处理的任务。
///
/// `target` 决定发到哪儿：普通会话新发（不引用原消息），频道帖编辑原帖。
struct Job {
    platform: Platform,
    target: Target,
    query: String,
    quality: Option<&'static str>,
}

impl Job {
    /// 这个任务最终要发媒体（音频 / 视频），还是只回文本（搜索结果 / 用法 / 报错）。
    fn needs_media(&self) -> bool {
        !self.query.trim().is_empty() && looks_like_reference(self.platform, &self.query)
    }
}

/// 一个任务的全部处理：先验频道权限，再判断是不是「只给用法」，最后按平台跑。
///
/// 这里是 tokio 任务体，**不在宿主回调线程上**。普通会话发送不引用原消息；
/// 频道帖编辑原帖（没有权限就整段跳过）。
async fn process(runtime: Arc<Runtime>, mut job: Job) {
    if runtime.stopping() {
        return;
    }

    // 频道：先验权限。没有编辑媒体权限就整段跳过（不解析、不编辑、不新发）。
    if job.target.is_edit() {
        match send::channel_rights(&runtime, &job.target).await {
            Ok(rights) => {
                let needs_media = job.needs_media();
                if needs_media && !rights.can_edit_media {
                    crate::log::info("频道帖没有编辑媒体的权限，跳过");
                    return;
                }
                if !needs_media && !rights.can_edit_text {
                    crate::log::info("频道帖没有编辑文本的权限，跳过");
                    return;
                }
                job.target = job.target.with_rights(rights);
            }
            Err(error) => {
                crate::log::warn(&format!("查频道帖编辑权限失败，跳过：{error}"));
                return;
            }
        }
    }

    // 只给了命令名：直接回用法（不碰任何接口）。
    if job.query.trim().is_empty() {
        send_text_logged(&runtime, &job.target, &usage(job.platform)).await;
        return;
    }
    crate::log::debug(&format!(
        "{}：query=`{}` quality={:?}",
        job.platform.name(),
        crate::log::shorten(&job.query, 80),
        job.quality
    ));
    // 频道里解析媒体前先占位（只有文本编辑权限时才会真的改原帖）。
    if job.target.is_edit() && job.needs_media() {
        send::announce_parsing(&runtime, &job.target).await;
    }
    if let Err(error) = run(&runtime, &job).await {
        crate::log::error(&format!("{}处理失败：{error}", job.platform.name()));
        send_text_logged(&runtime, &job.target, &format!("❌ {error}")).await;
    }
}

/// 回一段文本；失败只记日志（频道里没文本权限时不该让任务整体报错）。
async fn send_text_logged(runtime: &Runtime, target: &Target, text: &str) {
    if let Err(error) = send::send_text(runtime, target, text).await {
        crate::log::warn(&format!("发送文本失败：{error}"));
    }
}

async fn run(runtime: &Runtime, job: &Job) -> Result<(), String> {
    match job.platform {
        Platform::Netease => run_netease(runtime, job).await,
        Platform::Soda => run_soda(runtime, job).await,
    }
}

async fn run_netease(runtime: &Runtime, job: &Job) -> Result<(), String> {
    let auth = runtime.auth.netease.clone().unwrap_or_default();
    if netease::song_id_of(&job.query).is_some() || netease::first_link(&job.query).is_some() {
        let id = netease::resolve_id(&runtime.http, &job.query).await?;
        let quality = job.quality.unwrap_or(netease::DEFAULT_QUALITY);
        return send_netease(runtime, &auth, &job.target, id, quality).await;
    }

    let tracks = netease::search(&runtime.http, &auth, &job.query, SEARCH_LIMIT).await?;
    let mut lines = vec![format!("🎵 网易云音乐 · 「{}」", job.query)];
    for (index, track) in tracks.iter().take(SEARCH_LIMIT).enumerate() {
        let album = if track.album.trim().is_empty() {
            String::new()
        } else {
            format!(" / {}", track.album)
        };
        lines.push(format!(
            "{}. {} - {}{}\n   ID {}",
            index + 1,
            track.name,
            track.artist_text(),
            album,
            track.id
        ));
    }
    lines.push(format!(
        "发送 /{NETEASE_COMMAND} <ID> [音质] 获取音频\n音质：{}（默认 {}）",
        netease::quality_help(),
        netease::DEFAULT_QUALITY
    ));
    send::send_text(runtime, &job.target, &lines.join("\n")).await
}

async fn run_soda(runtime: &Runtime, job: &Job) -> Result<(), String> {
    let auth = runtime.auth.soda.clone().unwrap_or_default();
    if soda::track_id_of(&job.query).is_some() || soda::first_link(&job.query).is_some() {
        match soda::classify_share(&runtime.http, &auth, &job.query).await? {
            soda::Share::Track(id) => {
                let quality = job.quality.unwrap_or(soda::DEFAULT_QUALITY);
                return send_soda(runtime, &auth, &job.target, &id, quality).await;
            }
            soda::Share::Video(id) => {
                return send_soda_video(runtime, &auth, &job.target, &id).await;
            }
        }
    }

    let tracks = soda::search(&runtime.http, &auth, &job.query, SEARCH_LIMIT).await?;
    let mut lines = vec![format!("🎵 汽水音乐 · 「{}」", job.query)];
    for (index, track) in tracks.iter().take(SEARCH_LIMIT).enumerate() {
        let album = if track.album.trim().is_empty() {
            String::new()
        } else {
            format!(" / {}", track.album)
        };
        lines.push(format!(
            "{}. {} - {}{}\n   ID {}",
            index + 1,
            track.name,
            track.artist_text(),
            album,
            track.id
        ));
    }
    lines.push(format!(
        "发送 /{SODA_COMMAND} <ID> [音质] 获取音频\n音质：{}（默认 {}）",
        soda::quality_help(),
        soda::DEFAULT_QUALITY
    ));
    send::send_text(runtime, &job.target, &lines.join("\n")).await
}

// ── 网易云：缓存命中 / 完整流程 ─────────────────────────────────────

async fn send_netease(
    runtime: &Runtime,
    auth: &auth::NeteaseAuth,
    target: &Target,
    id: u64,
    quality: &str,
) -> Result<(), String> {
    // 缓存键用**用户请求的档位**，不是接口返回的实际档位（降级后仍要能命中同一键）。
    let key = cache::key(&["netease", &id.to_string(), quality]);
    if let Some(doc) = runtime.cache_get(&key).await {
        match send_cached_audio(runtime, target, &doc).await {
            Ok(()) => {
                crate::log::debug(&format!(
                    "网易云 {id} 缓存命中（{quality}，跳过解析与下载）"
                ));
                return Ok(());
            }
            Err(error) => {
                crate::log::warn(&format!(
                    "网易云 {id} 缓存命中但发送失败，删缓存回落完整流程：{error}"
                ));
                runtime.cache_remove(&key).await;
            }
        }
    }

    let resolved = netease::resolve(&runtime.http, auth, id, quality).await?;
    let dir = runtime.cache_dir()?;
    let mut temp = TempFiles::default();

    let mut headers = vec![
        ("User-Agent", auth::NETEASE_USER_AGENT),
        ("Referer", NETEASE_REFERER),
    ];
    if !auth.cookie_header.is_empty() {
        headers.push(("Cookie", auth.cookie_header.as_str()));
    }

    let extension = audio_extension(&resolved.stream.format);
    crate::log::debug(&format!(
        "网易云 {} 下载音频（{} 字节，{}）",
        id, resolved.stream.size, extension
    ));
    let file = http::download(
        &runtime.http,
        &resolved.stream.url,
        &headers,
        &dir,
        extension,
        &runtime.cancel,
    )
    .await?;
    temp.push(file.clone());

    let cover = download_cover(runtime, &resolved.track.cover, &headers, &dir, &mut temp).await;

    let size = std::fs::metadata(&file)
        .map(|meta| meta.len())
        .unwrap_or(resolved.stream.size);
    let meta = AudioMeta {
        title: resolved.track.name.clone(),
        performer: resolved.track.artist_text(),
        album: resolved.track.album.clone(),
        link: resolved.track.link(),
        quality: netease::quality_label(&resolved.stream.level),
        bitrate: resolved.stream.bitrate,
        size,
        duration: resolved.track.duration_secs(),
        trial: resolved.stream.trial,
    };
    let result = send::send_audio(
        runtime,
        target,
        InputFile::local(file.to_string_lossy()),
        cover.map(|cover| InputFile::local(cover.to_string_lossy())),
        &meta,
    )
    .await?;
    store_audio_cache(runtime, &key, &meta, &result).await;
    Ok(())
}

// ── 汽水：缓存命中 / 完整流程 ───────────────────────────────────────

async fn send_soda(
    runtime: &Runtime,
    auth: &auth::SodaAuth,
    target: &Target,
    id: &str,
    quality: &str,
) -> Result<(), String> {
    let key = cache::key(&["soda", id, quality]);
    if let Some(doc) = runtime.cache_get(&key).await {
        match send_cached_audio(runtime, target, &doc).await {
            Ok(()) => {
                crate::log::debug(&format!("汽水 {id} 缓存命中（{quality}，跳过解析与下载）"));
                return Ok(());
            }
            Err(error) => {
                crate::log::warn(&format!(
                    "汽水 {id} 缓存命中但发送失败，删缓存回落完整流程：{error}"
                ));
                runtime.cache_remove(&key).await;
            }
        }
    }

    let resolved = soda::resolve(&runtime.http, auth, id, quality).await?;
    let dir = runtime.cache_dir()?;
    let mut temp = TempFiles::default();

    let mut headers = vec![("User-Agent", soda::CHROME_UA), ("Referer", SODA_REFERER)];
    if !auth.cookie.trim().is_empty() {
        headers.push(("Cookie", auth.cookie.trim()));
    }

    // 加密流先按密文落盘（扩展名要能让 TDLib 认出最终格式，这里统一 m4a），
    // 解密后再写一份明文。
    let encrypted = resolved.stream.is_encrypted();
    let download_ext = if encrypted {
        "m4a"
    } else {
        resolved.extension()
    };
    crate::log::debug(&format!(
        "汽水 {} 下载音频（{} 字节，{}，加密={encrypted}）",
        id, resolved.stream.size, download_ext
    ));
    let downloaded = http::download(
        &runtime.http,
        &resolved.stream.url,
        &headers,
        &dir,
        download_ext,
        &runtime.cancel,
    )
    .await?;
    temp.push(downloaded.clone());

    let file = if encrypted {
        // 明文另落一个名字：`x.m4a` 的 with_extension("m4a") 还是同一个路径，
        // 直接写会把刚下完的密文覆盖掉，最后连解密结果一起删掉。
        let plain_path = downloaded.with_extension(format!("dec.{}", resolved.extension()));
        // 解密是 CPU + 磁盘密集的大文件操作：丢进 spawn_blocking，循环里看取消标志。
        let input = downloaded.clone();
        let output = plain_path.clone();
        let play_auth = resolved.stream.play_auth.clone();
        let cancel = Arc::clone(&runtime.cancel);
        let decrypted = tokio::task::spawn_blocking(move || {
            soda_crypto::decrypt_file_cancellable(&input, &output, &play_auth, &cancel)
        })
        .await
        .map_err(|error| format!("解密任务失败：{error}"))?;
        decrypted?;
        temp.push(plain_path.clone());
        let _ = std::fs::remove_file(&downloaded);
        plain_path
    } else {
        downloaded
    };

    let cover = download_cover(runtime, &resolved.track.cover, &headers, &dir, &mut temp).await;

    let size = std::fs::metadata(&file).map(|meta| meta.len()).unwrap_or(0);
    let meta = AudioMeta {
        title: resolved.track.name.clone(),
        performer: resolved.track.artist_text(),
        album: resolved.track.album.clone(),
        link: resolved.track.link(),
        quality: soda::quality_label(&resolved.stream.quality),
        bitrate: resolved.stream.bitrate.max(0) as u64,
        size,
        duration: if resolved.stream.duration > 0.0 {
            resolved.stream.duration.round() as i64
        } else {
            resolved.track.duration
        },
        trial: resolved.preview,
    };
    let result = send::send_audio(
        runtime,
        target,
        InputFile::local(file.to_string_lossy()),
        cover.map(|cover| InputFile::local(cover.to_string_lossy())),
        &meta,
    )
    .await?;
    store_audio_cache(runtime, &key, &meta, &result).await;
    Ok(())
}

/// 汽水 UGC 音乐视频：下载 mp4 后按抖音插件那套文案发视频。
async fn send_soda_video(
    runtime: &Runtime,
    auth: &auth::SodaAuth,
    target: &Target,
    id: &str,
) -> Result<(), String> {
    let key = cache::key(&["soda-video", id]);
    if let Some(doc) = runtime.cache_get(&key).await {
        match send_cached_video(runtime, target, &doc).await {
            Ok(()) => {
                crate::log::debug(&format!("汽水视频 {id} 缓存命中（跳过解析与下载）"));
                return Ok(());
            }
            Err(error) => {
                crate::log::warn(&format!(
                    "汽水视频 {id} 缓存命中但发送失败，删缓存回落完整流程：{error}"
                ));
                runtime.cache_remove(&key).await;
            }
        }
    }

    let post = soda_video::resolve(&runtime.http, auth, id).await?;
    let dir = runtime.cache_dir()?;
    let mut temp = TempFiles::default();

    let mut headers = vec![("User-Agent", soda::CHROME_UA), ("Referer", SODA_REFERER)];
    if !auth.cookie.trim().is_empty() {
        headers.push(("Cookie", auth.cookie.trim()));
    }

    let file = http::download(
        &runtime.http,
        &post.url,
        &headers,
        &dir,
        "mp4",
        &runtime.cancel,
    )
    .await?;
    temp.push(file.clone());
    let size = std::fs::metadata(&file).map(|meta| meta.len()).unwrap_or(0);
    if size == 0 {
        return Err("汽水视频下载下来是空文件".to_owned());
    }
    if size > MAX_VIDEO_BYTES {
        return Err(format!(
            "汽水视频有 {:.1} GB，超过 Telegram 单个文件上限",
            size as f64 / 1024.0 / 1024.0 / 1024.0
        ));
    }
    crate::log::debug(&format!("汽水视频 {} 已下载 {} 字节", post.id, size));

    let cover = download_cover(runtime, &post.cover, &headers, &dir, &mut temp).await;

    let meta = VideoMeta {
        title: post.title,
        author: post.author,
        duration: post.duration,
        width: post.width,
        height: post.height,
        preview: post.preview,
    };
    let result = send::send_video(
        runtime,
        target,
        InputFile::local(file.to_string_lossy()),
        cover.map(|cover| InputFile::local(cover.to_string_lossy())),
        &meta,
    )
    .await?;
    if let Some(sent) = sent_media(&result)
        && sent.file_id.is_some()
    {
        runtime.cache_put(&key, &video_doc(&meta, &sent)).await;
    }
    Ok(())
}

// ── 缓存文档 ────────────────────────────────────────────────────────

/// 从 `sendMessage` 的结果里取文件 id，连同文本信息写进缓存。
///
/// 拿不到 `file.id`（TDLib 没给 / 不是媒体消息）就不写——没有 id 的缓存命中也没用。
async fn store_audio_cache(runtime: &Runtime, key: &str, meta: &AudioMeta, result: &Value) {
    let Some(sent) = sent_media(result) else {
        return;
    };
    if sent.file_id.is_none() {
        return;
    }
    runtime.cache_put(key, &audio_doc(meta, &sent)).await;
}

fn audio_doc(meta: &AudioMeta, sent: &SentMedia) -> Value {
    json!({
        "v": 1,
        "kind": "audio",
        "file_id": sent.file_id,
        "cover_id": sent.cover_id,
        "title": meta.title,
        "performer": meta.performer,
        "album": meta.album,
        "link": meta.link,
        "quality_label": meta.quality,
        "bitrate": meta.bitrate,
        "size": meta.size,
        "duration": meta.duration,
        "trial": meta.trial,
    })
}

fn video_doc(meta: &VideoMeta, sent: &SentMedia) -> Value {
    json!({
        "v": 1,
        "kind": "video",
        "file_id": sent.file_id,
        "cover_id": sent.cover_id,
        "title": meta.title,
        "author": meta.author,
        "width": meta.width,
        "height": meta.height,
        "duration": meta.duration,
        "preview": meta.preview,
    })
}

/// 命中音频缓存：直接用 `inputFileId` 发送（零网络、零下载）。
async fn send_cached_audio(runtime: &Runtime, target: &Target, doc: &Value) -> Result<(), String> {
    let file_id = doc
        .get("file_id")
        .and_then(Value::as_i64)
        .ok_or_else(|| "缓存里没有 file_id".to_owned())?;
    let cover_id = doc.get("cover_id").and_then(Value::as_i64);
    let meta = AudioMeta {
        title: text_of(doc, "title"),
        performer: text_of(doc, "performer"),
        album: text_of(doc, "album"),
        link: text_of(doc, "link"),
        quality: text_of(doc, "quality_label"),
        bitrate: u64_of(doc, "bitrate"),
        size: u64_of(doc, "size"),
        duration: i64_of(doc, "duration"),
        trial: doc.get("trial").and_then(Value::as_bool).unwrap_or(false),
    };
    send::send_audio(
        runtime,
        target,
        InputFile::id(file_id),
        cover_id.map(InputFile::id),
        &meta,
    )
    .await
    .map(|_| ())
}

/// 命中视频缓存：直接用 `inputFileId` 发送（零网络、零下载）。
async fn send_cached_video(runtime: &Runtime, target: &Target, doc: &Value) -> Result<(), String> {
    let file_id = doc
        .get("file_id")
        .and_then(Value::as_i64)
        .ok_or_else(|| "缓存里没有 file_id".to_owned())?;
    let cover_id = doc.get("cover_id").and_then(Value::as_i64);
    let meta = VideoMeta {
        title: text_of(doc, "title"),
        author: text_of(doc, "author"),
        duration: i64_of(doc, "duration"),
        width: i64_of(doc, "width"),
        height: i64_of(doc, "height"),
        preview: doc.get("preview").and_then(Value::as_bool).unwrap_or(false),
    };
    send::send_video(
        runtime,
        target,
        InputFile::id(file_id),
        cover_id.map(InputFile::id),
        &meta,
    )
    .await
    .map(|_| ())
}

fn text_of(doc: &Value, key: &str) -> String {
    doc.get(key)
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned()
}

fn i64_of(doc: &Value, key: &str) -> i64 {
    doc.get(key).and_then(Value::as_i64).unwrap_or(0)
}

fn u64_of(doc: &Value, key: &str) -> u64 {
    doc.get(key)
        .and_then(Value::as_u64)
        .or_else(|| {
            doc.get(key)
                .and_then(Value::as_i64)
                .map(|n| n.max(0) as u64)
        })
        .unwrap_or(0)
}

/// 下载封面；失败只记日志（没有封面也能发音频）。
async fn download_cover(
    runtime: &Runtime,
    url: &str,
    headers: &[(&str, &str)],
    dir: &Path,
    temp: &mut TempFiles,
) -> Option<PathBuf> {
    if url.trim().is_empty() {
        return None;
    }
    match http::download(&runtime.http, url, headers, dir, "jpg", &runtime.cancel).await {
        Ok(path) => {
            temp.push(path.clone());
            Some(path)
        }
        Err(error) => {
            crate::log::warn(&format!("封面下载失败（不影响音频）：{error}"));
            None
        }
    }
}

/// 音频文件的扩展名：TDLib 靠它判断 MIME。
fn audio_extension(format: &str) -> &'static str {
    match format.trim().to_ascii_lowercase().as_str() {
        "flac" => "flac",
        "wav" => "wav",
        "m4a" | "mp4" | "aac" | "alac" => "m4a",
        "ogg" | "opus" => "ogg",
        "mp3" | "" => "mp3",
        _ => "mp3",
    }
}

/// 临时文件守卫：drop 就删。
#[derive(Default)]
struct TempFiles(Vec<PathBuf>);

impl TempFiles {
    fn push(&mut self, path: PathBuf) {
        self.0.push(path);
    }
}

impl Drop for TempFiles {
    fn drop(&mut self) {
        for path in self.0.drain(..) {
            // Windows 上 TDLib 读完上传文件到真正松手之间有个短暂窗口，立刻删会
            // 拿到独占句柄的「拒绝访问」。重试几次足够（最多 500ms 的短磁盘操作），
            // 不必把失败当常态。
            for attempt in 0..5 {
                match std::fs::remove_file(&path) {
                    Ok(()) => break,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => break,
                    Err(error) if attempt == 4 => {
                        crate::log::warn(&format!("清理缓存文件失败 {}：{error}", path.display()));
                    }
                    Err(_) => std::thread::sleep(std::time::Duration::from_millis(100)),
                }
            }
        }
    }
}

fn usage(platform: Platform) -> String {
    match platform {
        Platform::Netease => format!(
            "用法：\n\
             /{NETEASE_COMMAND} <关键词> — 搜索\n\
             /{NETEASE_COMMAND} <链接或 ID> [音质] — 发音频\n\
             音质：{}（默认 {}）\n\
             例：/{NETEASE_COMMAND} 晴天 无损",
            platform.quality_help(),
            platform.default_quality()
        ),
        Platform::Soda => format!(
            "用法：\n\
             /{SODA_COMMAND} <关键词> — 搜索\n\
             /{SODA_COMMAND} <链接或 ID> [音质] — 发音频\n\
             音质：{}（默认 {}）\n\
             例：/{SODA_COMMAND} 晴天 lossless",
            platform.quality_help(),
            platform.default_quality()
        ),
    }
}

/// 一条命令在宿主命令表里的登记入参。
///
/// 两个命令都是普通用户命令，私聊 / 群 / 频道都能用；频道里命令消息本身就是原帖，
/// 解析结果用编辑接口写回去。
fn registration_payload(name: &str, description: &str) -> serde_json::Value {
    serde_json::json!({
        "name": name,
        "description": description,
        "role": "user",
        "scopes": ["private", "group", "channel"],
    })
}

/// 把两个命令登记进宿主命令表。失败只记日志。
fn register_help(host: HostApi, name: &str, description: &str) {
    let input = registration_payload(name, description).to_string();
    match host.call_service(HOST_COMMANDS, "register", &input) {
        Ok(reply) => {
            let ok = serde_json::from_str::<serde_json::Value>(&reply)
                .ok()
                .and_then(|value| value.get("ok").and_then(|ok| ok.as_bool()))
                .unwrap_or(false);
            if !ok {
                crate::log::warn(&format!("/{name} 登记进宿主命令表被拒：{reply}"));
            }
        }
        Err(error) => {
            crate::log::debug(&format!(
                "/{name} 没能登记进宿主命令表（{HOST_COMMANDS}）：{error}"
            ));
        }
    }
}

/// 把命令参数拆成「查询 + 音质」。
///
/// 规则（与参考实现的 `/music163 <id> [音质]` 对齐，同时允许关键词带空格）：
/// **只有前半段确实是链接或纯数字 ID** 时，最后一段才当音质。
///
/// 不能只看「最后一段长得像音质」：`high` / `best` / `flac` / `320` 都是常见关键词，
/// `/soda High`、`/music163 Shape of You high` 会把搜索词吃掉。
fn split_args(platform: Platform, args: &str) -> (String, Option<&'static str>) {
    let tokens: Vec<&str> = args.split_whitespace().collect();
    if tokens.len() >= 2
        && let Some(quality) = platform.parse_quality(tokens[tokens.len() - 1])
    {
        let head = tokens[..tokens.len() - 1].join(" ");
        if looks_like_reference(platform, &head) {
            return (head, Some(quality));
        }
    }
    (args.trim().to_owned(), None)
}

/// 这段文本能不能直接解析成一首歌（链接 / 短链 / ID）。
fn looks_like_reference(platform: Platform, text: &str) -> bool {
    match platform {
        Platform::Netease => {
            netease::song_id_of(text).is_some() || netease::first_link(text).is_some()
        }
        Platform::Soda => soda::track_id_of(text).is_some() || soda::first_link(text).is_some(),
    }
}

// ── 扫链接 ──────────────────────────────────────────────────────────

/// 一条活着的「扫链接」订阅：**drop 就退订**。
pub struct ScanSubscription {
    host: HostApi,
    handle: SubscriptionHandle,
    handler: Option<Box<Box<MessageFn>>>,
}

impl Drop for ScanSubscription {
    fn drop(&mut self) {
        if self.handle.is_valid() {
            self.host.off(self.handle);
        }
        // `off` 只把注册项摘掉，**不等在途回调返回**：宿主的事件总线是在锁外
        // 对着订阅快照调回调的，所以刚 off 完可能还有线程停在 `scan_trampoline` 里。
        // 这里故意不释放闭包（每个插件生命周期只有一份，泄漏量可以忽略），
        // 否则就是一条「退订后解引用已释放内存」的 UAF。
        if let Some(handler) = self.handler.take() {
            std::mem::forget(handler);
        }
    }
}

type MessageFn = dyn Fn(&Message) + Send + Sync;

/// 一条待判断的消息（只带用得上的字段）。
#[derive(Debug, Clone)]
struct Message {
    chat_id: i64,
    message_id: i64,
    text: String,
    is_outgoing: bool,
    /// 发送者是频道（`messageSenderChat`）——群里匿名频道马甲也满足这一点。
    sender_is_chat: bool,
    /// 这条消息是转发的（`forward_info` 存在，来源可能被隐藏）。
    is_forwarded: bool,
    /// 真正的频道帖：TDLib `is_channel_post` 为真（见 [`channel_post_of`]）。
    is_channel_post: bool,
}

/// 自动识别要不要处理这条消息；返回「发到哪儿 + 链接」。
///
/// 过滤顺序：
/// 1. 自己发的不解析；频道帖例外（自家频道里发链接就是要插件编辑原帖）；
/// 2. `/…` 开头的是命令，交给命令入口，不抢；
/// 3. **群里由频道身份发出、而且是转发的消息**：发送人是 `messageSenderChat`
///    且 `forward_info` 存在——不是在群里正常发链接，不替别人的转发链产出内容。
fn auto_job(message: &Message) -> Option<Job> {
    if message.is_outgoing && !message.is_channel_post {
        return None;
    }
    if message.text.trim_start().starts_with('/') {
        return None;
    }
    if !message.is_channel_post && message.sender_is_chat && message.is_forwarded {
        return None;
    }
    let (platform, link) = if let Some(link) = netease::first_link(&message.text) {
        (Platform::Netease, link)
    } else {
        (Platform::Soda, soda::first_link(&message.text)?)
    };
    let target = if message.is_channel_post {
        Target::edit(message.chat_id, message.message_id)
    } else {
        Target::send(message.chat_id)
    };
    Some(Job {
        platform,
        target,
        query: link,
        quality: None,
    })
}

/// 订阅 `updateNewMessage`，把含分享链接的**非命令**消息交给处理链。
///
/// 回调里只做「认链接 + submit」，不发网络请求。
///
/// 闭包只持有 `Weak<Runtime>`：`ScanSubscription::drop` 为了躲开「退订后在途回调」
/// 的 UAF 会故意泄漏闭包；如果这里拿 `Arc`，`Runtime`（以及它的响应路由订阅）就
/// 永远释放不掉了。
fn scan_links(host: HostApi, runtime: Arc<Runtime>) -> ScanSubscription {
    let weak = Arc::downgrade(&runtime);
    let mut handler: Box<Box<MessageFn>> = Box::new(Box::new(move |message: &Message| {
        let Some(job) = auto_job(message) else {
            return;
        };
        let Some(runtime) = weak.upgrade() else {
            return;
        };
        runtime.submit(move |rt| async move { process(rt, job).await });
    }));
    // 与 `on_command` 同一套 user_data 约定：递进去的是**内层 Box 的地址**
    // （只解一层；外层 Box 活着它就稳定）。
    let user_data = (&raw mut *handler).cast::<std::ffi::c_void>();
    let handle = host.on("updateNewMessage", None, scan_trampoline, user_data);
    ScanSubscription {
        host,
        handle,
        handler: Some(handler),
    }
}

/// 扫链接的 C 回调：把负载解成 [`Message`]，再交给闭包。
extern "C-unwind" fn scan_trampoline(
    user_data: *mut std::ffi::c_void,
    payload: *const std::ffi::c_char,
) -> i32 {
    if user_data.is_null() || payload.is_null() {
        return -1;
    }
    // SAFETY: `user_data` 是 `scan_links` 里那个 Box，订阅活多久它就活多久。
    let handler = unsafe { &*(user_data as *const Box<MessageFn>) };
    // SAFETY: 宿主保证 `payload` 是 NUL 结尾的临时字符串。
    let text = unsafe { std::ffi::CStr::from_ptr(payload) }.to_string_lossy();
    let Ok(update) = serde_json::from_str::<serde_json::Value>(&text) else {
        return 0;
    };
    if let Some(message) = message_of(&update) {
        handler(&message);
    }
    0
}

/// 从 `updateNewMessage` 的负载里取出一条消息；不是文本消息就 `None`。
fn message_of(update: &serde_json::Value) -> Option<Message> {
    let value = update.get("message")?;
    let content = value.get("content")?;
    if content.get("@type").and_then(|kind| kind.as_str()) != Some("messageText") {
        return None;
    }
    let text = content.get("text")?.get("text")?.as_str()?;
    let sender_is_chat = value
        .get("sender_id")
        .and_then(|sender| sender.get("@type"))
        .and_then(|kind| kind.as_str())
        == Some("messageSenderChat");
    // 转发消息：`forward_info` 存在即可（来源被隐藏时 `origin` 可能为 null）。
    let is_forwarded = value
        .get("forward_info")
        .is_some_and(|info| !info.is_null());
    Some(Message {
        chat_id: value.get("chat_id")?.as_i64()?,
        message_id: value.get("id")?.as_i64()?,
        text: text.to_owned(),
        is_outgoing: value
            .get("is_outgoing")
            .and_then(|flag| flag.as_bool())
            .unwrap_or(false),
        sender_is_chat,
        is_forwarded,
        is_channel_post: channel_post_of(value),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quality_is_the_last_token_only_when_it_is_known() {
        assert_eq!(
            split_args(Platform::Netease, "186016 lossless"),
            ("186016".to_owned(), Some("lossless"))
        );
        assert_eq!(
            split_args(Platform::Netease, "周杰伦 晴天"),
            ("周杰伦 晴天".to_owned(), None),
            "最后一段不是音质时整串都是查询"
        );
        assert_eq!(
            split_args(
                Platform::Netease,
                "https://music.163.com/song?id=1 jymaster"
            ),
            (
                "https://music.163.com/song?id=1".to_owned(),
                Some("jymaster")
            )
        );
        assert_eq!(
            split_args(Platform::Soda, "7304719759323564095 highest"),
            ("7304719759323564095".to_owned(), Some("highest"))
        );
    }

    #[test]
    fn a_lone_quality_word_is_a_search_query() {
        // `/music163 lossless`、`/soda High` 都是正常搜索词，不能当成「只给了音质」。
        assert_eq!(
            split_args(Platform::Netease, "lossless"),
            ("lossless".to_owned(), None)
        );
        assert_eq!(
            split_args(Platform::Soda, "High"),
            ("High".to_owned(), None)
        );
        assert_eq!(
            split_args(Platform::Netease, "320"),
            ("320".to_owned(), None)
        );
        assert_eq!(split_args(Platform::Netease, ""), (String::new(), None));
    }

    #[test]
    fn a_quality_word_after_a_plain_keyword_is_not_a_quality() {
        // 尾词像音质，但前半段不是链接 / ID —— 整串都是搜索词。
        assert_eq!(
            split_args(Platform::Netease, "Shape of You high"),
            ("Shape of You high".to_owned(), None)
        );
        assert_eq!(
            split_args(Platform::Soda, "Lo-Fi best"),
            ("Lo-Fi best".to_owned(), None)
        );
    }

    #[test]
    fn usage_mentions_the_command_and_its_qualities() {
        let text = usage(Platform::Netease);
        assert!(text.contains("/music163 <关键词>"), "{text}");
        assert!(text.contains("exhigh"), "{text}");
        let text = usage(Platform::Soda);
        assert!(text.contains("/soda <关键词>"), "{text}");
        assert!(text.contains("lossless"), "{text}");
    }

    #[test]
    fn audio_extension_is_always_a_mime_recognisable_one() {
        assert_eq!(audio_extension("mp3"), "mp3");
        assert_eq!(audio_extension("flac"), "flac");
        assert_eq!(audio_extension("MP4"), "m4a");
        assert_eq!(audio_extension(""), "mp3");
        assert_eq!(audio_extension("weird"), "mp3");
    }

    #[test]
    fn update_new_message_is_read_into_a_message() {
        let chat_id: i64 = -1001234567890;
        let update = serde_json::json!({
            "@type": "updateNewMessage",
            "message": {
                "@type": "message",
                "id": 22,
                "chat_id": chat_id,
                "is_outgoing": false,
                "content": {
                    "@type": "messageText",
                    "text": { "@type": "formattedText", "text": "https://www.qishui.com/track/1", "entities": [] },
                },
            },
        });
        let message = message_of(&update).expect("文本消息应当解得出");
        assert_eq!(message.chat_id, chat_id);
        assert!(!message.is_outgoing);
        assert!(!message.is_channel_post);
        assert!(!message.sender_is_chat);
        assert!(!message.is_forwarded);
    }

    /// 自动识别的过滤与频道落点：与抖音插件同一套口径。
    #[test]
    fn channel_posts_and_channel_identity_forwards_are_classified() {
        let update = |chat_id: i64,
                      outgoing: bool,
                      is_channel_post: bool,
                      sender: serde_json::Value,
                      forward: Option<serde_json::Value>| {
            let mut message = serde_json::json!({
                "@type": "message",
                "id": 9,
                "chat_id": chat_id,
                "is_outgoing": outgoing,
                "is_channel_post": is_channel_post,
                "sender_id": sender,
                "content": {
                    "@type": "messageText",
                    "text": { "@type": "formattedText", "text": "https://music.163.com/song?id=1", "entities": [] },
                },
            });
            if let Some(forward) = forward {
                message["forward_info"] = forward;
            }
            serde_json::json!({ "@type": "updateNewMessage", "message": message })
        };

        let channel_persona = serde_json::json!({
            "@type": "messageSenderChat",
            "chat_id": -100999_i64
        });
        let forwarded = serde_json::json!({
            "origin": { "@type": "messageOriginChannel", "chat_id": -100999_i64, "message_id": 1 }
        });
        let user = serde_json::json!({ "@type": "messageSenderUser", "user_id": 42 });

        // 群 + 频道身份 + 转发 → 跳过
        let group_forward = message_of(&update(
            -100888,
            false,
            false,
            channel_persona.clone(),
            Some(forwarded.clone()),
        ))
        .unwrap();
        assert!(
            auto_job(&group_forward).is_none(),
            "群里频道身份发出的转发消息不该自动解析"
        );

        // 群 + 频道身份但非转发 → 照常
        let group_plain = message_of(&update(
            -100888,
            false,
            false,
            channel_persona.clone(),
            None,
        ))
        .unwrap();
        assert!(auto_job(&group_plain).is_some());

        // 普通成员转发 → 照常
        let user_forward = message_of(&update(
            -100888,
            false,
            false,
            user,
            Some(forwarded.clone()),
        ))
        .unwrap();
        assert!(auto_job(&user_forward).is_some());

        // 自家频道自己发的帖子：outgoing 也要解析，并走编辑
        let channel_post = message_of(&update(
            -100777,
            true,
            true,
            serde_json::json!({ "@type": "messageSenderChat", "chat_id": -100777_i64 }),
            Some(forwarded),
        ))
        .unwrap();
        let job = auto_job(&channel_post).expect("频道帖要解析");
        assert!(job.target.is_edit(), "频道帖走编辑");
    }

    #[test]
    fn non_text_messages_are_ignored() {
        let update = serde_json::json!({
            "@type": "updateNewMessage",
            "message": { "@type": "message", "id": 1, "chat_id": 1,
                "content": { "@type": "messagePhoto", "photo": {} } },
        });
        assert!(message_of(&update).is_none());
        assert!(message_of(&serde_json::json!({})).is_none());
    }

    #[test]
    fn sqlite_is_the_only_soft_dependency() {
        assert_eq!(
            <MusicPlugin as Plugin>::soft_inject(),
            vec![cache::SERVICE.to_owned()]
        );
        assert!(<MusicPlugin as Plugin>::inject().is_empty());
    }

    #[test]
    fn the_service_names_are_pinned() {
        assert_eq!(HOST_COMMANDS, "fuyubot.host.commands");
        assert_eq!(cache::SERVICE, "fuyubot.sqlite");
    }

    /// 登记入参带 `role` / `scopes`：宿主闸门与 `/help` 都按它们过滤。
    #[test]
    fn registration_payload_carries_role_and_scopes() {
        let input = registration_payload(NETEASE_COMMAND, NETEASE_DESCRIPTION);
        assert_eq!(input["name"], "music163");
        assert_eq!(input["role"], "user");
        assert_eq!(
            input["scopes"],
            serde_json::json!(["private", "group", "channel"]),
            "频道里命令要能编辑原帖"
        );
    }

    #[test]
    fn command_names_are_stable() {
        // 改了它们，用户的 /music163、/soda 就失灵了。
        assert_eq!(NETEASE_COMMAND, "music163");
        assert_eq!(SODA_COMMAND, "soda");
        assert_eq!(Platform::Netease.name(), "网易云音乐");
        assert_eq!(Platform::Soda.name(), "汽水音乐");
    }

    /// `user_data` 的取址层数曾经写错过（解两层），症状是订阅从来不触发且不报错。
    /// 这里钉的是这条指针路径真的能调到闭包。
    #[test]
    fn user_data_round_trip_reaches_the_closure() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        static CALLS: AtomicUsize = AtomicUsize::new(0);
        let mut owned: Box<Box<MessageFn>> = Box::new(Box::new(|_message: &Message| {
            CALLS.fetch_add(1, Ordering::SeqCst);
        }));
        let user_data = (&raw mut *owned).cast::<std::ffi::c_void>();
        let back = user_data as *const Box<MessageFn>;
        let handler = unsafe { &*back };
        handler(&Message {
            chat_id: 1,
            message_id: 1,
            text: "x".to_owned(),
            is_outgoing: false,
            sender_is_chat: false,
            is_forwarded: false,
            is_channel_post: false,
        });
        assert_eq!(CALLS.load(Ordering::SeqCst), 1, "回调必须真的调到闭包");
    }

    /// 缓存文档必须带着命中重发所需的全部字段。
    #[test]
    fn cache_documents_round_trip_through_json() {
        let meta = AudioMeta {
            title: "晴天".to_owned(),
            performer: "周杰伦".to_owned(),
            album: "叶惠美".to_owned(),
            link: "https://music.163.com/song?id=186016".to_owned(),
            quality: "极高（exhigh）".to_owned(),
            bitrate: 320_000,
            size: 10 * 1024 * 1024,
            duration: 269,
            trial: false,
        };
        let sent = SentMedia {
            kind: crate::simplify::KIND_AUDIO,
            file_id: Some(42),
            cover_id: Some(43),
            width: 0,
            height: 0,
            duration: 269,
        };
        let doc = audio_doc(&meta, &sent);
        assert_eq!(doc["v"], 1);
        assert_eq!(doc["kind"], "audio");
        assert_eq!(doc["file_id"], 42);
        assert_eq!(doc["cover_id"], 43);
        assert_eq!(doc["title"], "晴天");
        assert_eq!(doc["performer"], "周杰伦");
        assert_eq!(doc["album"], "叶惠美");
        assert_eq!(doc["link"], "https://music.163.com/song?id=186016");
        assert_eq!(doc["quality_label"], "极高（exhigh）");
        assert_eq!(doc["bitrate"], 320_000);
        assert_eq!(doc["size"], 10 * 1024 * 1024);
        assert_eq!(doc["duration"], 269);
        assert_eq!(doc["trial"], false);
        // 反过来也读得出来（命中路径依赖这些字段）。
        assert_eq!(text_of(&doc, "title"), "晴天");
        assert_eq!(u64_of(&doc, "bitrate"), 320_000);
        assert_eq!(i64_of(&doc, "duration"), 269);
    }

    #[test]
    fn video_cache_documents_carry_playback_shape() {
        let meta = VideoMeta {
            title: "塔不灭".to_owned(),
            author: "永川塔菲".to_owned(),
            duration: 33,
            width: 1920,
            height: 1080,
            preview: true,
        };
        let sent = SentMedia {
            kind: crate::simplify::KIND_VIDEO,
            file_id: Some(7),
            cover_id: None,
            width: 1920,
            height: 1080,
            duration: 33,
        };
        let doc = video_doc(&meta, &sent);
        assert_eq!(doc["kind"], "video");
        assert_eq!(doc["file_id"], 7);
        assert_eq!(doc["width"], 1920);
        assert_eq!(doc["height"], 1080);
        assert_eq!(doc["duration"], 33);
        assert_eq!(doc["preview"], true);
        assert!(doc["cover_id"].is_null(), "没封面也要能存");
    }

    /// 缓存键必须用**用户请求的档位**（不是接口降级后的实际档位）。
    #[test]
    fn cache_keys_are_stable_and_quality_scoped() {
        assert_eq!(
            cache::key(&["netease", "186016", "lossless"]),
            "netease:186016:lossless"
        );
        assert_eq!(
            cache::key(&["soda", "7304719759323564095", "best"]),
            "soda:7304719759323564095:best"
        );
        assert_eq!(
            cache::key(&["soda-video", "7625615883351772468"]),
            "soda-video:7625615883351772468"
        );
    }
}
