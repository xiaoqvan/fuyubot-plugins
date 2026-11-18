//! 解析结果的形状，以及「FxTwitter JSON → 这些结构」的提取。
//!
//! 只认 [FxTwitter / FxEmbed](https://docs.fxembed.com/api/introduction) 的 v2 形状：
//! `GET {api_base}/status/{id}` 回 `{"code":200,"tweet":{…}}`。选它是因为 X 自己的
//! 页面早就要登录了，而这个接口是公开只读、不需要 token、连 Article 的 Draft.js
//! 文档都原样给出来（`tweet.article.content`）。
//!
//! 这里只做**形状转换**，不做排版决策——块的语义归 [`crate::article`]，怎么发归
//! [`crate::send`]。

use std::collections::HashSet;

use chrono::TimeZone;
use serde_json::Value;

use crate::article::{self, Block};

/// 媒体种类：只要分得清「按图片发」还是「按视频发」就够了。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaKind {
    Photo,
    Video,
}

/// 一条可发送的媒体。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Media {
    pub kind: MediaKind,
    /// 直链：图片 `pbs.twimg.com`，视频 `video.twimg.com`。
    pub url: String,
    /// 视频封面（发视频时当 `inputThumbnail` 用）；GIF 也用它当静态图。
    pub thumbnail: Option<String>,
    /// 是不是 GIF（X 的 GIF 在接口里是单独的 `gif` 类型）。
    pub gif: bool,
    pub width: i32,
    pub height: i32,
    /// 秒。TDLib 的 `inputVideo.duration` 就是 i32 秒。
    pub duration: i32,
    /// 图片的 alt 文本（X 的无障碍描述）。
    pub alt: Option<String>,
}

impl Media {
    pub fn is_video(&self) -> bool {
        matches!(self.kind, MediaKind::Video)
    }

    /// 去重键：去掉查询串的 URL。
    ///
    /// `?name=large` / `?tag=21` 只是同一份文件的不同变体，同一个媒体在 Article 的
    /// `media_entities`、`cover_media` 和推文 `media.all` 里会出现好几次，必须按
    /// 「媒体本体」去重，否则相册里会出现重复图。
    pub fn key(&self) -> String {
        let base = self
            .url
            .split(['?', '#'])
            .next()
            .unwrap_or(self.url.as_str());
        base.to_owned()
    }
}

/// 作者信息。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Author {
    pub name: String,
    pub screen_name: String,
    pub avatar_url: String,
    pub verified: bool,
}

/// 互动数据，卡片底部那一排数字。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Metrics {
    pub replies: u64,
    pub retweets: u64,
    pub likes: u64,
    pub bookmarks: u64,
    pub views: u64,
}

impl Author {
    /// 卡片上那行 `@handle`。
    pub fn handle(&self) -> String {
        if self.screen_name.is_empty() {
            String::new()
        } else {
            format!("@{}", self.screen_name)
        }
    }
}

/// X 的长文（Article）。
#[derive(Debug, Clone, PartialEq)]
pub struct Article {
    pub id: String,
    pub title: String,
    /// 平台给的摘要（纯文本、可能为空）。
    pub preview_text: String,
    /// 文章头图。
    pub cover: Option<Media>,
    /// 按文档顺序排好的块（段落 / 标题 / 列表 / 图片 / 引用推文…）。
    pub blocks: Vec<Block>,
    /// 文章正文里出现的媒体（顺序 = 文档顺序，已去重）。
    pub media: Vec<Media>,
}

/// 一条帖子的解析结果。普通推文也有 Article 之外的全部字段。
#[derive(Debug, Clone, PartialEq)]
pub struct Post {
    pub id: String,
    pub url: String,
    /// 推文正文（长推是 note tweet 的全文；Article 时通常是空串或一句导语）。
    pub text: String,
    /// 接口给的语言（`en` / `zh-cn` …），用来决定卡片上显不显示「显示翻译」。
    pub lang: String,
    pub created_at: String,
    /// Unix 秒；接口没给就是 0，用来算「几小时前」。
    pub created_timestamp: i64,
    pub author: Author,
    pub article: Option<Article>,
    /// 推文自带的媒体（`media.all`：普通图片 / 视频 / GIF）。
    pub media: Vec<Media>,
    pub metrics: Metrics,
}

impl Post {
    /// 卡片 / 消息里用的日期。
    ///
    /// X 给的是 `Fri Jan 02 20:16:39 +0000 2026` 这种原始串，直接印出来又长又难读；
    /// 这里收敛成 `2026-01-02`，认不出来就原样返回（宁可不好看，也不猜错）。
    pub fn date_label(&self) -> String {
        short_date(&self.created_at)
    }

