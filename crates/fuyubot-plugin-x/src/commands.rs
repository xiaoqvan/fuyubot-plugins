//! 命令与自动识别：/x 显式触发 + 聊天里出现 X 链接就自动解析。
//!
//! 两个入口共用同一个处理函数（[handle]）：解析 → 发送 → 一行总结日志。
//!
//! ## 只认 X 的域名
//!
//! 必须按 **host** 判断，不能用子串 contains：https://x.com.evil.test/status/1
//! 这种「把目标域名当前缀」的链接用 contains 会命中，等于替别人家的域名触发解析。
//! 所以 [parse_url] 先把结构拆开（scheme / host / path），host 必须是白名单里的
//! 那六个之一，path 必须形如 …/status/<数字>。
//!
//! ## 自动识别的过滤
//!
//! 群里「发送人是频道身份（messageSenderChat）**且**这条是转发消息」时跳过：
//! 那不是在群里正常发链接，而是把频道的转发链带进来，替它自动产出内容没有意义。

use std::sync::Arc;

use fuyubot_abi::HostApi;

use crate::model::{self, Post};
use crate::runtime::Runtime;
use crate::send;

/// 命令名（/x）。
pub const COMMAND: &str = "x";

/// /x 在宿主命令表里的一行说明。
pub const COMMAND_DESCRIPTION: &str = "解析 X / Twitter 文章，发图片与视频";

/// 宿主预置的**命令能力表**：插件经它的 register 方法登记命令。
const HOST_COMMANDS: &str = "fuyubot.host.commands";

/// X 系的域名白名单。subdomain 只列真实存在的这几个，不做「任意子域」。
const HOSTS: &[&str] = &[
    "x.com",
    "www.x.com",
    "mobile.x.com",
    "twitter.com",
    "www.twitter.com",
    "mobile.twitter.com",
];

/// 一条认出来的 X 链接：只需要帖子 id（解析接口按 id 取）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Link {
    pub id: String,
}

impl Link {
    /// 规范化后的原帖地址（写日志 / 兜底展示用）。
    pub fn url(&self) -> String {
        format!("https://x.com/i/status/{}", self.id)
    }
}

/// 一段文本里第一条 X 链接。
///
/// 允许不带 scheme（x.com/…）；中文标点、括号这类紧贴链接的字符会被剪掉。
pub fn link_of(text: &str) -> Option<Link> {
    for (start, has_scheme) in url_starts(text) {
        let rest = &text[start..];
        let raw = rest[..url_end(rest)].trim_end_matches(|ch: char| {
            matches!(ch, '.' | ',' | ';' | ':' | '!' | '?' | ')' | ']' | '>')
        });
        if raw.len() <= "https://".len() {
            continue;
        }
        let candidate = if has_scheme {
            raw.to_owned()
        } else {
            format!("https://{raw}")
        };
        if let Some(link) = parse_url(&candidate) {
            return Some(link);
        }
    }
    None
}

/// 文本里所有候选链接的起始位置，以及它带不带 scheme。
///
/// 裸域名（x.com/…）必须从一个真正的域名边界开始：notx.com/… 里的 x.com/ 不算，
/// 否则「把目标域名当后缀」的链接会被误认。
fn url_starts(text: &str) -> Vec<(usize, bool)> {
    let mut starts: Vec<(usize, bool)> = Vec::new();
    for needle in ["https://", "http://"] {
        starts.extend(text.match_indices(needle).map(|(index, _)| (index, true)));
    }
    for needle in ["x.com/", "twitter.com/"] {
        for (index, _) in text.match_indices(needle) {
            let boundary = match text[..index].chars().next_back() {
                None => true,
                Some(ch) => {
                    !(ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | '_' | '/' | '@'))
                }
            };
            if boundary {
                starts.push((index, false));
            }
        }
    }
    starts.sort_unstable();
    starts.dedup();
    starts
}

/// URL 在文本里到哪儿结束：空白、非 ASCII（中文标点常常紧贴链接）和几个引号类字符。
fn url_end(rest: &str) -> usize {
    rest.char_indices()
        .find(|(_, ch)| {
            ch.is_whitespace()
                || !ch.is_ascii()
                || matches!(ch, '"' | '\'' | '(' | ')' | '<' | '>' | '[' | ']')
        })
        .map(|(index, _)| index)
        .unwrap_or(rest.len())
}

