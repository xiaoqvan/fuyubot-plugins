use super::*;
use serde_json::Value;

/// 真实接口响应的快照（2026-11 抓的 @X 文章），用来锁住 Article 的解析。
const ARTICLE: &str = include_str!("../fixtures/status_article.json");
/// 真实接口响应的快照：一条带视频的普通推文。
const VIDEO: &str = include_str!("../fixtures/status_video.json");

fn parse(text: &str) -> Post {
    let json: Value = serde_json::from_str(text).expect("fixture 应该是合法 JSON");
    from_api(&json).expect("应解析出 Post")
}

#[test]
fn article_payload_becomes_a_readable_post() {
    let post = parse(ARTICLE);
    assert_eq!(post.id, "2007184284944322584");
    assert!(post.author.screen_name.eq_ignore_ascii_case("x"));
    assert!(post.title().contains("this week on X"), "{}", post.title());

    let article = post.article.as_ref().expect("这是文章");
    assert!(!article.blocks.is_empty());
    assert!(article.title.contains("this week on X"));
}

#[test]
fn article_payload_carries_badge_and_counts() {
    let post = parse(ARTICLE);
    assert!(post.author.verified, "X 的官方号有认证");
    assert!(post.author.avatar_url.contains("pbs.twimg.com"));
    assert_eq!(post.metrics.likes, 1508);
    assert_eq!(post.metrics.views, 999281);
}

#[test]
fn article_blocks_include_headings_and_embedded_tweets() {
    let post = parse(ARTICLE);
    let article = post.article.as_ref().expect("这是文章");

    let mut headings = 0;
    let mut tweets = 0;
    let mut paragraphs = 0;
    for block in &article.blocks {
        match block.kind {
            crate::article::BlockKind::Heading(_) => headings += 1,
            crate::article::BlockKind::Tweet(_) => tweets += 1,
            crate::article::BlockKind::Paragraph => paragraphs += 1,
            _ => {}
        }
    }
    assert!(headings > 0, "文章里应该有 header-* 块");
    assert!(tweets > 0, "这篇文章嵌了推文");
    assert!(paragraphs > 0);
}

#[test]
fn article_cover_counts_as_the_only_media() {
    let post = parse(ARTICLE);
    let article = post.article.as_ref().expect("这是文章");
    // 这篇没有正文图片，只有头图。
    assert!(article.media.is_empty());
    let cover = article.cover.as_ref().expect("有头图");
    assert!(cover.url.contains("pbs.twimg.com"));
    assert!(cover.width > 0 && cover.height > 0);

    let media = post.media_for_send();
    assert_eq!(media.len(), 1, "头图就是这次要发的唯一媒体");
}

#[test]
fn video_payload_picks_an_mp4_direct_link() {
    let post = parse(VIDEO);
    assert_eq!(post.id, "2036082537949434164");
    assert!(post.article.is_none());

    let media = post.media_for_send();
    assert_eq!(media.len(), 1);
    let video = &media[0];
    assert!(video.is_video());
    assert!(
        video.url.starts_with("https://video.twimg.com"),
        "{}",
        video.url
    );
    assert!(video.url.ends_with(".mp4") || video.url.contains(".mp4?"));
    assert!(video.width > 0 && video.height > 0);
}

#[test]
fn media_dedup_key_ignores_the_query_string() {
    let first = Media {
        kind: MediaKind::Photo,
        url: "https://pbs.twimg.com/media/A.jpg?name=large".to_owned(),
        thumbnail: None,
        gif: false,
        width: 1,
        height: 1,
        duration: 0,
        alt: None,
    };
    let second = Media {
        url: "https://pbs.twimg.com/media/A.jpg?name=orig".to_owned(),
        ..first.clone()
    };
    // 去重键就是去掉查询串的 URL：同一张图的 name=large / name=orig 是同一条。
    assert_eq!(first.key(), second.key());
    assert_eq!(first.key(), "https://pbs.twimg.com/media/A.jpg");

    let other = Media {
        url: "https://pbs.twimg.com/media/B.jpg?name=orig".to_owned(),
        ..first.clone()
    };
    assert_ne!(first.key(), other.key());
}

#[test]
fn date_label_shortens_the_twitter_date() {
    let post = parse(ARTICLE);
    assert_eq!(post.date_label(), "2026-01-02");
}

#[test]
fn date_label_keeps_unknown_formats() {
    let post = Post {
        created_at: "2026 年 1 月 2 日".to_owned(),
        ..parse(ARTICLE)
    };
    assert_eq!(post.date_label(), "2026 年 1 月 2 日");
}

#[test]
fn api_errors_surface_the_platform_message() {
    let json: Value = serde_json::from_str(r#"{"code":404,"message":"NOT_FOUND"}"#).unwrap();
    let error = from_api(&json).expect_err("应该报错");
    assert!(error.contains("NOT_FOUND"), "{error}");
}

#[test]
fn tombstone_payloads_are_rejected() {
    let json: Value =
        serde_json::from_str(r#"{"code":200,"tweet":{"__typename":"TweetTombstone"}}"#).unwrap();
    assert!(from_api(&json).is_err());
}
