//! 缓存文档：把「发送成功的结果」变成可复用的 JSON，下次直接按文件 id 重发。
//!
//! 这一层**不碰网络、不碰 TDLib**，只做三件事：
//!
//! 1. [`from_sent`]：发送成功后，用 [`crate::simplify::sent_media_list`] 从最终消息里
//!    取 TDLib 文件 id，拼成要写进 SQL 缓存的文档；
//! 2. [`Cached::parse`]：把文档读回来并**校验形状**（版本 / 种类 / 必备字段），
//!    坏文档当未命中；
//! 3. [`Cached::medias`]：按缓存的种类重建 [`Media`] 列表（`inputFileId`，零下载）。
//!
//! ## 文档形状（版本 1）
//!
//! ```json
//! {"v":1,"kind":"video","file_id":111,"cover_id":222,"width":1080,"height":1920,
//!  "duration":15,"caption":"最终发出去的 markdown 文本"}
//! {"v":1,"kind":"photos","items":[{"file_id":1,"width":800,"height":600}],"caption":"…"}
//! {"v":1,"kind":"videos","items":[{"file_id":2,"width":1080,"height":1920,"duration":3}],"caption":"…"}
//! ```
//!
//! `caption` 存的是**最终发出去的 markdown 文本**（`build_caption` 的结果）原样，
//! 命中时直接 `Text::markdown(caption)`——保证第二次发的文案与第一次逐字一致，
//! 不用再解析分享页去重建作者 / 音乐那些字段。
//!
//! `>10` 张图集的分组信息**不存**：重发时按 10 张重新切组即可（组数只取决于项数）。

use serde_json::{Value, json};

use crate::model::VideoInfo;
use crate::simplify::{InputFile, Media, SentMedia, sent_media_list};

/// 文档版本。字段变了就加一，旧文档自动当未命中（不猜、不迁移）。
pub const VERSION: i64 = 1;

/// 单视频。
pub const KIND_VIDEO: &str = "video";
/// 图集（纯图片）。
pub const KIND_PHOTOS: &str = "photos";
/// 图集里夹着的视频（发的是视频相册）。
pub const KIND_VIDEOS: &str = "videos";

/// 缓存里的一行：三种可重发的形态。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Cached {
    /// 单个视频（可选封面）。
    Video {
        file_id: i64,
        cover_id: Option<i64>,
        width: i64,
        height: i64,
        duration: i64,
        caption: String,
    },
    /// 一组图片。
    Photos {
        items: Vec<CachedItem>,
        caption: String,
    },
    /// 一组视频（图集里的那些页）。
    Videos {
        items: Vec<CachedItem>,
        caption: String,
    },
}

/// 相册里的一项。`duration` 只对视频有意义（图片是 0）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CachedItem {
    pub file_id: i64,
    pub width: i64,
    pub height: i64,
    pub duration: i64,
}

impl CachedItem {
    fn to_json(&self) -> Value {
        json!({
            "file_id": self.file_id,
            "width": self.width,
            "height": self.height,
            "duration": self.duration,
        })
    }

    fn from_json(value: &Value) -> Option<Self> {
        Some(Self {
            file_id: positive(value, "file_id")?,
            width: integer(value, "width"),
            height: integer(value, "height"),
            duration: integer(value, "duration"),
        })
    }
}

impl Cached {
    /// 最终发出去的文案（命中时原样重发）。
    pub fn caption(&self) -> &str {
        match self {
            Self::Video { caption, .. }
            | Self::Photos { caption, .. }
            | Self::Videos { caption, .. } => caption,
        }
    }

