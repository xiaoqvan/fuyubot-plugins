//! X 文章正文：Draft.js 风格的 blocks + entityMap → 结构化的块。
//!
//! X 的 Article 正文不是 HTML，而是一份 Draft.js 文档：
//!
//! - content.blocks[]：每个块有 type（unstyled / header-two / atomic …）、text、
//!   inlineStyleRanges（Bold / Italic / Code）和 entityRanges（指向 entityMap 的 key）；
//! - content.entityMap[]：MEDIA（图 / 视频）、TWEET（嵌入推文）、MARKDOWN；
//! - media_entities[]：正文媒体的本体（media_id + media_info）。
//!
//! 这个模块把这三样合起来，产出「按文档顺序排好的块」和「按文档顺序排好的媒体」。
//! 顺序是重点：相册里的图片必须跟文章里的先后一致，不能按 JSON 里的偶然顺序。
//!
//! ## 为什么偏移量按 UTF-16 处理
//!
//! Draft.js 在 JS 里跑，offset / length 全是 **UTF-16 码元**。X 上的文章里有大量
//! emoji（占 2 个码元），拿 Rust 的 char 下标去切会错位——轻则样式串行，重则把
//! 一个 emoji 撕成两半。所以这里先把文本展开成码元、在码元上打标记，最后按字符
//! 逐个取出它的起始码元，组合成 run。

use std::collections::HashMap;

use serde_json::Value;

use crate::model::{self, Media};

/// 一段同格式的文本。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Run {
    pub text: String,
    pub bold: bool,
    pub italic: bool,
    pub code: bool,
    /// 行内链接（@提及 / URL / #话题 / $代码）。
    pub link: Option<String>,
}

impl Run {
    fn plain(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            bold: false,
            italic: false,
            code: false,
            link: None,
        }
    }

    pub(crate) fn same_style(&self, other: &Self) -> bool {
        self.bold == other.bold
            && self.italic == other.italic
            && self.code == other.code
            && self.link == other.link
    }
}

/// 一个块的语义。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BlockKind {
    Paragraph,
    /// 标题，1..=6（header-one … header-six）。
    Heading(u8),
    /// 列表项；ordered 决定画圆点还是序号。
    Item {
        ordered: bool,
        index: usize,
    },
    Quote,
    Code,
    /// 正文里的一张图 / 一个视频：索引进返回的媒体表。
    Media(usize),
    /// 嵌入的推文：推文 id。
    Tweet(String),
}

/// 一个块 = 语义 + 行内 run。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Block {
    pub kind: BlockKind,
    pub runs: Vec<Run>,
}

/// entityMap 里的一次引用。
#[derive(Debug, Clone, PartialEq, Eq)]
enum Entity {
    /// 媒体本体 id（对应 media_entities[].media_id）。
    Media(Vec<String>),
    /// 嵌入推文的 id。
    Tweet(String),
    /// 一段 markdown（X 用它塞外链卡片之类）。
    Markdown(String),
}

