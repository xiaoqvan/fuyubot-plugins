//! URL / 文本小工具：host 判定、百分号编码、分享链接提取。
//!
//! 三个平台（网易云 / 汽水 / YouTube）都要从聊天文本里挑链接，
//! 规则一样（按空白切、从 http 开始、去掉尾随标点、按 host 判），
//! 这里只放纯函数，不认任何平台。

/// 取出 URL 的 host（小写、去掉端口 / userinfo）。
///
/// 按 host 判，不做子串 contains：evil-music.163.com.attacker.tld 骗不过它。
pub fn host_of(url: &str) -> Option<String> {
    let rest = url.split_once("://").map(|(_, rest)| rest).unwrap_or(url);
    let authority = rest.split(['/', '?', '#']).next()?;
    let host = authority.rsplit('@').next()?;
    let host = host.split(':').next()?;
    (!host.is_empty()).then(|| host.to_ascii_lowercase())
}

/// 分享链接常见的尾随标点（中英文都算）。
pub fn trim_punctuation(text: &str) -> &str {
    text.trim_end_matches([
        ')', ']', '}', '>', '，', ',', '。', '.', '！', '!', '？', '?', '：', ':', ';', '；', '"',
        '\'', '」', '』', '】', '、', '…',
    ])
}

/// 文本里第一个满足 accept 的 http(s) 链接。
///
/// 从 http 开始截取是刻意的：Telegram 的分享文案经常是
/// 「看看这个 https://… 很好听」，前面还挂着中文；
/// 链接后面也可能直接粘着「，好听」而不是空格。
pub fn first_link_where(text: &str, accept: impl Fn(&str) -> bool) -> Option<String> {
    for candidate in text.split_whitespace() {
        let Some(start) = candidate.find("http") else {
            continue;
        };
        let trimmed = trim_punctuation(cut_at_terminator(&candidate[start..]));
        if trimmed.starts_with("http") && accept(trimmed) {
            return Some(trimmed.to_owned());
        }
    }
    None
}

/// 在第一个中文标点 / 引号处截断。
///
/// 不能只 trim_end：链接后面可能是「…，好听」，中文会整段挂在 URL 尾巴上。
fn cut_at_terminator(text: &str) -> &str {
    match text.find(|ch: char| {
        matches!(
            ch,
            '，' | '。'
                | '！'
                | '？'
                | '；'
                | '：'
                | '、'
                | '）'
                | '」'
                | '』'
                | '】'
                | '》'
                | '…'
                | '"'
                | '\''
        )
    }) {
        Some(index) => &text[..index],
        None => text,
    }
}

/// 百分号编码（UTF-8）；空格编码成 %20，不编码非保留字符。
pub fn percent_encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len() * 3);
    for byte in text.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*byte as char);
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// URL 查询串里的一个参数值（键大小写不敏感；空值当没有）。
pub fn query_value(url: &str, key: &str) -> Option<String> {
    let query = url.split_once('?')?.1.split('#').next()?;
    for pair in query.split('&') {
        let Some((name, value)) = pair.split_once('=') else {
            continue;
        };
        if name.eq_ignore_ascii_case(key) && !value.is_empty() {
            return Some(value.to_owned());
        }
    }
    None
}

#[cfg(test)]
#[path = "../tests/unit/url.rs"]
mod tests;