    /// 详情页时间戳里的「20:28」；解析不出来就是空串。
    pub fn time_of_day(&self) -> String {
        if let Some(local) = local_datetime(self.created_timestamp) {
            let time = local.format("%H:%M").to_string();
            return time.strip_prefix('0').map(str::to_owned).unwrap_or(time);
        }
        let parts: Vec<&str> = self.created_at.split_whitespace().collect();
        parts
            .get(3)
            .filter(|time| time.len() >= 5)
            .map(|time| time[..5].to_owned())
            .unwrap_or_default()
    }

    /// 详情页时间戳里的「2026年11月16日」，按宿主本地时区。
    pub fn full_date_label(&self) -> String {
        if let Some(local) = local_datetime(self.created_timestamp) {
            return local.format("%Y年%-m月%-d").to_string();
        }
        let label = self.date_label();
        let mut parts = label.split('-');
        let (Some(year), Some(month), Some(day)) = (parts.next(), parts.next(), parts.next())
        else {
            return label;
        };
        format!(
            "{year}年{}月{}日",
            month.trim_start_matches('0'),
            day.trim_start_matches('0')
        )
    }

    /// 送谷歌翻译的文本：推文正文，没有就用文章标题。
    pub fn translation_text(&self) -> &str {
        let text = self.text.trim();
        if !text.is_empty() {
            return text;
        }
        self.article
            .as_ref()
            .map(|article| article.title.trim())
            .unwrap_or("")
    }

    /// 标题：文章标题 → 推文首行 → 兜底。
    pub fn title(&self) -> String {
        if let Some(article) = &self.article
            && !article.title.trim().is_empty()
        {
            return article.title.trim().to_owned();
        }
        let first = self.text.lines().next().unwrap_or("").trim();
        if first.is_empty() {
            "X 帖子".to_owned()
        } else {
            crate::log::shorten(first, 80)
        }
    }

    /// 这次要发的媒体：**文章头图 + 文章正文媒体 + 推文媒体**，按出现顺序去重。
    ///
    /// 顺序是刻意的：头图在最前（它是文章的封面），正文媒体按块顺序，最后才是推文
    /// 自己的媒体（Article 推文的 `media.all` 往往就是头图，会被去重掉）。
    pub fn media_for_send(&self) -> Vec<Media> {
        let mut out: Vec<Media> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        let push = |media: &Media, out: &mut Vec<Media>, seen: &mut HashSet<String>| {
            if seen.insert(media.key()) {
                out.push(media.clone());
            }
        };
        if let Some(article) = &self.article {
            if let Some(cover) = &article.cover {
                push(cover, &mut out, &mut seen);
            }
            for media in &article.media {
                push(media, &mut out, &mut seen);
            }
        }
        for media in &self.media {
            push(media, &mut out, &mut seen);
        }
        out
    }
}

// ── 从 FxTwitter 的 JSON 提取 ──────────────────────────────────────

/// 把 `GET /status/{id}` 的整份响应解成 [`Post`]。
///
/// `code != 200` 时用平台给的 `message` 报错（那通常就是用户该看到的原因：
/// 帖子被删、账号受保护、需要登录…）。
pub fn from_api(json: &Value) -> Result<Post, String> {
    let code = json.get("code").and_then(Value::as_i64).unwrap_or(200);
    if code != 200 {
        let message = json
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("解析接口返回了错误");
        return Err(format!("{message}（{code}）"));
    }
    let tweet = json
        .get("tweet")
        .ok_or_else(|| "接口响应里没有 tweet 字段".to_owned())?;
    from_tweet(tweet)
}

/// 把 `tweet` 对象解成 [`Post`]（测试直接喂这一段）。
pub fn from_tweet(tweet: &Value) -> Result<Post, String> {
    // 墓志铭（帖子被删 / 被保护）：`{"__typename":"TweetTombstone",…}` 不是正常形状。
    if let Some(kind) = tweet.get("__typename").and_then(Value::as_str)
        && kind == "TweetTombstone"
    {
        return Err("这条帖子不可见（可能已删除或被保护）".to_owned());
    }
    let id = tweet
        .get("id")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| "响应里没有帖子 id".to_owned())?;
    let url = tweet
        .get("url")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .unwrap_or_else(|| format!("https://x.com/i/status/{id}"));
    let text = tweet
        .get("text")
        .and_then(Value::as_str)
        .map(clean_text)
        .unwrap_or_default();
    let lang = tweet
        .get("lang")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    let created_at = tweet
        .get("created_at")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    let created_timestamp = tweet
        .get("created_timestamp")
        .and_then(Value::as_i64)
        .unwrap_or(0);
    let author = author_of(tweet.get("author"));
    let article = tweet.get("article").and_then(article_of);
    let media = tweet
        .get("media")
        .and_then(|media| media.get("all"))
        .and_then(Value::as_array)
        .map(|items| items.iter().filter_map(media_of_tweet).collect())
        .unwrap_or_default();
    let metrics = Metrics {
        replies: count(tweet, "replies"),
        retweets: count(tweet, "retweets"),
        likes: count(tweet, "likes"),
        bookmarks: count(tweet, "bookmarks"),
        views: count(tweet, "views"),
    };

    Ok(Post {
        id,
        url,
        text,
        lang,
        created_at,
        created_timestamp,
        author,
        article,
        media,
        metrics,
    })
}