    /// 从文档读回并校验。版本不对 / 形状不对 / 关键字段缺失一律 `None`
    /// （= 当未命中，走完整解析）。
    pub fn parse(doc: &Value) -> Option<Self> {
        if doc.get("v").and_then(Value::as_i64) != Some(VERSION) {
            return None;
        }
        let caption = doc.get("caption")?.as_str()?.to_owned();
        match doc.get("kind")?.as_str()? {
            KIND_VIDEO => Some(Self::Video {
                file_id: positive(doc, "file_id")?,
                // 封面可以没有：`0` / 缺字段都当没有。
                cover_id: positive(doc, "cover_id"),
                width: integer(doc, "width"),
                height: integer(doc, "height"),
                duration: integer(doc, "duration"),
                caption,
            }),
            KIND_PHOTOS | KIND_VIDEOS => {
                let items: Vec<CachedItem> = doc
                    .get("items")?
                    .as_array()?
                    .iter()
                    .map(CachedItem::from_json)
                    .collect::<Option<_>>()?;
                if items.is_empty() {
                    return None;
                }
                if doc["kind"] == KIND_PHOTOS {
                    Some(Self::Photos { items, caption })
                } else {
                    Some(Self::Videos { items, caption })
                }
            }
            _ => None,
        }
    }

    /// 重建待发送的媒体列表（全部是 `inputFileId`：TDLib 不再下载、不再上传）。
    pub fn medias(&self) -> Vec<Media> {
        match self {
            Self::Video {
                file_id,
                cover_id,
                width,
                height,
                duration,
                ..
            } => {
                let mut media = Media::video(InputFile::id(*file_id))
                    .size(*width as i32, *height as i32)
                    .duration(*duration as i32)
                    .streaming(true);
                if let Some(cover_id) = cover_id {
                    media = media.cover(InputFile::id(*cover_id), 0);
                }
                vec![media]
            }
            Self::Photos { items, .. } => items
                .iter()
                .map(|item| {
                    Media::photo(InputFile::id(item.file_id))
                        .size(item.width as i32, item.height as i32)
                })
                .collect(),
            Self::Videos { items, .. } => items
                .iter()
                .map(|item| {
                    Media::video(InputFile::id(item.file_id))
                        .size(item.width as i32, item.height as i32)
                        .duration(item.duration as i32)
                        .streaming(true)
                })
                .collect(),
        }
    }
}

/// 发送成功后拼缓存文档；取不到可用文件 id 就返回 `None`（不写缓存，下次照常解析）。
///
/// `info` 决定**发出去的是哪一种形态**（与 [`crate::send::send_video`] 的分支一一对应：
/// 图集夹视频 → 视频相册；纯图集 → 图片相册；否则单视频）。
/// `messages` 是发送结果：单条消息是对象，相册是消息数组。
/// `caption` 是这次真正发出去的 markdown 文本。
pub fn from_sent(info: &VideoInfo, messages: &Value, caption: &str) -> Option<Value> {
    let medias = sent_media_list(messages);
    let images = info.images.as_deref().unwrap_or_default();
    let image_videos = info.image_videos.as_deref().unwrap_or_default();

    if images.is_empty() {
        // 单视频：取第一条带 file id 的视频消息。
        let media = medias
            .iter()
            .find(|media| media.file_id.is_some() && media.kind == crate::simplify::KIND_VIDEO)
            .or_else(|| medias.iter().find(|media| media.file_id.is_some()))?;
        return Some(json!({
            "v": VERSION,
            "kind": KIND_VIDEO,
            "file_id": media.file_id?,
            "cover_id": media.cover_id,
            "width": media.width,
            "height": media.height,
            "duration": media.duration,
            "caption": caption,
        }));
    }

    let (kind, expected) = if image_videos.is_empty() {
        (KIND_PHOTOS, crate::simplify::KIND_PHOTO)
    } else {
        (KIND_VIDEOS, crate::simplify::KIND_VIDEO)
    };
    let items: Vec<CachedItem> = medias
        .iter()
        .filter(|media| media.kind == expected)
        .filter_map(item_of)
        .collect();
    if items.is_empty() {
        return None;
    }
    Some(json!({
        "v": VERSION,
        "kind": kind,
        "items": items.iter().map(CachedItem::to_json).collect::<Vec<_>>(),
        "caption": caption,
    }))
}

