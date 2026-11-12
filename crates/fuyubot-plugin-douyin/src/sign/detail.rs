//! detail API 的已签名请求构造：`/aweme/v1/web/aweme/detail/`（签名保护区）。
//!
//! 平台的参数顺序（2026-09 从活页面逐字节捕获）：
//!
//! ```text
//! base_params → aweme_id → msToken → a_bogus → verifyFp → fp
//!             → uifid → timestamp → x-secsdk-web-signature
//! ```
//!
//! 两条签名叠在参数之上：**a_bogus**（`urlencode` 后的 query 的摘要）与
//! **web 签名**（覆盖其前全部内容的 MD5）。实测（2026-11，本机网络）只带
//! web 签名即可过门——a_bogus 当前不被强制校验，但站点自己的页面总带它，
//! 所以构造时照样生成，像它才是目的。
//!
//! 身份（`uifid` / `s_v_web_id` / `ttwid`）**来自用户浏览器的 cookie**：
//! 抖音的 `UIFID` 由页面脚本下发，纯 HTTP 铸不出来。jar 里没有 `uifid` 时
//! 本层直接放弃 API 路线（调用方回退 SSR 分享页）。

use std::collections::BTreeMap;

use super::abogus::{ABogus, DEFAULT_BROWSER_INFO};
use super::encode::{quote_strict, urlencode};
use super::websign;

/// 抖音网页端自称的客户端版本。**落后于线上大版本会被拒**，届时改这三行
/// （从 `params.py` 的 AID/VERSION_CODE 常量同步，不是随手编的）。
pub const AID: &str = "6383";
pub const CHANNEL: &str = "channel_pc_web";
pub const VERSION_CODE: &str = "290100";
pub const VERSION_NAME: &str = "29.1.0";
pub const UPDATE_VERSION_CODE: &str = "170400";

/// 与 [`base_params`] 的指纹自洽的桌面 UA。API 路线用它（SSR 路线仍是 iPhone UA）：
/// query 里回传的浏览器指纹必须与 User-Agent 自报的一致，否则本身就是信号。
pub const DESKTOP_UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 \
(KHTML, like Gecko) Chrome/130.0.0.0 Safari/537.36";

/// 生成伪 msToken 的字母表（V4 `gen_random_str` 的原样抄本，含尾部 `+-`）。
const MS_TOKEN_ALPHABET: &[u8] =
    b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+-";
/// 伪造 msToken 的主体长度（另加 `==` 后缀）。抖音对 detail 接口不校验它
/// （实测：伪造 / 缺失 / 与 Cookie 矛盾都返回完整数据），但不能形状怪异。
const MS_TOKEN_BODY_LEN: usize = 126;

/// 生产用随机源：xorshift64*，种子取系统时间纳秒 + 栈地址做熵。
///
/// 签名里用随机的地方（伪 msToken / a_bogus 噪声）只要「看起来随机」，
/// 不做密码学用途；免得为它引一个 `rand` 依赖进动态库。
pub fn entropy() -> impl FnMut() -> f64 {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos() as u64)
        .unwrap_or(0x9E37_79B9_7F4A_7C15);
    let mut state = nanos ^ (&nanos as *const u64 as u64).rotate_left(17);
    if state == 0 {
        state = 0x9E37_79B9_7F4A_7C15;
    }
    move || {
        state ^= state >> 12;
        state ^= state << 25;
        state ^= state >> 27;
        ((state.wrapping_mul(0x2545_F491_4F6C_DD1D)) >> 11) as f64 / (1u64 << 53) as f64
    }
}

/// 浏览器 cookie 串 → 键值表（键保留原样，值去掉一层空白）。
pub fn parse_cookie_jar(cookie: &str) -> BTreeMap<String, String> {
    cookie
        .split(';')
        .filter_map(|part| {
            let (key, value) = part.split_once('=')?;
            let key = key.trim();
            if key.is_empty() {
                return None;
            }
            Some((key.to_owned(), value.trim().to_owned()))
        })
        .collect()
}

/// 每次调用都换一个伪 msToken（形状对了就行，别复用）。
fn false_ms_token(random: &mut dyn FnMut() -> f64) -> String {
    let mut token = String::with_capacity(MS_TOKEN_BODY_LEN + 2);
    for _ in 0..MS_TOKEN_BODY_LEN {
        let index = (random() * MS_TOKEN_ALPHABET.len() as f64) as usize % MS_TOKEN_ALPHABET.len();
        token.push(MS_TOKEN_ALPHABET[index] as char);
    }
    token.push_str("==");
    token
}