/// 接口里的计数字段，缺失或类型不对都按 0。
fn count(node: &Value, key: &str) -> u64 {
    node.get(key).and_then(Value::as_u64).unwrap_or(0)
}

/// 推文正文里去掉尾巴上的 `t.co` 短链（它是平台给媒体 / 外链自动加的）。
fn clean_text(text: &str) -> String {
    let mut lines: Vec<&str> = text.lines().collect();
    while let Some(last) = lines.last() {
        let trimmed = last.trim();
        if trimmed.is_empty() || trimmed.starts_with("https://t.co/") {
            lines.pop();
            continue;
        }
        break;
    }
    lines.join("\n").trim().to_owned()
}

fn author_of(node: Option<&Value>) -> Author {
    let node = node.unwrap_or(&Value::Null);
    Author {
        name: node
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned(),
        screen_name: node
            .get("screen_name")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned(),
        avatar_url: node
            .get("avatar_url")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned(),
        verified: node
            .get("verification")
            .and_then(|verification| verification.get("verified"))
            .and_then(Value::as_bool)
            .or_else(|| node.get("verified").and_then(Value::as_bool))
            .unwrap_or(false),
    }
}

fn article_of(node: &Value) -> Option<Article> {
    let content = node.get("content").cloned().unwrap_or(Value::Null);
    let media_entities = node
        .get("media_entities")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let (blocks, media) = article::parse_content(&content, &media_entities);
    Some(Article {
        id: node
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned(),
        title: node
            .get("title")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned(),
        preview_text: node
            .get("preview_text")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned(),
        cover: node.get("cover_media").and_then(from_article_media_node),
        blocks,
        media,
    })
}

/// 推文 `media.all` 里的一项。
///
/// | `type` | 结果 |
/// | --- | --- |
/// | `photo` / `gif`（图片位） | 图片，URL 用 `url` |
/// | `video` / `gif`（视频位） | 视频，从 `formats` 挑最高码率的 mp4 |
/// | `mosaic_photo` | 拼图，用 `formats.jpeg` |
fn media_of_tweet(node: &Value) -> Option<Media> {
    let kind = node.get("type").and_then(Value::as_str).unwrap_or("");
    let width = int_of(node.get("width"));
    let height = int_of(node.get("height"));
    let alt = node
        .get("altText")
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
        .map(str::to_owned);
    match kind {
        "photo" | "image" => {
            let url = node.get("url").and_then(Value::as_str)?.to_owned();
            Some(Media {
                kind: MediaKind::Photo,
                url,
                thumbnail: None,
                gif: false,
                width,
                height,
                duration: 0,
                alt,
            })
        }
        "gif" => {
            let url = node.get("url").and_then(Value::as_str)?.to_owned();
            Some(Media {
                kind: MediaKind::Photo,
                url,
                thumbnail: node
                    .get("thumbnail_url")
                    .and_then(Value::as_str)
                    .filter(|text| !text.is_empty())
                    .map(str::to_owned),
                gif: true,
                width,
                height,
                duration: 0,
                alt,
            })
        }
        "video" => {
            let url = best_video_url(node)?;
            let thumbnail = node
                .get("thumbnail_url")
                .and_then(Value::as_str)
                .filter(|text| !text.is_empty())
                .map(str::to_owned);
            Some(Media {
                kind: MediaKind::Video,
                url,
                thumbnail,
                gif: false,
                width,
                height,
                duration: int_of(node.get("duration")),
                alt,
            })
        }
        "mosaic_photo" => {
            let formats = node.get("formats")?;
            let url = formats
                .get("jpeg")
                .and_then(Value::as_str)
                .or_else(|| formats.get("webp").and_then(Value::as_str))?
                .to_owned();
            Some(Media {
                kind: MediaKind::Photo,
                url,
                thumbnail: None,
                gif: false,
                width,
                height,
                duration: 0,
                alt,
            })
        }
        _ => None,
    }
}

