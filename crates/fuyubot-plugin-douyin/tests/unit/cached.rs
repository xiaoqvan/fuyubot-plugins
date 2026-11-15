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
