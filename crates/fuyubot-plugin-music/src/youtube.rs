//! YouTube / YouTube Music：InnerTube 取流 + WEB_REMIX 搜索。
//!
//! ## 为什么是 ANDROID_VR
//!
//! YouTube 的播放接口（youtubei/v1/player）按客户端下发不同的流。网页端 / 普通
//! Android 端现在经常要求 PO token 或签名解密；ANDROID_VR（Oculus Quest 客户端）
//! 仍然直接下发 googlevideo 直链（itag 139/140 …），没有 signatureCipher，
//! 也不需要 PO token。公开项目 yt-innertube 用的就是这套。
//!
//! 取流失败最常见的原因是 YouTube 的 Sign in to confirm you're not a bot：
//! 数据中心 IP 上很常见，住宅网络一般不会。真遇到时可以在
//! [plugin.xiaoqvan.music] 里配 youtube cookie（见 README）。
//!
//! ## 只管两件事
//!
//! | 能力 | 入口 |
//! | --- | --- |
//! | 搜索 | [search]（music.youtube.com 的 WEB_REMIX 搜索） |
//! | 单曲取流 | [resolve]（视频 ID → 音频直链） |

use serde_json::{Value, json};

use crate::http::{self, Agent};
use crate::log;

/// player 端点：ANDROID_VR 客户端。Key 是公开的 Android 客户端 key。
const PLAYER_URL: &str =
    "https://www.youtube.com/youtubei/v1/player?key=AIzaSyA8eiZmM1FaDVjRy-df2KTyQ_vz_yYM39w";
/// WEB_REMIX 搜索（music.youtube.com）。
const SEARCH_URL: &str =
    "https://music.youtube.com/youtubei/v1/search?key=AIzaSyAO_FJ2SlqU8Q4STEHLGCilw_Y9_11qcW8";
/// WEB_REMIX 浏览（播放列表）。
const BROWSE_URL: &str =
    "https://music.youtube.com/youtubei/v1/browse?key=AIzaSyAO_FJ2SlqU8Q4STEHLGCilw_Y9_11qcW8";
/// ANDROID_VR 的 UA：player 请求与 googlevideo 下载必须一致。
pub const ANDROID_VR_USER_AGENT: &str =
    "com.google.android.apps.youtube.vr.oculus/1.60.19 (Linux; U; Android 12; en-US; Quest 3) gzip";
/// IOS 客户端 UA。
const IOS_USER_AGENT: &str =
    "com.google.ios.youtube/20.10.4 (iPhone16,2; U; CPU iOS 18_3_1 like Mac OS X;)";
/// 普通 ANDROID 客户端 UA。
const ANDROID_USER_AGENT: &str = "com.google.android.youtube/20.10.38 (Linux; U; Android 14) gzip";

/// 网页搜索 / oEmbed 用的普通浏览器 UA。
pub const BROWSER_USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36";

/// 一个可选音质。实际都是 m4a（mp4a），只是码率档不同。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Quality {
    pub key: &'static str,
    pub label: &'static str,
}

/// 可用音质（顺序 = 帮助信息展示顺序）。
pub const QUALITIES: &[Quality] = &[
    Quality {
        key: "best",
        label: "最佳",
    },
    Quality {
        key: "medium",
        label: "标准",
    },
    Quality {
        key: "low",
        label: "流畅",
    },
];

/// 默认音质。YouTube 单曲最高通常就是 itag 140（128 kbps）。
pub const DEFAULT_QUALITY: &str = "best";

/// 解析用户输入的音质名；别名也认。
pub fn parse_quality(name: &str) -> Option<&'static str> {
    let name = name.trim().to_ascii_lowercase();
    let canonical = match name.as_str() {
        "best" | "high" | "highest" | "最佳" | "最高" | "高" => "best",
        "medium" | "mid" | "standard" | "标准" | "中" => "medium",
        "low" | "流畅" | "低" => "low",
        _ => return None,
    };
    QUALITIES
        .iter()
        .find(|quality| quality.key == canonical)
        .map(|quality| quality.key)
}

