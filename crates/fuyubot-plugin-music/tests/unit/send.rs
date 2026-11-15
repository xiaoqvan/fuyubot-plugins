use super::*;

fn meta() -> AudioMeta {
    AudioMeta {
        title: "晴天".to_owned(),
        performer: "周杰伦".to_owned(),
        album: "叶惠美".to_owned(),
        link: "https://music.163.com/song?id=186016".to_owned(),
        quality: "极高（exhigh）".to_owned(),
        bitrate: 320_000,
        size: 10 * 1024 * 1024,
        duration: 269,
        trial: false,
    }
}

#[test]
fn caption_carries_title_quality_and_link() {
    let caption = meta().caption();
    assert!(caption.starts_with("晴天 - 周杰伦"), "{caption}");
    assert!(caption.contains("专辑：叶惠美"), "{caption}");
    assert!(caption.contains("320 kbps"), "{caption}");
    assert!(caption.contains("10.0 MB"), "{caption}");
    assert!(
        caption.contains("https://music.163.com/song?id=186016"),
        "{caption}"
    );
    assert!(!caption.contains("试听"), "{caption}");
}

#[test]
fn trial_streams_say_so() {
    let mut value = meta();
    value.trial = true;
    assert!(value.caption().contains("试听片段"));
}

#[test]
fn empty_optional_fields_do_not_leave_empty_lines() {
    let mut value = meta();
    value.album.clear();
    value.link.clear();
    value.bitrate = 0;
    value.size = 0;
    let caption = value.caption();
    assert!(!caption.contains("专辑："), "{caption}");
    assert!(!caption.ends_with('\n'), "{caption}");
    assert!(caption.contains("音质：极高（exhigh）"), "{caption}");
}

fn video_meta() -> VideoMeta {
    VideoMeta {
        title: "塔不灭 #永雏塔菲 #重金属 #关注塔菲谢谢喵关注#塔菲 谢谢喵！".to_owned(),
        author: "永川塔菲".to_owned(),
        duration: 33,
        width: 1920,
        height: 1080,
        preview: false,
    }
}

#[test]
fn video_caption_follows_the_douyin_shape() {
    let caption = video_meta().caption();
    assert!(
        caption.contains("#关注塔菲谢谢喵关注 #塔菲"),
        "相邻话题要拆开：{caption}"
    );
    assert!(caption.ends_with("By @永川塔菲"), "{caption}");
    assert!(caption.starts_with("塔不灭 #永雏塔菲"), "{caption}");
    assert!(!caption.contains("试听"), "{caption}");
}

#[test]
fn preview_videos_say_so_and_empty_fields_have_fallbacks() {
    let mut value = video_meta();
    value.preview = true;
    assert!(value.caption().contains("试听片段"));
    value.preview = false;
    value.author.clear();
    value.title.clear();
    let caption = value.caption();
    assert!(caption.starts_with("汽水音乐视频"), "{caption}");
    assert!(caption.contains("By @汽水音乐"), "{caption}");
}