/// 一条已发送媒体 → 缓存项；没有 file id 的跳过。
fn item_of(media: &SentMedia) -> Option<CachedItem> {
    Some(CachedItem {
        file_id: media.file_id?,
        width: media.width,
        height: media.height,
        duration: media.duration,
    })
}

/// 必填的正整数（文件 id 0 是 TDLib 的「空文件」占位，不算）。
fn positive(value: &Value, key: &str) -> Option<i64> {
    value.get(key).and_then(Value::as_i64).filter(|id| *id > 0)
}

/// 可选整数（缺省 0 = 让 TDLib 自己判断）。
fn integer(value: &Value, key: &str) -> i64 {
    value.get(key).and_then(Value::as_i64).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Author, Image, Video, VideoInfo};

    fn video_info() -> VideoInfo {
        VideoInfo {
            detail_id: "7401".to_owned(),
            title: "标题".to_owned(),
            cover_url: "https://cover/1".to_owned(),
            video: Some(Video {
                video_url: "https://play/1".to_owned(),
                fps: Some(30),
                duration: Some(15),
                height: 1920,
                width: 1080,
            }),
            images: None,
            image_videos: None,
            author: Author::default(),
            music: None,
            cooperation_info: Vec::new(),
        }
    }

    fn sent_video_message(file_id: i64, cover_id: i64) -> Value {
        json!({
            "content": {
                "@type": "messageVideo",
                "video": {
                    "video": { "id": file_id },
                    "width": 1080, "height": 1920, "duration": 15,
                },
                "cover": { "sizes": [
                    { "photo": { "id": cover_id }, "width": 320, "height": 320 },
                ]},
            },
        })
    }

    fn sent_photo_message(file_id: i64, width: i64, height: i64) -> Value {
        json!({
            "content": {
                "@type": "messagePhoto",
                "photo": { "sizes": [
                    { "photo": { "id": file_id }, "width": width, "height": height },
                ]},
            },
        })
    }

    /// 单视频：file id + 封面 id + 尺寸时长 + 文案，序列化再读回来完全一致。
    #[test]
    fn video_document_round_trips() {
        let info = video_info();
        let message = sent_video_message(111, 222);
        let doc = from_sent(&info, &message, "标题\n\nBy @作者").expect("应当拼出文档");
        assert_eq!(doc["v"], 1);
        assert_eq!(doc["kind"], KIND_VIDEO);
        assert_eq!(doc["file_id"], 111);
        assert_eq!(doc["cover_id"], 222);
        assert_eq!(doc["width"], 1080);
        assert_eq!(doc["height"], 1920);
        assert_eq!(doc["duration"], 15);
        assert_eq!(doc["caption"], "标题\n\nBy @作者");

        // 落盘 → 读回（走一遍 JSON 文本，确认能反序列化）
        let text = doc.to_string();
        let parsed = Cached::parse(&serde_json::from_str(&text).unwrap()).expect("必须能读回");
        assert_eq!(
            parsed,
            Cached::Video {
                file_id: 111,
                cover_id: Some(222),
                width: 1080,
                height: 1920,
                duration: 15,
                caption: "标题\n\nBy @作者".to_owned(),
            }
        );
        // 重建出来的媒体是 inputFileId，且带封面
        let media = parsed.medias();
        assert_eq!(media.len(), 1);
        match &media[0] {
            Media::Video { file, cover, .. } => {
                assert_eq!(*file, InputFile::id(111));
                assert_eq!(*cover, Some(InputFile::id(222)));
            }
            other => panic!("应当是视频，实际 {other:?}"),
        }
    }

    /// 没有封面的视频也能缓存（`cover_id` 缺省）。
    #[test]
    fn video_document_without_cover_round_trips() {
        let message = json!({
            "content": {
                "@type": "messageVideo",
                "video": { "video": { "id": 9 }, "width": 1, "height": 2, "duration": 3 },
            },
        });
        let info = video_info();
        let doc = from_sent(&info, &message, "c").expect("没有封面也要能缓存");
        assert!(doc.get("cover_id").is_none() || doc["cover_id"].is_null());
        let parsed = Cached::parse(&doc).unwrap();
        assert_eq!(parsed.medias().len(), 1);
    }

    /// 图集：每张图一个 file id；重发时按 inputFileId 重建（不再下载）。
    #[test]
    fn photos_document_round_trips() {
        let mut info = video_info();
        info.video = None;
        info.images = Some(vec![
            Image {
                height: 600,
                width: 800,
                url: String::new(),
            },
            Image {
                height: 601,
                width: 801,
                url: String::new(),
            },
        ]);
        let messages = json!([
            sent_photo_message(1, 800, 600),
            sent_photo_message(2, 801, 601),
        ]);
        let doc = from_sent(&info, &messages, "图集文案").expect("应当拼出图集文档");
        assert_eq!(doc["kind"], KIND_PHOTOS);
        let items = doc["items"].as_array().unwrap();
        assert_eq!(items.len(), 2);

        let parsed = Cached::parse(&doc).unwrap();
        assert_eq!(parsed.caption(), "图集文案");
        let medias = parsed.medias();
        assert_eq!(medias.len(), 2);
        match &medias[1] {
            Media::Photo {
                file,
                width,
                height,
                ..
            } => {
                assert_eq!(*file, InputFile::id(2));
                assert_eq!((*width, *height), (801, 601));
            }
            other => panic!("应当是图片，实际 {other:?}"),
        }
    }

    /// 图集里夹视频：发的是视频相册，缓存也存视频项（带时长）。
    #[test]
    fn videos_document_round_trips() {
        let mut info = video_info();
        info.video = None;
        info.images = Some(vec![Image {
            height: 1,
            width: 1,
            url: String::new(),
        }]);
        info.image_videos = Some(vec![Video {
            video_url: String::new(),
            fps: None,
            duration: None,
            height: 16,
            width: 9,
        }]);
        let messages = json!([sent_video_message(7, 0)]);
        let doc = from_sent(&info, &messages, "视频集").expect("应当拼出视频集文档");
        assert_eq!(doc["kind"], KIND_VIDEOS);
        let parsed = Cached::parse(&doc).unwrap();
        let medias = parsed.medias();
        assert_eq!(medias.len(), 1);
        match &medias[0] {
            Media::Video { file, duration, .. } => {
                assert_eq!(*file, InputFile::id(7));
                assert_eq!(*duration, 15);
            }
            other => panic!("应当是视频，实际 {other:?}"),
        }
    }

    /// 版本 / 种类不对、必备字段缺失、空 items：一律当未命中（`None`），不 panic。
    #[test]
    fn malformed_documents_are_rejected() {
        let cases = [
            json!({"v": 2, "kind": "video", "file_id": 1, "caption": "x"}),
            json!({"kind": "video", "file_id": 1, "caption": "x"}),
            json!({"v": 1, "kind": "video", "caption": "x"}),
            json!({"v": 1, "kind": "video", "file_id": 0, "caption": "x"}),
            json!({"v": 1, "kind": "unknown", "file_id": 1, "caption": "x"}),
            json!({"v": 1, "kind": "photos", "items": [], "caption": "x"}),
            json!({"v": 1, "kind": "photos", "items": [{"width": 1}], "caption": "x"}),
            json!({"v": 1, "kind": "photos", "items": [{"file_id": 1}]}),
            json!({"v": 1, "kind": "videos", "items": "not an array", "caption": "x"}),
        ];
        for doc in cases {
            assert!(Cached::parse(&doc).is_none(), "坏文档必须当未命中：{doc}");
        }
    }

    /// 发送结果里取不到文件 id（比如 TDLib 只回了空文件对象）时不写缓存。
    #[test]
    fn a_send_result_without_file_ids_is_not_cached() {
        let info = video_info();
        let message = json!({"content": {"@type": "messageVideo", "video": {"video": {"id": 0}}}});
        assert!(from_sent(&info, &message, "x").is_none());
        assert!(from_sent(&info, &json!([]), "x").is_none());
    }
}