/// 音质 key → 展示名。
pub fn quality_label(key: &str) -> String {
    QUALITIES
        .iter()
        .find(|quality| quality.key.eq_ignore_ascii_case(key))
        .map(|quality| format!("{}（{}）", quality.label, quality.key))
        .unwrap_or_else(|| key.to_owned())
}

/// 可用音质列表（帮助 / 报错文案用）。
pub fn quality_help() -> String {
    QUALITIES
        .iter()
        .map(|quality| quality.key)
        .collect::<Vec<_>>()
        .join(" / ")
}

/// 一条 YouTube / YouTube Music 单曲的展示信息。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Track {
    pub id: String,
    pub name: String,
    pub artists: Vec<String>,
    pub album: String,
    pub cover: String,
    /// 秒。
    pub duration: i64,
    /// 搜索结果的 MUSIC_VIDEO_TYPE_*（ATV / OMV / UGC …）；取流时为空。
    pub video_type: String,
}

impl Track {
    pub fn artist_text(&self) -> String {
        if self.artists.is_empty() {
            "未知歌手".to_owned()
        } else {
            self.artists.join("/")
        }
    }

    pub fn link(&self) -> String {
        format!("https://music.youtube.com/watch?v={}", self.id)
    }
}

/// 实际拿到的音频流。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Stream {
    pub url: String,
    pub itag: u64,
    pub mime: String,
    pub bitrate: u64,
    pub size: u64,
}

impl Stream {
    /// TDLib 靠扩展名判 MIME。
    pub fn extension(&self) -> &'static str {
        extension_of(&self.mime)
    }
}

/// mimeType → 落盘扩展名。
pub fn extension_of(mime: &str) -> &'static str {
    if mime.contains("mp4") {
        "m4a"
    } else if mime.contains("webm") {
        "webm"
    } else {
        "m4a"
    }
}

/// 解析 + 取流的最终结果。
#[derive(Debug, Clone)]
pub struct Resolved {
    pub track: Track,
    pub stream: Stream,
}

// ── 链接 / ID ───────────────────────────────────────────────────────

/// 视频 ID：11 个 [A-Za-z0-9_-]。
fn clean_id(raw: &str) -> Option<String> {
    let id: String = raw
        .trim()
        .chars()
        .take_while(|ch| ch.is_ascii_alphanumeric() || *ch == '-' || *ch == '_')
        .collect();
    (id.len() == 11).then_some(id)
}

fn is_youtube_host(url: &str) -> bool {
    crate::url::host_of(url).is_some_and(|host| {
        host == "youtu.be"
            || host.ends_with(".youtu.be")
            || host == "youtube.com"
            || host.ends_with(".youtube.com")
            || host == "youtube-nocookie.com"
            || host.ends_with(".youtube-nocookie.com")
    })
}

/// 文本里第一个 YouTube 链接（含 youtu.be 短链、music.youtube.com）。
pub fn first_link(text: &str) -> Option<String> {
    crate::url::first_link_where(text, is_youtube_host)
}

/// 文本里第一个 **YouTube Music** 链接（只有 music.youtube.com）。
///
/// 自动识别用它而不是 first_link：聊天里的 youtu.be / youtube.com 普通视频
/// 不该被音乐插件抢走；那类链接交给 /ytmusic 命令显式触发。
pub fn first_music_link(text: &str) -> Option<String> {
    crate::url::first_link_where(text, |url| {
        crate::url::host_of(url)
            .is_some_and(|host| host == "music.youtube.com" || host.ends_with(".music.youtube.com"))
    })
}