/// 正文解析结果：块 + 按文档顺序排好的媒体。
///
/// 媒体表的索引就是 BlockKind::Media 里那个数字；末尾可能多出几个没被任何块引用
/// 的媒体（media_entities 里有、正文里没放的），它们也会被发出去。
pub fn parse_content(content: &Value, media_entities: &[Value]) -> (Vec<Block>, Vec<Media>) {
    let entities = entities_of(content);
    let blocks_json = content
        .get("blocks")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    let mut blocks: Vec<Block> = Vec::new();
    let mut media: Vec<Media> = Vec::new();
    let mut by_id: HashMap<String, usize> = HashMap::new();
    // 有序列表的序号：只在连续的 ordered-list-item 之间累加。
    let mut ordered_index = 0usize;

    for block in &blocks_json {
        let kind = block
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("unstyled");
        if kind == "atomic" {
            ordered_index = 0;
            for key in entity_keys(block) {
                match entities.get(&key) {
                    Some(Entity::Media(ids)) => {
                        for id in ids {
                            let index = media_index_of(id, media_entities, &mut media, &mut by_id);
                            if let Some(index) = index {
                                blocks.push(Block {
                                    kind: BlockKind::Media(index),
                                    runs: Vec::new(),
                                });
                            }
                        }
                    }
                    Some(Entity::Tweet(tweet_id)) => blocks.push(Block {
                        kind: BlockKind::Tweet(tweet_id.clone()),
                        runs: Vec::new(),
                    }),
                    Some(Entity::Markdown(text)) if !text.trim().is_empty() => {
                        blocks.push(Block {
                            kind: BlockKind::Paragraph,
                            runs: vec![Run::plain(text.clone())],
                        });
                    }
                    _ => {}
                }
            }
            continue;
        }

        let runs = runs_of(block);
        if runs.iter().all(|run| run.text.trim().is_empty()) {
            continue;
        }

        let block_kind = if let Some(level) = heading_level(kind) {
            ordered_index = 0;
            BlockKind::Heading(level)
        } else {
            match kind {
                "unordered-list-item" => {
                    ordered_index = 0;
                    BlockKind::Item {
                        ordered: false,
                        index: 0,
                    }
                }
                "ordered-list-item" => {
                    ordered_index += 1;
                    BlockKind::Item {
                        ordered: true,
                        index: ordered_index,
                    }
                }
                "blockquote" => {
                    ordered_index = 0;
                    BlockKind::Quote
                }
                "code-block" => {
                    ordered_index = 0;
                    BlockKind::Code
                }
                _ => {
                    ordered_index = 0;
                    BlockKind::Paragraph
                }
            }
        };
        blocks.push(Block {
            kind: block_kind,
            runs,
        });
    }

    // media_entities 里有、正文没引用的媒体：排在最后，不丢。
    for node in media_entities {
        let Some(id) = media_id_of(node) else {
            continue;
        };
        if by_id.contains_key(id) {
            continue;
        }
        if let Some(media_node) = model::from_article_media_node(node) {
            by_id.insert(id.to_owned(), media.len());
            media.push(media_node);
        }
    }

    (blocks, media)
}

