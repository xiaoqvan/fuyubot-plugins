//! 排版：解析结果 → Telegram 的 HTML 文本 / X 风格卡片 HTML。
//!
//! 两条输出线：
//!
//! 1. **文字消息**（Telegram parse_mode = html）。Telegram 的 HTML 只认
//!    b / i / code / pre / blockquote / a 这几个标签，没有 p、h1、ul，所以段落之间
//!    只能靠空行、列表靠「• 」前缀、标题靠加粗——这里负责把这些语义翻译过去。
//! 2. **卡片图**（纯文本文章）。一份自包含 HTML，交给 fuyubot.html2img 渲染成
//!    JPEG/PNG。它按 X 帖子卡片的排版走：作者行、正文、底部互动栏。
//!
//! 分片是这里的重点：Telegram 一条消息 4096 字符、相册标题 1024，而 X 文章动辄几千字。
//! 切分只切在「块」的边界上，而且每个片段自带闭合标签（Fragment 的 prefix/suffix），
//! 所以不会出现把 b 标签切成两半、导致整条消息解析失败的情况。

use crate::article::{Block, BlockKind, Run};
use crate::config::Config;
use crate::log;
use crate::model::{Media, Post};

/// Telegram 单条消息的硬上限。
pub const TELEGRAM_LIMIT: usize = 4096;

/// 相册标题（caption）的硬上限。
pub const CAPTION_LIMIT: usize = 1024;

// ── 转义 ──────────────────────────────────────────────────────────

/// HTML 文本转义。
pub fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            _ => out.push(ch),
        }
    }
    out
}

/// 属性值转义（在文本转义之外还要处理引号）。
pub fn escape_attr(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in escape(text).chars() {
        match ch {
            '"' => out.push_str("&quot;"),
            _ => out.push(ch),
        }
    }
    out
}

/// 一个字符转义后占几个字符（含 &amp; 的 5 个）。
fn escaped_char_len(ch: char) -> usize {
    match ch {
        '&' => 5,
        '<' | '>' => 4,
        _ => 1,
    }
}

/// 按字符截断（不切断 UTF-8，也不动 HTML——传入的必须是**未转义**的纯文本）。
pub fn truncate_chars(text: &str, limit: usize) -> String {
    if limit == 0 {
        return String::new();
    }
    let mut out = String::new();
    for (index, ch) in text.chars().enumerate() {
        if index >= limit {
            break;
        }
        out.push(ch);
    }
    out
}

// ── 行内 ──────────────────────────────────────────────────────────

fn run_html(run: &Run) -> String {
    let text = if run.link.is_some() {
        display_link(&run.text)
    } else {
        run.text.as_str()
    };
    let mut html = escape(text);
    if run.code {
        html = format!("<code>{html}</code>");
    }
    if run.italic {
        html = format!("<i>{html}</i>");
    }
    if run.bold {
        html = format!("<b>{html}</b>");
    }
    if let Some(link) = &run.link {
        html = format!("<a href=\"{}\">{html}</a>", escape_attr(link));
    }
    html
}

/// 链接的显示文本：去掉 `http(s)://` 和末尾的 `/`。
fn display_link(text: &str) -> &str {
    let text = text
        .strip_prefix("https://")
        .or_else(|| text.strip_prefix("http://"))
        .unwrap_or(text);
    text.trim_end_matches('/')
}

fn is_rich_char(ch: char) -> bool {
    ch.is_alphanumeric() || ch == '_'
}

/// 把正文里的 URL、#话题、@用户名 画成蓝色链接；URL 不显示 `http(s)://` 前缀。
pub fn linkify(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 32);
    let mut index = 0;
    while index < text.len() {
        let rest = &text[index..];
        if rest.starts_with("https://") || rest.starts_with("http://") {
            let end = rest
                .find(|ch: char| ch.is_whitespace() || ch == '<' || ch == '>')
                .unwrap_or(rest.len());
            let raw = &rest[..end];
            let url = raw.trim_end_matches(|ch: char| {
                matches!(ch, '.' | ',' | ';' | ':' | '!' | '?' | ')' | ']' | '}')
                    || ch == '"'
                    || ch == '\''
            });
            if url.len() > 8 {
                out.push_str("<a class=\"rich\" href=\"");
                out.push_str(&escape_attr(url));
                out.push_str("\">");
                out.push_str(&escape(display_link(url)));
                out.push_str("</a>");
                out.push_str(&escape(&raw[url.len()..]));
                index += end;
                continue;
            }
        }
        if (rest.starts_with('#') || rest.starts_with('@'))
            && rest[1..].chars().next().is_some_and(is_rich_char)
        {
            let mut chars = rest.char_indices();
            let (_, marker) = chars.next().unwrap();
            let end = chars
                .find(|(_, ch)| !is_rich_char(*ch))
                .map(|(offset, _)| offset)
                .unwrap_or(rest.len());
            let token = &rest[..end];
            let href = if marker == '#' {
                format!("https://x.com/hashtag/{}", &token[1..])
            } else {
                format!("https://x.com/{}", &token[1..])
            };
            out.push_str("<a class=\"rich\" href=\"");
            out.push_str(&escape_attr(&href));
            out.push_str("\">");
            out.push_str(&escape(token));
            out.push_str("</a>");
            index += end;
            continue;
        }
        let ch = rest.chars().next().unwrap();
        out.push_str(&escape(&ch.to_string()));
        index += ch.len_utf8();
    }
    out
}

