//! 网易云音乐：eapi 签名（AES-128-ECB + MD5）+ 搜索 / 详情 / 取流。
//!
//! 接口形状照参考实现（`plugins/163music/music.ts`）：所有请求都走 eapi，
//! 请求体加密后再以 `params=<hex>` 表单发出；服务端用同一套规则校验。
//!
//! ```text
//! digest = md5("nobody" + /api/路径 + "use" + JSON + "md5forencrypt")
//! 明文   = /api/路径 + "-36cd479b6b5-" + JSON + "-36cd479b6b5-" + digest
//! params = HEX(AES-128-ECB(明文, key="e82ckenh8dichen8"))
//! ```
//!
//! 鉴权材料只有 cookie（`MUSIC_U` 是登录态，`os` / `appver` 是客户端身份）。
//! 读不到 cookie 也能匿名搜索、取免费曲目。

use aes::Aes128;
#[cfg(test)]
use aes::cipher::BlockDecrypt;
use aes::cipher::{BlockEncrypt, KeyInit, generic_array::GenericArray};
use serde::{Deserialize, Deserializer};

use crate::auth::{NETEASE_USER_AGENT, NeteaseAuth};
use crate::http::{self, Agent};
use crate::log;

/// eapi 固定密钥（客户端内置）。
const EAPI_KEY: &[u8; 16] = b"e82ckenh8dichen8";

/// 路径 / JSON / digest 之间的固定分隔符。
const EAPI_SEPARATOR: &str = "-36cd479b6b5-";

const SEARCH_URL: &str = "https://interface.music.163.com/eapi/search/song/list/page";
const DETAIL_URL: &str = "https://interfacepc.music.163.com/eapi/v3/song/detail";
const PLAY_URL: &str = "https://interface3.music.163.com/eapi/song/enhance/player/url/v1";

/// 一个可选音质。`key` 是接口要的 level，`label` 是给用户看的名字。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Quality {
    pub key: &'static str,
    pub label: &'static str,
}

/// 支持的全部音质（顺序 = 帮助信息里的展示顺序）。
pub const QUALITIES: &[Quality] = &[
    Quality {
        key: "standard",
        label: "标准",
    },
    Quality {
        key: "higher",
        label: "较高",
    },
    Quality {
        key: "exhigh",
        label: "极高",
    },
    Quality {
        key: "lossless",
        label: "无损",
    },
    Quality {
        key: "hires",
        label: "Hi-Res",
    },
    Quality {
        key: "jymaster",
        label: "超清母带",
    },
    Quality {
        key: "sky",
        label: "沉浸环绕",
    },
];

/// 默认音质（与参考实现的 `/music163 <id>` 默认值一致）。
pub const DEFAULT_QUALITY: &str = "exhigh";

/// 把用户输入的音质名归一成接口 level。别名也认。
pub fn parse_quality(name: &str) -> Option<&'static str> {
    let name = name.trim().to_ascii_lowercase();
    let canonical = match name.as_str() {
        "standard" | "std" | "128" | "128k" | "标准" => "standard",
        "higher" | "high" | "192" | "192k" | "较高" => "higher",
        "exhigh" | "extreme" | "320" | "320k" | "极高" => "exhigh",
        // 参考实现把 `set` 当无损；`sq` / `flac` 是常见别名。
        "lossless" | "set" | "sq" | "flac" | "无损" => "lossless",
        "hires" | "hi-res" | "hr" | "高清臻音" => "hires",
        "jymaster" | "master" | "超清母带" => "jymaster",
        "sky" | "沉浸环绕" => "sky",
        _ => return None,
    };
    QUALITIES
        .iter()
        .find(|quality| quality.key == canonical)
        .map(|quality| quality.key)
}

/// 音质 key → 展示名；接口回的实际档位可能是降级后的，展示用它。
pub fn quality_label(key: &str) -> String {
    QUALITIES
        .iter()
        .find(|quality| quality.key.eq_ignore_ascii_case(key))
        .map(|quality| format!("{}（{}）", quality.label, quality.key))
        .unwrap_or_else(|| key.to_owned())
}

