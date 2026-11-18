//! 自定义 emoji（custom emoji）广告：解析 → 出图 → 交 AI → 拉黑。
//!
//! ## 为什么需要这一层
//!
//! Telegram 的自定义 emoji 在消息里**不是一个字符，而是一个实体**：
//! messageText.text 是 formattedText { text, entities }，其中
//! textEntityTypeCustomEmoji { custom_emoji_id } 指着一张（通常来自某个
//! 「自定义 emoji 集」的）贴纸。text 里留着的是**兜底字符**（发的人选的那个普通
//! emoji，比如 😀），所以：
//!
//! * 关键词检测看到的是 😀😀😀😀，一个字都匹配不上；
//! * AI 看到的也是 😀😀😀😀，它更没得判。
//!
//! 而广告方正是靠这个：**用自定义 emoji 当字**拼出「加微信 / 接单 / 代理」这类图案，
//! 或者干脆把整条广告画进一组 emoji 里。可用的对抗手段只剩「把它渲染成图」。
//!
//! ## 这一层的四步
//!
//! 1. [report]：从 formattedText.entities 里把 custom_emoji_id 抽出来，同时算出最长的
//!    **连排段**（[Report::max_run]：中间只有空白、没有文本）。**整条消息没有文字、
//!    且连续 5 个以上**才当成「用 emoji 当字」——这条既是判据，也是要不要花 AI 钱的闸门。
//! 2. [resolve]：getCustomEmojiStickers 一次问清 id → 贴纸（格式 / 集合 / 缩略图）。
//!    结果进程内缓存，同一条广告刷屏不会反复问服务器。
//! 3. [compose]：把每个 emoji 画成一张**联系表**（白色底、按阅读顺序、每行两张），
//!    再写成一个 PNG。**不做像素级还原**：真要还原整条消息得自带中文字体、处理换行
//!    与 RTL，代价远大于收益；而「文本里 [E1] 这种占位符 + emoji 联系表」已经足够
//!    让模型把图案读出来（见 ai/prompts/media/customEmoji.md）。
//! 4. [blacklist]：拉黑**集合**（复用 sticker_set_blacklist，自定义 emoji 集就是
//!    贴纸集）与**单个 emoji**（新集合 custom_emoji_blacklist）。
//!
//! ## 动图怎么取帧
//!
//! | sticker.format | 实际文件 | 这里怎么画 |
//! | --- | --- | --- |
//! | stickerFormatWebp | 静态 WebP | 原图直接用（无损、最清楚） |
//! | stickerFormatTgs | gzip 的 Lottie JSON | **用服务器的静态缩略图**（thumbnail），不解码 |
//! | stickerFormatWebm | VP9 视频 | 同上，用缩略图 |
//!
//! Lottie 与 VP9 解码都要额外的 C/C++ 依赖（rlottie / libvpx），对一个跨平台 cdylib
//! 插件来说不划算；而 TDLib 的 sticker.thumbnail 本来就是服务器给的**一帧静态图**，
//! 拿它做联系表足够了。真正需要「中间帧」的是**动图 WebP**（部分视频贴纸的缩略图就是
//! 动画 WebP）——这种用纯 Rust 的 image-webp 解出所有帧，取时长中点那一帧，
//! 见 [webp_middle_frame]。

use std::collections::HashMap;
use std::io::Cursor;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

use serde_json::{Value, json};

use crate::ai::{CustomEmoji, ModerationInput};
use crate::api::Td;
use crate::runtime::Runtime;