/// 一组 run → HTML。
pub fn runs_html(runs: &[Run]) -> String {
    runs.iter().map(run_html).collect()
}

fn plain_run(text: impl Into<String>) -> Run {
    Run {
        text: text.into(),
        bold: false,
        italic: false,
        code: false,
        link: None,
    }
}

// ── 片段：自带闭合标签的最小发送单位 ───────────────────────────────

/// 一段可单独成条的 HTML：prefix + runs + suffix。
///
/// prefix/suffix 让「加粗的标题」「blockquote」「pre」这些包裹型语义在**任何**一条
/// 分片里都是闭合的——切分只发生在 runs 内部，标签永远不会被切开。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fragment {
    pub prefix: String,
    pub suffix: String,
    pub runs: Vec<Run>,
}

impl Fragment {
    pub fn new(prefix: impl Into<String>, suffix: impl Into<String>, runs: Vec<Run>) -> Self {
        Self {
            prefix: prefix.into(),
            suffix: suffix.into(),
            runs,
        }
    }

    pub fn plain(runs: Vec<Run>) -> Self {
        Self::new("", "", runs)
    }

    pub fn render(&self) -> String {
        format!("{}{}{}", self.prefix, runs_html(&self.runs), self.suffix)
    }

    /// 超过 limit 个字符就切开；每一份都保留 prefix/suffix。
    pub fn split_to(&self, limit: usize) -> Vec<Fragment> {
        let limit = limit.max(96);
        if self.render().chars().count() <= limit {
            return vec![self.clone()];
        }
        let reserve = self.prefix.chars().count() + self.suffix.chars().count() + 16;
        let budget = limit.saturating_sub(reserve).max(32);

        let mut out: Vec<Fragment> = Vec::new();
        let mut runs: Vec<Run> = Vec::new();
        let mut used = 0usize;
        for run in &self.runs {
            for ch in run.text.chars() {
                let cost = escaped_char_len(ch);
                if used + cost > budget && !runs.is_empty() {
                    out.push(Fragment::new(
                        self.prefix.clone(),
                        self.suffix.clone(),
                        std::mem::take(&mut runs),
                    ));
                    used = 0;
                }
                used += cost;
                let mut piece = run.clone();
                piece.text = ch.to_string();
                match runs.last_mut() {
                    Some(last) if last.same_style(&piece) => last.text.push(ch),
                    _ => runs.push(piece),
                }
            }
        }
        if !runs.is_empty() {
            out.push(Fragment::new(
                self.prefix.clone(),
                self.suffix.clone(),
                runs,
            ));
        }
        if out.is_empty() {
            out.push(self.clone());
        }
        out
    }
}

// ── 块 → 片段 ─────────────────────────────────────────────────────

/// 正文的片段序列（不含标题与页脚）。
///
/// 图片 / 视频块**不产生文字**：媒体本身由 send 模块按相册发，正文里再画一行
/// 「[图片]」只会干扰阅读。
pub fn fragments_of(post: &Post) -> Vec<Fragment> {
    let mut out: Vec<Fragment> = Vec::new();
    if let Some(article) = &post.article {
        for block in &article.blocks {
            if let Some(fragment) = fragment_of(block) {
                out.push(fragment);
            }
        }
    } else if !post.text.trim().is_empty() {
        out.push(Fragment::plain(vec![plain_run(post.text.clone())]));
    }
    out
}

fn fragment_of(block: &Block) -> Option<Fragment> {
    match &block.kind {
        BlockKind::Media(_) => None,
        BlockKind::Tweet(id) => Some(Fragment::plain(vec![plain_run(format!(
            "https://x.com/i/status/{id}"
        ))])),
        BlockKind::Paragraph => Some(Fragment::plain(block.runs.clone())),
        BlockKind::Heading(_) => Some(Fragment::new("<b>", "</b>", block.runs.clone())),
        BlockKind::Item { ordered, index } => {
            let prefix = if *ordered {
                format!("{index}. ")
            } else {
                "• ".to_owned()
            };
            Some(Fragment::new(prefix, "", block.runs.clone()))
        }
        BlockKind::Quote => Some(Fragment::new(
            "<blockquote>",
            "</blockquote>",
            block.runs.clone(),
        )),
        BlockKind::Code => Some(Fragment::new("<pre>", "</pre>", block.runs.clone())),
    }
}

// ── 文字消息 ──────────────────────────────────────────────────────

/// 一条消息的头部：标题 + 作者 / 时间 + 原文链接。
pub fn header(post: &Post) -> String {
    let mut parts: Vec<String> = Vec::new();
    // 只有文章才有独立于正文的标题；普通推文的 title() 就是正文首行，
    // 再加一行加粗标题等于把第一句重复一遍。
    if post.article.is_some() {
        parts.push(format!("<b>{}</b>", escape(&post.title())));
    }

    let mut meta: Vec<String> = Vec::new();
    if !post.author.name.is_empty() {
        meta.push(escape(&post.author.name));
    }
    let handle = post.author.handle();
    if !handle.is_empty() {
        meta.push(format!(
            "<a href=\"https://x.com/{}\">{}</a>",
            escape_attr(&post.author.screen_name),
            escape(&handle)
        ));
    }
    let date = post.date_label();
    if !date.is_empty() {
        meta.push(escape(&date));
    }
    if !meta.is_empty() {
        parts.push(meta.join(" · "));
    }
    parts.push(format!(
        "<a href=\"{}\">{}</a>",
        escape_attr(&post.url),
        escape(&post.url)
    ));
    parts.join("\n")
}

