//! 汽水音乐（Soda Music）：搜索 / 分享链接解析 / 取流 / 音质。
//!
//! 接口与行为参考 `sodahub-org/libresoda`（`music-lib` 的 Rust 复刻），这里只保留
//! 本插件要用的那条链路：
//!
//! ```text
//! 搜索      GET  https://api.qishui.com/luna/search/track   （Android 客户端参数）
//! 详情      GET  https://api.qishui.com/luna/pc/track_v2     （Web 侧，免签名）
//!           GET  https://beta-luna.douyin.com/luna/h5/seo_track   （兜底）
//! 整曲      POST https://api.qishui.com/luna/pc/track_v2     （要 cookie + x-helios/x-medusa）
//! 播放信息  GET  track_player.url_player_info
//! ```
//!
//! 音质档位用客户端的 gear key（`best` / `lossless` / `highest` / `medium` / `low`），
//! 只在「不超过该档位」的候选里挑最优；该档位没有候选就回退整体最优。

use std::collections::BTreeMap;

use serde::Deserialize;
use serde_json::Value;

use crate::auth::SodaAuth;
use crate::http::{self, Agent};
use crate::log;

/// Web / 分享页 UA。
pub const CHROME_UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/134.0.0.0 Safari/537.36";
/// PC 客户端 UA（`pc/track_v2` 等 App 端点用）。
pub const PC_APP_UA: &str = "LunaPC/3.3.0(359450208)";
/// H5 SEO 单曲接口（免签名）。
const SEO_TRACK: &str = "https://beta-luna.douyin.com/luna/h5/seo_track";
/// PC App 网关。
const PC_API: &str = "https://api.qishui.com";
/// 抖音图床前缀。
const IMAGE_BASE: &str = "https://p3-luna.douyinpic.com/img/";

/// 一个可选音质档位（`key` 是客户端 gear key）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Quality {
    pub key: &'static str,
    pub label: &'static str,
}

/// 支持的全部档位，顺序 = 帮助里的展示顺序。
pub const QUALITIES: &[Quality] = &[
    Quality {
        key: "best",
        label: "最优",
    },
    Quality {
        key: "hires",
        label: "Hi-Res",
    },
    Quality {
        key: "lossless",
        label: "无损",
    },
    Quality {
        key: "dolby",
        label: "杜比全景声",
    },
    Quality {
        key: "highest",
        label: "极高",
    },
    Quality {
        key: "medium",
        label: "标准",
    },
    Quality {
        key: "low",
        label: "省流",
    },
];

/// 默认档位：不限制，拿得到什么就发什么（VIP 曲目没签名时至少能发试听）。
pub const DEFAULT_QUALITY: &str = "best";

/// 把用户输入归一成 gear key。
pub fn parse_quality(name: &str) -> Option<&'static str> {
    let name = name.trim().to_ascii_lowercase();
    let canonical = match name.as_str() {
        "best" | "auto" | "最优" => "best",
        "hires" | "hi-res" | "hi_res" | "母带" => "hires",
        "lossless" | "flac" | "无损" => "lossless",
        "dolby" | "dolby_atmos" | "杜比" => "dolby",
        "highest" | "hq" | "high" | "极高" => "highest",
        "medium" | "standard" | "320k" | "标准" => "medium",
        "low" | "128k" | "saver" | "省流" => "low",
        _ => return None,
    };
    QUALITIES
        .iter()
        .find(|quality| quality.key == canonical)
        .map(|quality| quality.key)
}

pub fn quality_label(key: &str) -> String {
    QUALITIES
        .iter()
        .find(|quality| quality.key.eq_ignore_ascii_case(key))
        .map(|quality| format!("{}（{}）", quality.label, quality.key))
        .unwrap_or_else(|| key.to_owned())
}

pub fn quality_help() -> String {
    QUALITIES
        .iter()
        .map(|quality| quality.key)
        .collect::<Vec<_>>()
        .join(" / ")
}

/// 流的质量分：数值越大越好。判定规则与参考实现一致。
pub fn quality_rank(quality: &str, format: &str, bitrate: i64) -> i64 {
    let quality = quality.trim().to_ascii_lowercase();
    let format = format.trim().to_ascii_lowercase();
    let bitrate = normalize_bitrate(bitrate);

    let lossless_format =
        format.contains("flac") || format.contains("alac") || format.contains("wav");
    let lossless_label = quality.contains("lossless")
        || quality.contains("flac")
        || quality.contains("sq")
        || quality.contains("svip");
    let hires = quality.contains("hires") || quality.contains("master");

    if hires && (lossless_format || bitrate >= 900) {
        return 110;
    }
    if lossless_label || lossless_format || bitrate >= 900 {
        return 100;
    }
    if hires {
        return 90;
    }
    if quality.contains("atmos") || quality.contains("dolby") || quality.contains("spatial") {
        return 88;
    }
    if quality.contains("highest")
        || quality.contains("excellent")
        || quality.contains("superhigh")
        || quality.contains("hq")
    {
        return 80;
    }
    if quality.contains("higher") || quality == "high" || quality.contains("320") {
        return 70;
    }
    if quality.contains("standard")
        || quality.contains("medium")
        || quality.contains("normal")
        || quality.contains("128")
    {
        return 50;
    }
    if quality.contains("low") || quality.contains("preview") {
        return 10;
    }
    match bitrate {
        value if value >= 900 => 100,
        value if value >= 320 => 70,
        value if value >= 256 => 65,
        value if value >= 192 => 55,
        value if value >= 128 => 50,
        value if value > 0 => 20,
        _ => 0,
    }
}

/// 档位偏好 → 允许的最高分。`None` = 不限制。
pub fn preference_rank(preference: &str) -> Option<i64> {
    match preference.trim().to_ascii_lowercase().as_str() {
        "" | "best" | "auto" => None,
        "hires" | "hi-res" | "hi_res" => Some(110),
        "lossless" | "flac" => Some(100),
        "dolby" | "dolby_atmos" => Some(88),
        "highest" | "hq" | "high" | "320k" => Some(80),
        // 汽水实际档位：medium ≈ 64k、higher ≈ 128k、highest ≈ 256k。
        // 必须和 `quality_rank` 的分值对齐，否则「标准」会放出「较高」。
        "medium" | "standard" | "128k" => Some(50),
        "low" | "saver" => Some(20),
        // 认不出的档位当不限制，别让用户的设置变成「拿不到流」
        _ => None,
    }
}