/// 一个 URL 是不是 X 的帖子 / 文章链接。
pub fn parse_url(url: &str) -> Option<Link> {
    let (scheme, rest) = url.split_once("://")?;
    if scheme != "http" && scheme != "https" {
        return None;
    }
    let (authority, path) = match rest.split_once('/') {
        Some((authority, path)) => (authority, path),
        None => (rest, ""),
    };
    // 去掉 user@ 与 :port，只留主机名。
    let host = authority
        .rsplit('@')
        .next()?
        .split(':')
        .next()?
        .to_ascii_lowercase();
    if !HOSTS.contains(&host.as_str()) {
        return None;
    }
    // 查询串 / 片段对 id 没影响：?s=20 是分享参数。
    let path = path.split(['?', '#']).next().unwrap_or("");
    let segments: Vec<&str> = path.split('/').filter(|part| !part.is_empty()).collect();
    // 认 …/status/<id> 与 …/article/<id>；<id> 后面还可能跟 /photo/1 这类后缀。
    let id = segments
        .windows(2)
        .find(|pair| pair[0] == "status" || pair[0] == "article")
        .map(|pair| pair[1])?;
    if id.is_empty() || !id.chars().all(|ch| ch.is_ascii_digit()) {
        return None;
    }
    Some(Link { id: id.to_owned() })
}

/// 按 id 取一条帖子（含 Article）。
pub async fn fetch(rt: &Runtime, link: &Link) -> Result<Post, String> {
    let url = format!("{}/status/{}", rt.config.api_base, link.id);
    crate::log::debug(&format!("解析 {url}"));
    let json = rt
        .http
        .get_json(&url)
        .await
        .map_err(|error| error.to_string())?;
    model::from_api(&json)
}

/// 处理一条链接：解析 → 发送 → 总结日志。失败时把原因贴回聊天（用户看不到宿主日志）。
async fn handle(rt: Arc<Runtime>, chat_id: i64, link: Link) {
    let post = match fetch(&rt, &link).await {
        Ok(post) => post,
        Err(error) => {
            crate::log::warn(&format!("解析 {} 失败：{error}", link.url()));
            rt.say(chat_id, &format!("❌ 解析失败：{error}")).await;
            return;
        }
    };
    let media = post.media_for_send().len();
    crate::log::info(&format!(
        "解析到「{}」（{}，媒体 {} 个）",
        crate::log::shorten(&post.title(), 60),
        if post.article.is_some() {
            "文章"
        } else {
            "推文"
        },
        media
    ));
    match send::deliver(&rt, &post, chat_id).await {
        Ok(stats) => crate::log::info(&format!("已发送：{}", stats.summary())),
        Err(error) => {
            crate::log::warn(&format!("发送失败：{error}"));
            rt.say(chat_id, &format!("❌ 发送失败：{error}")).await;
        }
    }
}

/// 把 /x 登记进宿主命令表。
///
/// 走宿主预置的 fuyubot.host.commands（call_service），入参
/// {name, description, role, scopes}，回 {"ok":true,…}。
/// 失败只记 Warn、不返回 Err：/x 本身能正常工作，只是 /help 里少一行说明。
pub fn register(host: HostApi) {
    let payload = serde_json::json!({
        "name": COMMAND,
        "description": COMMAND_DESCRIPTION,
        "role": "user",
        "scopes": ["private", "group", "channel"],
    })
    .to_string();
    match host.call_service(HOST_COMMANDS, "register", &payload) {
        Ok(reply) => {
            let ok = serde_json::from_str::<serde_json::Value>(&reply)
                .ok()
                .and_then(|value| value.get("ok").and_then(|ok| ok.as_bool()))
                .unwrap_or(false);
            if ok {
                crate::log::info(&format!("/{COMMAND} 已登记进宿主命令表"));
            } else {
                crate::log::warn(&format!("/{COMMAND} 登记被拒：{reply}"));
            }
        }
        Err(error) => crate::log::warn(&format!(
            "/{COMMAND} 没能登记进宿主命令表（{HOST_COMMANDS}）：{error}"
        )),
    }
}

// ── 消息订阅 ──────────────────────────────────────────────────────

use std::ffi::{CStr, c_char, c_void};

use fuyubot_abi::SubscriptionHandle;
use serde_json::Value;

use crate::simplify::{self, Command};

/// 一条待判断的消息（只带用得上的那几个字段）。
#[derive(Debug, Clone)]
pub struct Message {
    pub chat_id: i64,
    pub text: String,
    pub is_outgoing: bool,
    /// 发送者是频道（messageSenderChat）——群里匿名频道马甲也满足这一点。
    pub sender_is_chat: bool,
    /// 这条消息是转发的。
    pub is_forwarded: bool,
    /// 真正的频道帖：TDLib is_channel_post 为真。
    pub is_channel_post: bool,
    /// 文本以 / 开头时解析出来的命令。
    pub command: Option<Command>,
}

/// 一条活着的消息订阅：**drop 就退订**。
pub struct MessageSubscription {
    host: HostApi,
    handle: SubscriptionHandle,
    _handler: Box<Box<MessageFn>>,
}

impl Drop for MessageSubscription {
    fn drop(&mut self) {
        if self.handle.is_valid() {
            self.host.off(self.handle);
        }
    }
}

