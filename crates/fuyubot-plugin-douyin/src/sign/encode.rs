//! 百分号编码：与 Python `urllib.parse` 的三种语义逐字节对齐。
//!
//! 签名对字节敏感——**被哈希的串就是被发送的串**，所以这里不追求「标准」，
//! 只追求与对拍基准（参考实现的 Python）一致：
//!
//! | 函数 | Python 原型 | 保留字符 | 空格 |
//! | --- | --- | --- | --- |
//! | [`quote_keep_star`] | `quote(s, safe='*-._')` | 字母数字 + `_.-~` + `*` | `%20` |
//! | [`quote_strict`] | `quote(s, safe='')` | 字母数字 + `_.-~` | `%20` |
//! | [`quote_plus`] | `quote_plus(s)` | 字母数字 + `_.-~` | `+` |
//!
//! 其余字节（UTF-8 逐字节）转成**大写** `%XX`。`~` 与 JS `URLSearchParams`
//! 的行为不同（JS 会转义它）——以 Python 参考实现实测通过为准。

/// Python `_ALWAYS_SAFE` + `safe`：字母数字与 `_.-~` 永不转义。
fn keep_byte(byte: u8, star: bool) -> bool {
    byte.is_ascii_alphanumeric()
        || matches!(byte, b'_' | b'.' | b'-' | b'~')
        || (star && byte == b'*')
}

fn quote_with(text: &str, star: bool, space_plus: bool) -> String {
    let mut out = String::with_capacity(text.len());
    for &byte in text.as_bytes() {
        if keep_byte(byte, star) {
            out.push(byte as char);
        } else if byte == b' ' && space_plus {
            out.push('+');
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// `quote(s, safe='*-._')`——[`super::websign`] 的 `URLSearchParams` 序列化用它。
pub fn quote_keep_star(text: &str) -> String {
    quote_with(text, true, false)
}

/// `quote(s, safe='')`——a_bogus 值回填 query 时用它（s4 字母表含 `/` 与 `=`）。
pub fn quote_strict(text: &str) -> String {
    quote_with(text, false, false)
}

/// `quote_plus(s)`——Python `urlencode()` 的构件。
pub fn quote_plus(text: &str) -> String {
    quote_with(text, false, true)
}

/// `urlencode(pairs)`：`k=quote_plus(k)&v=quote_plus(v)` 按序相连。
pub fn urlencode(pairs: &[(String, String)]) -> String {
    pairs
        .iter()
        .map(|(key, value)| format!("{}={}", quote_plus(key), quote_plus(value)))
        .collect::<Vec<_>>()
        .join("&")
}

/// `urllib.parse.unquote`：`%XX` 解码为字节（无效 UTF-8 有损替换），`+` 保持原样。
pub fn unquote(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 3 <= bytes.len() {
            let high = bytes[index + 1];
            let low = bytes[index + 2];
            if high.is_ascii_hexdigit() && low.is_ascii_hexdigit() {
                out.push((hex_value(high) << 4) | hex_value(low));
                index += 3;
                continue;
            }
        }
        out.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_value(byte: u8) -> u8 {
    match byte {
        b'0'..=b'9' => byte - b'0',
        b'a'..=b'f' => byte - b'a' + 10,
        _ => byte - b'A' + 10,
    }
}

/// `part.partition("=")`：按**第一个** `=` 拆成 `(k, v)`，`v` 可再含 `=`。
pub fn split_pair(part: &str) -> (&str, &str) {
    match part.split_once('=') {
        Some((key, value)) => (key, value),
        None => (part, ""),
    }
}

#[cfg(test)]
#[path = "../../tests/unit/sign__encode.rs"]
mod tests;