fn normalize_bitrate(bitrate: i64) -> i64 {
    if bitrate > 1000 {
        bitrate / 1000
    } else {
        bitrate
    }
}

fn normalize_duration(duration: f64) -> f64 {
    if duration > 1000.0 {
        duration / 1000.0
    } else {
        duration
    }
}

// ── 数据类型 ────────────────────────────────────────────────────────

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
struct Image {
    urls: Vec<String>,
    uri: String,
    template_prefix: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
struct Artist {
    name: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
struct Album {
    name: String,
    url_cover: Image,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
struct BitRate {
    #[serde(rename = "br")]
    br: i64,
    quality: String,
    size: i64,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
struct ApiStatus {
    status_msg: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
struct Policy {
    need_vip: bool,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
struct Benefit {
    play_detail: Option<Policy>,
    download_detail: Option<Policy>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
struct LabelInfo {
    only_vip_download: bool,
    only_vip_playable: bool,
    quality_only_vip_can_download: Vec<String>,
    quality_only_vip_can_play: Vec<String>,
    quality_map: BTreeMap<String, Benefit>,
}

impl LabelInfo {
    fn is_vip(&self) -> bool {
        if self.only_vip_download
            || self.only_vip_playable
            || !self.quality_only_vip_can_download.is_empty()
            || !self.quality_only_vip_can_play.is_empty()
        {
            return true;
        }
        self.quality_map.values().any(|benefit| {
            benefit
                .play_detail
                .as_ref()
                .is_some_and(|policy| policy.need_vip)
                || benefit
                    .download_detail
                    .as_ref()
                    .is_some_and(|policy| policy.need_vip)
        })
    }
}

/// 接口里的流描述（`audio_info.play_info_list` 与 `video_model` 共用）。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
struct PlayInfo {
    main_play_url: String,
    backup_play_url: String,
    play_auth: String,
    size: i64,
    format: String,
    bitrate: i64,
    quality: String,
    duration: f64,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
struct TrackRaw {
    id: String,
    name: String,
    duration: i64,
    artists: Vec<Artist>,
    album: Album,
    bit_rates: Vec<BitRate>,
    label_info: LabelInfo,
    audio_info: AudioInfo,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
struct AudioInfo {
    play_info_list: Vec<PlayInfo>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
struct TrackPlayer {
    url_player_info: String,
    video_model: Option<Value>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
struct TrackResponse {
    status_code: i64,
    status_info: ApiStatus,
    track: TrackRaw,
    track_info: TrackRaw,
    track_player: TrackPlayer,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
struct SeoTrack {
    track: TrackRaw,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
struct SeoResponse {
    status_code: i64,
    status_info: ApiStatus,
    track_player: TrackPlayer,
    seo_track: SeoTrack,
}

/// 搜索 / 详情解析出来的歌。
#[derive(Debug, Clone, Default)]
pub struct Track {
    pub id: String,
    pub name: String,
    pub artists: Vec<String>,
    pub album: String,
    pub cover: String,
    pub duration: i64,
    pub is_vip: bool,
    /// 详情里直接给的流（`audio_info.play_info_list`）。
    plays: Vec<PlayInfo>,
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
        format!("https://www.qishui.com/track/{}", self.id)
    }
}

/// 实际拿到的流。
#[derive(Debug, Clone, Default)]
pub struct Stream {
    pub url: String,
    pub play_auth: String,
    pub format: String,
    pub size: i64,
    pub bitrate: i64,
    pub quality: String,
    pub duration: f64,
}

impl Stream {
    fn rank(&self) -> i64 {
        quality_rank(&self.quality, &self.format, self.bitrate)
    }

