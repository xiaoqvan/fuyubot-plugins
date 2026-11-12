//! 汽水 UGC 音乐视频（`qishui.douyin.com/s/…` 落到 `share/ugc_video` 的那类分享）。
//!
//! 单曲分享页是 `share/track?track_id=…`，UGC 视频是
//! `share/ugc_video?ugc_video_id=…`。后者没有 `track_v2` 的曲目结构，
//! 播放信息直接写在分享页 SSR 的 `_ROUTER_DATA` 里：
//!
//! ```text
//! loaderData.ugc_video_page.videoOptions = {
//!   url, duration, width, height, videoName, artistName,
//!   coverURL, metaURL, firstFrameURL, status_code, previewStart/End
//! }
//! ```
//!
//! 这里只做「取一条可直接下载的 mp4 + 标题/作者/封面/时长」。
//! 文案格式交给 `send`，与抖音插件一致。

use serde_json::Value;

use crate::auth::SodaAuth;
use crate::http::{self, Agent};
use crate::log;
use crate::soda::percent_encode;

/// 分享页宿主（短链最终落点）。
const SHARE_HOST: &str = "https://music.douyin.com";
/// 分享页 UA。
const UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/134.0.0.0 Safari/537.36";

/// 一条 UGC 音乐视频。
#[derive(Debug, Clone, Default)]
pub struct VideoPost {
    pub id: String,
    pub title: String,
    pub author: String,
    pub cover: String,
    pub url: String,
    /// 秒。
    pub duration: i64,
    pub width: i64,
    pub height: i64,
    /// 只是试听片段（`previewEnd` 明显短于总时长）。
    pub preview: bool,
}

/// 从文本里认出 UGC 视频 ID：`ugc_video_id=…`、`/ugc_video/…`、页面里的 `"video_id":"…"`。
pub fn video_id_of(text: &str) -> Option<String> {
    let text = text.trim();
    let decoded = crate::soda::percent_decode(text);
    for candidate in [text, decoded.as_str()] {
        for key in ["ugc_video_id", "video_id"] {
            for pattern in [format!("{key}="), format!("\"{key}\":\"")] {
                if let Some(rest) = candidate.split(pattern.as_str()).nth(1) {
                    let digits: String =
                        rest.chars().take_while(|ch| ch.is_ascii_digit()).collect();
                    if digits.len() > 10 {
                        return Some(digits);
                    }
                }
            }
        }
        let lower = candidate.to_ascii_lowercase();
        if let Some(index) = lower.find("/ugc_video/") {
            let rest = &candidate[index + "/ugc_video/".len()..];
            let digits: String = rest.chars().take_while(|ch| ch.is_ascii_digit()).collect();
            if digits.len() > 10 {
                return Some(digits);
            }
        }
    }
    None
}

/// 分享页地址：`ugc_video_id` + 官方 H5 需要的那几个查询参数。
fn share_url(id: &str) -> String {
    format!(
        "{SHARE_HOST}/qishui/share/ugc_video?ugc_video_id={}&hybrid_sdk_version=bullet&auto_play_bgm=1",
        percent_encode(id)
    )
}

/// 拉分享页并解析出可下载的视频信息。
pub async fn resolve(agent: &Agent, auth: &SodaAuth, id: &str) -> Result<VideoPost, String> {
    let url = share_url(id);
    let cookie = auth.cookie.trim();
    let mut headers = vec![
        ("User-Agent", UA),
        ("Accept", "text/html,application/xhtml+xml"),
    ];
    if !cookie.is_empty() {
        headers.push(("Cookie", cookie));
    }
    let reply = http::get(agent, &url, &headers).await?;
    if !(200..300).contains(&reply.status) {
        return Err(format!(
            "汽水视频分享页 HTTP {}：{}",
            reply.status, reply.final_url
        ));
    }
    let html = reply.text();
    let data = router_data(&html)
        .ok_or_else(|| "汽水视频分享页里没有 _ROUTER_DATA（可能已下架）".to_owned())?;
    let options = data
        .pointer("/loaderData/ugc_video_page/videoOptions")
        .ok_or_else(|| "汽水视频分享页里没有 videoOptions".to_owned())?;

    let status = options
        .get("status_code")
        .and_then(Value::as_i64)
        .unwrap_or(0);
    if status != 0 {
        return Err(format!("汽水视频分享页 status_code={status}"));
    }

    let video_url = string_of(options, "url");
    if video_url.trim().is_empty() {
        return Err("汽水视频没有给出播放地址（可能已下架 / 地区受限）".to_owned());
    }
    let duration = options
        .get("duration")
        .and_then(Value::as_f64)
        .unwrap_or(0.0);
    let preview_end = options
        .get("previewEnd")
        .and_then(Value::as_f64)
        .unwrap_or(duration);
    let post = VideoPost {
        id: id.to_owned(),
        title: string_of(options, "videoName"),
        author: string_of(options, "artistName"),
        cover: first_non_empty(&[
            string_of(options, "coverURL"),
            string_of(options, "metaURL"),
            string_of(options, "firstFrameURL"),
        ]),
        url: video_url,
        duration: duration.round() as i64,
        width: int_of(options, "width"),
        height: int_of(options, "height"),
        preview: duration > 0.0 && preview_end + 1.0 < duration,
    };
    log::debug(&format!(
        "汽水视频 {}《{}》 {}s {}x{} preview={}",
        post.id, post.title, post.duration, post.width, post.height, post.preview
    ));
    Ok(post)
}