/// 文章正文要发的全部消息（每条都不超过 config.text_chunk）。
pub fn messages(post: &Post, config: &Config) -> Vec<String> {
    let limit = config.text_chunk.clamp(200, TELEGRAM_LIMIT);
    let mut fragments: Vec<String> = vec![header(post)];
    for fragment in fragments_of(post) {
        for piece in fragment.split_to(limit.saturating_sub(64)) {
            let html = piece.render();
            if !html.trim().is_empty() {
                fragments.push(html);
            }
        }
    }
    pack(fragments, limit)
}

/// 贪心装箱：片段之间用空行分隔，装不下就开新消息。
fn pack(fragments: Vec<String>, limit: usize) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut current = String::new();
    for fragment in fragments {
        let extra = if current.is_empty() {
            0
        } else {
            2 // 两个换行
        };
        let projected = current.chars().count() + extra + fragment.chars().count();
        if !current.is_empty() && projected > limit {
            out.push(std::mem::take(&mut current));
        }
        if !current.is_empty() {
            current.push_str("\n\n");
        }
        current.push_str(&fragment);
    }
    if !current.is_empty() {
        out.push(current);
    }
    out
}

// ── 相册标题 ──────────────────────────────────────────────────────

/// 相册第一项带的标题：串文用文章标题，普通推文用推文正文。
///
/// 返回 (html, 是否截断)。链接**永远留在标题里**：用户点回去看原帖是刚需。
pub fn caption(post: &Post, limit: usize) -> (String, bool) {
    let url_html = format!(
        "<a href=\"{}\">{}</a>",
        escape_attr(&post.url),
        escape(&post.url)
    );
    let handle = post.author.handle();
    let handle_html = if handle.is_empty() {
        String::new()
    } else {
        format!("\n{}", escape(&handle))
    };
    let body = match &post.article {
        Some(_) => log::shorten(&post.title(), 160),
        None => post.text.clone(),
    };
    let reserve = url_html.chars().count() + handle_html.chars().count() + 4;
    let mut budget = limit.saturating_sub(reserve).max(32);
    let body_chars = body.chars().count();
    loop {
        let mut html = escape(&truncate_chars(&body, budget));
        html.push_str(&handle_html);
        html.push('\n');
        html.push_str(&url_html);
        let truncated = budget < body_chars;
        if html.chars().count() <= limit || budget <= 32 {
            return (html, truncated);
        }
        budget = budget.saturating_mul(9) / 10;
    }
}

// ── X 风格卡片 ────────────────────────────────────────────────────