    /// 加密流要先解 `play_auth`。
    pub fn is_encrypted(&self) -> bool {
        !self.play_auth.trim().is_empty()
    }
}

/// 一次解析的最终结果。
#[derive(Debug, Clone)]
pub struct Resolved {
    pub track: Track,
    pub stream: Stream,
    /// 只是试听片段（明显短于整曲）。
    pub preview: bool,
}

impl Resolved {
    /// 解密后的文件扩展名：只按真实编码给，TDLib 靠它判断 MIME。
    ///
    /// 接口偶尔不给 `format`，这时从 URL 末段猜；再猜不出就按 m4a（汽水
    /// 绝大多数流是 MP4 容器，加密的也一定是）。
    pub fn extension(&self) -> &'static str {
        let format = self.stream.format.trim().to_ascii_lowercase();
        let format = if format.is_empty() {
            let path = self.stream.url.split(['?', '#']).next().unwrap_or("");
            path.rsplit('.').next().unwrap_or("").to_ascii_lowercase()
        } else {
            format
        };
        match format.as_str() {
            "flac" => "flac",
            "mp3" => "mp3",
            "wav" => "wav",
            "ogg" | "opus" => "ogg",
            _ => "m4a",
        }
    }
}

// ── 搜索 ────────────────────────────────────────────────────────────

/// 搜索歌曲，最多 `limit` 条。
pub async fn search(
    agent: &Agent,
    auth: &SodaAuth,
    keyword: &str,
    limit: usize,
) -> Result<Vec<Track>, String> {
    let url = search_url(keyword, limit.clamp(1, 30));
    let reply = get_with_cookie(agent, &url, auth, CHROME_UA).await?;
    if !(200..300).contains(&reply.status) {
        return Err(format!("汽水搜索 HTTP {}", reply.status));
    }
    let value = reply.json()?;
    let tracks = parse_search(&value)?;
    if tracks.is_empty() {
        return Err(format!("汽水没搜到「{keyword}」相关的歌"));
    }
    Ok(tracks)
}

fn parse_search(value: &Value) -> Result<Vec<Track>, String> {
    let mut tracks = Vec::new();
    let mut seen: Vec<String> = Vec::new();
    // 匿名搜不到东西时服务端只回 `extra.empty_search`，既没有 `result_groups`
    // 也没有错误码——那叫「没结果」，不是接口故障。
    let Some(groups) = value.get("result_groups").and_then(Value::as_array) else {
        return Ok(Vec::new());
    };
    for group in groups {
        let Some(items) = group.get("data").and_then(Value::as_array) else {
            continue;
        };
        for item in items {
            let Some(track) = item.pointer("/entity/track") else {
                continue;
            };
            let Ok(raw) = serde_json::from_value::<TrackRaw>(track.clone()) else {
                continue;
            };
            if raw.id.is_empty() || seen.contains(&raw.id) {
                continue;
            }
            seen.push(raw.id.clone());
            tracks.push(track_from_raw(raw));
        }
    }
    Ok(tracks)
}

fn track_from_raw(raw: TrackRaw) -> Track {
    Track {
        id: raw.id,
        name: raw.name,
        artists: raw.artists.into_iter().map(|artist| artist.name).collect(),
        album: raw.album.name,
        cover: image_url(&raw.album.url_cover, "~c5_320x320.jpg"),
        duration: normalize_duration(raw.duration as f64).round() as i64,
        is_vip: raw.label_info.is_vip(),
        plays: raw.audio_info.play_info_list,
    }
}

fn image_url(image: &Image, suffix: &str) -> String {
    let Some(first) = image.urls.first() else {
        return String::new();
    };
    let mut cover = first.trim().to_owned();
    let uri = image.uri.trim();
    let prefix = image.template_prefix.trim();
    if !uri.is_empty() && !prefix.is_empty() {
        // 320×320 的 JPEG（~25KB）：封面要当 `inputThumbnail` 发，Telegram 对缩略图
        // 的体积有限制，960×960 的 PNG 有 1.5MB，会被拒。
        return format!(
            "{}/{uri}~{prefix}-resize:320:320.jpg",
            IMAGE_BASE.trim_end_matches('/')
        );
    }
    if !uri.is_empty() && !cover.contains(uri) {
        cover.push_str(uri);
    }
    if !cover.is_empty() && !suffix.is_empty() && !cover.contains('~') {
        cover.push_str(suffix);
    }
    cover
}

/// 综合搜索（官方搜索页用的就是它）：匿名即可用，付费曲目也能列出。
///
/// 之前用的 Android `/luna/search/track` 现在对匿名请求直接回 `empty_search`，
/// 所以搜索走 Web 侧；带 cookie 时结果会更全。
fn search_url(keyword: &str, count: usize) -> String {
    let params = [
        ("q", keyword.to_owned()),
        ("aid", "386088".to_owned()),
        ("device_platform", "web".to_owned()),
        ("channel", "pc_web".to_owned()),
        ("count", count.to_string()),
    ];
    let query = params
        .iter()
        .map(|(key, value)| format!("{}={}", percent_encode(key), percent_encode(value)))
        .collect::<Vec<_>>()
        .join("&");
    format!("{PC_API}/luna/search/all?{query}")
}

pub fn percent_encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

// ── 详情 / 取流 ─────────────────────────────────────────────────────

/// 详情 + 播放信息。`web` 是 Web 侧免签名的那个接口。
struct Detail {
    track: Track,
    player: TrackPlayer,
}

async fn fetch_detail(agent: &Agent, auth: &SodaAuth, id: &str) -> Result<Detail, String> {
    match fetch_web_detail(agent, auth, id).await {
        Ok(detail) => Ok(detail),
        Err(web_error) => {
            // 部分曲目 / 风控下 Web 详情会回 0 字节，SEO 兜底是正常路径，不必吓人。
            log::debug(&format!("汽水 Web 详情失败，改用 SEO 兜底：{web_error}"));
            fetch_seo_detail(agent, auth, id)
                .await
                .map_err(|seo_error| {
                    format!("汽水 Web 详情失败：{web_error}；SEO 兜底也失败：{seo_error}")
                })
        }
    }
}

async fn fetch_web_detail(agent: &Agent, auth: &SodaAuth, id: &str) -> Result<Detail, String> {
    let url = format!(
        "{PC_API}/luna/pc/track_v2?track_id={}&media_type=track&aid=386088&device_platform=web&channel=pc_web",
        percent_encode(id)
    );
    let reply = get_with_cookie(agent, &url, auth, CHROME_UA).await?;
    let value = reply.json()?;
    let response: TrackResponse =
        serde_json::from_value(value).map_err(|error| format!("汽水详情解析失败：{error}"))?;
    if response.status_code != 0 {
        return Err(format!(
            "汽水详情返回 status_code={}：{}",
            response.status_code,
            status_message(&response.status_info)
        ));
    }
    let raw = if response.track.id.is_empty() {
        response.track_info
    } else {
        response.track
    };
    if raw.id.is_empty() {
        return Err("汽水详情里没有歌曲信息".to_owned());
    }
    Ok(Detail {
        track: track_from_raw(raw),
        player: response.track_player,
    })
}

async fn fetch_seo_detail(agent: &Agent, auth: &SodaAuth, id: &str) -> Result<Detail, String> {
    let url = format!(
        "{SEO_TRACK}?track_id={}&device_platform=web",
        percent_encode(id)
    );
    let reply = get_with_cookie(agent, &url, auth, CHROME_UA).await?;
    let value = reply.json()?;
    let response: SeoResponse =
        serde_json::from_value(value).map_err(|error| format!("汽水 SEO 详情解析失败：{error}"))?;
    if response.status_code != 0 {
        return Err(format!(
            "汽水 SEO 详情返回 status_code={}：{}",
            response.status_code,
            status_message(&response.status_info)
        ));
    }
    let raw = response.seo_track.track;
    if raw.id.is_empty() {
        return Err("汽水 SEO 详情里没有歌曲信息".to_owned());
    }
    Ok(Detail {
        track: track_from_raw(raw),
        player: response.track_player,
    })
}

/// 带 cookie 的 GET（cookie 为空就不发这个头）。
async fn get_with_cookie(
    agent: &Agent,
    url: &str,
    auth: &SodaAuth,
    user_agent: &'static str,
) -> Result<http::Reply, String> {
    let cookie = auth.cookie.trim();
    let mut headers = vec![("User-Agent", user_agent), ("Accept", "*/*")];
    if !cookie.is_empty() {
        headers.push(("Cookie", cookie));
    }
    http::get(agent, url, &headers).await
}

fn status_message(status: &ApiStatus) -> &str {
    if status.status_msg.trim().is_empty() {
        "未知错误"
    } else {
        &status.status_msg
    }
}

/// PC App 端点取流：需要 cookie + 应用签名头（`x-helios` / `x-medusa`）。
async fn fetch_pc_detail(agent: &Agent, auth: &SodaAuth, id: &str) -> Result<Detail, String> {
    if auth.cookie.trim().is_empty() {
        return Err("没有 cookie".to_owned());
    }
    if !auth.has_signature() && !auth.has_signer() {
        return Err(
            "没有 x-helios / x-medusa，也没有配置签名器（soda.json 的 signer_url）".to_owned(),
        );
    }

    let body = serde_json::to_vec(&serde_json::json!({
        "track_id": id,
        "media_type": "track",
        "queue_type": "favorite_track_playlist",
        "scene_name": "library",
    }))
    .map_err(|error| format!("汽水请求体编码失败：{error}"))?;

    let url = format!("{PC_API}/luna/pc/track_v2?{}", pc_query(auth));
    let body_text = String::from_utf8_lossy(&body).to_string();
    let stub = crate::md5_hex(&body_text).to_uppercase();
    let user_agent = if auth.user_agent.trim().is_empty() {
        PC_APP_UA
    } else {
        auth.user_agent.trim()
    };

    let headers = vec![
        ("User-Agent".to_owned(), user_agent.to_owned()),
        ("x-luna-background-type".to_owned(), "foreground".to_owned()),
        ("x-luna-is-background-req".to_owned(), "0".to_owned()),
        ("x-luna-is-local-user".to_owned(), "1".to_owned()),
        ("Cookie".to_owned(), auth.cookie.trim().to_owned()),
        ("X-SS-STUB".to_owned(), stub),
    ];
    // 有签名器就用逐请求签名（签名覆盖 URL + 头 + body）；否则退回静态抓包头。
    let signed = crate::soda_sign::signed_request(agent, auth, &url, &body_text, headers).await?;
    if !signed
        .headers
        .iter()
        .any(|(name, _)| name.eq_ignore_ascii_case("x-helios"))
    {
        return Err("拿不到 x-helios：签名器没给，soda.json 里也没填（整曲取流需要它）".to_owned());
    }
    let header_refs: Vec<(&str, &str)> = signed
        .headers
        .iter()
        .map(|(name, value)| (name.as_str(), value.as_str()))
        .collect();
    let reply = http::post_json(agent, &signed.url, &body, &header_refs).await?;
    if reply.body.is_empty() {
        return Err(
            "汽水 App 端点返回空 body：签名头可能已过期，或签名器算的不是这次请求（body / track_id 变了要重签）"
                .to_owned(),
        );
    }
    let value = reply.json()?;
    let response: TrackResponse =
        serde_json::from_value(value).map_err(|error| format!("汽水 App 详情解析失败：{error}"))?;
    if response.status_code != 0 {
        return Err(format!(
            "汽水 App 详情返回 status_code={}：{}",
            response.status_code,
            status_message(&response.status_info)
        ));
    }
    let raw = if response.track.id.is_empty() {
        response.track_info
    } else {
        response.track
    };
    if raw.id.is_empty() {
        return Err("汽水 App 详情里没有歌曲信息".to_owned());
    }
    Ok(Detail {
        track: track_from_raw(raw),
        player: response.track_player,
    })
}

fn pc_query(auth: &SodaAuth) -> String {
    let params = [
        ("aid", "386088".to_owned()),
        ("app_name", "luna_pc".to_owned()),
        ("region", "cn".to_owned()),
        ("geo_region", "cn".to_owned()),
        ("os_region", "cn".to_owned()),
        ("sim_region", String::new()),
        ("device_id", auth.device_id.clone()),
        ("cdid", String::new()),
        ("iid", auth.iid.clone()),
        ("version_name", "3.3.0".to_owned()),
        ("version_code", "30030000".to_owned()),
        ("channel", "official".to_owned()),
        ("build_mode", "master".to_owned()),
        ("network_carrier", String::new()),
        ("ac", "wifi".to_owned()),
        ("tz_name", "Asia/Shanghai".to_owned()),
        ("resolution", String::new()),
        ("device_platform", "windows".to_owned()),
        ("device_type", "Windows".to_owned()),
        ("os_version", "Windows 11".to_owned()),
        ("fp", auth.fp_or_device_id()),
    ];
    params
        .iter()
        .map(|(key, value)| format!("{}={}", percent_encode(key), percent_encode(value)))
        .collect::<Vec<_>>()
        .join("&")
}

/// 解析一首歌并挑出「不超过偏好档位」的最优流。
pub async fn resolve(
    agent: &Agent,
    auth: &SodaAuth,
    text: &str,
    preference: &str,
) -> Result<Resolved, String> {
    let id = resolve_id(agent, auth, text).await?;
    let detail = fetch_detail(agent, auth, &id).await?;
    let track = detail.track;
    let mut candidates = collect_candidates(agent, auth, &detail.player, &track).await;

    let mut best = pick(&candidates, preference, track.duration);
    log::debug(&format!(
        "汽水 {}《{}》：Web 侧候选 {} 个，挑中 {:?}",
        track.id,
        track.name,
        candidates.len(),
        best.as_ref().map(|stream| (&stream.quality, stream.rank()))
    ));

    // 会员曲目 / 只有试听 / 没拿到无损时，再试一次 App 端点（需要签名头）。
    let need_pc = (auth.has_signature() || auth.has_signer())
        && !auth.cookie.trim().is_empty()
        && best
            .as_ref()
            .map(|stream| is_preview(stream, track.duration) || stream.rank() < 100)
            .unwrap_or(true);
    if need_pc {
        match fetch_pc_detail(agent, auth, &id).await {
            Ok(pc) => {
                let pc_candidates = collect_candidates(agent, auth, &pc.player, &pc.track).await;
                if let Some(pc_best) = pick(&pc_candidates, preference, pc.track.duration)
                    && !is_preview(&pc_best, pc.track.duration)
                    && best
                        .as_ref()
                        .map(|current| better(&pc_best, current, pc.track.duration))
                        .unwrap_or(true)
                {
                    log::debug(&format!(
                        "汽水 {}：改用 App 端点的 {}（{}）",
                        track.id,
                        pc_best.quality,
                        pc_best.rank()
                    ));
                    best = Some(pc_best);
                    candidates.clear();
                }
            }
            Err(error) => log::warn(&format!("汽水 App 端点没拿到整曲：{error}")),
        }
    }

    let Some(stream) = best else {
        return Err(format!(
            "汽水没给出《{}》的播放地址（可能已下架或地区受限）",
            track.name
        ));
    };
    let preview = is_preview(&stream, track.duration);
    if preview {
        log::warn(&format!(
            "汽水 {}《{}》只拿到试听（{} 秒 / 整曲 {} 秒）",
            track.id, track.name, stream.duration, track.duration
        ));
    }
    Ok(Resolved {
        track,
        stream,
        preview,
    })
}

/// 收集所有候选流：详情里的 `audio_info`、`video_model`，以及 `url_player_info`。
async fn collect_candidates(
    agent: &Agent,
    auth: &SodaAuth,
    player: &TrackPlayer,
    track: &Track,
) -> Vec<Stream> {
    let mut infos = track.plays.clone();
    if let Some(model) = &player.video_model {
        let mut nested = Vec::new();
        collect_model(model, "", "", 0.0, &mut nested);
        infos.extend(nested);
    }
    if !player.url_player_info.trim().is_empty()
        && let Ok(list) = fetch_player_info(agent, auth, &player.url_player_info).await
    {
        infos.extend(list);
    }

    infos
        .into_iter()
        .filter_map(|info| {
            let url = if info.main_play_url.trim().is_empty() {
                info.backup_play_url.trim().to_owned()
            } else {
                info.main_play_url.trim().to_owned()
            };
            if url.is_empty() {
                return None;
            }
            Some(Stream {
                url,
                play_auth: info.play_auth.trim().to_owned(),
                format: info.format.trim().to_owned(),
                size: info.size,
                bitrate: info.bitrate,
                quality: info.quality.trim().to_owned(),
                duration: normalize_duration(info.duration),
            })
        })
        .collect()
}

async fn fetch_player_info(
    agent: &Agent,
    auth: &SodaAuth,
    url: &str,
) -> Result<Vec<PlayInfo>, String> {
    let reply = get_with_cookie(agent, url, auth, CHROME_UA).await?;
    let value = reply.json()?;
    let list = value
        .pointer("/Result/Data/PlayInfoList")
        .and_then(Value::as_array)
        .ok_or_else(|| "播放信息里没有 PlayInfoList".to_owned())?;
    let mut out = Vec::new();
    for item in list {
        let string = |keys: &[&str]| -> String {
            for key in keys {
                if let Some(text) = item.get(*key).and_then(Value::as_str) {
                    return text.to_owned();
                }
            }
            String::new()
        };
        let number = |keys: &[&str]| -> f64 {
            for key in keys {
                if let Some(number) = item.get(*key).and_then(Value::as_f64) {
                    return number;
                }
            }
            0.0
        };
        out.push(PlayInfo {
            main_play_url: string(&["MainPlayUrl"]),
            backup_play_url: string(&["BackupPlayUrl"]),
            play_auth: string(&["PlayAuth"]),
            size: number(&["Size"]) as i64,
            format: string(&["Format"]),
            bitrate: number(&["Bitrate"]) as i64,
            quality: string(&["Quality"]),
            duration: number(&["Duration"]),
        });
    }
    Ok(out)
}

/// 递归扫 `video_model`：字段名在不同版本里五花八门，按别名表取。
fn collect_model(
    value: &Value,
    key_hint: &str,
    inherited_auth: &str,
    inherited_duration: f64,
    out: &mut Vec<PlayInfo>,
) {
    match value {
        Value::Object(map) => {
            let own_auth = map
                .get("encrypt_info")
                .and_then(Value::as_object)
                .map(|encrypt| string_of(encrypt, &["spade_a", "play_auth"]))
                .filter(|auth| !auth.is_empty())
                .or_else(|| {
                    let auth = string_of(map, &["play_auth"]);
                    (!auth.is_empty()).then_some(auth)
                });
            let auth = own_auth.unwrap_or_else(|| inherited_auth.to_owned());

            let own_duration = float_of(map, &["video_duration", "duration"]);
            let duration = if own_duration > 0.0 {
                normalize_duration(own_duration)
            } else {
                inherited_duration
            };

            let main_play_url = string_of(map, &["main_play_url", "main_url", "url", "play_url"]);
            let backup_play_url = string_of(map, &["backup_play_url", "backup_url"]);
            if !main_play_url.is_empty() || !backup_play_url.is_empty() {
                let mut quality = string_of(map, &["quality", "definition", "quality_type"]);
                if quality.is_empty() {
                    quality = quality_hint(&string_of(map, &["gear_des_key"]));
                }
                if quality.is_empty() {
                    quality = quality_hint(key_hint);
                }
                let mut format = string_of(map, &["format", "vtype", "file_format"]);
                let mut bitrate = int_of(map, &["bitrate", "br", "bit_rate"]);
                let mut size = int_of(map, &["size", "file_size", "data_size"]);
                if let Some(meta) = map.get("video_meta").and_then(Value::as_object) {
                    if format.is_empty() {
                        format = string_of(meta, &["format", "vtype", "codec_type"]);
                    }
                    if bitrate == 0 {
                        bitrate = int_of(meta, &["bitrate", "real_bitrate", "bit_rate"]);
                    }
                    if size == 0 {
                        size = int_of(meta, &["size", "file_size"]);
                    }
                    if quality.is_empty() {
                        quality = string_of(meta, &["quality", "definition", "quality_type"]);
                    }
                }
                out.push(PlayInfo {
                    main_play_url,
                    backup_play_url: backup_play_url_or_list(map),
                    play_auth: auth.clone(),
                    size,
                    format,
                    bitrate,
                    quality,
                    duration,
                });
            }

            for (key, child) in map {
                collect_model(child, key, &auth, duration, out);
            }
        }
        Value::Array(items) => {
            for child in items {
                collect_model(child, key_hint, inherited_auth, inherited_duration, out);
            }
        }
        Value::String(text) => {
            // 有些版本把 `video_model` 再编码成字符串（可能套了几层）。
            if (text.starts_with('{') || text.starts_with('['))
                && let Ok(parsed) = serde_json::from_str::<Value>(text)
            {
                collect_model(&parsed, key_hint, inherited_auth, inherited_duration, out);
            }
        }
        _ => {}
    }
}

fn backup_play_url_or_list(map: &serde_json::Map<String, Value>) -> String {
    let direct = string_of(map, &["backup_play_url", "backup_url"]);
    if !direct.is_empty() {
        return direct;
    }
    for key in ["backup_urls", "backupUrls", "url_list", "UrlList"] {
        if let Some(first) = map
            .get(key)
            .and_then(Value::as_array)
            .and_then(|list| list.first())
            && let Some(text) = first.as_str()
        {
            return text.to_owned();
        }
    }
    String::new()
}

fn quality_hint(key: &str) -> String {
    let key = key.trim().to_ascii_lowercase();
    for token in [
        "hires", "lossless", "flac", "sq", "highest", "higher", "standard", "normal",
    ] {
        if key.contains(token) {
            return token.to_owned();
        }
    }
    String::new()
}

fn string_of(map: &serde_json::Map<String, Value>, keys: &[&str]) -> String {
    for key in keys {
        if let Some(text) = map.get(*key).and_then(Value::as_str)
            && !text.trim().is_empty()
        {
            return text.trim().to_owned();
        }
    }
    String::new()
}

fn int_of(map: &serde_json::Map<String, Value>, keys: &[&str]) -> i64 {
    for key in keys {
        if let Some(number) = map.get(*key) {
            if let Some(value) = number.as_i64() {
                return value;
            }
            if let Some(value) = number.as_f64() {
                return value as i64;
            }
            if let Some(text) = number.as_str()
                && let Ok(value) = text.parse::<i64>()
            {
                return value;
            }
        }
    }
    0
}

fn float_of(map: &serde_json::Map<String, Value>, keys: &[&str]) -> f64 {
    for key in keys {
        if let Some(number) = map.get(*key) {
            if let Some(value) = number.as_f64() {
                return value;
            }
            if let Some(text) = number.as_str()
                && let Ok(value) = text.parse::<f64>()
            {
                return value;
            }
        }
    }
    0.0
}

/// 按偏好挑最优：先筛「不超过档位分」，筛空了就回退全量。
fn pick(candidates: &[Stream], preference: &str, track_duration: i64) -> Option<Stream> {
    if candidates.is_empty() {
        return None;
    }
    let cap = preference_rank(preference);
    let allowed: Vec<&Stream> = match cap {
        None => candidates.iter().collect(),
        Some(cap) => {
            let filtered: Vec<&Stream> = candidates
                .iter()
                .filter(|stream| stream.rank() <= cap)
                .collect();
            if filtered.is_empty() {
                // 没有「不超过偏好」的候选时，取**最接近**的（分数最低），
                // 而不是直接退回整体最优，免得「省流」拿回一首无损。
                let lowest = candidates
                    .iter()
                    .map(|stream| stream.rank())
                    .min()
                    .unwrap_or(0);
                candidates
                    .iter()
                    .filter(|stream| stream.rank() == lowest)
                    .collect()
            } else {
                filtered
            }
        }
    };
    let mut best: Option<&Stream> = None;
    for stream in allowed {
        match best {
            None => best = Some(stream),
            Some(current) => {
                if better(stream, current, track_duration) {
                    best = Some(stream);
                }
            }
        }
    }
    best.cloned()
}

/// `a` 是否优于 `b`：先看完整度（时长），再看档位 / 码率 / 体积。
fn better(a: &Stream, b: &Stream, track_duration: i64) -> bool {
    let a_preview = is_preview(a, track_duration);
    let b_preview = is_preview(b, track_duration);
    if a_preview != b_preview {
        return !a_preview;
    }
    if (a.duration - b.duration).abs() > 1.0 {
        return a.duration > b.duration;
    }
    if a.rank() != b.rank() {
        return a.rank() > b.rank();
    }
    if normalize_bitrate(a.bitrate) != normalize_bitrate(b.bitrate) {
        return normalize_bitrate(a.bitrate) > normalize_bitrate(b.bitrate);
    }
    a.size > b.size
}

/// 明显短于整曲就是试听片段。
fn is_preview(stream: &Stream, track_duration: i64) -> bool {
    if stream.duration <= 0.0 || track_duration <= 0 {
        return false;
    }
    stream.duration + 5.0 < track_duration as f64
}

// ── 链接 / ID ───────────────────────────────────────────────────────

/// 从文本里认出 track id（汽水的 id 至少 10 位数字）。
pub fn track_id_of(text: &str) -> Option<String> {
    let text = text.trim();
    if text.len() > 10 && text.chars().all(|ch| ch.is_ascii_digit()) {
        return Some(text.to_owned());
    }
    let decoded = percent_decode(text);
    for candidate in [text, decoded.as_str()] {
        for key in ["track_id", "trackId"] {
            for prefix in [format!("{key}="), format!("\"{key}\":\"")] {
                if let Some(rest) = candidate.split(prefix.as_str()).nth(1) {
                    let digits: String =
                        rest.chars().take_while(|ch| ch.is_ascii_digit()).collect();
                    if digits.len() > 10 {
                        return Some(digits);
                    }
                }
            }
        }
        let lower = candidate.to_ascii_lowercase();
        for marker in ["/track/", "/song/", "%2ftrack%2f", "%2fsong%2f"] {
            if let Some(index) = lower.find(marker) {
                let rest = &candidate[index + marker.len()..];
                let digits: String = rest.chars().take_while(|ch| ch.is_ascii_digit()).collect();
                if digits.len() > 10 {
                    return Some(digits);
                }
            }
        }
    }
    None
}

/// 文本里第一个汽水链接。
pub fn first_link(text: &str) -> Option<String> {
    for candidate in text.split_whitespace() {
        let start = match candidate.find("http") {
            Some(start) => start,
            None => continue,
        };
        let trimmed = candidate[start..].trim_end_matches([
            ')', '，', ',', '。', '.', '！', '!', '？', '?', '：', ':', ';', '；', '"', '」', '』',
            '】', '、',
        ]);
        if is_soda_host(trimmed) {
            return Some(trimmed.to_owned());
        }
    }
    None
}

fn is_soda_host(url: &str) -> bool {
    let Some(host) = crate::netease::host_of(url) else {
        return false;
    };
    if host == "music.douyin.com" {
        // 短链最终会落到 `music.douyin.com/qishui/share/track?track_id=…`：
        // 只认 `/qishui/` 下的路径，别把同域的其它分享页也吞进来。
        return url.contains("/qishui/");
    }
    host == "qishui.com"
        || host.ends_with(".qishui.com")
        || host == "qishui.douyin.com"
        || host.ends_with(".qishui.douyin.com")
}

/// 一条汽水分享链接最终指向什么。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Share {
    /// 单曲（`/track/<id>`、`track_id=…`）。
    Track(String),
    /// UGC 音乐视频（`ugc_video_id=…`）。
    Video(String),
}

/// 从分享链接 / 短链 / 文本里判断是单曲还是 UGC 视频。
///
/// 短链要跟一次重定向才能知道落点是 `share/track` 还是 `share/ugc_video`。
pub async fn classify_share(agent: &Agent, auth: &SodaAuth, text: &str) -> Result<Share, String> {
    if let Some(share) = classify_text(text) {
        return Ok(share);
    }
    let Some(link) = first_link(text) else {
        return Err(format!(
            "没认出汽水歌曲 / 视频 ID：`{}`",
            crate::log::shorten(text, 60)
        ));
    };
    // 短链偶尔会回一个没有 SSR 数据的中间页（风控 / CDN 抖动），重试一次。
    for attempt in 0..2 {
        let reply = get_with_cookie(agent, &link, auth, CHROME_UA).await?;
        if let Some(id) = track_id_of(&reply.final_url) {
            return Ok(Share::Track(id));
        }
        if let Some(id) = crate::soda_video::video_id_of(&reply.final_url) {
            return Ok(Share::Video(id));
        }
        let body = reply.text();
        if let Some(id) = track_id_of(&body) {
            return Ok(Share::Track(id));
        }
        if let Some(id) = crate::soda_video::video_id_of(&body) {
            return Ok(Share::Video(id));
        }
        if attempt == 0 {
            tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        }
    }
    Err("分享页跟到了最终地址，但里面没有歌曲 / 视频 ID".to_owned())
}

/// 纯文本判定（不发网络）：先单曲后视频。
fn classify_text(text: &str) -> Option<Share> {
    if let Some(id) = track_id_of(text) {
        return Some(Share::Track(id));
    }
    crate::soda_video::video_id_of(text).map(Share::Video)
}

/// 把「分享链接 / 短链 / ID」解成 track id。
///
/// 分享短链（`qishui.douyin.com/s/…`）要跟一次重定向；目标地址或落地页正文里才有 id。
pub async fn resolve_id(agent: &Agent, auth: &SodaAuth, text: &str) -> Result<String, String> {
    if let Some(id) = track_id_of(text) {
        return Ok(id);
    }
    let Some(link) = first_link(text) else {
        return Err(format!(
            "没认出汽水歌曲 ID：`{}`",
            crate::log::shorten(text, 60)
        ));
    };
    let reply = get_with_cookie(agent, &link, auth, CHROME_UA).await?;
    if let Some(id) = track_id_of(&reply.final_url) {
        return Ok(id);
    }
    if let Some(id) = track_id_of(&reply.text()) {
        return Ok(id);
    }
    Err("分享页跟到了最终地址，但里面没有歌曲 ID".to_owned())
}

pub fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            let hex = &text[index + 1..index + 3];
            if let Ok(value) = u8::from_str_radix(hex, 16) {
                out.push(value);
                index += 3;
                continue;
            }
        }
        out.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&out).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quality_aliases_and_default_are_usable() {
        assert_eq!(parse_quality("LOSSLESS"), Some("lossless"));
        assert_eq!(parse_quality("无损"), Some("lossless"));
        assert_eq!(parse_quality("hq"), Some("highest"));
        assert_eq!(parse_quality("nope"), None);
        assert_eq!(parse_quality(DEFAULT_QUALITY), Some(DEFAULT_QUALITY));
    }