/// 触发线：**整条消息没有文字**、且**连续**这么多个自定义 emoji（中间也不夹文本），
/// 才当成「用 emoji 当字」。
///
/// 3~4 个多半是正常反应（😂😂😂😂），5 个连排才开始像图案；真正决定处置的仍是 AI。
/// 两个条件都写死在 [Report::is_glyph_art] 里，后续要放宽（例如允许「图案 + 一句话」）
/// 就改那一个函数。
pub const MIN_RUN: usize = 5;
/// 联系表最多贴几张：控制下载量、请求体大小与视觉 token。
pub const MAX_TILES: usize = 12;
/// 贴图边长（px）。160 已经够模型看清字形，整张图也不会超过 1MB。
const TILE: u32 = 160;
/// 每行几个。**顺序由提示词说明**（行优先），不靠排版暗示。
const COLUMNS: u32 = 2;
/// 贴图之间的留白。
const GAP: u32 = 6;
/// 底色：白。深色底会把「本来就是黑」的自绘 emoji 吃掉。
const BACKGROUND: [u8; 3] = [255, 255, 255];
/// 需要按文字色重绘的单色 emoji 在新底色上用的颜色。
const REPAINT: [u8; 3] = [17, 17, 17];
/// 缓存上限（id → 贴纸、set_id → 集合信息）。
const CACHE_LIMIT: usize = 2048;

// ── 解析 ──────────────────────────────────────────────────────────

/// 一个自定义 emoji 实体（offset / length 是 **UTF-16** 单位，与 TDLib 一致）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub id: i64,
    pub offset: usize,
    pub length: usize,
}

/// 一条消息里的自定义 emoji 使用情况。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Report {
    /// 原文（含兜底字符）。
    pub text: String,
    /// 把每个 emoji 换成 [E1] / [E2]… 之后的文本：交给 AI 的就是它。
    pub opaque: String,
    /// 出现的 id（**去重**，按首次出现顺序；[En] 的 n 就是这里的下标 + 1）。
    pub ids: Vec<i64>,
    /// 出现次数（含重复）。
    pub count: usize,
    /// 去掉 emoji 实体之后剩下的**字母 / 数字 / 汉字**个数。
    pub plain_chars: usize,
    /// 最长的一段「连排 emoji」：中间只有空白 / 零宽连接符，没有夹任何文本。
    pub max_run: usize,
}

impl Report {
    /// 「用 emoji 当字」：**整条消息没有任何文本**，且**连续 >= MIN_RUN 个**自定义 emoji。
    ///
    /// 两个条件都写死在这里，故意不做成配置：
    ///
    /// * 只看连排 → 「😂😂😂😂😂 笑死」这种正常回复也会被送进视觉审核（白花钱）；
    /// * 只看无文本 → 「😀😀😀 加微信」这种「图案 + 文案」的组合反而钻空子。
    ///
    /// 注意 plain_chars 只数**字母 / 数字 / 汉字**：纯标点不算文本，但标点出现在两个
    /// emoji 之间会打断连排（见 split_text）。这里只是**要不要花钱**的闸门
    /// （下载 + 视觉 token），真正的判定仍然交给模型。
    pub fn is_glyph_art(&self) -> bool {
        self.plain_chars == 0 && self.max_run >= MIN_RUN
    }
}

/// 从消息内容里抽自定义 emoji（messageText / 各种 caption / 富消息）。
pub fn report(content: &Value) -> Option<Report> {
    match content.get("@type").and_then(Value::as_str)? {
        "messageText" => report_of_formatted(content.get("text")),
        "messagePhoto" | "messageVideo" | "messageAnimation" | "messageDocument"
        | "messageAudio" | "messageVoiceNote" | "messageVideoNote" => {
            report_of_formatted(content.get("caption"))
        }
        "messageRichMessage" => report_of_rich(content),
        _ => None,
    }
}