/// 卡片样式。$XXX 是占位符，用 replace 注入主题色——不用 format! 是为了避免把
/// CSS 里每个 { } 都写成 {{ }}。
const CARD_CSS: &str = r#"
* { box-sizing: border-box; }
html, body { margin: 0; padding: 0; background: $BG; }
body {
  font-family: "TwitterChirp", -apple-system, "Segoe UI", Roboto, "Noto Sans SC",
    "Noto Sans CJK SC", "PingFang SC", "Hiragino Sans GB", "Microsoft YaHei", Arial, sans-serif;
  color: $FG;
  -webkit-font-smoothing: antialiased;
}
.card { width: 100%; padding: 0; background: $BG; }
.tweet { padding: 12px 16px 8px; background: $BG; }
.head { display: flex; flex-direction: row; align-items: flex-start; }
.avatar {
  position: relative; width: 44px; height: 44px; border-radius: 22px;
  background: $ACCENT; color: #ffffff; font-size: 20px; font-weight: 700;
  line-height: 44px; text-align: center; margin-right: 10px;
  flex-grow: 0; flex-shrink: 0; overflow: hidden;
}
.avatar img { position: absolute; top: 0; left: 0; width: 44px; height: 44px; border-radius: 22px; }
.who { flex-grow: 1; flex-shrink: 1; min-width: 0; }
.name-row { display: flex; flex-direction: row; align-items: center; }
.name {
  font-size: 16px; line-height: 21px; font-weight: 700; color: $FG;
  white-space: nowrap; overflow: hidden; text-overflow: ellipsis;
}
.badge { width: 19px; height: 19px; margin-left: 2px; flex-grow: 0; flex-shrink: 0; }
.handle {
  font-size: 16px; line-height: 21px; color: $MUTED;
  white-space: nowrap; overflow: hidden; text-overflow: ellipsis;
}
.translate {
  display: flex; flex-direction: row; align-items: center; margin-top: 4px;
  color: $ACCENT; font-size: 14px; line-height: 18px;
}
.translate svg { width: 17px; height: 17px; margin-right: 4px; }
.top { display: flex; flex-direction: row; align-items: center; flex-grow: 0; flex-shrink: 0; margin-left: 8px; }
.menu { display: flex; width: 16px; height: 16px; }
.menu svg { width: 16px; height: 16px; }
.title { font-size: 23px; line-height: 28px; font-weight: 700; margin: 12px 0 2px; color: $FG; }
.body { font-size: 17px; line-height: 24px; color: $FG; margin-top: 12px; }
.body p { margin: 0 0 14px; white-space: pre-wrap; }
.body h2 { font-size: 20px; line-height: 24px; font-weight: 700; margin: 24px 0 8px; }
.body h3 { font-size: 18px; line-height: 22px; font-weight: 700; margin: 20px 0 6px; }
.body .item { padding-left: 18px; text-indent: -18px; }
.body blockquote { margin: 0 0 14px; padding: 0 0 0 12px; border-left: 3px solid $LINE; color: $FG; }
.body pre {
  font-family: "SFMono-Regular", Consolas, "Liberation Mono", Menlo, monospace;
  font-size: 13px; line-height: 18px; background: $SOFT; padding: 12px;
  border-radius: 8px; margin: 0 0 14px; white-space: pre-wrap;
}
.body a { color: $ACCENT; text-decoration: none; }
.body .embed { margin: 0 0 12px; padding: 10px 12px; border: 1px solid $LINE; border-radius: 12px; }
.body .embed-label { font-size: 13px; line-height: 16px; color: $MUTED; }
.body .embed-url { font-size: 14px; line-height: 18px; color: $ACCENT; margin-top: 2px; }
.more { color: $MUTED; }
.translation { margin-top: 12px; padding-top: 12px; border-top: 1px solid $LINE; }
.translation-head {
  display: flex; flex-direction: row; align-items: center;
  margin: 0 0 4px; font-size: 13px; line-height: 16px;
}
.translation-head svg { width: 16px; height: 16px; margin-right: 4px; flex-grow: 0; flex-shrink: 0; }
.translation-src { color: $MUTED; }
.translation-original { color: $ACCENT; margin-left: 6px; }
.translation-text { font-size: 17px; line-height: 24px; color: $FG; white-space: pre-wrap; }
.translation-text a { color: $ACCENT; text-decoration: none; }
.meta { margin-top: 12px; font-size: 15px; line-height: 20px; color: $MUTED; }
.views { font-size: 14px; line-height: 16px; }
.views b { color: $FG; font-weight: 700; }
.media { display: flex; flex-direction: row; justify-content: space-between; margin-top: 12px; }
.media.single { justify-content: flex-start; }
.shot { position: relative; overflow: hidden; border-radius: 16px; border: 1px solid $LINE; background: $SOFT; }
.shot img { display: block; width: 100%; height: 100%; object-fit: cover; }
.play {
  position: absolute; left: 50%; top: 50%; width: 52px; height: 52px;
  margin-left: -26px; margin-top: -26px; border-radius: 26px;
  background: rgba(0, 0, 0, 0.68); display: flex; align-items: center; justify-content: center;
}
.gif {
  position: absolute; left: 8px; bottom: 8px; padding: 2px 6px; border-radius: 4px;
  background: rgba(0, 0, 0, 0.72); color: #ffffff; font-size: 12px;
  font-weight: 700; letter-spacing: 0.5px;
}
.actions { display: flex; flex-direction: row; align-items: center; justify-content: space-between; margin: 12px -8px 0; }
.footer {
  display: flex; flex-direction: row; align-items: center; justify-content: center;
  padding: 6px 16px;
  border-top: 1px solid $LINE; color: $MUTED; font-size: 12px; line-height: 16px;
}
.footer-avatar { flex: 0 0 auto; width: 16px; height: 16px; border-radius: 50%; margin-right: 5px; background: $SOFT; object-fit: cover; }
.footer-icon { flex: 0 0 auto; width: 16px; height: 16px; border-radius: 4px; margin: 0 4px; background: $SOFT; }
.footer-text { flex: 0 0 auto; line-height: 16px; }
.action { display: flex; flex-direction: row; align-items: center; }
.action svg { width: 18.75px; height: 18.75px; }
.action .count { font-size: 13px; line-height: 16px; color: $MUTED; margin-left: 4px; }
"#;

/// 整条帖子的 X 风格卡片 HTML（交给 html2img 渲染）。
///
/// 正文超过 config.card_max_chars 会截断，并补一句「完整内容见原链接」。
pub fn card_html(post: &Post, config: &Config) -> String {
    card_html_with_translation(post, config, None)
}

/// 带机翻的卡片：`translation` 有值时画「谷歌翻译」块，不再画「显示翻译」入口。
pub fn card_html_with_translation(
    post: &Post,
    config: &Config,
    translation: Option<&str>,
) -> String {
    card_html_with_footer(post, config, translation, None, None, None)
}