/// 从「链接 / 纯视频 ID」里认出视频 ID。
///
/// 支持：youtu.be/ID、youtube.com/watch?v=ID、music.youtube.com/watch?v=ID、
/// /shorts/ID、/embed/ID、/live/ID、/v/ID、youtube:video:ID、以及裸 11 位 ID。
pub fn video_id_of(text: &str) -> Option<String> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    if let Some(rest) = text.strip_prefix("youtube:video:") {
        return clean_id(rest);
    }
    if !text.to_ascii_lowercase().starts_with("http") {
        return clean_id(text);
    }
    let host = crate::url::host_of(text)?;
    if host == "youtu.be" || host.ends_with(".youtu.be") {
        let rest = text.split_once("://")?.1;
        let path = rest.split(['?', '#']).next()?;
        return clean_id(path.split('/').nth(1)?);
    }
    if !(host == "youtube.com"
        || host.ends_with(".youtube.com")
        || host == "youtube-nocookie.com"
        || host.ends_with(".youtube-nocookie.com"))
    {
        return None;
    }
    if let Some(value) = crate::url::query_value(text, "v") {
        return clean_id(&value);
    }
    let rest = text.split_once("://")?.1;
    let path = rest.split(['?', '#']).next()?;
    for marker in ["/shorts/", "/embed/", "/live/", "/v/"] {
        if let Some(rest) = path.split(marker).nth(1) {
            return clean_id(rest.split('/').next()?);
        }
    }
    None
}

/// YouTube 播放列表 ID（list= 查询参数）。
pub fn playlist_id_of(text: &str) -> Option<String> {
    let text = text.trim();
    if !text.to_ascii_lowercase().starts_with("http") || !is_youtube_host(text) {
        return None;
    }
    let list = crate::url::query_value(text, "list")?;
    (!list.trim().is_empty()).then(|| list.trim().to_owned())
}

// ── InnerTube 请求 ──────────────────────────────────────────────────

/// ANDROID_VR 客户端上下文。
fn client_context() -> Value {
    json!({
        "client": {
            "clientName": "ANDROID_VR",
            "clientVersion": "1.60.19",
            "deviceMake": "Oculus",
            "deviceModel": "Quest 3",
            "osName": "Android",
            "osVersion": "12",
            "androidSdkVersion": 32,
            "hl": "en",
            "gl": "US",
            "userAgent": ANDROID_VR_USER_AGENT,
        }
    })
}

/// WEB_REMIX（music.youtube.com）客户端上下文。
fn webmix_context() -> Value {
    json!({
        "client": {
            "clientName": "WEB_REMIX",
            "clientVersion": "1.20250101.01.00",
            "hl": "en",
            "gl": "US",
        }
    })
}

/// 发一个 InnerTube POST，返回 JSON。
///
/// cookie 只在这里拼（去掉 CR/LF，避免 header 注入）；空串就不带 Cookie 头。
async fn post_innertube(
    agent: &Agent,
    url: &str,
    body: &Value,
    cookie: &str,
    user_agent: &str,
    origin: &str,
    referer: &str,
) -> Result<Value, String> {
    let cookie = cookie.replace(['\r', '\n'], "");
    let mut headers = vec![
        ("User-Agent", user_agent),
        ("Accept", "*/*"),
        ("Origin", origin),
        ("Referer", referer),
    ];
    let cookie = cookie.trim();
    if !cookie.is_empty() {
        headers.push(("Cookie", cookie));
    }
    let payload = body.to_string();
    let reply = http::post_json(agent, url, payload.as_bytes(), &headers).await?;
    if !(200..300).contains(&reply.status) {
        return Err(format!(
            "YouTube 接口 HTTP {}：{}",
            reply.status,
            log::shorten(&reply.text(), 160)
        ));
    }
    reply.json()
}

/// 除 ANDROID_VR 之外再准备两个客户端：某一家的流被风控时换一家再试。
///
/// 这和 yt-dlp 默认的多 player_client 策略一致。注意：同一 IP 上被
/// "Sign in to confirm you're not a bot" 拦下时，换客户端通常也没用
/// （风控按 IP + 视频算），真正的解法仍是 cookie 或住宅网络。
fn client_profiles() -> Vec<(&'static str, Value, &'static str)> {
    vec![
        ("ANDROID_VR", client_context(), ANDROID_VR_USER_AGENT),
        ("IOS", ios_context(), IOS_USER_AGENT),
        ("ANDROID", android_context(), ANDROID_USER_AGENT),
    ]
}

/// IOS 客户端上下文。
fn ios_context() -> Value {
    json!({
        "client": {
            "clientName": "IOS",
            "clientVersion": "20.10.4",
            "deviceMake": "Apple",
            "deviceModel": "iPhone16,2",
            "osName": "iPhone",
            "osVersion": "18.3.1.22D72",
            "hl": "en",
            "gl": "US",
            "userAgent": IOS_USER_AGENT,
        }
    })
}

