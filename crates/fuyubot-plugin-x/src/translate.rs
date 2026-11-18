//! 谷歌机翻：非中文帖子的卡片先翻成中文再画。
//!
//! 用的是 translate.googleapis.com 的免费接口，不需要 key；网络不通或触发谷歌
//! 的人机校验时返回 Err，卡片退回「显示翻译」入口，不阻塞发送。

use serde_json::Value;

use crate::http::Agent;
use crate::model::Post;

const ENDPOINT: &str = "https://translate.googleapis.com/translate_a/single";

/// 这条帖子要不要翻译：中文不翻，只有链接的帖子也不翻。
pub fn needs(post: &Post) -> bool {
    let lang = post.lang.to_ascii_lowercase();
    if lang.starts_with("zh") || post.article.is_some() {
        return false;
    }
    let text = post.translation_text().trim();
    if text.is_empty() {
        return false;
    }
    if text.starts_with("http") && !text.contains(' ') {
        return false;
    }
    if lang.is_empty() {
        return text.chars().any(|ch| ch.is_ascii_alphabetic());
    }
    true
}

/// 调谷歌翻译，返回中文译文。
pub async fn to_chinese(agent: &Agent, text: &str) -> Result<String, String> {
    let text = text.trim();
    if text.is_empty() {
        return Err("没有可翻译的正文".to_owned());
    }
    let url = format!(
        "{ENDPOINT}?client=gtx&sl=auto&tl=zh-CN&dt=t&q={}",
        encode(text)
    );
    let raw = agent.get(&url).await.map_err(|error| error.0)?;
    if !raw.is_success() {
        return Err(format!("谷歌翻译返回 HTTP {}", raw.status));
    }
    parse(&raw.body)
}

/// 谷歌的响应是 `[[["译文","原文",…], …], null, "en"]`，把每段的第一个字段拼起来。
fn parse(body: &str) -> Result<String, String> {
    let value: Value =
        serde_json::from_str(body).map_err(|error| format!("谷歌翻译返回的不是 JSON：{error}"))?;
    let segments = value
        .get(0)
        .and_then(Value::as_array)
        .ok_or_else(|| "谷歌翻译返回格式不认识".to_owned())?;
    let mut out = String::new();
    for segment in segments {
        if let Some(text) = segment.get(0).and_then(Value::as_str) {
            out.push_str(text);
        }
    }
    if out.trim().is_empty() {
        Err("谷歌翻译返回空结果".to_owned())
    } else {
        Ok(out)
    }
}

/// 只编码 query 的值，保留 RFC 3986 的 unreserved 字符。
fn encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len() * 3);
    for byte in text.as_bytes() {
        match *byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*byte as char);
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

#[cfg(test)]
#[path = "../tests/unit/translate.rs"]
mod tests;
