use super::*;
use crate::article::Block;
use crate::model::{Author, Media as PostMedia, MediaKind};

fn post(text: &str) -> Post {
    Post {
        id: "1".to_owned(),
        url: "https://x.com/i/status/1".to_owned(),
        text: text.to_owned(),
        lang: String::new(),
        created_at: "2026 年 1 月 2 日".to_owned(),
        created_timestamp: 0,
        author: Author {
            name: "作者".to_owned(),
            screen_name: "author".to_owned(),
            avatar_url: String::new(),
            verified: false,
        },
        article: None,
        media: Vec::new(),
        metrics: Default::default(),
    }
}

fn bold_run(text: &str) -> Run {
    Run {
        text: text.to_owned(),
        bold: true,
        italic: false,
        code: false,
        link: None,
    }
}

#[test]
fn escape_covers_html_metacharacters() {
    assert_eq!(escape("<b>&</b>"), "&lt;b&gt;&amp;&lt;/b&gt;");
    assert_eq!(escape_attr("a\"b"), "a&quot;b");
}

#[test]
fn messages_carry_author_and_source_link() {
    let messages = messages(&post("你好世界"), &Config::default());
    assert_eq!(messages.len(), 1);
    let message = &messages[0];
    assert!(message.contains("你好世界"));
    assert!(message.contains("@author"));
    assert!(message.contains("https://x.com/i/status/1"));
    // 普通推文的正文只出现一次（不加一行重复的加粗标题）。
    assert_eq!(message.matches("你好世界").count(), 1);
}

#[test]
fn article_messages_start_with_the_bold_title() {
    let mut post = post("导语");
    post.article = Some(crate::model::Article {
        id: "9".to_owned(),
        title: "文章标题".to_owned(),
        preview_text: String::new(),
        cover: None,
        blocks: vec![Block {
            kind: BlockKind::Paragraph,
            runs: vec![Run {
                text: "正文第一段".to_owned(),
                bold: false,
                italic: false,
                code: false,
                link: None,
            }],
        }],
        media: Vec::new(),
    });
    let messages = messages(&post, &Config::default());
    let message = &messages[0];
    assert!(message.starts_with("<b>文章标题</b>"), "{message}");
    assert!(message.contains("正文第一段"));
}

#[test]
fn a_long_post_is_split_into_several_messages() {
    let body = "很长的正文。".repeat(1200);
    let config = Config::default();
    let messages = messages(&post(&body), &config);
    assert!(messages.len() > 1, "应切成多条：{}", messages.len());
    for message in &messages {
        assert!(
            message.chars().count() <= config.text_chunk,
            "单条超限：{}",
            message.chars().count()
        );
    }
}

#[test]
fn split_fragments_keep_tags_closed() {
    let fragment = Fragment::new("<b>", "</b>", vec![bold_run(&"加粗".repeat(300))]);
    let pieces = fragment.split_to(120);
    assert!(pieces.len() > 1);
    for piece in pieces {
        let html = piece.render();
        assert!(html.starts_with("<b>"));
        assert!(html.ends_with("</b>"));
        assert!(html.chars().count() <= 120);
    }
}

#[test]
fn pack_never_exceeds_the_limit() {
    let fragments: Vec<String> = (0..30).map(|index| format!("第 {index} 段")).collect();
    let packed = pack(fragments, 30);
    assert!(packed.len() > 1);
    for message in packed {
        assert!(message.chars().count() <= 30);
    }
}

#[test]
fn caption_keeps_the_link_when_the_text_is_too_long() {
    let body = "很长的推文".repeat(500);
    let (caption, truncated) = caption(&post(&body), CAPTION_LIMIT);
    assert!(truncated);
    assert!(caption.chars().count() <= CAPTION_LIMIT);
    assert!(caption.contains("https://x.com/i/status/1"));
}

#[test]
fn short_caption_is_not_truncated() {
    let (caption, truncated) = caption(&post("一句话"), CAPTION_LIMIT);
    assert!(!truncated);
    assert!(caption.contains("一句话"));
}

#[test]
fn card_is_a_self_contained_html_document() {
    let config = Config::default();
    let html = card_html(&post("纯文本正文"), &config);
    assert!(html.starts_with("<!doctype html>"));
    assert!(html.contains("纯文本正文"));
    assert!(html.contains("class=\"tweet\""));
    assert!(html.contains("class=\"actions\""));
    assert!(!html.contains("<script"));
}