fn report_of_formatted(formatted: Option<&Value>) -> Option<Report> {
    let formatted = formatted?;
    let text = formatted.get("text").and_then(Value::as_str).unwrap_or("");
    let entities = formatted
        .get("entities")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();

    let mut spans: Vec<Span> = Vec::new();
    for entity in entities {
        let kind = entity
            .get("type")
            .and_then(|kind| kind.get("@type"))
            .and_then(Value::as_str)
            .unwrap_or("");
        if kind != "textEntityTypeCustomEmoji" {
            continue;
        }
        let Some(id) = entity
            .get("type")
            .and_then(|kind| kind.get("custom_emoji_id"))
            .and_then(Value::as_i64)
        else {
            continue;
        };
        spans.push(Span {
            id,
            offset: entity
                .get("offset")
                .and_then(Value::as_i64)
                .unwrap_or(0)
                .max(0) as usize,
            length: entity
                .get("length")
                .and_then(Value::as_i64)
                .unwrap_or(0)
                .max(0) as usize,
        });
    }
    if spans.is_empty() {
        return None;
    }
    spans.sort_by_key(|span| span.offset);

    let (opaque, plain_chars, max_run) = split_text(text, &spans);
    let mut ids: Vec<i64> = Vec::new();
    for span in &spans {
        if !ids.contains(&span.id) {
            ids.push(span.id);
        }
    }
    Some(Report {
        text: text.to_owned(),
        opaque,
        ids,
        count: spans.len(),
        plain_chars,
        max_run,
    })
}

/// 富消息：richTextCustomEmoji 节点只有 custom_emoji_id 与 alternative_text，
/// 没有 offset。按文档顺序遍历：emoji 记连排，夹了文本就断段。
fn report_of_rich(content: &Value) -> Option<Report> {
    let mut ids: Vec<i64> = Vec::new();
    let mut plain = String::new();
    let mut run = 0usize;
    let mut max_run = 0usize;
    walk_rich(content, &mut ids, &mut plain, &mut run, &mut max_run);
    if ids.is_empty() {
        return None;
    }
    let plain_chars = plain.chars().filter(|c| c.is_alphanumeric()).count();
    let mut opaque = plain.clone();
    for index in 1..=ids.len() {
        opaque.push_str(&format!("[E{index}]"));
    }
    Some(Report {
        text: plain,
        opaque,
        count: ids.len(),
        ids,
        plain_chars,
        max_run,
    })
}

fn walk_rich(
    value: &Value,
    ids: &mut Vec<i64>,
    text: &mut String,
    run: &mut usize,
    max_run: &mut usize,
) {
    match value {
        Value::Object(map) => {
            if map.get("@type").and_then(Value::as_str) == Some("richTextCustomEmoji") {
                if let Some(id) = map.get("custom_emoji_id").and_then(Value::as_i64) {
                    ids.push(id);
                }
                *run += 1;
                *max_run = (*max_run).max(*run);
                return;
            }
            if let Some(child) = map.get("text") {
                match child {
                    Value::String(chunk) => {
                        text.push_str(chunk);
                        if chunk.chars().any(|ch| !is_ignorable(ch)) {
                            *run = 0;
                        }
                        return;
                    }
                    other => {
                        walk_rich(other, ids, text, run, max_run);
                        return;
                    }
                }
            }
            for (key, child) in map {
                // alternative_text 是 emoji 的替代文本，不是消息正文。
                if key == "alternative_text" {
                    continue;
                }
                walk_rich(child, ids, text, run, max_run);
            }
        }
        Value::Array(items) => {
            for child in items {
                walk_rich(child, ids, text, run, max_run);
            }
        }
        _ => {}
    }
}

/// UTF-16 偏移 → 字节偏移。越界（实体坏数据）返回 None，调用方跳过这一条。
fn byte_at(text: &str, utf16_offset: usize) -> Option<usize> {
    if utf16_offset == 0 {
        return Some(0);
    }
    let mut units = 0usize;
    for (byte, ch) in text.char_indices() {
        if units == utf16_offset {
            return Some(byte);
        }
        units += ch.len_utf16();
        if units > utf16_offset {
            return None;
        }
    }
    (units == utf16_offset).then_some(text.len())
}