/// Article 的 `cover_media` / `media_entities[]`（形状一样）。
pub fn from_article_media_node(node: &Value) -> Option<Media> {
    let info = node.get("media_info").unwrap_or(node);
    match info.get("__typename").and_then(Value::as_str).unwrap_or("") {
        "ApiImage" => {
            let url = info.get("original_img_url").and_then(Value::as_str)?;
            Some(Media {
                kind: MediaKind::Photo,
                url: url.to_owned(),
                thumbnail: None,
                gif: false,
                width: int_of(info.get("original_img_width")),
                height: int_of(info.get("original_img_height")),
                duration: 0,
                alt: None,
            })
        }
        kind @ ("ApiVideo" | "ApiGif") => {
            let gif = kind == "ApiGif";
            let url = article_video_url(info)?;
            let duration = info
                .get("video_info")
                .and_then(|video| video.get("duration_millis"))
                .and_then(Value::as_u64)
                .unwrap_or(0) as i32
                / 1000;
            let original = info.get("original_info");
            let width = int_of(original.and_then(|original| original.get("width")));
            let height = int_of(original.and_then(|original| original.get("height")));
            Some(Media {
                kind: MediaKind::Video,
                url,
                thumbnail: info
                    .get("media_url_https")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                gif,
                width,
                height,
                duration,
                alt: None,
            })
        }
        _ => None,
    }
}

/// Article 视频的直链：`video_info.variants` 里挑最高码率的 mp4。
fn article_video_url(info: &Value) -> Option<String> {
    let variants = info
        .get("video_info")
        .and_then(|video| video.get("variants"))
        .and_then(Value::as_array);
    if let Some(variants) = variants
        && let Some(best) = variants
            .iter()
            .filter(|variant| {
                variant
                    .get("content_type")
                    .and_then(Value::as_str)
                    .map(|kind| kind.contains("mp4"))
                    .unwrap_or(false)
            })
            .max_by_key(|variant| variant.get("bitrate").and_then(Value::as_u64).unwrap_or(0))
        && let Some(url) = best.get("url").and_then(Value::as_str)
    {
        return Some(url.to_owned());
    }
    info.get("media_url_https")
        .and_then(Value::as_str)
        .map(str::to_owned)
}

/// 推文视频的直链：`formats` 里挑最高码率的 mp4，没有就退回 `url`。
pub fn best_video_url(node: &Value) -> Option<String> {
    let formats = node.get("formats").and_then(Value::as_array);
    if let Some(formats) = formats
        && let Some(best) = formats
            .iter()
            .filter(|format| {
                format
                    .get("container")
                    .and_then(Value::as_str)
                    .map(|container| container == "mp4")
                    .unwrap_or(false)
            })
            .max_by_key(|format| format.get("bitrate").and_then(Value::as_u64).unwrap_or(0))
        && let Some(url) = best.get("url").and_then(Value::as_str)
    {
        return Some(url.to_owned());
    }
    node.get("url").and_then(Value::as_str).map(str::to_owned)
}

fn local_datetime(timestamp: i64) -> Option<chrono::DateTime<chrono::Local>> {
    if timestamp <= 0 {
        return None;
    }
    chrono::Utc
        .timestamp_opt(timestamp, 0)
        .single()
        .map(|utc| utc.with_timezone(&chrono::Local))
}

/// `Fri Jan 02 20:16:39 +0000 2026` → `2026-01-02`；格式不对就原样返回。
fn short_date(raw: &str) -> String {
    let raw = raw.trim();
    let parts: Vec<&str> = raw.split_whitespace().collect();
    if parts.len() < 6 {
        return raw.to_owned();
    }
    let month = match parts[1].to_ascii_lowercase().as_str() {
        "jan" => "01",
        "feb" => "02",
        "mar" => "03",
        "apr" => "04",
        "may" => "05",
        "jun" => "06",
        "jul" => "07",
        "aug" => "08",
        "sep" => "09",
        "oct" => "10",
        "nov" => "11",
        "dec" => "12",
        _ => return raw.to_owned(),
    };
    let year = parts[5];
    if year.len() != 4 || !year.chars().all(|ch| ch.is_ascii_digit()) {
        return raw.to_owned();
    }
    let day: String = parts[2].chars().filter(|ch| ch.is_ascii_digit()).collect();
    if day.is_empty() {
        return raw.to_owned();
    }
    let day = format!("{day:0>2}");
    format!("{year}-{month}-{day}")
}

fn int_of(node: Option<&Value>) -> i32 {
    node.and_then(Value::as_i64).unwrap_or(0) as i32
}

#[cfg(test)]
#[path = "../tests/unit/model.rs"]
mod tests;
