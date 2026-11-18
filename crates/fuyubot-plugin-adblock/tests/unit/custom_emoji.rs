use super::*;
use serde_json::json;

fn entity(id: i64, offset: usize, length: usize) -> serde_json::Value {
    json!({
        "@type": "textEntity",
        "offset": offset,
        "length": length,
        "type": { "@type": "textEntityTypeCustomEmoji", "custom_emoji_id": id },
    })
}

fn text_message(text: &str, entities: Vec<serde_json::Value>) -> serde_json::Value {
    json!({
        "@type": "messageText",
        "text": { "@type": "formattedText", "text": text, "entities": entities },
    })
}

#[test]
fn placeholders_follow_first_appearance_order() {
    // 「😀加😀」：UTF-16 单位 2 + 1 + 2 = 5
    let content = text_message(
        "\u{1F600}加\u{1F600}",
        vec![entity(11, 0, 2), entity(12, 3, 2)],
    );
    let report = report(&content).expect("should see custom emoji");
    assert_eq!(report.ids, vec![11, 12]);
    assert_eq!(report.count, 2);
    assert_eq!(report.opaque, "[E1]加[E2]");
    assert_eq!(report.plain_chars, 1);
    assert!(!report.is_glyph_art());
}

#[test]
fn repeated_emoji_shares_one_placeholder() {
    let content = text_message(
        "\u{1F600}\u{1F600}\u{1F600}",
        vec![entity(7, 0, 2), entity(7, 2, 2), entity(7, 4, 2)],
    );
    let report = report(&content).expect("should see custom emoji");
    assert_eq!(report.ids, vec![7]);
    assert_eq!(report.count, 3);
    assert_eq!(report.opaque, "[E1][E1][E1]");
    assert_eq!(report.plain_chars, 0);
    assert_eq!(report.max_run, 3);
    assert!(!report.is_glyph_art(), "3 个连排还不到 5 个的触发线");
}

#[test]
fn normal_sentence_with_three_emoji_is_not_glyph_art() {
    let text = "今天天气不错，我们一起出去玩吧顺便说一句这个真的很好吃";
    let letters = text.chars().filter(|c| c.is_alphanumeric()).count();
    let entities = vec![entity(1, 0, 2), entity(2, 2, 2), entity(3, 4, 2)];
    let content = text_message(&format!("\u{1F600}\u{1F600}\u{1F600}{text}"), entities);
    let report = report(&content).expect("should see custom emoji");
    assert_eq!(report.plain_chars, letters);
    assert!(!report.is_glyph_art());
}

#[test]
fn eight_in_a_row_with_text_is_not_triggered() {
    let mut entities = Vec::new();
    let mut text = String::new();
    for index in 0..8 {
        entities.push(entity(100 + index as i64, index * 2, 2));
        text.push('\u{1F600}');
    }
    text.push_str("这是一段足够长的正文，用来验证数量本身就能触发");
    let content = text_message(&text, entities);
    let report = report(&content).expect("should see custom emoji");
    assert_eq!(report.count, 8);
    assert_eq!(report.max_run, 8);
    assert!(
        !report.is_glyph_art(),
        "后面跟了正文，就不是「整条消息无文本」"
    );
}

#[test]
fn emoji_only_message_with_trailing_punctuation_triggers() {
    // 纯标点不算文本（plain_chars 只数字母 / 数字 / 汉字）。
    let content = text_message(
        "\u{1F600}\u{1F600}\u{1F600}\u{1F600}\u{1F600}！",
        vec![
            entity(1, 0, 2),
            entity(2, 2, 2),
            entity(3, 4, 2),
            entity(4, 6, 2),
            entity(5, 8, 2),
        ],
    );
    let report = report(&content).unwrap();
    assert_eq!(report.plain_chars, 0);
    assert_eq!(report.max_run, 5);
    assert!(report.is_glyph_art());
}

#[test]
fn five_in_a_row_triggers() {
    let content = text_message(
        "\u{1F600}\u{1F600}\u{1F600}\u{1F600}\u{1F600}",
        vec![
            entity(1, 0, 2),
            entity(2, 2, 2),
            entity(3, 4, 2),
            entity(4, 6, 2),
            entity(5, 8, 2),
        ],
    );
    let report = report(&content).unwrap();
    assert_eq!(report.max_run, 5);
    assert!(report.is_glyph_art());
}

#[test]
fn four_in_a_row_does_not_trigger() {
    let content = text_message(
        "\u{1F600}\u{1F600}\u{1F600}\u{1F600}",
        vec![
            entity(1, 0, 2),
            entity(2, 2, 2),
            entity(3, 4, 2),
            entity(4, 6, 2),
        ],
    );
    let report = report(&content).unwrap();
    assert_eq!(report.max_run, 4);
    assert!(!report.is_glyph_art(), "4 个是正常反应，不该花钱");
}

#[test]
fn whitespace_keeps_the_run() {
    let content = text_message(
        "\u{1F600} \u{1F600} \u{1F600} \u{1F600} \u{1F600}",
        vec![
            entity(1, 0, 2),
            entity(2, 3, 2),
            entity(3, 6, 2),
            entity(4, 9, 2),
            entity(5, 12, 2),
        ],
    );
    let report = report(&content).unwrap();
    assert_eq!(report.max_run, 5, "空格不算文本");
    assert!(report.is_glyph_art());
}