/// 原文 → 占位文本 + 「正经字符」个数 + 最长连排段。
fn split_text(text: &str, spans: &[Span]) -> (String, usize, usize) {
    let mut opaque = String::with_capacity(text.len() + spans.len() * 3);
    let mut plain_chars = 0usize;
    let mut cursor = 0usize;
    let mut seen: Vec<i64> = Vec::new();
    let mut run = 0usize;
    let mut max_run = 0usize;

    for span in spans {
        let (Some(start), Some(end)) = (
            byte_at(text, span.offset),
            byte_at(text, span.offset.saturating_add(span.length)),
        ) else {
            continue;
        };
        if start < cursor || end > text.len() {
            continue;
        }
        let between = &text[cursor..start];
        plain_chars += between.chars().filter(|c| c.is_alphanumeric()).count();
        // 两个 emoji 之间只有空白 / 零宽连接符才算连排；夹了任何文字就断段。
        if between.chars().any(|ch| !is_ignorable(ch)) {
            run = 0;
        }
        opaque.push_str(between);
        let index = match seen.iter().position(|id| *id == span.id) {
            Some(index) => index + 1,
            None => {
                seen.push(span.id);
                seen.len()
            }
        };
        opaque.push_str(&format!("[E{index}]"));
        run += 1;
        max_run = max_run.max(run);
        cursor = end;
    }
    let rest = &text[cursor..];
    plain_chars += rest.chars().filter(|c| c.is_alphanumeric()).count();
    opaque.push_str(rest);
    (opaque, plain_chars, max_run)
}

/// 不打断连排的字符：空白、零宽连接符、变体选择符。
fn is_ignorable(ch: char) -> bool {
    ch.is_whitespace() || ch == '\u{200d}' || ch == '\u{fe0f}'
}

// ── 贴纸 ──────────────────────────────────────────────────────────

/// 一个自定义 emoji 对应的贴纸（只留渲染要用的字段）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Emoji {
    /// sticker.id，也就是 custom_emoji_id。
    pub id: i64,
    /// 所属集合（sticker.set_id）。自定义 emoji 集就是贴纸集，0 表示不在集合里。
    pub set_id: i64,
    /// 这张贴纸「代表」的普通 emoji（兜底字符）。
    pub emoji: String,
    /// webp / tgs / webm（去掉 stickerFormat 前缀）。
    pub format: String,
    /// 单色 emoji：客户端会按文字色重绘，画到白底前得先染成深色。
    pub needs_repainting: bool,
    /// 静态缩略图（thumbnail.file.id）：TGS / WEBM 唯一能直接拿到的位图。
    pub thumbnail_file: Option<i64>,
    /// 完整贴纸文件（sticker.file.id）。
    pub full_file: Option<i64>,
}

impl Emoji {
    fn from_sticker(sticker: &Value) -> Option<Self> {
        let id = sticker.get("id").and_then(Value::as_i64)?;
        let full_type = sticker.get("full_type").unwrap_or(&Value::Null);
        let needs_repainting = full_type
            .get("needs_repainting")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        Some(Self {
            id,
            set_id: sticker.get("set_id").and_then(Value::as_i64).unwrap_or(0),
            emoji: sticker
                .get("emoji")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned(),
            format: sticker
                .get("format")
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim_start_matches("stickerFormat")
                .to_ascii_lowercase(),
            needs_repainting,
            thumbnail_file: sticker
                .pointer("/thumbnail/file/id")
                .and_then(Value::as_i64),
            full_file: sticker.pointer("/sticker/id").and_then(Value::as_i64),
        })
    }

    /// 画它用哪个文件：静态 WebP 用原图，其余只能靠缩略图。
    pub fn render_file(&self) -> Option<i64> {
        if self.format == "webp"
            && let Some(full) = self.full_file
        {
            return Some(full);
        }
        self.thumbnail_file.or(self.full_file)
    }
}

fn emoji_cache() -> &'static Mutex<HashMap<i64, Emoji>> {
    static CACHE: OnceLock<Mutex<HashMap<i64, Emoji>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn set_cache() -> &'static Mutex<HashMap<i64, EmojiSet>> {
    static CACHE: OnceLock<Mutex<HashMap<i64, EmojiSet>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// 一个自定义 emoji 集（getStickerSet 的裁剪版）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EmojiSet {
    pub id: i64,
    pub title: String,
    /// 集合里的贴纸是否单色、需要重绘。
    pub needs_repainting: bool,
}

