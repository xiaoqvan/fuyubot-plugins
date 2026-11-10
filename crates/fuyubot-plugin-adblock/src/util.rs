//! 与业务无关的小工具：时间戳、ID、base64、回调参数解析。
//!
//! 时间展示用 `chrono` 的本地时区（TS 版日志口径）；其余仍是标准库 +
//! 本仓库已有的依赖，不额外拖运行时。

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use chrono::{Local, TimeZone};

/// 当前 Unix 毫秒时间戳。
pub fn now_ms() -> i64 {
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(duration) => duration.as_millis() as i64,
        Err(_) => 0,
    }
}

/// 生成一个 24 位十六进制 ID。
///
/// TS 版用 MongoDB 的 `ObjectId`，插件里到处都是 `^[a-fA-F0-9]{24}$` 的校验
/// （`/qadid`、`/delad`、`/editkw`）。这里保持同样的**形状**，只是把来源换成
/// 「毫秒时间戳 + 进程内自增」：可排序、不重复、能过所有既有校验。
pub fn new_id() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let millis = now_ms().max(0) as u64;
    let counter = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!(
        "{:012x}{:012x}",
        millis & 0x0000_FFFF_FFFF_FFFF,
        counter & 0x0000_FFFF_FFFF_FFFF
    )
}

/// 是不是 24 位十六进制（与 TS 版 `ObjectId.isValid` 用的正则同形）。
pub fn is_object_id(text: &str) -> bool {
    text.len() == 24 && text.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// Unix 毫秒 → 本地时区的 ISO 时间（`YYYY-MM-DDTHH:MM:SS+08:00`）。
///
/// TS 版 `/qban` 用的是 `toISOString()`（UTC）；用户要求所有时间跟 TS 的
/// **本地时间口径**一致，不要 UTC，所以这里显式带本地 offset。
pub fn fmt_local(ms: i64) -> String {
    Local
        .timestamp_millis_opt(ms)
        .single()
        .map(|time| time.format("%Y-%m-%dT%H:%M:%S%:z").to_string())
        .unwrap_or_else(|| "-".to_owned())
}

/// Unix 毫秒 → 本地时区的 `YYYY-MM-DD HH:MM:SS`（GKY 转发同款）。
pub fn fmt_local_plain(ms: i64) -> String {
    Local
        .timestamp_millis_opt(ms)
        .single()
        .map(|time| time.format("%Y-%m-%d %H:%M:%S").to_string())
        .unwrap_or_else(|| "-".to_owned())
}

/// 本地日期 `YYYY-MM-DD`，用于 AI token 的「按天重置」。
pub fn local_day() -> String {
    Local::now().format("%Y-%m-%d").to_string()
}

/// base64 解码（TDLib 的回调数据是 base64 字节串）。
///
/// 宽容：忽略空白、接受无 padding 的输入；遇到非法字符返回 `None`。
pub fn base64_decode(text: &str) -> Option<Vec<u8>> {
    const TABLE: [i8; 256] = build_decode_table();
    let mut out = Vec::with_capacity(text.len() * 3 / 4);
    let mut buffer: u32 = 0;
    let mut bits = 0_u32;
    for byte in text.bytes() {
        if byte == b'\n' || byte == b'\r' || byte == b' ' || byte == b'\t' {
            continue;
        }
        if byte == b'=' {
            break;
        }
        let value = TABLE[byte as usize];
        if value < 0 {
            return None;
        }
        buffer = (buffer << 6) | value as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buffer >> bits) as u8);
            buffer &= (1 << bits) - 1;
        }
    }
    Some(out)
}

/// base64 编码（回调数据发出去之前用；TDLib 的 `bytes` 字段要 base64）。
pub fn base64_encode(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
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

const fn build_decode_table() -> [i8; 256] {
    let mut table = [-1_i8; 256];
    let mut index = 0;
    while index < 64 {
        let byte = match index {
            0..=25 => b'A' + index as u8,
            26..=51 => b'a' + (index - 26) as u8,
            52..=61 => b'0' + (index - 52) as u8,
            62 => b'+',
            _ => b'/',
        };
        table[byte as usize] = index as i8;
        index += 1;
    }
    table
}

/// 回调数据：`unban?cid=-100123&u=456` → `("unban", {"cid": "-100123", "u": "456"})`。
///
/// TS 版用 `URLSearchParams`；这里只需要「数字参数」，所以不做百分号解码，
/// 只按 `&` / `=` 切（插件自己能生成的值里没有需要转义的字符）。
pub fn parse_callback(raw: &str) -> (String, Vec<(String, String)>) {
    let (command, query) = match raw.split_once('?') {
        Some((command, query)) => (command, query),
        None => (raw, ""),
    };
    let mut params = Vec::new();
    for pair in query.split('&') {
        if pair.is_empty() {
            continue;
        }
        match pair.split_once('=') {
            Some((key, value)) => params.push((key.to_owned(), value.to_owned())),
            None => params.push((pair.to_owned(), String::new())),
        }
    }
    (command.to_owned(), params)
}

/// 从回调参数里取一个 `i64`；缺省 / 非法都是 `None`。
pub fn param_i64(params: &[(String, String)], key: &str) -> Option<i64> {
    params
        .iter()
        .find(|(name, _)| name == key)
        .and_then(|(_, value)| value.parse::<i64>().ok())
}

/// 从回调参数里取一个字符串。
pub fn param_str<'a>(params: &'a [(String, String)], key: &str) -> Option<&'a str> {
    params
        .iter()
        .find(|(name, _)| name == key)
        .map(|(_, value)| value.as_str())
}

/// 截断到 `limit` 个字符（按 `char` 边界，不会切坏中文）。
pub fn truncate(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.to_owned();
    }
    let mut out: String = text.chars().take(limit).collect();
    out.push('…');
    out
}

/// 把一段用户输入里会破坏 MarkdownV2 的内联代码分隔符换掉（`/adkew list` 用）。
pub fn escape_inline_code(text: &str) -> String {
    text.replace('`', "'")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn object_id_shape() {
        let id = new_id();
        assert_eq!(id.len(), 24);
        assert!(is_object_id(&id));
        assert!(!is_object_id("xyz"));
    }

    #[test]
    fn base64_round_trip() {
        for text in ["", "a", "ab", "abc", "unban?cid=-100&u=1", "中文"] {
            let encoded = base64_encode(text.as_bytes());
            let decoded = base64_decode(&encoded).expect("合法 base64");
            assert_eq!(decoded, text.as_bytes(), "{text}");
        }
    }

    #[test]
    fn base64_without_padding() {
        assert_eq!(base64_decode("YQ").unwrap(), b"a");
    }

    #[test]
    fn callback_is_split() {
        let (command, params) = parse_callback("unban?cid=-100123&u=456");
        assert_eq!(command, "unban");
        assert_eq!(param_i64(&params, "cid"), Some(-100123));
        assert_eq!(param_i64(&params, "u"), Some(456));
        assert_eq!(param_i64(&params, "missing"), None);
    }

    #[test]
    fn local_time_is_not_utc() {
        let text = fmt_local(0);
        assert!(text.starts_with("19") || text.starts_with("20"), "{text}");
        assert!(text.contains('T'), "{text}");
        assert!(!text.ends_with('Z'), "本地时间不应带 Z：{text}");

        let plain = fmt_local_plain(0);
        assert_eq!(plain.len(), 19, "{plain}");
        assert!(!plain.contains('T'), "{plain}");
        assert_eq!(local_day().len(), 10);
    }
}