    #[test]
    fn quality_ranking_prefers_lossless_then_bitrate() {
        assert!(
            quality_rank("lossless", "flac", 900_000) > quality_rank("highest", "mp3", 320_000)
        );
        assert!(
            quality_rank("hires", "flac", 1_500_000) > quality_rank("lossless", "flac", 900_000)
        );
        assert_eq!(quality_rank("", "", 320_000), 70);
        assert_eq!(quality_rank("medium", "aac", 128_000), 50);
    }

    #[test]
    fn preference_caps_and_unknown_values() {
        assert_eq!(preference_rank("best"), None);
        assert_eq!(preference_rank("lossless"), Some(100));
        assert_eq!(preference_rank("highest"), Some(80));
        assert_eq!(preference_rank("medium"), Some(50));
        assert_eq!(preference_rank("low"), Some(20));
        assert_eq!(preference_rank("unknown"), None);
    }

    fn stream(url: &str, quality: &str, bitrate: i64, duration: f64) -> Stream {
        Stream {
            url: url.to_owned(),
            quality: quality.to_owned(),
            bitrate,
            duration,
            ..Stream::default()
        }
    }

    #[test]
    fn picking_respects_preference_and_preview_length() {
        let full = stream("a", "lossless", 900_000, 240.0);
        let high = stream("b", "highest", 320_000, 240.0);
        let preview = stream("c", "lossless", 900_000, 30.0);
        let candidates = vec![preview.clone(), full.clone(), high.clone()];

        // 不限制：完整优先，其次音质
        assert_eq!(pick(&candidates, "best", 240).unwrap().url, "a");
        // 极高及以下：只留 b（c 的档位超了；即便不过滤也会因时长被淘汰）
        assert_eq!(pick(&candidates, "highest", 240).unwrap().url, "b");
        // 偏好档位没有候选 → 取最接近的（80），而不是直接跳到无损（100）
        assert_eq!(pick(&candidates, "low", 240).unwrap().url, "b");
        assert_eq!(pick(&candidates, "medium", 240).unwrap().url, "b");
        assert!(pick(&[], "best", 240).is_none());
    }

