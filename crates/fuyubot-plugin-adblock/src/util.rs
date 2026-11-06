//! 与业务无关的小工具：时间戳、ID、base64、回调参数解析。
//!
//! 刻意只依赖标准库——插件是 cdylib，能少一个依赖就少一份「宿主那边编不了」的风险。

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

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

/// Unix 毫秒 → `YYYY-MM-DDTHH:MM:SSZ`（UTC）。
///
/// TS 版 `qban` 用 `toISOString()`，也就是这个形状。**刻意不做本地时区**：
/// 插件是动态库，拿本地时区要么拖 `chrono`，要么走平台 FFI，两条都不划算。
pub fn fmt_utc(ms: i64) -> String {
    let seconds = ms.div_euclid(1000);
    let days = seconds.div_euclid(86_400);
    let rest = seconds.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    let hour = rest / 3600;
    let minute = (rest % 3600) / 60;
    let second = rest % 60;
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

/// Unix 毫秒 → `YYYY-MM-DD HH:MM:SS`（UTC，给人看）。
pub fn fmt_utc_plain(ms: i64) -> String {
    fmt_utc(ms)
        .replace('T', " ")
        .trim_end_matches('Z')
        .to_owned()
}

/// Howard Hinnant 的 `civil_from_days`：把「1970-01-01 起的天数」还原成日期。
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    (if m <= 2 { y + 1 } else { y }, m, d)
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
    fn utc_format_is_iso() {
        assert_eq!(fmt_utc(0), "1970-01-01T00:00:00Z");
        assert_eq!(fmt_utc(1_600_000_000_000), "2020-09-13T12:26:40Z");
    }
}