/// 带机翻和底部署名的卡片。`footer` 由调用方查宿主的身份服务后传进来。
pub fn card_html_with_footer(
    post: &Post,
    config: &Config,
    translation: Option<&str>,
    footer: Option<&str>,
    footer_avatar: Option<&str>,
    footer_avatar_svg: Option<&str>,
) -> String {
    let dark = config.card_dark;
    let palette = if dark {
        [
            "#000000", "#e7e9ea", "#71767b", "#2f3336", "#16181c", "#1d9bf0",
        ]
    } else {
        [
            "#ffffff", "#0f1419", "#536471", "#cfd9de", "#f7f9f9", "#1d9bf0",
        ]
    };
    let css = CARD_CSS
        .replace("$BG", palette[0])
        .replace("$FG", palette[1])
        .replace("$MUTED", palette[2])
        .replace("$LINE", palette[3])
        .replace("$SOFT", palette[4])
        .replace("$ACCENT", palette[5]);

    let name = if post.author.name.is_empty() {
        escape(&post.author.screen_name)
    } else {
        escape(&post.author.name)
    };
    let handle = escape(&post.author.handle());
    let show_translate = crate::translate::needs(post) && translation.is_none();
    let initial = post
        .author
        .name
        .chars()
        .next()
        .or_else(|| post.author.screen_name.chars().next())
        .map(|ch| escape(&ch.to_uppercase().collect::<String>()))
        .unwrap_or_else(|| "X".to_owned());

    let mut html = String::with_capacity(8192);
    html.push_str("<!doctype html><html><head><meta charset=\"utf-8\"><style>");
    html.push_str(&css);
    html.push_str("</style></head><body><div class=\"card\"><div class=\"tweet\">");

    // 作者行：头像、名字、认证、@handle、时间，右边是「···」。
    html.push_str("<div class=\"head\"><div class=\"avatar\">");
    html.push_str(&initial);
    if !post.author.avatar_url.is_empty() {
        html.push_str("<img src=\"");
        html.push_str(&escape_attr(&post.author.avatar_url));
        html.push_str("\" alt=\"\">");
    }
    html.push_str("</div><div class=\"who\"><div class=\"name-row\"><span class=\"name\">");
    html.push_str(&name);
    html.push_str("</span>");
    if post.author.verified {
        html.push_str(VERIFIED_BADGE);
    }
    html.push_str("</div>");
    if !handle.is_empty() {
        html.push_str("<div class=\"handle\">");
        html.push_str(&handle);
        html.push_str("</div>");
    }
    if show_translate {
        html.push_str(&translate_row());
    }
    html.push_str("</div><div class=\"top\"><span class=\"menu\">");
    html.push_str(&more_icon(palette[2]));
    html.push_str("</span></div></div>");

    if post.article.is_some() {
        let title = escape(&post.title());
        if !title.is_empty() {
            html.push_str("<div class=\"title\">");
            html.push_str(&title);
            html.push_str("</div>");
        }
    }

    html.push_str("<div class=\"body\"");
    if show_translate {
        html.push_str(" style=\"margin-top:0;\"");
    }
    html.push('>');
    html.push_str(&card_body(post, config.card_max_chars));
    html.push_str("</div>");
    if let Some(text) = translation
        && !text.trim().is_empty()
    {
        html.push_str("<div class=\"translation\">");
        html.push_str(&translation_head(post));
        html.push_str("<div class=\"translation-text\">");
        html.push_str(&linkify(text));
        html.push_str("</div></div>");
    }
    html.push_str(&card_media(post));
    html.push_str(&card_meta(post));
    html.push_str(&action_bar(post, palette[2]));
    html.push_str("</div>");
    if let Some(text) = footer.filter(|value| !value.trim().is_empty()) {
        let (bot_part, plugin_part) = text.split_once(" · ").unwrap_or(("", text));
        html.push_str("<div class=\"footer\">");
        if !bot_part.is_empty() {
            if let Some(svg) = footer_avatar_svg.filter(|value| !value.trim().is_empty()) {
                html.push_str(svg);
            } else if let Some(avatar) = footer_avatar.filter(|value| !value.trim().is_empty()) {
                html.push_str("<img class=\"footer-avatar\" src=\"");
                html.push_str(&escape_attr(avatar));
                html.push_str("\" alt=\"\">");
            }
            html.push_str("<span class=\"footer-text\">");
            html.push_str(&escape(bot_part));
            html.push_str(" · </span>");
        }
        html.push_str(PLUGIN_ICON);
        html.push_str("<span class=\"footer-text\">");
        html.push_str(&escape(plugin_part));
        html.push_str("</span></div>");
    }
    html.push_str("</div></body></html>");
    html
}

/// 卡片里的媒体预览：最多两张；视频用封面，图片用原图。
fn card_media(post: &Post) -> String {
    let media = post.media_for_send();
    let previews: Vec<&Media> = media.iter().take(2).collect();
    if previews.is_empty() {
        return String::new();
    }

    let single = previews.len() == 1;
    let mut images: Vec<(String, f32, f32, bool, bool)> = Vec::new();
    for media in &previews {
        let url: &str = if media.is_video() || media.gif {
            match media.thumbnail.as_deref() {
                Some(url) if !url.is_empty() => url,
                _ => continue,
            }
        } else {
            media.url.as_str()
        };
        let (mut width, mut height) = (if single { 450.0 } else { 220.0 }, 260.0);
        if media.width > 0 && media.height > 0 {
            let ratio = media.width as f32 / media.height as f32;
            height = width / ratio;
            if single && height > 460.0 {
                // 竖图不裁成宽幅：限高之后把宽度收窄，像 X 那样靠左。
                height = 460.0;
                width = (height * ratio).round();
            } else if !single && height > 520.0 {
                // 两张图按窄半宽保留比例；特别高的图才限高裁切。
                height = 520.0;
            }
        }
        images.push((escape_attr(url), width, height, media.is_video(), media.gif));
    }
    if images.is_empty() {
        return String::new();
    }

    // 两张图共用同一个高度，避免一高一矮；object-fit 负责裁切。
    let height = images.iter().map(|item| item.2).fold(0.0, f32::max);
    let mut html = if single {
        String::from("<div class=\"media single\">")
    } else {
        String::from("<div class=\"media\">")
    };
    for (url, width, _, video, gif) in &images {
        html.push_str("<div class=\"shot\" style=\"");
        if single {
            html.push_str(&format!("width:{width}px;"));
        } else {
            html.push_str("width:45.8%;");
        }
        html.push_str(&format!("height:{height}px;\">"));
        html.push_str("<img src=\"");
        html.push_str(url);
        html.push_str("\" alt=\"\">");
        if *video {
            html.push_str("<div class=\"play\">");
            html.push_str(PLAY_ICON);
            html.push_str("</div>");
        }
        if *gif {
            html.push_str("<div class=\"gif\">GIF</div>");
        }
        html.push_str("</div>");
    }
    html.push_str("</div>");
    html
}