#[test]
fn card_turns_embedded_tweets_into_a_compact_label() {
    let mut post = post("导语");
    post.created_at = "Fri Jan 02 20:16:39 +0000 2026".to_owned();
    post.article = Some(crate::model::Article {
        id: "9".to_owned(),
        title: "标题".to_owned(),
        preview_text: String::new(),
        cover: None,
        blocks: vec![Block {
            kind: BlockKind::Tweet("1234567890".to_owned()),
            runs: Vec::new(),
        }],
        media: Vec::new(),
    });
    let html = card_html(&post, &Config::default());
    assert!(html.contains("嵌入推文"), "{html}");
    assert!(html.contains("x.com/i/status/1234567890"));
    // 时间戳是详情页的「时间 · 中文日期」；作者 handle 只在头部出现一次。
    assert!(html.contains("20:16"), "{html}");
    assert!(html.contains("2026年1月2日"), "{html}");
    assert!(!html.contains("Fri Jan 02"));
    assert_eq!(html.matches("@author").count(), 1);
}

#[test]
fn card_puts_the_handle_below_the_name() {
    let html = card_html(&post("正文"), &Config::default());
    let name = html.find("class=\"name\"").expect("有 name");
    let handle = html.find("class=\"handle\"").expect("有 handle");
    assert!(name < handle);
    assert!(html[name..handle].contains("作者"));
}

#[test]
fn card_shows_translate_only_for_foreign_posts() {
    let mut english = post("hello");
    english.lang = "en".to_owned();
    assert!(card_html(&english, &Config::default()).contains("显示翻译"));

    let chinese = post("你好");
    assert!(!card_html(&chinese, &Config::default()).contains("显示翻译"));
}

#[test]
fn links_and_hashtags_are_blue_and_scheme_is_stripped() {
    let html = linkify("看 https://artgf.app 和 #测试 @user");
    assert!(html.contains(">artgf.app</a>"), "{html}");
    assert!(!html.contains(">https://artgf.app</a>"));
    assert!(html.contains(">#测试</a>"), "{html}");
    assert!(html.contains(">@user</a>"), "{html}");
}

#[test]
fn card_footer_sits_after_the_actions() {
    let html = card_html_with_footer(
        &post("正文"),
        &Config::default(),
        None,
        Some("@FuyuBot · fuyubot_plugin.xiaoqvan.x"),
        None,
        None,
    );
    assert!(html.contains(">@FuyuBot · </span>"), "{html}");
    assert!(html.contains(">fuyubot_plugin.xiaoqvan.x</span>"), "{html}");
    let actions = html.find("class=\"actions\"").unwrap();
    let footer = html.find("class=\"footer\"").unwrap();
    assert!(actions < footer, "{html}");
    assert!(!html.contains("<img class=\"footer-avatar\""), "{html}");
    assert!(!html.contains("<svg class=\"footer-avatar\""), "{html}");
}

#[test]
fn card_footer_shows_bot_avatar() {
    let avatar = "https://t.me/i/userpic/320/BotFather.jpg";
    let html = card_html_with_footer(
        &post("正文"),
        &Config::default(),
        None,
        Some("@FuyuBot · fuyubot_plugin.xiaoqvan.x"),
        Some(avatar),
        None,
    );
    assert!(html.contains("class=\"footer-avatar\""), "{html}");
    assert!(html.contains(avatar), "{html}");
}

#[test]
fn card_footer_accepts_inline_avatar_svg() {
    let svg = "<svg class=\"footer-avatar\"></svg>";
    let html = card_html_with_footer(
        &post("正文"),
        &Config::default(),
        None,
        Some("@FuyuBot · fuyubot_plugin.xiaoqvan.x"),
        None,
        Some(svg),
    );
    assert!(html.contains(svg), "{html}");
}

#[test]
fn translation_header_follows_the_post_language() {
    let mut japanese = post("こんにちは");
    japanese.lang = "ja".to_owned();
    let html = card_html_with_translation(&japanese, &Config::default(), Some("你好"));
    assert!(html.contains("翻译自 日语"), "{html}");
    assert_eq!(language_name("ja-JP"), "日语");
    assert_eq!(language_name("en"), "英语");
}

#[test]
fn translated_card_keeps_original_for_comparison() {
    let mut english = post("hello");
    english.lang = "en".to_owned();
    let html = card_html_with_translation(&english, &Config::default(), Some("你好"));
    assert!(html.contains("翻译自 英语"), "{html}");
    assert!(html.contains("原文对照"), "{html}");
    assert!(html.contains(">hello<"), "{html}");
    assert!(html.contains("你好"), "{html}");
    assert!(!html.contains("显示翻译"), "{html}");
}

fn media(url: &str, kind: MediaKind, thumbnail: Option<&str>) -> PostMedia {
    PostMedia {
        kind,
        url: url.to_owned(),
        thumbnail: thumbnail.map(str::to_owned),
        gif: false,
        width: 1200,
        height: 800,
        duration: 0,
        alt: None,
    }
}

