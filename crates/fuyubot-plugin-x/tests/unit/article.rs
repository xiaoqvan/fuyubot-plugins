use super::*;
use serde_json::{Value, json};

fn image(media_id: &str) -> Value {
    json!({
        "id": media_id,
        "media_id": media_id,
        "media_info": {
            "__typename": "ApiImage",
            "original_img_url": format!("https://pbs.twimg.com/media/{media_id}.jpg?name=orig"),
            "original_img_width": 1200,
            "original_img_height": 800
        }
    })
}

fn content(blocks: Value, entities: Value) -> Value {
    json!({ "blocks": blocks, "entityMap": entities })
}

fn media_entity(key: u64, media_id: &str) -> Value {
    json!({
        "key": key,
        "value": {
            "type": "MEDIA",
            "mutability": "Immutable",
            "data": {
                "entityKey": format!("entity-{key}"),
                "mediaItems": [
                    { "localMediaId": media_id, "mediaCategory": "DraftTweetImage", "mediaId": media_id }
                ]
            }
        }
    })
}

fn atomic(key: u64) -> Value {
    json!({
        "type": "atomic",
        "text": " ",
        "entityRanges": [ { "key": key, "offset": 0, "length": 1 } ],
        "inlineStyleRanges": [],
        "data": {}
    })
}

fn paragraph(text: &str) -> Value {
    json!({
        "type": "unstyled",
        "text": text,
        "entityRanges": [],
        "inlineStyleRanges": [],
        "data": {}
    })
}

#[test]
fn media_follow_the_document_order_not_the_entities_array() {
    // entityMap 里 key 1 在前、key 0 在后；正文里 0 先出现。
    let blocks = json!([paragraph("开头"), atomic(0), atomic(1)]);
    let entities = json!([media_entity(1, "200"), media_entity(0, "100")]);
    let store = vec![image("200"), image("100")];

    let (blocks, media) = parse_content(&content(blocks, entities), &store);

    assert_eq!(media.len(), 2);
    assert!(
        media[0].url.contains("100"),
        "第一张应该是正文里先出现的 100"
    );
    assert!(media[1].url.contains("200"));
    let indexes: Vec<usize> = blocks
        .iter()
        .filter_map(|block| match block.kind {
            BlockKind::Media(index) => Some(index),
            _ => None,
        })
        .collect();
    assert_eq!(indexes, vec![0, 1]);
}

#[test]
fn unreferenced_media_entities_are_appended_at_the_end() {
    let blocks = json!([paragraph("只有文字")]);
    let store = vec![image("300")];
    let (_, media) = parse_content(&content(blocks, json!([])), &store);
    assert_eq!(media.len(), 1);
    assert!(media[0].url.contains("300"));
}

#[test]
fn headings_and_ordered_list_indexes() {
    let blocks = json!([
        { "type": "header-two", "text": "小标题", "entityRanges": [], "inlineStyleRanges": [] },
        { "type": "ordered-list-item", "text": "第一条", "entityRanges": [], "inlineStyleRanges": [] },
        { "type": "ordered-list-item", "text": "第二条", "entityRanges": [], "inlineStyleRanges": [] },
        paragraph("普通段落"),
        { "type": "ordered-list-item", "text": "重新计数", "entityRanges": [], "inlineStyleRanges": [] }
    ]);
    let (blocks, _) = parse_content(&content(blocks, json!([])), &[]);

    assert_eq!(blocks[0].kind, BlockKind::Heading(2));
    assert_eq!(
        blocks[1].kind,
        BlockKind::Item {
            ordered: true,
            index: 1
        }
    );
    assert_eq!(
        blocks[2].kind,
        BlockKind::Item {
            ordered: true,
            index: 2
        }
    );
    assert_eq!(blocks[3].kind, BlockKind::Paragraph);
    // 段落打断了列表，序号重新从 1 开始。
    assert_eq!(
        blocks[4].kind,
        BlockKind::Item {
            ordered: true,
            index: 1
        }
    );
}

#[test]
fn quote_and_code_blocks_keep_their_kind() {
    let blocks = json!([
        { "type": "blockquote", "text": "引用", "entityRanges": [], "inlineStyleRanges": [] },
        { "type": "code-block", "text": "let x = 1;", "entityRanges": [], "inlineStyleRanges": [] }
    ]);
    let (blocks, _) = parse_content(&content(blocks, json!([])), &[]);
    assert_eq!(blocks[0].kind, BlockKind::Quote);
    assert_eq!(blocks[1].kind, BlockKind::Code);
}

#[test]
fn inline_styles_use_utf16_offsets() {
    // emoji 占 2 个 UTF-16 码元：Bold 覆盖 0..2，Italic 覆盖 2..5。
    let blocks = json!([{
        "type": "unstyled",
        "text": "😀abc",
        "entityRanges": [],
        "inlineStyleRanges": [
            { "offset": 0, "length": 2, "style": "Bold" },
            { "offset": 2, "length": 3, "style": "Italic" }
        ]
    }]);
    let (blocks, _) = parse_content(&content(blocks, json!([])), &[]);
    let runs = &blocks[0].runs;

    assert_eq!(runs.len(), 2);
    assert_eq!(runs[0].text, "😀");
    assert!(runs[0].bold && !runs[0].italic);
    assert_eq!(runs[1].text, "abc");
    assert!(runs[1].italic && !runs[1].bold);
}

#[test]
fn adjacent_runs_with_the_same_style_are_merged() {
    let blocks = json!([{
        "type": "unstyled",
        "text": "加粗",
        "entityRanges": [],
        "inlineStyleRanges": [
            { "offset": 0, "length": 1, "style": "Bold" },
            { "offset": 1, "length": 1, "style": "Bold" }
        ]
    }]);
    let (blocks, _) = parse_content(&content(blocks, json!([])), &[]);
    assert_eq!(blocks[0].runs.len(), 1);
    assert_eq!(blocks[0].runs[0].text, "加粗");
    assert!(blocks[0].runs[0].bold);
}

#[test]
fn mention_spans_become_profile_links() {
    let blocks = json!([{
        "type": "unstyled",
        "text": "谢谢 @alice",
        "entityRanges": [],
        "inlineStyleRanges": [],
        "data": {
            "mentions": [
                { "fromIndex": 3, "toIndex": 9, "text": "alice" }
            ]
        }
    }]);
    let (blocks, _) = parse_content(&content(blocks, json!([])), &[]);
    let linked: Vec<&Run> = blocks[0]
        .runs
        .iter()
        .filter(|run| run.link.is_some())
        .collect();
    assert_eq!(linked.len(), 1);
    assert_eq!(linked[0].text, "@alice");
    assert_eq!(linked[0].link.as_deref(), Some("https://x.com/alice"));
}

#[test]
fn empty_blocks_are_dropped() {
    let blocks = json!([paragraph("   "), paragraph("真正的内容")]);
    let (blocks, _) = parse_content(&content(blocks, json!([])), &[]);
    assert_eq!(blocks.len(), 1);
    assert_eq!(blocks[0].runs[0].text, "真正的内容");
}

#[test]
fn embedded_tweets_become_their_own_block() {
    let blocks = json!([atomic(7)]);
    let entities = json!([{
        "key": 7,
        "value": {
            "type": "TWEET",
            "mutability": "Immutable",
            "data": { "entityKey": "tweet-entity", "tweetId": "2007184284944322584" }
        }
    }]);
    let (blocks, media) = parse_content(&content(blocks, entities), &[]);
    assert!(media.is_empty());
    assert_eq!(
        blocks[0].kind,
        BlockKind::Tweet("2007184284944322584".to_owned())
    );
}
