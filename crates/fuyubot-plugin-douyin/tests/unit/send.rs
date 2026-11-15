use super::*;
use crate::model::{Author, Coauthor, Music, Video};

fn info() -> VideoInfo {
    VideoInfo {
        detail_id: "1".to_owned(),
        title: "标题 #A#B".to_owned(),
        cover_url: "https://cover/1".to_owned(),
        video: Some(Video {
            video_url: "https://play/1".to_owned(),
            fps: Some(30),
            duration: Some(12),
            height: 1920,
            width: 1080,
        }),
        images: None,
        image_videos: None,
        author: Author {
            name: "作者".to_owned(),
            home_url: Some("https://www.douyin.com/user/MS4w".to_owned()),
            ..Author::default()
        },
        music: None,
        cooperation_info: Vec::new(),
    }
}

/// 频道占位文案对齐 TS：原文 + 27 个 `\-` + 「正在解析抖音视频...」。
#[test]
fn channel_parsing_placeholder_matches_ts() {
    let text = parsing_placeholder("https://v.douyin.com/x/");
    assert!(text.starts_with("https://v.douyin.com/x/\n"), "{text}");
    assert_eq!(text.matches("\\-").count(), 27, "{text}");
    assert!(text.ends_with("\n\n正在解析抖音视频..."), "{text}");
}

#[test]
fn adjacent_hashtags_are_separated() {
    assert_eq!(sanitize_title("#开头"), "#开头", "首字符不插空格");
    assert_eq!(sanitize_title("a#b"), "a #b");
    assert_eq!(
        sanitize_title("已经 #好 的"),
        "已经 #好 的",
        "本来有空格就不动"
    );
    assert_eq!(sanitize_title(""), "");
}

#[test]
fn caption_ends_with_a_markdown_author_link() {
    let caption = build_caption(&info());
    assert!(caption.starts_with("标题 #A #B"), "{caption}");
    assert!(
        caption.ends_with("By [@作者](https://www.douyin.com/user/MS4w)"),
        "{caption}"
    );
}

#[test]
fn an_author_without_home_url_falls_back_to_plain_text() {
    let mut value = info();
    value.author.home_url = Some("https://www.douyin.com/user/".to_owned());
    assert!(build_caption(&value).ends_with("By @作者"));

    let mut value = info();
    value.author.home_url = None;
    assert!(build_caption(&value).ends_with("By @作者"));
}

#[test]
fn coauthors_are_appended_after_the_main_author() {
    let mut value = info();
    value.cooperation_info = vec![Coauthor {
        nickname: Some("摄影君".to_owned()),
        role: Some("摄影".to_owned()),
        home_url: Some("https://www.douyin.com/user/abc".to_owned()),
        ..coauthor()
    }];
    let caption = build_caption(&value);
    assert!(
        caption.ends_with("[@摄影君(摄影)](https://www.douyin.com/user/abc)"),
        "{caption}"
    );
}

/// TS 版那个奇怪的判据：只有「有音乐 **且** 有图集视频」时才挂音乐链接。
#[test]
fn music_only_shows_when_image_videos_exist() {
    let mut value = info();
    value.music = Some(music());
    assert!(
        !build_caption(&value).contains("歌名"),
        "纯视频作品不挂音乐"
    );

    value.image_videos = Some(vec![Video {
        video_url: "https://iv/1".to_owned(),
        ..info().video.unwrap()
    }]);
    let caption = build_caption(&value);
    assert!(caption.contains("[歌名](https://m/1)"), "{caption}");
}

/// 单条 `sendMessage` 的 op 形状：op 名 + 超时 + 内容，且**不带 `reply_to`**。
#[test]
fn message_request_builds_a_send_message_op_without_reply() {
    let target = Target::send(42);
    let params = SendMessageParams {
        text: Some(Text::markdown("hi")),
        media: Some(Media::video(InputFile::id(5))),
        timeout_ms: 1234,
    };
    let (op, args) = message_request(&params, &target);
    assert_eq!(op, "send_message");
    assert_eq!(args["chat_id"], 42);
    assert_eq!(args["timeout_ms"], 1234);
    assert!(args.get("reply_to").is_none(), "不该引用原消息：{args}");
    assert_eq!(
        args["input_message_content"]["video"]["video"],
        json!({"@type": "inputFileId", "id": 5})
    );
}

/// 频道：同一个媒体改成 `edit_message_media`，键是 `chat_id` + `message_id`。
#[test]
fn message_request_in_channel_becomes_edit_media() {
    let target = Target::edit(42, 99);
    let params = SendMessageParams {
        text: Some(Text::markdown("hi")),
        media: Some(Media::video(InputFile::id(5))),
        timeout_ms: 10,
    };
    let (op, args) = message_request(&params, &target);
    assert_eq!(op, "edit_message_media");
    assert_eq!(args["chat_id"], 42);
    assert_eq!(args["message_id"], 99);
    assert!(args.get("chat_id").is_some());
    assert!(args.get("reply_to").is_none());
}

/// 相册：op 名与 `input_message_contents` 数组，且**不带 `reply_to`**。
#[test]
fn album_request_builds_a_send_message_album_op_without_reply() {
    let target = Target::send(42);
    let params = SendAlbumParams {
        medias: vec![
            Media::photo(InputFile::id(1)),
            Media::photo(InputFile::id(2)),
        ],
        caption: Some(Text::markdown("c")),
        timeout_ms: 5,
    };
    let (op, args) = album_request(&params, &target);
    assert_eq!(op, "send_message_album");
    assert_eq!(args["timeout_ms"], 5);
    assert!(args.get("reply_to").is_none(), "不该引用原消息：{args}");
    assert_eq!(args["input_message_contents"].as_array().unwrap().len(), 2);
    assert_eq!(
        args["input_message_contents"][0]["photo"]["photo"],
        json!({"@type": "inputFileId", "id": 1})
    );
}

fn music() -> Music {
    Music {
        id: 1,
        id_str: "m".to_owned(),
        name: "歌名".to_owned(),
        url: "https://m/1".to_owned(),
        author: "演唱者".to_owned(),
        sec_uid: String::new(),
        avatar: String::new(),
        cover: String::new(),
    }
}

fn coauthor() -> Coauthor {
    Coauthor {
        avatar: None,
        nickname: None,
        role: None,
        uid: None,
        sec_uid: None,
        home_url: None,
    }
}