/// 业务参数（`base_params` + `aweme_id` + `msToken`），顺序即发送顺序。
fn detail_params(aweme_id: &str, ms_token: &str) -> Vec<(String, String)> {
    let pairs: [(&str, &str); 27] = [
        ("device_platform", "webapp"),
        ("aid", AID),
        ("channel", CHANNEL),
        ("pc_client_type", "1"),
        ("version_code", VERSION_CODE),
        ("version_name", VERSION_NAME),
        ("cookie_enabled", "true"),
        ("screen_width", "1920"),
        ("screen_height", "1080"),
        ("browser_language", "zh-CN"),
        ("browser_platform", "Win32"),
        ("browser_name", "Chrome"),
        ("browser_version", "130.0.0.0"),
        ("browser_online", "true"),
        ("engine_name", "Blink"),
        ("engine_version", "130.0.0.0"),
        ("os_name", "Windows"),
        ("os_version", "10"),
        ("cpu_core_num", "12"),
        ("device_memory", "8"),
        ("platform", "PC"),
        ("downlink", "10"),
        ("effective_type", "4g"),
        ("round_trip_time", "0"),
        ("update_version_code", UPDATE_VERSION_CODE),
        ("aweme_id", aweme_id),
        ("msToken", ms_token),
    ];
    pairs
        .iter()
        .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
        .collect()
}

/// 一份待发的 detail 请求。
pub struct DetailRequest {
    /// 完整 URL（含签名后的 query）。
    pub url: String,
    /// 除 Cookie / User-Agent 外的额外请求头（web 签名三件套）。
    pub headers: Vec<(String, String)>,
}

/// 构造 detail 请求。`cookie` 是浏览器整串 Cookie。
///
/// jar 里没有 `uifid` 时返回 `None`——调用方回退 SSR 路线。
pub fn build_detail_request(
    aweme_id: &str,
    cookie: &str,
    timestamp: i64,
    now_ms: u64,
    random: &mut dyn FnMut() -> f64,
) -> Option<DetailRequest> {
    let jar = parse_cookie_jar(cookie);
    let uifid = websign::pick_uifid(&jar)?;
    let verify_fp = jar.get(websign::VERIFY_FP_COOKIE).cloned();

    let ms_token = jar
        .get("msToken")
        .filter(|value| !value.is_empty())
        .cloned()
        .unwrap_or_else(|| false_ms_token(random));

    // ① 业务参数 → urlencode（a_bogus 的被哈希面）
    let query = urlencode(&detail_params(aweme_id, &ms_token));
    // ② a_bogus：站点自己的页面总带，我们也带
    let bogus =
        ABogus::new(DESKTOP_UA, DEFAULT_BROWSER_INFO, random).get_value(&query, "", "", now_ms);
    let query = format!("{query}&a_bogus={}", quote_strict(&bogus));
    // ③ web 签名：verifyFp/fp → uifid → timestamp → x-secsdk-web-signature
    let (signed_query, headers) =
        websign::add_web_signature(&query, Some(&uifid), verify_fp.as_deref(), timestamp);

    Some(DetailRequest {
        url: format!("https://www.douyin.com/aweme/v1/web/aweme/detail/?{signed_query}"),
        headers,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jar_parsing_keeps_values_verbatim() {
        let jar = parse_cookie_jar(" a=1 ; b=x=y ;UIFID=U;empty=");
        assert_eq!(jar.get("a").map(String::as_str), Some("1"));
        assert_eq!(jar.get("b").map(String::as_str), Some("x=y"));
        assert_eq!(jar.get("UIFID").map(String::as_str), Some("U"));
    }

    #[test]
    fn no_uifid_means_no_request() {
        assert!(build_detail_request("1", "ttwid=x", 0, 0, &mut || 0.5).is_none());
    }

    #[test]
    fn request_shape_is_the_platforms() {
        let request = build_detail_request(
            "42",
            "UIFID=U1;s_v_web_id=V1;ttwid=T1",
            1762600000,
            1762600000000,
            &mut || 0.5,
        )
        .expect("有 uifid 就有请求");
        // 平台顺序：…&aweme_id=&msToken=&a_bogus=&verifyFp=&fp=&uifid=&timestamp=&x-secsdk-web-signature=
        let order = [
            "aweme_id=42",
            "&msToken=",
            "&a_bogus=",
            "&verifyFp=V1",
            "&fp=V1",
            "&uifid=U1",
            "&timestamp=1762600000",
            "&x-secsdk-web-signature=",
        ];
        let mut cursor = 0;
        for needle in order {
            let found = request.url[cursor..]
                .find(needle)
                .unwrap_or_else(|| panic!("顺序里缺 {needle}"));
            cursor += found + needle.len();
        }
        let names: Vec<&str> = request
            .headers
            .iter()
            .map(|(name, _)| name.as_str())
            .collect();
        assert_eq!(
            names,
            ["uifid", "x-secsdk-web-signature", "x-secsdk-web-expire"]
        );
    }
}