#[test]
fn card_embeds_at_most_two_images() {
    let mut post = post("正文");
    post.media = vec![
        media("https://pbs.twimg.com/a.jpg", MediaKind::Photo, None),
        media("https://pbs.twimg.com/b.jpg", MediaKind::Photo, None),
        media("https://pbs.twimg.com/c.jpg", MediaKind::Photo, None),
    ];
    let html = card_html(&post, &Config::default());
    assert!(html.contains("a.jpg"), "{html}");
    assert!(html.contains("b.jpg"));
    assert!(!html.contains("c.jpg"));
}

#[test]
fn card_marks_video_with_play_and_gif_with_badge() {
    let mut video_post = post("正文");
    video_post.media = vec![media(
        "https://video.twimg.com/x.mp4",
        MediaKind::Video,
        Some("https://pbs.twimg.com/cover.jpg"),
    )];
    let html = card_html(&video_post, &Config::default());
    assert!(html.contains("class=\"play\""), "{html}");

    let mut gif = media(
        "https://video.twimg.com/tweet_video/g.mp4",
        MediaKind::Photo,
        Some("https://pbs.twimg.com/gif-cover.jpg"),
    );
    gif.gif = true;
    let mut gif_post = post("正文");
    gif_post.media = vec![gif];
    let html = card_html(&gif_post, &Config::default());
    assert!(html.contains(">GIF</div>"), "{html}");
    assert!(html.contains("gif-cover.jpg"));
    assert!(!html.contains("class=\"play\""));

    // 没有封面的 GIF 不画空框。
    let mut bare = media(
        "https://video.twimg.com/tweet_video/bare.mp4",
        MediaKind::Photo,
        None,
    );
    bare.gif = true;
    let mut bare_post = post("正文");
    bare_post.media = vec![bare];
    let html = card_html(&bare_post, &Config::default());
    assert!(!html.contains("class=\"media\""), "{html}");
}

#[test]
fn card_uses_video_cover_and_link_only_caption() {
    let mut post = post("正文");
    post.media = vec![media(
        "https://video.twimg.com/x.mp4",
        MediaKind::Video,
        Some("https://pbs.twimg.com/cover.jpg"),
    )];
    let html = card_html(&post, &Config::default());
    assert!(html.contains("cover.jpg"), "{html}");
    assert!(!html.contains("x.mp4"));
    assert_eq!(card_caption(&post), "https://x.com/i/status/1");
}

#[test]
fn card_shows_avatar_badge_and_action_counts() {
    let mut post = post("正文");
    post.author.avatar_url = "https://pbs.twimg.com/a.jpg".to_owned();
    post.author.verified = true;
    post.metrics = crate::model::Metrics {
        replies: 113,
        retweets: 599,
        likes: 6546,
        bookmarks: 3,
        views: 280000,
    };
    let html = card_html(&post, &Config::default());
    assert!(html.contains("pbs.twimg.com/a.jpg"), "{html}");
    assert!(html.contains("class=\"badge\""));
    assert!(html.contains("113"));
    assert!(html.contains("599"));
    assert!(html.contains("6546"));
    assert!(html.contains("28万"));
}

#[test]
fn card_truncates_very_long_articles() {
    let config = Config {
        card_max_chars: 200,
        ..Config::default()
    };
    let html = card_html(&post(&"很长".repeat(2000)), &config);
    assert!(html.contains("完整内容见原链接"));
}

#[test]
fn card_escapes_user_text() {
    let config = Config::default();
    let html = card_html(&post("<script>alert(1)</script>"), &config);
    assert!(!html.contains("<script>alert"));
    assert!(html.contains("&lt;script&gt;"));
}

#[test]
fn block_fragments_map_to_telegram_html() {
    let heading = Block {
        kind: BlockKind::Heading(2),
        runs: vec![Run {
            text: "标题".to_owned(),
            bold: false,
            italic: false,
            code: false,
            link: None,
        }],
    };
    let fragment = fragment_of(&heading).expect("标题要能变成片段");
    assert_eq!(fragment.render(), "<b>标题</b>");

    let quote = Block {
        kind: BlockKind::Quote,
        runs: vec![Run {
            text: "引用".to_owned(),
            bold: false,
            italic: false,
            code: false,
            link: None,
        }],
    };
    assert_eq!(
        fragment_of(&quote).expect("引用要能变成片段").render(),
        "<blockquote>引用</blockquote>"
    );
}

#[test]
fn media_blocks_produce_no_text() {
    let media = Block {
        kind: BlockKind::Media(0),
        runs: Vec::new(),
    };
    assert!(fragment_of(&media).is_none());
}