/// 一个块引用了哪些 entityMap key（按 entityRanges 的先后）。
fn entity_keys(block: &Value) -> Vec<String> {
    block
        .get("entityRanges")
        .and_then(Value::as_array)
        .map(|ranges| {
            ranges
                .iter()
                .filter_map(|range| {
                    range.get("key").map(|key| match key {
                        Value::Number(number) => number.to_string(),
                        Value::String(text) => text.clone(),
                        _ => String::new(),
                    })
                })
                .filter(|key| !key.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

/// entityMap：key → 语义。
fn entities_of(content: &Value) -> HashMap<String, Entity> {
    let mut map = HashMap::new();
    let Some(entries) = content.get("entityMap").and_then(Value::as_array) else {
        return map;
    };
    for entry in entries {
        let Some(key) = entry.get("key").map(|key| match key {
            Value::Number(number) => number.to_string(),
            Value::String(text) => text.clone(),
            _ => String::new(),
        }) else {
            continue;
        };
        if key.is_empty() {
            continue;
        }
        if let Some(entity) = entity_of(entry.get("value").unwrap_or(&Value::Null)) {
            map.insert(key, entity);
        }
    }
    map
}

fn entity_of(value: &Value) -> Option<Entity> {
    let kind = value.get("type").and_then(Value::as_str)?;
    let data = value.get("data").unwrap_or(&Value::Null);
    match kind {
        "MEDIA" => {
            let ids: Vec<String> = data
                .get("mediaItems")
                .and_then(Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .filter_map(|item| item.get("mediaId").and_then(Value::as_str))
                        .map(str::to_owned)
                        .collect()
                })
                .unwrap_or_default();
            Some(Entity::Media(ids))
        }
        "TWEET" => data
            .get("tweetId")
            .and_then(Value::as_str)
            .map(|id| Entity::Tweet(id.to_owned())),
        "MARKDOWN" => Some(Entity::Markdown(
            data.get("markdown")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned(),
        )),
        _ => None,
    }
}

/// 媒体 id → 媒体表下标；第一次见就解析出来追加到表尾。
fn media_index_of(
    id: &str,
    media_entities: &[Value],
    media: &mut Vec<Media>,
    by_id: &mut HashMap<String, usize>,
) -> Option<usize> {
    if let Some(index) = by_id.get(id) {
        return Some(*index);
    }
    let node = media_entities
        .iter()
        .find(|node| media_id_of(node) == Some(id))?;
    let parsed = model::from_article_media_node(node)?;
    let index = media.len();
    by_id.insert(id.to_owned(), index);
    media.push(parsed);
    Some(index)
}

fn media_id_of(node: &Value) -> Option<&str> {
    node.get("media_id")
        .and_then(Value::as_str)
        .or_else(|| node.get("id").and_then(Value::as_str))
}

fn heading_level(kind: &str) -> Option<u8> {
    let rest = kind.strip_prefix("header-")?;
    let level = match rest {
        "one" => 1,
        "two" => 2,
        "three" => 3,
        "four" => 4,
        "five" => 5,
        "six" => 6,
        _ => return None,
    };
    Some(level)
}

/// 一个块的文本 → 带样式的 run。
fn runs_of(block: &Value) -> Vec<Run> {
    let text = block.get("text").and_then(Value::as_str).unwrap_or("");
    if text.is_empty() {
        return Vec::new();
    }
    let units = text.encode_utf16().count();
    let mut bold = vec![false; units];
    let mut italic = vec![false; units];
    let mut code = vec![false; units];

    for style in block
        .get("inlineStyleRanges")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let offset = style.get("offset").and_then(Value::as_u64).unwrap_or(0) as usize;
        let length = style.get("length").and_then(Value::as_u64).unwrap_or(0) as usize;
        let name = style
            .get("style")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_ascii_lowercase();
        let end = (offset + length).min(units);
        for slot in offset..end {
            match name.as_str() {
                "bold" => bold[slot] = true,
                "italic" => italic[slot] = true,
                "code" => code[slot] = true,
                _ => {}
            }
        }
    }

    let links = links_of(block, units);

    let mut runs: Vec<Run> = Vec::new();
    let mut offset = 0usize;
    for ch in text.chars() {
        if offset >= units {
            break;
        }
        let run = Run {
            text: ch.to_string(),
            bold: bold[offset],
            italic: italic[offset],
            code: code[offset],
            link: links[offset].clone(),
        };
        offset += ch.len_utf16();
        match runs.last_mut() {
            Some(last) if last.same_style(&run) => last.text.push(ch),
            _ => runs.push(run),
        }
    }
    runs
}

/// 行内链接：block.data 里的 mentions / urls / hashtags / cashtags。
///
/// 它们的 fromIndex / toIndex 与 Draft.js 的样式偏移一样，是 UTF-16 码元。
fn links_of(block: &Value, units: usize) -> Vec<Option<String>> {
    let mut links: Vec<Option<String>> = vec![None; units];
    let Some(data) = block.get("data") else {
        return links;
    };
    for (key, kind) in [
        ("mentions", "mention"),
        ("urls", "url"),
        ("hashtags", "hashtag"),
        ("cashtags", "cashtag"),
    ] {
        let Some(items) = data.get(key).and_then(Value::as_array) else {
            continue;
        };
        for item in items {
            let from = item.get("fromIndex").and_then(Value::as_u64).unwrap_or(0) as usize;
            let to = item.get("toIndex").and_then(Value::as_u64).unwrap_or(0) as usize;
            let text = item.get("text").and_then(Value::as_str).unwrap_or("");
            if text.is_empty() {
                continue;
            }
            let href = match kind {
                "mention" => format!("https://x.com/{text}"),
                "hashtag" => format!("https://x.com/hashtag/{text}"),
                "cashtag" => format!("https://x.com/search?q=%24{text}"),
                _ => text.to_owned(),
            };
            for link in links.iter_mut().take(to.min(units)).skip(from) {
                if link.is_none() {
                    *link = Some(href.clone());
                }
            }
        }
    }
    links
}

#[cfg(test)]
#[path = "../tests/unit/article.rs"]
mod tests;