/// 可用音质列表（给帮助 / 报错文案用）。
pub fn quality_help() -> String {
    QUALITIES
        .iter()
        .map(|quality| quality.key)
        .collect::<Vec<_>>()
        .join(" / ")
}

// ── eapi 签名 ───────────────────────────────────────────────────────

/// 计算 eapi 的 MD5 摘要（小写十六进制）。
pub fn digest(api_path: &str, payload_json: &str) -> String {
    crate::md5_hex(&format!("nobody{api_path}use{payload_json}md5forencrypt"))
}

/// 把 payload 加密成 `params=` 的值（小写十六进制）。
pub fn encrypt_params(api_path: &str, payload_json: &str) -> String {
    let digest = digest(api_path, payload_json);
    let plain = format!("{api_path}{EAPI_SEPARATOR}{payload_json}{EAPI_SEPARATOR}{digest}");
    hex_lower(&aes_ecb_encrypt(plain.as_bytes()))
}

/// `/eapi/...` → `/api/...`（签名与 digest 都用 API 路径）。
pub fn api_path_of(eapi_path: &str) -> String {
    eapi_path.replacen("/eapi/", "/api/", 1)
}

fn aes_ecb_encrypt(plain: &[u8]) -> Vec<u8> {
    let cipher = Aes128::new(GenericArray::from_slice(EAPI_KEY));
    let mut padded = plain.to_vec();
    let pad = 16 - plain.len() % 16;
    padded.extend(std::iter::repeat_n(pad as u8, pad));
    for chunk in padded.chunks_mut(16) {
        cipher.encrypt_block(GenericArray::from_mut_slice(chunk));
    }
    padded
}

#[cfg(test)]
fn aes_ecb_decrypt(cipher_text: &[u8]) -> Vec<u8> {
    let cipher = Aes128::new(GenericArray::from_slice(EAPI_KEY));
    let mut data = cipher_text.to_vec();
    for chunk in data.chunks_mut(16) {
        cipher.decrypt_block(GenericArray::from_mut_slice(chunk));
    }
    let pad = *data.last().unwrap_or(&0) as usize;
    if (1..=16).contains(&pad) && data.len() >= pad {
        data.truncate(data.len() - pad);
    }
    data
}

fn hex_lower(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

#[cfg(test)]
fn hex_decode(text: &str) -> Option<Vec<u8>> {
    let bytes = text.as_bytes();
    if !bytes.len().is_multiple_of(2) {
        return None;
    }
    let mut out = Vec::with_capacity(bytes.len() / 2);
    for pair in bytes.chunks(2) {
        let high = (pair[0] as char).to_digit(16)?;
        let low = (pair[1] as char).to_digit(16)?;
        out.push((high * 16 + low) as u8);
    }
    Some(out)
}

// ── 请求 ────────────────────────────────────────────────────────────

/// 发一个 eapi 请求，返回解析后的 JSON。
async fn eapi_request(
    agent: &Agent,
    auth: &NeteaseAuth,
    url: &str,
    payload: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    let path = api_path_of(url.split("music.163.com").nth(1).unwrap_or(url));
    let payload_json =
        serde_json::to_string(payload).map_err(|error| format!("请求体编码失败：{error}"))?;
    let params = encrypt_params(&path, &payload_json);

    let mut headers = vec![
        ("User-Agent", NETEASE_USER_AGENT),
        ("Accept", "*/*"),
        ("Referer", "https://music.163.com"),
    ];
    if !auth.cookie_header.is_empty() {
        headers.push(("Cookie", auth.cookie_header.as_str()));
    }
    log::debug(&format!(
        "网易云 eapi {}（Cookie {}）",
        path,
        log::cookie_summary(&auth.cookie_header)
    ));

    let reply = http::post_form(agent, url, &[("params", &params)], &headers).await?;
    if !(200..300).contains(&reply.status) {
        return Err(format!(
            "网易云接口 HTTP {}：{}",
            reply.status,
            reply.text()
        ));
    }
    let value = reply.json()?;
    if let Some(code) = value.get("code").and_then(|code| code.as_i64())
        && code != 200
    {
        let message = value
            .get("message")
            .or_else(|| value.get("msg"))
            .and_then(|item| item.as_str())
            .unwrap_or("未知错误");
        return Err(format!("网易云接口返回 code={code}：{message}"));
    }
    Ok(value)
}

/// 每次请求带一个随机 requestId（参考实现同一套 header 形状）。
///
/// 用 `serde_json` 拼而不是格式化字符串：cookie 里的 `deviceId` 可能带引号，
/// 手写模板会把整个 payload 的 JSON 弄坏。
fn header_json(auth: &NeteaseAuth) -> String {
    let device_id = auth
        .cookies
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("deviceId"))
        .map(|(_, value)| value.as_str())
        .unwrap_or("fuyubot");
    let request_id = crate::now_millis() % 10_000_000 + 20_000_000;
    serde_json::json!({
        "os": "pc",
        "appver": "",
        "osver": "",
        "deviceId": device_id,
        "requestId": request_id.to_string(),
    })
    .to_string()
}

