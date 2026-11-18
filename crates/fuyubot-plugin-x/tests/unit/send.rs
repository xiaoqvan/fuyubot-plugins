use super::*;
use crate::model::{Media as PostMedia, MediaKind};

fn photo(url: &str) -> PostMedia {
    PostMedia {
        kind: MediaKind::Photo,
        url: url.to_owned(),
        thumbnail: None,
        gif: false,
        width: 1200,
        height: 800,
        duration: 0,
        alt: None,
    }
}

#[test]
fn album_hard_limit_matches_telegram() {
    // Telegram 的相册只收 2..=10 项，这个常量是所有分组逻辑的依据。
    assert_eq!(ALBUM_HARD_LIMIT, 10);
    assert_eq!(crate::config::ALBUM_MAX, ALBUM_HARD_LIMIT);
}

#[test]
fn stats_summary_describes_what_was_sent() {
    let stats = Stats {
        texts: 2,
        albums: 3,
        media: 23,
        failed: 0,
        card: false,
    };
    let summary = stats.summary();
    assert!(summary.contains("文字 2 条"), "{summary}");
    assert!(summary.contains("相册 3 组"), "{summary}");
    assert!(!summary.contains("失败"), "{summary}");
}

#[test]
fn stats_summary_reports_a_card_and_failures() {
    let stats = Stats {
        texts: 0,
        albums: 0,
        media: 1,
        failed: 2,
        card: true,
    };
    let summary = stats.summary();
    assert!(summary.contains("卡片 1 张"), "{summary}");
    assert!(summary.contains("失败 2"), "{summary}");
}

#[test]
fn temp_file_names_are_stable_and_unique_per_url() {
    let url = "https://pbs.twimg.com/media/AAA.jpg?name=orig";
    assert_eq!(file_name(&photo(url)), file_name(&photo(url)));
    assert_ne!(
        file_name(&photo(url)),
        file_name(&photo("https://pbs.twimg.com/media/BBB.jpg?name=orig"))
    );
    assert!(file_name(&photo(url)).ends_with(".jpg"));

    let video = PostMedia {
        kind: MediaKind::Video,
        ..photo("https://video.twimg.com/x.mp4")
    };
    assert!(file_name(&video).ends_with(".mp4"));
}