    #[test]
    fn requested_quality_is_never_exceeded_when_a_suitable_candidate_exists() {
        let medium = stream("m", "medium", 65_000, 200.0);
        let higher = stream("h", "higher", 130_000, 200.0);
        let candidates = vec![higher.clone(), medium.clone()];
        assert_eq!(pick(&candidates, "medium", 200).unwrap().url, "m");
        assert_eq!(
            pick(&candidates, "low", 200).unwrap().url,
            "m",
            "没有更低时取最接近"
        );
        assert_eq!(pick(&candidates, "highest", 200).unwrap().url, "h");
        assert_eq!(pick(&candidates, "best", 200).unwrap().url, "h");
    }

    #[test]
    fn search_payload_is_parsed_from_result_groups() {
        let value = serde_json::json!({
            "result_groups": [{
                "data": [{
                    "entity": {
                        "track": {
                            "id": "7304719759323564095",
                            "name": "晴天",
                            "duration": 269,
                            "artists": [{"name": "周杰伦"}],
                            "album": {"name": "叶惠美", "url_cover": {"urls": ["https://p/1.jpg"]}},
                            "label_info": {"only_vip_playable": true}
                        }
                    }
                }]
            }]
        });
        let tracks = parse_search(&value).unwrap();
        assert_eq!(tracks.len(), 1);
        assert_eq!(tracks[0].id, "7304719759323564095");
        assert_eq!(tracks[0].artist_text(), "周杰伦");
        assert!(tracks[0].is_vip);
        assert!(tracks[0].cover.ends_with(".jpg"), "{}", tracks[0].cover);
    }