// ── 数据类型 ────────────────────────────────────────────────────────

/// 一首歌的基础信息（搜索结果里就有）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Track {
    pub id: u64,
    pub name: String,
    pub artists: Vec<String>,
    pub album: String,
    pub cover: String,
    pub duration_ms: u64,
}

impl Track {
    pub fn artist_text(&self) -> String {
        if self.artists.is_empty() {
            "未知歌手".to_owned()
        } else {
            self.artists.join("/")
        }
    }

    pub fn duration_secs(&self) -> i64 {
        (self.duration_ms / 1000) as i64
    }

    pub fn link(&self) -> String {
        format!("https://music.163.com/song?id={}", self.id)
    }
}

/// 实际拿到的流。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Stream {
    pub url: String,
    /// 服务端实际给的档位（可能比请求的低）。
    pub level: String,
    pub format: String,
    pub size: u64,
    pub bitrate: u64,
    /// 是否只是试听片段（`freeTrialInfo` 非空）。
    pub trial: bool,
}

/// 解析 + 取流的最终结果。
#[derive(Debug, Clone)]
pub struct Resolved {
    pub track: Track,
    pub stream: Stream,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SearchResponse {
    #[serde(default)]
    data: SearchData,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SearchData {
    #[serde(default)]
    resources: Vec<Resource>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Resource {
    #[serde(default)]
    base_info: BaseInfo,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BaseInfo {
    #[serde(default)]
    simple_song_data: SimpleSong,
}

#[derive(Debug, Default, Deserialize)]
struct SimpleSong {
    #[serde(default)]
    id: u64,
    #[serde(default)]
    name: String,
    #[serde(default)]
    ar: Vec<Artist>,
    #[serde(default)]
    al: Album,
    #[serde(default)]
    dt: u64,
}

#[derive(Debug, Default, Deserialize)]
struct Artist {
    #[serde(default)]
    name: String,
}

#[derive(Debug, Default, Deserialize)]
struct Album {
    #[serde(default)]
    name: String,
    #[serde(default, rename = "picUrl")]
    pic_url: String,
}

#[derive(Debug, Deserialize)]
struct DetailResponse {
    #[serde(default)]
    songs: Vec<SimpleSong>,
}

#[derive(Debug, Deserialize)]
struct PlayResponse {
    #[serde(default)]
    data: Vec<PlayEntry>,
}

#[derive(Debug, Default, Deserialize)]
struct PlayEntry {
    #[serde(default)]
    url: Option<String>,
    // 没版权 / 需要会员时，这些字段会是 **null** 而不是缺省：`#[serde(default)]`
    // 只兜「字段不存在」，兜不住 null，所以统一按「null = 默认值」处理。
    #[serde(default, deserialize_with = "null_as_default")]
    level: String,
    #[serde(default, rename = "type", deserialize_with = "null_as_default")]
    format: String,
    #[serde(default, deserialize_with = "null_as_default")]
    size: u64,
    #[serde(default, deserialize_with = "null_as_default")]
    br: u64,
    #[serde(default, deserialize_with = "null_as_default")]
    code: i64,
    #[serde(default, rename = "freeTrialInfo")]
    free_trial_info: Option<serde_json::Value>,
}

/// `null` 当作类型默认值（`0` / 空串）。网易云没版权时整条记录字段全是 null。
fn null_as_default<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: Default + Deserialize<'de>,
{
    Ok(Option::<T>::deserialize(deserializer)?.unwrap_or_default())
}

/// 封面统一换成 320×320 的小图：它要当 Telegram 的 `inputThumbnail` 发。
fn thumbnail_cover(url: &str) -> String {
    let url = url.trim();
    if url.is_empty() || url.contains("param=") {
        return url.to_owned();
    }
    if url.contains('?') {
        format!("{url}&param=320y320")
    } else {
        format!("{url}?param=320y320")
    }
}

impl SimpleSong {
    fn into_track(self) -> Track {
        Track {
            id: self.id,
            name: self.name,
            artists: self.ar.into_iter().map(|artist| artist.name).collect(),
            album: self.al.name,
            cover: thumbnail_cover(&self.al.pic_url),
            duration_ms: self.dt,
        }
    }
}

// ── 搜索 / 详情 / 取流 ──────────────────────────────────────────────

/// 搜索歌曲，最多 `limit` 条。
pub async fn search(
    agent: &Agent,
    auth: &NeteaseAuth,
    keyword: &str,
    limit: usize,
) -> Result<Vec<Track>, String> {
    let limit = limit.clamp(1, 30);
    let payload = serde_json::json!({
        "keyword": keyword,
        "scene": "NORMAL",
        "needCorrect": "true",
        "limit": limit,
        "offset": 0,
        "e_r": false,
        "header": header_json(auth),
    });
    let value = eapi_request(agent, auth, SEARCH_URL, &payload).await?;
    let response: SearchResponse = serde_json::from_value(value)
        .map_err(|error| format!("网易云搜索返回解析失败：{error}"))?;
    let tracks: Vec<Track> = response
        .data
        .resources
        .into_iter()
        .map(|resource| resource.base_info.simple_song_data.into_track())
        .filter(|track| track.id != 0)
        .collect();
    if tracks.is_empty() {
        return Err(format!("网易云没搜到「{keyword}」相关的歌"));
    }
    Ok(tracks)
}

/// 歌曲详情（标题 / 歌手 / 专辑 / 时长）。
pub async fn detail(agent: &Agent, auth: &NeteaseAuth, id: u64) -> Result<Track, String> {
    let c = format!(r#"[{{"id":"{id}","v":0}}]"#);
    let payload = serde_json::json!({
        "c": c,
        "trialMode": "-1",
        "e_r": false,
        "header": header_json(auth),
    });
    let value = eapi_request(agent, auth, DETAIL_URL, &payload).await?;
    let response: DetailResponse = serde_json::from_value(value)
        .map_err(|error| format!("网易云歌曲详情解析失败：{error}"))?;
    response
        .songs
        .into_iter()
        .next()
        .map(SimpleSong::into_track)
        .filter(|track| track.id != 0)
        .ok_or_else(|| format!("网易云没有 ID {id} 这首歌（可能已下架）"))
}

/// 取播放地址。`level` 必须是 [`QUALITIES`] 里的 key。
pub async fn play_url(
    agent: &Agent,
    auth: &NeteaseAuth,
    id: u64,
    level: &str,
) -> Result<Stream, String> {
    let immerse = if level == "sky" { "c51" } else { "standard" };
    let payload = serde_json::json!({
        "ids": [id],
        "level": level,
        "encodeType": "flac",
        "header": header_json(auth),
        "immerseType": immerse,
    });
    let value = eapi_request(agent, auth, PLAY_URL, &payload).await?;
    let response: PlayResponse = serde_json::from_value(value)
        .map_err(|error| format!("网易云播放地址解析失败：{error}"))?;
    let entry = response
        .data
        .into_iter()
        .next()
        .ok_or_else(|| "网易云没有返回播放地址".to_owned())?;

    let url = entry.url.unwrap_or_default();
    if url.trim().is_empty() {
        return Err(format!(
            "网易云没给这首歌的「{level}」流（code={}）。常见原因：没登录 / 不是 VIP / 版权受限；\
             换个低音质或补上 MUSIC_U 再试",
            entry.code
        ));
    }

    Ok(Stream {
        url,
        level: if entry.level.trim().is_empty() {
            level.to_owned()
        } else {
            entry.level
        },
        format: if entry.format.trim().is_empty() {
            "mp3".to_owned()
        } else {
            entry.format
        },
        size: entry.size,
        bitrate: entry.br,
        trial: entry.free_trial_info.is_some_and(|info| !info.is_null()),
    })
}

/// 一次拿齐详情 + 流。
pub async fn resolve(
    agent: &Agent,
    auth: &NeteaseAuth,
    id: u64,
    level: &str,
) -> Result<Resolved, String> {
    let track = detail(agent, auth, id).await?;
    let stream = play_url(agent, auth, id, level).await?;
    log::debug(&format!(
        "网易云 {}《{}》：请求 {}, 实际 {}，{} 字节{}",
        id,
        track.name,
        level,
        stream.level,
        stream.size,
        if stream.trial { "（试听）" } else { "" }
    ));
    Ok(Resolved { track, stream })
}

// ── 链接 / ID ───────────────────────────────────────────────────────

/// 从文本里认出歌曲 ID：纯数字、`/song/<id>`、`id=<id>` 都算。
pub fn song_id_of(text: &str) -> Option<u64> {
    let text = text.trim();
    if !text.is_empty() && text.chars().all(|ch| ch.is_ascii_digit()) {
        return text.parse().ok();
    }
    if !text.to_ascii_lowercase().contains("song") {
        return None;
    }
    for marker in ["/song/", "song?id=", "id="] {
        if let Some(rest) = text.split(marker).nth(1) {
            let digits: String = rest.chars().take_while(|ch| ch.is_ascii_digit()).collect();
            if !digits.is_empty() {
                return digits.parse().ok();
            }
        }
    }
    None
}

/// 文本里第一个网易云链接（含短链 `163cn.tv`）。
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
        if is_netease_host(trimmed) {
            return Some(trimmed.to_owned());
        }
    }
    None
}

fn is_netease_host(url: &str) -> bool {
    let Some(host) = host_of(url) else {
        return false;
    };
    host == "music.163.com"
        || host == "y.music.163.com"
        || host.ends_with(".music.163.com")
        || host == "163cn.tv"
        || host.ends_with(".163cn.tv")
}

/// 取出 URL 的 host（小写、去掉端口 / userinfo）。按 host 判，不做子串 contains。
pub fn host_of(url: &str) -> Option<String> {
    let rest = url.split_once("://").map(|(_, rest)| rest).unwrap_or(url);
    let authority = rest.split(['/', '?', '#']).next()?;
    let host = authority.rsplit('@').next()?;
    let host = host.split(':').next()?;
    (!host.is_empty()).then(|| host.to_ascii_lowercase())
}

/// 把「分享链接 / 短链 / ID」解成歌曲 ID。
///
/// 短链（`163cn.tv`）要跟一次重定向：目标 URL 的查询串里才有 `id`。
pub async fn resolve_id(agent: &Agent, text: &str) -> Result<u64, String> {
    if let Some(id) = song_id_of(text) {
        return Ok(id);
    }
    let Some(link) = first_link(text) else {
        return Err(format!(
            "没认出网易云歌曲 ID：`{}`",
            crate::log::shorten(text, 60)
        ));
    };
    if host_of(&link).is_some_and(|host| host.ends_with("163cn.tv")) {
        let reply = http::get(
            agent,
            &link,
            &[("User-Agent", NETEASE_USER_AGENT), ("Accept", "*/*")],
        )
        .await?;
        if let Some(id) = song_id_of(&reply.final_url) {
            return Ok(id);
        }
        return Err("短链跟到了最终地址，但里面没有歌曲 ID".to_owned());
    }
    Err(format!(
        "没从链接里认出歌曲 ID：{}",
        crate::log::redact_url(&link)
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn md5_matches_a_known_vector() {
        assert_eq!(crate::md5_hex("abc"), "900150983cd24fb0d6963f7d28e17f72");
    }

    #[test]
    fn eapi_params_round_trip_contains_path_json_and_digest() {
        let path = "/api/song/enhance/player/url/v1";
        let json = r#"{"ids":[1],"level":"exhigh"}"#;
        let params = encrypt_params(path, json);
        assert!(
            params
                .chars()
                .all(|ch| ch.is_ascii_hexdigit() && !ch.is_ascii_uppercase()),
            "params 必须是小写十六进制"
        );
        let plain = String::from_utf8(aes_ecb_decrypt(&hex_decode(&params).unwrap())).unwrap();
        let parts: Vec<&str> = plain.split(EAPI_SEPARATOR).collect();
        assert_eq!(parts.len(), 3);
        assert_eq!(parts[0], path);
        assert_eq!(parts[1], json);
        assert_eq!(parts[2], digest(path, json));
    }

    /// 与 OpenSSL 独立算出来的向量对齐：证明 AES-128-ECB + PKCS#7 + MD5 +
    /// 小写 hex 这一整套和服务端认的完全一致，而不是「自己加自己解都对」。
    #[test]
    fn eapi_output_matches_an_independent_openssl_vector() {
        let params = encrypt_params(
            "/api/song/enhance/player/url/v1",
            r#"{"ids":[1],"level":"exhigh"}"#,
        );
        assert_eq!(
            params,
            "fa90b329e9614f79e79598f37dc2edb487f00d1bc4c9b24cd57e6c318b9073569338432cd7d98d1a3626e997a2c5312171f9fa98ff5799fa3c4c6026223e0fa27c8e643e149384e4c350b4297c7189ef99d82df6f21020b859f72380357dfa34422775dd462499320db0bec309e203b493580b8c9132e49722a0fc34cd2fe64c"
        );
    }

    #[test]
    fn api_path_drops_the_eapi_prefix() {
        assert_eq!(
            api_path_of("/eapi/song/enhance/player/url/v1"),
            "/api/song/enhance/player/url/v1"
        );
        assert_eq!(api_path_of("/api/search"), "/api/search");
    }

    #[test]
    fn every_quality_has_a_working_alias() {
        assert_eq!(parse_quality("exhigh"), Some("exhigh"));
        assert_eq!(parse_quality("SQ"), Some("lossless"));
        assert_eq!(parse_quality("set"), Some("lossless"));
        assert_eq!(parse_quality("无损"), Some("lossless"));
        assert_eq!(parse_quality("jymaster"), Some("jymaster"));
        assert_eq!(parse_quality("nope"), None);
        // 默认值必须真的可用
        assert_eq!(parse_quality(DEFAULT_QUALITY), Some(DEFAULT_QUALITY));
    }

    #[test]
    fn song_ids_are_recognised_in_every_shape() {
        assert_eq!(song_id_of("186016"), Some(186016));
        assert_eq!(
            song_id_of("https://music.163.com/song?id=186016"),
            Some(186016)
        );
        assert_eq!(
            song_id_of("https://y.music.163.com/m/song/186016/?uct=1"),
            Some(186016)
        );
        assert_eq!(
            song_id_of("https://music.163.com/#/song?id=186016"),
            Some(186016)
        );
        assert_eq!(song_id_of("周杰伦 晴天"), None);
        assert_eq!(song_id_of("https://music.163.com/album?id=1"), None);
    }

    #[test]
    fn only_netease_hosts_are_picked_up() {
        assert!(first_link("听 https://music.163.com/song?id=1 吧").is_some());
        assert!(first_link("https://163cn.tv/abc").is_some());
        assert!(first_link("https://music.163.com.evil.test/song?id=1").is_none());
        assert!(first_link("https://example.com/music.163.com").is_none());
        assert!(first_link("没有链接").is_none());
    }

    #[test]
    fn lookalike_domains_are_rejected() {
        assert!(!is_netease_host(
            "https://music.163.com.evil.test/song?id=1"
        ));
        assert!(!is_netease_host("https://notmusic.163.com/song?id=1"));
        assert!(is_netease_host("https://y.music.163.com/m/song?id=1"));
        assert!(is_netease_host("https://163cn.tv/x"));
    }

    #[test]
    fn trailing_punctuation_is_stripped_from_links() {
        assert_eq!(
            first_link("看看这个：https://music.163.com/song?id=1。").as_deref(),
            Some("https://music.163.com/song?id=1")
        );
    }

    #[test]
    fn play_entry_tolerates_null_fields_when_the_song_is_unavailable() {
        // 匿名请求 VIP 曲目时网易云真的会这样回：url/level/type/size/br 全 null。
        let response: PlayResponse = serde_json::from_str(
            r#"{"data":[{"url":null,"level":null,"type":null,"size":null,"br":null,"code":404}]}"#,
        )
        .expect("null 字段不能把解析打挂");
        let entry = response.data.into_iter().next().unwrap();
        assert!(entry.url.is_none());
        assert_eq!(entry.code, 404);
        assert!(entry.level.is_empty());
        assert!(entry.format.is_empty());
    }

    #[test]
    fn play_entry_marks_trial_streams() {
        let value: PlayResponse = serde_json::from_str(
            r#"{"data":[{"url":"http://x/1.mp3","level":"standard","type":"mp3","size":10,"br":128000,"code":200,"freeTrialInfo":{"start":0,"end":30}}]}"#,
        )
        .unwrap();
        let entry = value.data.into_iter().next().unwrap();
        assert!(entry.free_trial_info.is_some());
        assert_eq!(entry.format, "mp3");
    }

    #[test]
    fn covers_are_requested_as_small_thumbnails() {
        assert_eq!(
            thumbnail_cover("http://p1.music.126.net/a.jpg"),
            "http://p1.music.126.net/a.jpg?param=320y320"
        );
        assert_eq!(
            thumbnail_cover("http://p1.music.126.net/a.jpg?x=1"),
            "http://p1.music.126.net/a.jpg?x=1&param=320y320"
        );
        assert_eq!(
            thumbnail_cover("http://p1.music.126.net/a.jpg?param=300y300"),
            "http://p1.music.126.net/a.jpg?param=300y300",
            "已经有 param 的不重复加"
        );
        assert_eq!(thumbnail_cover(""), "");
    }

    #[test]
    fn search_response_is_read_from_the_official_shape() {
        let value: SearchResponse = serde_json::from_str(
            r#"{"code":200,"data":{"resources":[{"baseInfo":{"simpleSongData":{
                "id":186016,"name":"晴天","dt":269000,
                "ar":[{"name":"周杰伦"}],"al":{"name":"叶惠美","picUrl":"http://p/1.jpg"}
            }}}]}}"#,
        )
        .unwrap();
        let track = value
            .data
            .resources
            .into_iter()
            .next()
            .unwrap()
            .base_info
            .simple_song_data
            .into_track();
        assert_eq!(track.id, 186016);
        assert_eq!(track.artist_text(), "周杰伦");
        assert_eq!(track.album, "叶惠美");
        assert_eq!(track.duration_secs(), 269);
    }
}