/// 卡片消息的配文：图片已经把内容画进去了，只留原文链接。
pub fn card_caption(post: &Post) -> String {
    escape(&post.url)
}

/// 详情页的「显示翻译」行。
/// 谷歌的 G 图标（四色）。
/// 底部署名里的插件图标（64px 裁剪后内嵌 PNG 的 SVG，避免依赖外部图床）。
const PLUGIN_ICON: &str = include_str!("../assets/plugin-icon-inline.svg");

const GOOGLE_ICON: &str = "<svg viewBox=\"0 0 48 48\" aria-hidden=\"true\"><path fill=\"#EA4335\" d=\"M24 9.5c3.54 0 6.71 1.22 9.21 3.6l6.85-6.85C35.9 2.38 30.47 0 24 0 14.62 0 6.51 5.38 2.56 13.22l7.98 6.19C12.43 13.72 17.74 9.5 24 9.5z\"/><path fill=\"#4285F4\" d=\"M46.98 24.55c0-1.57-.15-3.09-.38-4.55H24v9.02h12.94c-.58 2.96-2.26 5.48-4.78 7.18l7.73 6c4.51-4.18 7.09-10.36 7.09-17.65z\"/><path fill=\"#FBBC05\" d=\"M10.53 28.59c-.48-1.45-.76-2.99-.76-4.59s.27-3.14.76-4.59l-7.98-6.19C.92 16.46 0 20.12 0 24c0 3.88.92 7.54 2.56 10.78l7.97-6.19z\"/><path fill=\"#34A853\" d=\"M24 48c6.48 0 11.93-2.13 15.89-5.81l-7.73-6c-2.15 1.45-4.92 2.3-8.16 2.3-6.26 0-11.57-4.22-13.47-9.91l-7.98 6.19C6.51 42.62 14.62 48 24 48z\"/></svg>";

fn translate_row() -> String {
    format!("<div class=\"translate\">{GOOGLE_ICON}显示翻译</div>")
}

fn translation_head(post: &Post) -> String {
    format!(
        "<div class=\"translation-head\">{GOOGLE_ICON}<span class=\"translation-src\">翻译自 {}</span><span class=\"translation-original\">原文对照</span></div>",
        language_name(&post.lang)
    )
}

fn language_name(lang: &str) -> &'static str {
    match lang.to_ascii_lowercase().split('-').next().unwrap_or("") {
        "en" => "英语",
        "ja" => "日语",
        "ko" => "韩语",
        "fr" => "法语",
        "de" => "德语",
        "es" => "西班牙语",
        "ru" => "俄语",
        "pt" => "葡萄牙语",
        "it" => "意大利语",
        "ar" => "阿拉伯语",
        "th" => "泰语",
        "vi" => "越南语",
        "id" => "印尼语",
        "tr" => "土耳其语",
        _ => "外文",
    }
}

/// 详情页时间戳：`20:28 · 2026年11月16日 · 8.4万 浏览`。
fn card_meta(post: &Post) -> String {
    let time = post.time_of_day();
    let date = post.full_date_label();
    let mut parts: Vec<String> = Vec::new();
    if !time.is_empty() {
        parts.push(escape(&time));
    }
    if !date.is_empty() {
        parts.push(escape(&date));
    }
    if parts.is_empty() && post.metrics.views == 0 {
        return String::new();
    }
    let mut html = String::from("<div class=\"meta\">");
    html.push_str(&parts.join(" · "));
    if post.metrics.views > 0 {
        if !parts.is_empty() {
            html.push_str(" · ");
        }
        html.push_str("<span class=\"views\"><b>");
        html.push_str(&format_count(post.metrics.views));
        html.push_str("</b> 浏览</span>");
    }
    html.push_str("</div>");
    html
}

/// 视频封面上的播放按钮。
const PLAY_ICON: &str = "<svg viewBox=\"0 0 24 24\" width=\"24\" height=\"24\" fill=\"#ffffff\" aria-hidden=\"true\"><path d=\"M8 5v14l11-7z\"/></svg>";