/// getCustomEmojiStickers：按 id 批量拿贴纸（进程内缓存）。
pub async fn resolve(td: &Td, ids: &[i64]) -> Vec<Emoji> {
    let mut found: HashMap<i64, Emoji> = HashMap::new();
    {
        let cache = emoji_cache()
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        for id in ids {
            if let Some(emoji) = cache.get(id) {
                found.insert(*id, emoji.clone());
            }
        }
    }

    let missing: Vec<i64> = ids
        .iter()
        .copied()
        .filter(|id| !found.contains_key(id))
        .collect();
    if !missing.is_empty()
        && let Ok(response) = td
            .call(
                "getCustomEmojiStickers",
                json!({ "custom_emoji_ids": missing }),
            )
            .await
    {
        let stickers = response
            .get("stickers")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let mut cache = emoji_cache()
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if cache.len() > CACHE_LIMIT {
            cache.clear();
        }
        for sticker in &stickers {
            if let Some(emoji) = Emoji::from_sticker(sticker) {
                cache.insert(emoji.id, emoji.clone());
                found.insert(emoji.id, emoji);
            }
        }
    }

    ids.iter().filter_map(|id| found.get(id).cloned()).collect()
}

/// getStickerSet：集合标题（写日志 / 拉黑时当备注）+ 是否单色。
pub async fn set_info(td: &Td, set_id: i64) -> Option<EmojiSet> {
    if set_id <= 0 {
        return None;
    }
    if let Some(cached) = set_cache()
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .get(&set_id)
    {
        return Some(cached.clone());
    }
    let set = td.sticker_set(set_id).await?;
    let info = EmojiSet {
        id: set_id,
        title: set
            .get("title")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned(),
        needs_repainting: set
            .get("needs_repainting")
            .and_then(Value::as_bool)
            .unwrap_or(false),
    };
    let mut cache = set_cache()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    if cache.len() > CACHE_LIMIT {
        cache.clear();
    }
    cache.insert(set_id, info.clone());
    Some(info)
}

// ── 出图 ──────────────────────────────────────────────────────────

/// 一张贴图：序号（从 1 开始，对应 [E1]）与本地文件。
#[derive(Debug, Clone)]
pub struct Tile {
    pub index: usize,
    pub path: PathBuf,
    pub emoji: Emoji,
}

/// 把若干贴图拼成一张 PNG（行优先、每行 COLUMNS 张）。
///
/// 返回 None 表示一张都解不开（下载失败 / 格式不认识），调用方退回纯文本审核。
pub fn compose(tiles: &[Tile]) -> Option<Vec<u8>> {
    // 按 Tile.index 落位：某一张下载失败就留一个空位，后面的 [En] 不会整体错位。
    let count = tiles.iter().map(|tile| tile.index).max().unwrap_or(0);
    if count == 0 {
        return None;
    }
    let mut slots: Vec<Option<image::RgbaImage>> = vec![None; count];
    for tile in tiles {
        if tile.index == 0 || tile.index > count {
            continue;
        }
        let Ok(bytes) = std::fs::read(&tile.path) else {
            continue;
        };
        let Some(mut bitmap) = decode(&bytes) else {
            continue;
        };
        if tile.emoji.needs_repainting {
            repaint(&mut bitmap);
        }
        slots[tile.index - 1] = Some(image::imageops::resize(
            &bitmap,
            TILE,
            TILE,
            image::imageops::FilterType::Triangle,
        ));
    }
    if slots.iter().all(Option::is_none) {
        return None;
    }

    let count = count as u32;
    let columns = COLUMNS.min(count).max(1);
    let rows = count.div_ceil(columns);
    let width = columns * TILE + (columns + 1) * GAP;
    let height = rows * TILE + (rows + 1) * GAP;
    let mut canvas = image::RgbaImage::from_pixel(
        width,
        height,
        image::Rgba([BACKGROUND[0], BACKGROUND[1], BACKGROUND[2], 255]),
    );
    for (slot, bitmap) in slots.iter().enumerate() {
        let Some(bitmap) = bitmap else {
            continue;
        };
        let slot = slot as u32;
        let x = GAP + (slot % columns) * (TILE + GAP);
        let y = GAP + (slot / columns) * (TILE + GAP);
        image::imageops::overlay(&mut canvas, bitmap, i64::from(x), i64::from(y));
    }

    let mut png = Vec::new();
    image::DynamicImage::ImageRgba8(canvas)
        .write_to(&mut Cursor::new(&mut png), image::ImageFormat::Png)
        .ok()?;
    Some(png)
}