    #[test]
    fn track_ids_are_recognised_in_every_shape() {
        assert_eq!(
            track_id_of("7304719759323564095").as_deref(),
            Some("7304719759323564095")
        );
        assert_eq!(
            track_id_of("https://www.qishui.com/track/7304719759323564095").as_deref(),
            Some("7304719759323564095")
        );
        assert_eq!(
            track_id_of("https://x/?track_id=7304719759323564095").as_deref(),
            Some("7304719759323564095")
        );
        assert_eq!(
            track_id_of("%2Ftrack%2F7304719759323564095").as_deref(),
            Some("7304719759323564095")
        );
        assert_eq!(track_id_of("周杰伦"), None);
    }

    #[test]
    fn share_text_tells_tracks_from_ugc_videos() {
        assert_eq!(
            classify_text("https://www.qishui.com/track/7304719759323564095"),
            Some(Share::Track("7304719759323564095".to_owned()))
        );
        assert_eq!(
            classify_text(
                "https://music.douyin.com/qishui/share/ugc_video?ugc_video_id=7625615883351772468"
            ),
            Some(Share::Video("7625615883351772468".to_owned()))
        );
        // 短链本身看不出类型，需要联网跟随重定向。
        assert_eq!(classify_text("https://qishui.douyin.com/s/iXvvC5YV/"), None);
    }