/// 订阅回调的签名。
type MessageFn = dyn Fn(&Message) + Send + Sync;

/// 订阅 updateNewMessage，把每条文本消息交给 [dispatch]。
///
/// 回调跑在宿主的接收循环线程上，所以闭包里只做分流 + submit，不做任何网络请求。
pub fn subscribe(host: HostApi, rt: Arc<Runtime>) -> Result<MessageSubscription, String> {
    let mut owned: Box<Box<MessageFn>> = Box::new(Box::new(move |message: &Message| {
        dispatch(&rt, message);
    }));
    // 递给 ABI 的是这个 Box 的地址：它活着，指针就稳定。
    let user_data = (&raw mut *owned).cast::<c_void>();
    let handle = host.on("updateNewMessage", None, trampoline, user_data);
    if !handle.is_valid() {
        return Err("订阅 updateNewMessage 失败".to_owned());
    }
    Ok(MessageSubscription {
        host,
        handle,
        _handler: owned,
    })
}

/// 命令订阅的 C 回调：把负载解成 [Message] 再交给闭包。
extern "C-unwind" fn trampoline(user_data: *mut c_void, payload: *const c_char) -> i32 {
    if user_data.is_null() || payload.is_null() {
        return -1;
    }
    // SAFETY: user_data 是 subscribe 里那个 Box，订阅活多久它就活多久。
    let state = unsafe { &*(user_data as *const Box<MessageFn>) };
    // SAFETY: 宿主保证 payload 是 NUL 结尾的临时字符串。
    let text = unsafe { CStr::from_ptr(payload) }.to_string_lossy();
    let Ok(update) = serde_json::from_str::<Value>(&text) else {
        return 0;
    };
    if let Some(message) = message_of(&update) {
        (**state)(&message);
    }
    0
}

/// 从 updateNewMessage 的负载里取出用得上的字段。
///
/// 字段路径照真 td_api.tl：updateNewMessage.message → content:messageText →
/// text:formattedText → text:string。
fn message_of(update: &Value) -> Option<Message> {
    let message = update.get("message")?;
    let chat_id = message.get("chat_id")?.as_i64()?;
    let is_channel_post = simplify::channel_post_of(message);
    let is_outgoing = message
        .get("is_outgoing")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let sender_is_chat = message
        .get("sender_id")
        .and_then(|sender| sender.get("@type"))
        .and_then(Value::as_str)
        == Some("messageSenderChat");
    let is_forwarded = message.get("forward_info").is_some();
    // 只认文本消息：图片 / 投票 / 贴纸这类没有可解析的正文。
    let text = message
        .get("content")
        .filter(|content| content.get("@type").and_then(Value::as_str) == Some("messageText"))
        .and_then(|content| content.get("text"))
        .and_then(|text| text.get("text"))
        .and_then(Value::as_str)
        .unwrap_or("");
    if text.is_empty() {
        return None;
    }
    Some(Message {
        chat_id,
        text: text.to_owned(),
        is_outgoing,
        sender_is_chat,
        is_forwarded,
        is_channel_post,
        command: simplify::command_of(update),
    })
}

/// 分流：/x 命令 → 命令分支；否则（开着自动识别时）扫链接。
fn dispatch(rt: &Arc<Runtime>, message: &Message) {
    if let Some(command) = &message.command {
        if !command.is(COMMAND) {
            return;
        }
        // 点名给别人的（/x@别的Bot）不抢；裸命令与点名自己的都算。
        if !command.is_for(&simplify::my_usernames(&rt.host)) {
            return;
        }
        let chat_id = command.chat_id;
        match link_of(&command.args).or_else(|| link_of(&command.text)) {
            Some(link) => {
                let rt = Arc::clone(rt);
                rt.submit(move |rt| async move { handle(rt, chat_id, link).await });
            }
            None => {
                let rt = Arc::clone(rt);
                rt.submit(move |rt| async move {
                    rt.say(chat_id, "用法：/x <X / Twitter 链接>").await;
                });
            }
        }
        return;
    }

    if !rt.config.auto_parse {
        return;
    }
    // 自己发的不解析（否则机器人自己贴的链接会被再解析一遍，无限套娃）；
    // 频道帖例外——在自己频道里贴链接本来就是要插件产出内容。
    if message.is_outgoing && !message.is_channel_post {
        return;
    }
    if !message.is_channel_post && message.sender_is_chat && message.is_forwarded {
        return;
    }
    if message.text.trim_start().starts_with('/') {
        return;
    }
    let Some(link) = link_of(&message.text) else {
        return;
    };
    let chat_id = message.chat_id;
    let rt = Arc::clone(rt);
    rt.submit(move |rt| async move { handle(rt, chat_id, link).await });
}

#[cfg(test)]
#[path = "../tests/unit/commands.rs"]
mod tests;