fn decode(bytes: &[u8]) -> Option<image::RgbaImage> {
    if let Some(frame) = webp_middle_frame(bytes) {
        return Some(frame);
    }
    image::load_from_memory(bytes)
        .ok()
        .map(|bitmap| bitmap.to_rgba8())
}

/// 动图 WebP 取「时长中点」那一帧；静态图 / 解不开返回 None（交给通用解码）。
///
/// 为什么要中间帧：贴纸常把「字」放在动画中段才出现，首帧往往还是空白或只有底色。
fn webp_middle_frame(bytes: &[u8]) -> Option<image::RgbaImage> {
    let mut decoder = image_webp::WebPDecoder::new(Cursor::new(bytes)).ok()?;
    if !decoder.is_animated() {
        return None;
    }
    let (width, height) = decoder.dimensions();
    let size = decoder.output_buffer_size()?;
    let has_alpha = decoder.has_alpha();
    let total = decoder.loop_duration();
    let target = total / 2;
    let middle = decoder.num_frames() / 2;

    let mut buffer = vec![0u8; size];
    let mut elapsed: u64 = 0;
    let mut chosen: Option<Vec<u8>> = None;
    for index in 0..decoder.num_frames() {
        let duration = u64::from(decoder.read_frame(&mut buffer).ok()?);
        if total > 0 {
            elapsed += duration;
            if elapsed >= target {
                chosen = Some(buffer.clone());
                break;
            }
        } else if index >= middle {
            chosen = Some(buffer.clone());
            break;
        }
    }
    let data = chosen.or_else(|| Some(buffer.clone()))?;

    if has_alpha {
        image::RgbaImage::from_raw(width, height, data)
    } else {
        let rgb = image::RgbImage::from_raw(width, height, data)?;
        Some(image::DynamicImage::ImageRgb8(rgb).to_rgba8())
    }
}

/// 单色 emoji：保留 alpha，把颜色统一成深色（否则白底上看不见）。
fn repaint(image: &mut image::RgbaImage) {
    for pixel in image.pixels_mut() {
        if pixel.0[3] > 0 {
            pixel.0[0] = REPAINT[0];
            pixel.0[1] = REPAINT[1];
            pixel.0[2] = REPAINT[2];
        }
    }
}

// ── 给 AI 的一次准备 ──────────────────────────────────────────────

/// prepare 的产物：进 ModerationInput 的信息 + 可选的图片路径。
#[derive(Debug, Default)]
pub struct Prepared {
    pub report: Report,
    #[allow(dead_code)]
    pub emojis: Vec<Emoji>,
    #[allow(dead_code)]
    pub sets: Vec<EmojiSet>,
    /// 联系表 PNG 的本地路径；None 表示没出图（不值得 / 下载失败）。
    pub image: Option<PathBuf>,
}

impl Prepared {
    /// 用完删掉临时 PNG。失败无所谓（系统临时目录自己会清）。
    pub fn cleanup(self) {
        if let Some(path) = self.image {
            let _ = std::fs::remove_file(path);
        }
    }
}