/// 普通 ANDROID 客户端上下文。
fn android_context() -> Value {
    json!({
        "client": {
            "clientName": "ANDROID",
            "clientVersion": "20.10.38",
            "androidSdkVersion": 30,
            "osName": "Android",
            "osVersion": "14",
            "hl": "en",
            "gl": "US",
            "userAgent": ANDROID_USER_AGENT,
        }
    })
}

/// 一次 player 请求的失败：网络层失败没必要换客户端；播放状态失败才值得换。
enum PlayerFailure {
    Transport(String),
    Playability(String),
}

/// 用某个客户端请求一次 player。
async fn player_once(
    agent: &Agent,
    cookie: &str,
    video_id: &str,
    context: &Value,
    user_agent: &str,
) -> Result<Value, PlayerFailure> {
    let body = json!({
        "context": context,
        "videoId": video_id,
        "contentCheckOk": true,
        "racyCheckOk": true,
    });
    let value = post_innertube(
        agent,
        PLAYER_URL,
        &body,
        cookie,
        user_agent,
        "https://www.youtube.com",
        "https://www.youtube.com/",
    )
    .await
    .map_err(PlayerFailure::Transport)?;
    let status = value
        .pointer("/playabilityStatus/status")
        .and_then(Value::as_str)
        .unwrap_or("");
    if status != "OK" {
        let reason = value
            .pointer("/playabilityStatus/reason")
            .and_then(Value::as_str)
            .unwrap_or("");
        return Err(PlayerFailure::Playability(playability_message(
            status, reason,
        )));
    }
    Ok(value)
}

/// player：按客户端顺序试，播放状态失败就换下一个。
async fn player(agent: &Agent, cookie: &str, video_id: &str) -> Result<Value, String> {
    let mut last = String::from("YouTube 没有返回播放状态");
    for (name, context, user_agent) in client_profiles() {
        match player_once(agent, cookie, video_id, &context, user_agent).await {
            Ok(value) => {
                log::debug(&format!("YouTube {video_id} 用 {name} 客户端取流成功"));
                return Ok(value);
            }
            // 网络层失败重试也没意义，直接报。
            Err(PlayerFailure::Transport(error)) => return Err(error),
            Err(PlayerFailure::Playability(error)) => {
                log::debug(&format!(
                    "YouTube {video_id} 的 {name} 客户端不可播：{error}"
                ));
                last = error;
            }
        }
    }
    Err(last)
}

/// 播放状态 → 用户能看懂的错误。
fn playability_message(status: &str, reason: &str) -> String {
    let reason = reason.trim();
    match status {
        "LOGIN_REQUIRED" => format!(
            "YouTube 要求登录验证（{reason}）。数据中心 IP 上很常见：\
             可以在 [plugin.xiaoqvan.music] 配置 youtube cookie，或者换一个网络再试"
        ),
        "ERROR" | "UNPLAYABLE" => format!("YouTube 不能播放这个视频（{reason}）"),
        "" => "YouTube 没有返回播放状态（接口结构可能变了）".to_owned(),
        other => format!("YouTube 返回 {other}（{reason}）"),
    }
}

// ── 格式选择 ────────────────────────────────────────────────────────

fn mime_of(format: &Value) -> &str {
    format.get("mimeType").and_then(Value::as_str).unwrap_or("")
}

fn bitrate_of(format: &Value) -> u64 {
    format.get("bitrate").and_then(Value::as_u64).unwrap_or(0)
}

fn number_of(value: Option<&Value>) -> Option<u64> {
    match value? {
        Value::Number(number) => number.as_u64(),
        Value::String(text) => text.trim().parse().ok(),
        _ => None,
    }
}