#[test]
fn punctuation_breaks_the_run() {
    let content = text_message(
        "\u{1F600}，\u{1F600}，\u{1F600}，\u{1F600}，\u{1F600}",
        vec![
            entity(1, 0, 2),
            entity(2, 3, 2),
            entity(3, 6, 2),
            entity(4, 9, 2),
            entity(5, 12, 2),
        ],
    );
    let report = report(&content).unwrap();
    assert_eq!(report.max_run, 1, "标点会把连排打断");
    assert!(!report.is_glyph_art());
}

#[test]
fn text_after_a_long_run_is_not_triggered() {
    let content = text_message(
        "\u{1F600}\u{1F600}\u{1F600}\u{1F600}\u{1F600}加微信",
        vec![
            entity(1, 0, 2),
            entity(2, 2, 2),
            entity(3, 4, 2),
            entity(4, 6, 2),
            entity(5, 8, 2),
        ],
    );
    let report = report(&content).unwrap();
    assert_eq!(report.max_run, 5);
    assert_eq!(report.plain_chars, 3);
    assert!(
        !report.is_glyph_art(),
        "有「加微信」三个字，就不满足「整条消息无文本」"
    );
}

#[test]
fn caption_entities_are_read() {
    let content = json!({
        "@type": "messagePhoto",
        "caption": {
            "@type": "formattedText",
            "text": "\u{1F600}\u{1F600}",
            "entities": [entity(5, 0, 2), entity(6, 2, 2)],
        },
    });
    let report = report(&content).expect("caption 里的 emoji 也算");
    assert_eq!(report.ids, vec![5, 6]);
}

#[test]
fn rich_message_emoji_are_collected() {
    let content = json!({
        "@type": "messageRichMessage",
        "message": {
            "@type": "richMessage",
            "blocks": [
                {"@type": "pageBlockParagraph", "text": {"@type": "richTexts", "texts": [
                    {"@type": "richTextPlain", "text": "加"},
                    {"@type": "richTextCustomEmoji", "custom_emoji_id": 9, "alternative_text": "😀"}
                ]}}
            ],
        },
    });
    let report = report(&content).expect("富消息里的自定义 emoji 也要认");
    assert_eq!(report.ids, vec![9]);
    assert!(report.opaque.ends_with("[E1]"));
}

#[test]
fn utf16_offsets_survive_emoji_and_cjk() {
    assert_eq!(byte_at("\u{1F600}x", 2), Some(4));
    assert_eq!(byte_at("\u{1F600}x", 0), Some(0));
    assert_eq!(byte_at("\u{1F600}x", 3), Some(5));
    assert_eq!(
        byte_at("\u{1F600}x", 4),
        None,
        "越界实体要能跳过而不是 panic"
    );
}

#[test]
fn tgs_uses_thumbnail_and_webp_uses_full_file() {
    let animated = json!({
        "id": 42i64,
        "set_id": 77i64,
        "emoji": "\u{1F600}",
        "format": "stickerFormatTgs",
        "full_type": {"@type": "stickerFullTypeCustomEmoji", "custom_emoji_id": 42i64, "needs_repainting": true},
        "thumbnail": {"@type": "thumbnail", "file": {"@type": "file", "id": 9001i64}},
        "sticker": {"@type": "file", "id": 9002i64},
    });
    let emoji = Emoji::from_sticker(&animated).expect("sticker 应该能解析");
    assert_eq!(emoji.format, "tgs");
    assert!(emoji.needs_repainting);
    assert_eq!(emoji.render_file(), Some(9001), "动图只能拿缩略图");

    let static_webp = json!({
        "id": 43i64,
        "set_id": 0i64,
        "format": "stickerFormatWebp",
        "thumbnail": {"@type": "thumbnail", "file": {"@type": "file", "id": 9101i64}},
        "sticker": {"@type": "file", "id": 9102i64},
    });
    let emoji = Emoji::from_sticker(&static_webp).unwrap();
    assert_eq!(emoji.render_file(), Some(9102), "静态图直接解原图");
}

#[test]
fn compose_builds_two_column_sheet() {
    let dir = std::env::temp_dir().join("fuyubot-adblock-emoji-test");
    std::fs::create_dir_all(&dir).unwrap();
    let mut tiles = Vec::new();
    for index in 0..3i64 {
        let path = dir.join(format!("tile-{index}.png"));
        image::RgbaImage::from_pixel(32, 32, image::Rgba([200, 40, 40, 255]))
            .save(&path)
            .unwrap();
        tiles.push(Tile {
            index: index as usize + 1,
            path,
            emoji: Emoji {
                id: index,
                needs_repainting: false,
                ..Emoji::default()
            },
        });
    }
    let png = compose(&tiles).expect("应该拼出一张图");
    let sheet = image::load_from_memory(&png).expect("输出必须是合法 PNG");
    let (width, height) = (sheet.width(), sheet.height());
    assert_eq!(width, 2 * TILE + 3 * GAP);
    assert_eq!(height, 2 * TILE + 3 * GAP, "3 张图排成 2 行");

    // 白底：左上角的留白像素必须是底色。
    let rgba = sheet.to_rgba8();
    let pixel = rgba.get_pixel(0, 0).0;
    assert_eq!(&pixel[0..3], &BACKGROUND);
}

#[test]
fn compose_without_readable_tiles_returns_none() {
    let missing = Tile {
        index: 1,
        path: std::path::PathBuf::from("不存在的文件.png"),
        emoji: Emoji::default(),
    };
    assert!(compose(&[missing]).is_none());
}