/// 蓝标（X 的认证徽章）。
const VERIFIED_BADGE: &str = "<svg class=\"badge\" viewBox=\"0 0 22 22\" aria-hidden=\"true\"><path fill=\"#1d9bf0\" d=\"M20.396 11c-.018-.646-.215-1.275-.57-1.816-.354-.54-.852-.972-1.438-1.246.223-.607.27-1.264.14-1.897-.131-.634-.437-1.218-.882-1.687-.47-.445-1.053-.75-1.687-.882-.633-.13-1.29-.083-1.897.14-.273-.587-.704-1.086-1.245-1.44S11.647 1.62 11 1.604c-.646.017-1.273.213-1.813.568s-.969.854-1.24 1.44c-.608-.223-1.267-.272-1.902-.14-.635.13-1.22.436-1.69.882-.445.47-.749 1.055-.878 1.688-.13.633-.08 1.29.144 1.896-.587.274-1.087.705-1.443 1.245-.356.54-.555 1.17-.574 1.817.02.647.218 1.276.574 1.817.356.54.856.972 1.443 1.245-.224.606-.274 1.263-.144 1.896.13.634.433 1.218.877 1.688.47.443 1.054.747 1.687.878.633.132 1.29.084 1.897-.136.274.586.705 1.084 1.246 1.439.54.354 1.17.551 1.816.569.647-.016 1.276-.213 1.817-.567s.972-.854 1.245-1.44c.604.239 1.266.296 1.903.164.636-.132 1.22-.447 1.68-.907.46-.46.776-1.044.908-1.681s.075-1.299-.165-1.903c.586-.274 1.084-.705 1.439-1.246.354-.54.551-1.17.569-1.816z\"/><path fill=\"#fff\" d=\"M9.662 14.85l-3.429-3.428 1.293-1.302 2.072 2.072 4.4-4.794 1.347 1.246z\"/></svg>";

fn action_icon(paths: &str, color: &str) -> String {
    format!("<svg viewBox=\"0 0 24 24\" fill=\"{color}\" aria-hidden=\"true\">{paths}</svg>")
}

/// 右上角的「···」。
fn more_icon(color: &str) -> String {
    format!(
        "<svg viewBox=\"0 0 24 24\" fill=\"{color}\" aria-hidden=\"true\"><path d=\"M14 12a2 2 0 1 1-4 0 2 2 0 0 1 4 0m7 0a2 2 0 1 1-4 0 2 2 0 0 1 4 0M7 12a2 2 0 1 1-4 0 2 2 0 0 1 4 0\"/></svg>"
    )
}

/// 卡片底部那一排互动图标，有数据才带数字。路径来自 X 自己的 SVG。
fn action_bar(post: &Post, muted: &str) -> String {
    let reply = action_icon(
        "<path d=\"M1.75 10a8 8 0 0 1 8-8h4.37a8.13 8.13 0 0 1 3.93 15.24L10 21.7V18h-.07a8 8 0 0 1-8.18-8m2 0a6 6 0 0 0 6.14 6H12v2.3l5.09-2.8A6.13 6.13 0 0 0 14.12 4H9.75a6 6 0 0 0-6 6\"/>",
        muted,
    );
    let repost = action_icon(
        "<path d=\"M13.1 4q1.65-.01 2.71.06c.73.06 1.37.18 1.96.48a5 5 0 0 1 2.19 2.19 5 5 0 0 1 .48 1.96q.08 1.06.06 2.71v5.05l2.07-1.93 1.36 1.46-4.43 4.14-4.43-4.14 1.36-1.46 2.07 1.93V11.4c0-1.14 0-1.93-.05-2.55a3 3 0 0 0-.27-1.21 3 3 0 0 0-1.32-1.31 3 3 0 0 0-1.21-.28C15.03 6 14.24 6 13.1 6H11V4zM8.93 8.02 7.57 9.48 5.5 7.55v5.05c0 1.14 0 1.93.05 2.55.05.6.15.95.28 1.21a3 3 0 0 0 1.31 1.31c.26.14.61.23 1.22.28s1.4.05 2.54.05H13v2h-2.1q-1.64.02-2.7-.06a5 5 0 0 1-1.97-.48 5 5 0 0 1-2.18-2.19 5 5 0 0 1-.49-1.96q-.08-1.06-.06-2.7V7.54L1.43 9.48.07 8.02 4.5 3.88z\"/>",
        muted,
    );
    let like = action_icon(
        "<path d=\"M17.06 3a5.7 5.7 0 0 1 4.81 3.16c.94 1.9.89 4.36-.54 6.98-1.42 2.61-4.2 5.4-8.83 8.09l-.5.29-.5-.3c-4.62-2.68-7.4-5.47-8.83-8.08S1.2 8.05 2.13 6.16a5.7 5.7 0 0 1 4.8-3.15c1.75-.09 3.56.6 5.07 2.15a6.6 6.6 0 0 1 5.06-2.15m-.1 2c-1.3-.06-2.86.55-4.15 2.3L12 8.4l-.81-1.1C9.9 5.55 8.35 4.94 7.04 5a3.7 3.7 0 0 0-3.12 2.05c-.6 1.2-.67 2.97.5 5.14 1.15 2.1 3.47 4.53 7.58 7 4.1-2.47 6.43-4.9 7.57-7 1.18-2.17 1.1-3.94.5-5.14A3.7 3.7 0 0 0 16.98 5\"/>",
        muted,
    );
    let bookmark = action_icon(
        "<path d=\"M18.5 7c0-.72 0-1.2-.03-1.56a2 2 0 0 0-.13-.62 1.5 1.5 0 0 0-.66-.66c-.1-.05-.26-.1-.62-.13S16.22 4 15.5 4h-7c-.72 0-1.2 0-1.56.03s-.52.08-.62.13a1.5 1.5 0 0 0-.66.66c-.05.1-.1.26-.13.62-.03.37-.03.84-.03 1.56v11.33l6.5-3.46 6.5 3.46zm2 14.67L12 17.13l-8.5 4.54V7q-.01-1.03.04-1.72c.04-.48.12-.94.34-1.37a3.5 3.5 0 0 1 1.53-1.53c.43-.22.89-.3 1.37-.34q.7-.05 1.72-.04h7q1.02-.01 1.72.04c.48.04.94.12 1.37.34.66.34 1.2.87 1.53 1.53.22.43.3.89.34 1.37q.05.7.04 1.72z\"/>",
        muted,
    );
    let share = action_icon(
        "<path d=\"M5 16c0 .72 0 1.2.03 1.56s.08.52.13.62q.23.43.66.66c.1.05.26.1.62.13.37.03.84.03 1.56.03h8c.72 0 1.2 0 1.56-.03s.52-.08.62-.13q.43-.23.66-.66c.05-.1.1-.26.13-.62.03-.37.03-.84.03-1.56v-2h2v2q.01 1.02-.04 1.72a4 4 0 0 1-.34 1.37 3.5 3.5 0 0 1-1.53 1.53c-.43.22-.89.3-1.37.34q-.7.05-1.72.04H8q-1.03.01-1.72-.04a4 4 0 0 1-1.37-.34 3.5 3.5 0 0 1-1.53-1.53 4 4 0 0 1-.34-1.37Q2.99 17.02 3 16v-2h2z\"/><path d=\"m17.7 8.04-1.4 1.42-3.3-3.3V16h-2V6.16l-3.3 3.3-1.4-1.42 5.7-5.7z\"/>",
        muted,
    );
    let items = [
        (reply, post.metrics.replies),
        (repost, post.metrics.retweets),
        (like, post.metrics.likes),
        (bookmark, post.metrics.bookmarks),
        (share, 0),
    ];
    let mut html = String::from("<div class=\"actions\">");
    for (icon, count) in items {
        html.push_str("<div class=\"action\">");
        html.push_str(&icon);
        let label = format_count(count);
        if !label.is_empty() {
            html.push_str("<span class=\"count\">");
            html.push_str(&label);
            html.push_str("</span>");
        }
        html.push_str("</div>");
    }
    html.push_str("</div>");
    html
}