/// 在候选里挑一个音质。
///
/// 优先 audio/mp4（mp4a）：TDLib 用 .m4a 能正确识别成音频；没有时才退 opus/webm。
fn pick_format<'a>(formats: &[&'a Value], quality: &str) -> Option<&'a Value> {
    let mp4: Vec<&Value> = formats
        .iter()
        .copied()
        .filter(|format| mime_of(format).contains("audio/mp4"))
        .collect();
    let mut pool = if mp4.is_empty() {
        formats.to_vec()
    } else {
        mp4
    };
    pool.sort_by_key(|format| bitrate_of(format));
    match quality {
        "low" => pool.first().copied(),
        // 128 kbps 是 YouTube 的「标准」档，medium 取最接近的。
        "medium" => pool
            .iter()
            .copied()
            .min_by_key(|format| (bitrate_of(format) as i64 - 128_000).abs()),
        _ => pool.last().copied(),
    }
}

/// 从 player 响应里挑出音频流。
fn pick_stream(value: &Value, quality: &str) -> Result<Stream, String> {
    let formats: Vec<&Value> = value
        .pointer("/streamingData/adaptiveFormats")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter(|format| mime_of(format).starts_with("audio/"))
                .collect()
        })
        .unwrap_or_default();
    if formats.is_empty() {
        return Err("YouTube 没有返回音频流（可能是直播 / 只有视频）".to_owned());
    }
    let chosen =
        pick_format(&formats, quality).ok_or_else(|| "YouTube 没有可用的音频格式".to_owned())?;
    let url = chosen.get("url").and_then(Value::as_str).unwrap_or("");
    if url.trim().is_empty() {
        return Err(
            "YouTube 只返回了需要签名解密的流（没有直链）；稍后重试或配置 youtube cookie"
                .to_owned(),
        );
    }
    Ok(Stream {
        url: url.to_owned(),
        itag: chosen.get("itag").and_then(Value::as_u64).unwrap_or(0),
        mime: mime_of(chosen).to_owned(),
        bitrate: bitrate_of(chosen),
        size: number_of(chosen.get("contentLength")).unwrap_or(0),
    })
}

/// 实际流的展示名（码率在 AudioMeta 里另算）。
pub fn stream_label(stream: &Stream) -> String {
    let codec = if stream.mime.contains("mp4") {
        "m4a"
    } else if stream.mime.contains("webm") {
        "webm"
    } else {
        "音频"
    };
    format!("YouTube {codec}")
}

// ── 取流 ────────────────────────────────────────────────────────────

/// 视频 ID → 音频直链 + 展示信息。
pub async fn resolve(
    agent: &Agent,
    cookie: &str,
    video_id: &str,
    quality: &str,
) -> Result<Resolved, String> {
    let value = player(agent, cookie, video_id).await?;
    let details = value.get("videoDetails").cloned().unwrap_or(Value::Null);
    let title = details
        .get("title")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_owned();
    let author = details
        .get("author")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_owned();
    // Topic 频道（自动生成的「- Topic」）不是歌手名，去掉后缀。
    let author = author
        .strip_suffix(" - Topic")
        .unwrap_or(&author)
        .trim()
        .to_owned();
    let duration = details
        .get("lengthSeconds")
        .and_then(|value| {
            value
                .as_str()
                .and_then(|text| text.parse::<i64>().ok())
                .or_else(|| value.as_i64())
        })
        .unwrap_or(0);
    let cover = details
        .pointer("/thumbnail/thumbnails")
        .and_then(Value::as_array)
        .and_then(|items| items.last())
        .and_then(|item| item.get("url"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    let stream = pick_stream(&value, quality)?;
    log::debug(&format!(
        "YouTube {video_id}：{}，itag {}，{} kbps，{} 字节",
        title,
        stream.itag,
        stream.bitrate / 1000,
        stream.size
    ));
    Ok(Resolved {
        track: Track {
            id: video_id.to_owned(),
            name: if title.is_empty() {
                "YouTube 音频".to_owned()
            } else {
                title
            },
            artists: if author.is_empty() {
                Vec::new()
            } else {
                vec![author]
            },
            album: String::new(),
            cover,
            duration,
            video_type: String::new(),
        },
        stream,
    })
}

// ── 搜索 ────────────────────────────────────────────────────────────

/// 递归收集某个 key 下的对象（InnerTube 的响应层级很深）。
fn collect_renderers<'a>(value: &'a Value, key: &str, out: &mut Vec<&'a Value>) {
    match value {
        Value::Object(map) => {
            if let Some(found) = map.get(key) {
                out.push(found);
            }
            for item in map.values() {
                collect_renderers(item, key, out);
            }
        }
        Value::Array(items) => {
            for item in items {
                collect_renderers(item, key, out);
            }
        }
        _ => {}
    }
}

/// 递归找第一个 key。
fn find_key<'a>(value: &'a Value, key: &str) -> Option<&'a Value> {
    match value {
        Value::Object(map) => {
            if let Some(found) = map.get(key) {
                return Some(found);
            }
            map.values().find_map(|item| find_key(item, key))
        }
        Value::Array(items) => items.iter().find_map(|item| find_key(item, key)),
        _ => None,
    }
}