fn string_of(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_owned()
}

fn int_of(value: &Value, key: &str) -> i64 {
    value.get(key).and_then(Value::as_i64).unwrap_or(0)
}

fn first_non_empty(values: &[String]) -> String {
    values
        .iter()
        .find(|value| !value.trim().is_empty())
        .cloned()
        .unwrap_or_default()
}

/// 从分享页 HTML 里取出 `_ROUTER_DATA = {…}`（按括号配对，别用正则截断）。
pub fn router_data(html: &str) -> Option<Value> {
    let marker = html.find("_ROUTER_DATA")?;
    let rest = &html[marker..];
    let start = rest.find('{')?;
    let bytes = rest.as_bytes();
    let mut depth = 0i32;
    let mut in_string = false;
    let mut escaped = false;
    for index in start..bytes.len() {
        let byte = bytes[index];
        if in_string {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                in_string = false;
            }
            continue;
        }
        match byte {
            b'"' => in_string = true,
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return serde_json::from_str(&rest[start..=index]).ok();
                }
            }
            _ => {}
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn video_ids_are_recognised_in_every_shape() {
        assert_eq!(
            video_id_of("https://music.douyin.com/qishui/share/ugc_video?ugc_video_id=7625615883351772468&x=1")
                .as_deref(),
            Some("7625615883351772468")
        );
        assert_eq!(
            video_id_of(r#"{"video_id":"7625615883351772468"}"#).as_deref(),
            Some("7625615883351772468")
        );
        assert_eq!(
            video_id_of("%2Fugc_video%2F7625615883351772468").as_deref(),
            Some("7625615883351772468")
        );
        assert_eq!(video_id_of("https://www.qishui.com/track/123"), None);
        assert_eq!(video_id_of("没有 ID"), None);
    }

    #[test]
    fn router_data_is_extracted_with_brace_matching() {
        let html = r#"<html><script>window._ROUTER_DATA = {"loaderData":{"ugc_video_page":{"videoOptions":{"videoName":"a}b","duration":3}}}};</script></html>"#;
        let value = router_data(html).expect("应该能解出来");
        assert_eq!(
            value.pointer("/loaderData/ugc_video_page/videoOptions/videoName"),
            Some(&Value::String("a}b".to_owned())),
            "字符串里的右花括号不能提前结束"
        );
        assert!(router_data("<html>没有数据</html>").is_none());
    }

    #[test]
    fn video_options_are_mapped_to_a_post() {
        let value = serde_json::json!({
            "loaderData": {"ugc_video_page": {"videoOptions": {
                "url": "https://cdn/v.mp4",
                "duration": 33.855,
                "width": 1920,
                "height": 1080,
                "videoName": "塔不灭 #永雏塔菲",
                "artistName": "永川塔菲",
                "coverURL": "https://cdn/c.jpg",
                "status_code": 0,
                "previewStart": 0,
                "previewEnd": 33.855
            }}}
        });
        let options = value
            .pointer("/loaderData/ugc_video_page/videoOptions")
            .unwrap();
        assert_eq!(
            options.get("duration").and_then(Value::as_f64),
            Some(33.855)
        );
        assert_eq!(int_of(options, "width"), 1920);
        assert_eq!(string_of(options, "artistName"), "永川塔菲");
    }

    #[test]
    fn share_url_carries_the_encoded_id() {
        let url = share_url("7625615883351772468");
        assert!(url.contains("ugc_video_id=7625615883351772468"), "{url}");
        assert!(
            url.starts_with("https://music.douyin.com/qishui/share/ugc_video"),
            "{url}"
        );
    }
}