/// 把这条消息的自定义 emoji 解析成「AI 能看懂」的输入。
///
/// * 填 input.custom_emoji（数量、集合名、是否像 emoji 画）；
/// * 把 input.message 换成 [E1] 占位文本（原文的兜底字符对模型毫无信息量）；
/// * 只在 Report::is_glyph_art 时才真的下载 + 出图——那是唯一值得花下载与
///   视觉 token 的场景。
pub async fn prepare(
    rt: &Runtime,
    content: &Value,
    input: &mut ModerationInput,
) -> Option<Prepared> {
    let report = report(content)?;
    if report.ids.is_empty() {
        return None;
    }
    let glyph_art = report.is_glyph_art();

    input.custom_emoji = Some(CustomEmoji {
        count: report.count,
        unique: report.ids.len(),
        glyph_art,
        run: report.max_run,
        ids: report.ids.clone(),
        sets: Vec::new(),
    });
    if !report.opaque.trim().is_empty() {
        input.message = Some(report.opaque.clone());
    }

    // 不是「emoji 画」：不花任何网络请求（贴纸信息 / 集合 / 图片都不取）。
    // 这一步很重要：带自定义 emoji 的正常消息远多于广告，一次 TDLib 往返都不该发。
    if !glyph_art {
        return Some(Prepared {
            report,
            emojis: Vec::new(),
            sets: Vec::new(),
            image: None,
        });
    }

    let ids: Vec<i64> = report.ids.iter().copied().take(MAX_TILES).collect();
    let emojis = resolve(&rt.td, &ids).await;

    let mut set_ids: Vec<i64> = Vec::new();
    for emoji in &emojis {
        if emoji.set_id > 0 && !set_ids.contains(&emoji.set_id) {
            set_ids.push(emoji.set_id);
        }
    }
    let mut sets: Vec<EmojiSet> = Vec::new();
    for set_id in set_ids.iter().copied().take(4) {
        if let Some(info) = set_info(&rt.td, set_id).await {
            sets.push(info);
        }
    }
    if let Some(custom_emoji) = input.custom_emoji.as_mut() {
        custom_emoji.sets = sets.iter().map(|set| set.title.clone()).collect();
    }

    let mut image = None;
    if glyph_art {
        let mut tiles: Vec<Tile> = Vec::new();
        for (index, emoji) in emojis.iter().enumerate() {
            let Some(file_id) = emoji.render_file() else {
                continue;
            };
            let Some(path) = rt.td.download_file(file_id).await else {
                continue;
            };
            tiles.push(Tile {
                index: index + 1,
                path: PathBuf::from(path),
                emoji: emoji.clone(),
            });
        }
        if let Some(png) = compose(&tiles) {
            let path =
                std::env::temp_dir().join(format!("fuyubot-emoji-{}.png", crate::util::new_id()));
            if std::fs::write(&path, &png).is_ok() {
                image = Some(path);
            }
        }
    }

    Some(Prepared {
        report,
        emojis,
        sets,
        image,
    })
}

// ── 黑名单 ────────────────────────────────────────────────────────

/// 把一次命中的自定义 emoji 落进黑名单：单个 id + 所属集合。
///
/// 返回被写进去的 (集合名, 记录 id)，调用方原样进日志。集合是**共用**资源
/// （一个正常 emoji 包可能被人拿去做广告），所以这里只按「这张图 / 这个集合」
/// 精确拉黑，不做模糊匹配。
pub async fn blacklist(rt: &Runtime, report: &Report) -> Vec<(String, String)> {
    let mut inserted: Vec<(String, String)> = Vec::new();
    if !report.is_glyph_art() {
        return inserted;
    }
    let ids: Vec<i64> = report.ids.iter().copied().take(MAX_TILES).collect();
    for id in &ids {
        if let Ok(record) = rt.db.add_custom_emoji(&id.to_string()).await {
            inserted.push(("custom_emoji_blacklist".to_owned(), record));
        }
    }
    let emojis = resolve(&rt.td, &ids).await;
    let mut set_ids: Vec<i64> = Vec::new();
    for emoji in &emojis {
        if emoji.set_id > 0 && !set_ids.contains(&emoji.set_id) {
            set_ids.push(emoji.set_id);
        }
    }
    for set_id in set_ids {
        let title = set_info(&rt.td, set_id)
            .await
            .map(|info| info.title)
            .unwrap_or_default();
        if let Ok(record) = rt.db.add_sticker_set(&set_id.to_string(), &title).await {
            inserted.push(("sticker_set_blacklist".to_owned(), record));
        }
    }
    inserted
}

#[cfg(test)]
#[path = "../tests/unit/custom_emoji.rs"]
mod tests;