/// runs 数组 / simpleText → 纯文本。
fn runs_text(value: &Value) -> String {
    if let Some(runs) = value.get("runs").and_then(Value::as_array) {
        return runs
            .iter()
            .filter_map(|run| run.get("text").and_then(Value::as_str))
            .collect();
    }
    value
        .get("simpleText")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned()
}

/// flexColumns[i] 的文本。
fn flex_text(item: &Value, index: usize) -> String {
    item.get("flexColumns")
        .and_then(Value::as_array)
        .and_then(|columns| columns.get(index))
        .and_then(|column| column.get("musicResponsiveListItemFlexColumnRenderer"))
        .and_then(|column| column.get("text"))
        .map(runs_text)
        .unwrap_or_default()
}

/// fixedColumns[i] 的文本（时长通常在这里）。
fn fixed_text(item: &Value, index: usize) -> String {
    item.get("fixedColumns")
        .and_then(Value::as_array)
        .and_then(|columns| columns.get(index))
        .and_then(|column| column.get("musicResponsiveListItemFixedColumnRenderer"))
        .and_then(|column| column.get("text"))
        .map(runs_text)
        .unwrap_or_default()
}

/// 列表项缩略图（取最大的一张）。
fn thumbnail_of(item: &Value) -> String {
    item.pointer("/thumbnail/musicThumbnailRenderer/thumbnail/thumbnails")
        .and_then(Value::as_array)
        .and_then(|items| items.last())
        .and_then(|item| item.get("url"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned()
}

/// 第一个 watchEndpoint 的 videoId（playlistItemData 缺失时的兜底）。
fn first_watch_id(item: &Value) -> Option<String> {
    find_key(item, "watchEndpoint")
        .and_then(|endpoint| endpoint.get("videoId"))
        .and_then(Value::as_str)
        .map(str::to_owned)
}

/// 音乐视频类型（ATV / OMV / UGC …）。
fn video_type_of(item: &Value) -> String {
    find_key(item, "musicVideoType")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned()
}

/// m:ss / h:mm:ss → 秒。
pub fn parse_clock(text: &str) -> Option<i64> {
    let text = text.trim();
    let parts: Vec<&str> = text.split(':').collect();
    if parts.len() < 2 || parts.len() > 3 {
        return None;
    }
    let mut total = 0i64;
    for part in parts {
        if part.is_empty() || !part.chars().all(|ch| ch.is_ascii_digit()) {
            return None;
        }
        total = total * 60 + part.parse::<i64>().ok()?;
    }
    (total > 0).then_some(total)
}

/// 从副标题里找最后一个时长（「Rick Astley • 6.3M views • 9:20」）。
fn last_clock(text: &str) -> Option<i64> {
    text.split(['•', '·', ',', ' '])
        .filter_map(parse_clock)
        .next_back()
}

/// 副标题 → 歌手。
///
/// WEB_REMIX 的副标题形如：
/// * 「Rick Astley • 6.3M views • 3:33」
/// * 「Song • Rick Astley」
/// * 「Album • Various Artists • 2020」
fn artists_of(subtitle: &str) -> Vec<String> {
    let parts: Vec<&str> = subtitle
        .split(['•', '·'])
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .collect();
    let Some(first) = parts.first() else {
        return Vec::new();
    };
    let kind = first.to_ascii_lowercase();
    let artist = if matches!(
        kind.as_str(),
        "song" | "single" | "album" | "ep" | "playlist" | "video" | "podcast" | "episode"
    ) {
        parts.get(1).copied().unwrap_or("")
    } else {
        first
    };
    if artist.is_empty() {
        Vec::new()
    } else {
        vec![artist.to_owned()]
    }
}

/// 一条 musicResponsiveListItemRenderer → Track。
fn track_from_renderer(item: &Value) -> Option<Track> {
    let id = item
        .pointer("/playlistItemData/videoId")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .or_else(|| first_watch_id(item))?;
    let name = flex_text(item, 0).trim().to_owned();
    if name.is_empty() {
        return None;
    }
    let subtitle = flex_text(item, 1);
    let duration = parse_clock(&fixed_text(item, 0))
        .or_else(|| last_clock(&subtitle))
        .unwrap_or(0);
    Some(Track {
        id,
        name,
        artists: artists_of(&subtitle),
        album: String::new(),
        cover: thumbnail_of(item),
        duration,
        video_type: video_type_of(item),
    })
}

/// 搜索 YouTube Music。
pub async fn search(
    agent: &Agent,
    cookie: &str,
    keyword: &str,
    limit: usize,
) -> Result<Vec<Track>, String> {
    let limit = limit.clamp(1, 30);
    let body = json!({ "context": webmix_context(), "query": keyword });
    let value = post_innertube(
        agent,
        SEARCH_URL,
        &body,
        cookie,
        BROWSER_USER_AGENT,
        "https://music.youtube.com",
        "https://music.youtube.com/",
    )
    .await?;
    let mut renderers = Vec::new();
    collect_renderers(&value, "musicResponsiveListItemRenderer", &mut renderers);
    let mut tracks: Vec<Track> = Vec::new();
    for item in renderers {
        let Some(track) = track_from_renderer(item) else {
            continue;
        };
        if tracks.iter().any(|existing| existing.id == track.id) {
            continue;
        }
        tracks.push(track);
        if tracks.len() >= limit {
            break;
        }
    }
    if tracks.is_empty() {
        return Err(format!(
            "YouTube Music 没搜到「{}」",
            log::shorten(keyword, 40)
        ));
    }
    Ok(tracks)
}

/// 播放列表标题（musicDetailHeaderRenderer / musicResponsiveHeaderRenderer）。
fn header_title(value: &Value) -> Option<String> {
    for key in ["musicDetailHeaderRenderer", "musicResponsiveHeaderRenderer"] {
        if let Some(header) = find_key(value, key)
            && let Some(title) = header.get("title")
        {
            let text = runs_text(title);
            if !text.trim().is_empty() {
                return Some(text);
            }
        }
    }
    None
}

/// 播放列表 → (标题, 前 limit 首)。
pub async fn playlist(
    agent: &Agent,
    cookie: &str,
    list_id: &str,
    limit: usize,
) -> Result<(String, Vec<Track>), String> {
    let limit = limit.clamp(1, 50);
    let browse_id = if list_id.starts_with("VL") {
        list_id.to_owned()
    } else {
        format!("VL{list_id}")
    };
    let body = json!({ "context": webmix_context(), "browseId": browse_id });
    let value = post_innertube(
        agent,
        BROWSE_URL,
        &body,
        cookie,
        BROWSER_USER_AGENT,
        "https://music.youtube.com",
        "https://music.youtube.com/",
    )
    .await?;
    let mut renderers = Vec::new();
    collect_renderers(&value, "musicResponsiveListItemRenderer", &mut renderers);
    let mut tracks: Vec<Track> = Vec::new();
    for item in renderers {
        let Some(track) = track_from_renderer(item) else {
            continue;
        };
        if tracks.iter().any(|existing| existing.id == track.id) {
            continue;
        }
        tracks.push(track);
        if tracks.len() >= limit {
            break;
        }
    }
    if tracks.is_empty() {
        return Err("这个播放列表里没有可解析的单曲（可能是空列表 / 私人列表）".to_owned());
    }
    Ok((
        header_title(&value).unwrap_or_else(|| "YouTube Music 播放列表".to_owned()),
        tracks,
    ))
}

#[cfg(test)]
#[path = "../tests/unit/youtube.rs"]
mod tests;