/// 数字按 X 的习惯缩：一万以上用「万」，千位加逗号。
fn format_count(value: u64) -> String {
    if value == 0 {
        return String::new();
    }
    if value >= 100_000_000 {
        let text = format!("{:.1}", value as f64 / 100_000_000.0);
        return format!("{}亿", text.trim_end_matches(".0"));
    }
    if value >= 10_000 {
        let wan = value as f64 / 10_000.0;
        let text = if wan >= 100.0 {
            format!("{wan:.0}")
        } else {
            format!("{wan:.1}")
        };
        return format!("{}万", text.trim_end_matches(".0"));
    }
    value.to_string()
}

/// 卡片正文：按块的类型排版，嵌入推文画成一行灰色提示。
///
/// 文字消息里嵌入推文是**裸链接**（Telegram 会把它变成可点的链接），但图片里链接
/// 不可点，一整段 https://x.com/i/status/… 只会把版面刷成链接墙——所以卡片里换成
/// 「嵌入推文 · x.com/i/status/…」这种灰色小字。
fn card_body(post: &Post, budget: usize) -> String {
    let mut pieces: Vec<(String, usize)> = Vec::new();
    match &post.article {
        Some(article) => {
            for block in &article.blocks {
                match &block.kind {
                    // 媒体由相册单独发；卡片只画文字，这里连占位都不留。
                    BlockKind::Media(_) => {}
                    BlockKind::Tweet(id) => {
                        let label = format!(
                            "<div class=\"embed\"><div class=\"embed-label\">嵌入推文</div><div class=\"embed-url\">x.com/i/status/{}</div></div>",
                            escape(id)
                        );
                        pieces.push((label, 16));
                    }
                    _ => {
                        if let Some(fragment) = fragment_of(block) {
                            let chars = fragment
                                .runs
                                .iter()
                                .map(|run| run.text.chars().count())
                                .sum::<usize>();
                            pieces.push((card_fragment(&fragment), chars));
                        }
                    }
                }
            }
        }
        None => {
            if !post.text.trim().is_empty() {
                pieces.push((card_para(&linkify(&post.text)), post.text.chars().count()));
            }
        }
    }

    let mut html = String::new();
    let mut used = 0usize;
    for (piece, chars) in pieces {
        if used + chars > budget {
            html.push_str("<p class=\"more\">…… 完整内容见原链接</p>");
            return html;
        }
        used += chars;
        html.push_str(&piece);
    }
    html
}

fn card_fragment(fragment: &Fragment) -> String {
    let inner = runs_html(&fragment.runs);
    match fragment.prefix.as_str() {
        "<b>" => format!("<h2>{inner}</h2>"),
        "<blockquote>" => format!("<blockquote>{inner}</blockquote>"),
        "<pre>" => format!("<pre>{inner}</pre>"),
        prefix if prefix.starts_with("• ") || prefix.ends_with(". ") && prefix.len() <= 4 => {
            format!("<p class=\"item\">{}{inner}</p>", escape(prefix))
        }
        _ => card_para(&inner),
    }
}

fn card_para(inner: &str) -> String {
    format!("<p>{inner}</p>")
}

#[cfg(test)]
#[path = "../tests/unit/render.rs"]
mod tests;