    #[test]
    fn only_soda_hosts_are_picked_up() {
        assert!(first_link("https://www.qishui.com/track/1").is_some());
        assert!(first_link("https://qishui.douyin.com/s/abc/").is_some());
        assert!(first_link("https://qishui.com.evil.test/track/1").is_none());
        assert!(first_link("https://music.163.com/song?id=1").is_none());
        // 短链的最终落地页
        assert!(
            first_link("https://music.douyin.com/qishui/share/track?track_id=7649060217267111937")
                .is_some()
        );
        assert!(first_link("https://music.douyin.com/other/share?x=1").is_none());
    }

    #[test]
    fn video_model_is_flattened_with_inherited_auth_and_duration() {
        let model = serde_json::json!({
            "video_model": {
                "duration": 240,
                "encrypt_info": {"spade_a": "auth-value"},
                "gear_list": {
                    "lossless": {
                        "main_play_url": "https://cdn/lossless.m4a",
                        "format": "m4a",
                        "bitrate": 900000,
                        "size": 1000
                    }
                }
            }
        });
        let mut out = Vec::new();
        collect_model(&model, "", "", 0.0, &mut out);
        assert_eq!(out.len(), 1, "{out:?}");
        assert_eq!(out[0].main_play_url, "https://cdn/lossless.m4a");
        assert_eq!(out[0].play_auth, "auth-value");
        assert_eq!(out[0].quality, "lossless", "档位从 key 名猜出来");
        assert_eq!(out[0].duration, 240.0, "时长要能继承");
    }

    #[test]
    fn player_info_pascal_case_is_mapped() {
        // 只测映射逻辑需要的形状，不经过网络。
        let value = serde_json::json!({
            "Result": {"Data": {"PlayInfoList": [{
                "MainPlayUrl": "https://cdn/a.m4a",
                "PlayAuth": "auth",
                "Format": "m4a",
                "Bitrate": 320000,
                "Size": 1234,
                "Duration": 240.0,
                "Quality": "highest"
            }]}}
        });
        let list = value
            .pointer("/Result/Data/PlayInfoList")
            .unwrap()
            .as_array()
            .unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(
            list[0].get("MainPlayUrl").and_then(Value::as_str),
            Some("https://cdn/a.m4a")
        );
    }

    #[test]
    fn percent_decoding_is_only_a_fallback() {
        assert_eq!(percent_decode("a%2Fb"), "a/b");
        assert_eq!(percent_decode("no-escapes"), "no-escapes");
    }

    #[test]
    fn the_default_is_an_unlimited_gear() {
        assert_eq!(preference_rank(DEFAULT_QUALITY), None);
    }
}
