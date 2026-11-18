//! TDLib 名字解析：更新缓存优先，`getChat` / `getUser` 兜底。
//!
//! 消息本身只带 `chat_id` / `sender_id`，标题和名字得另外拿。日志是热路径，所以分三层：
//!
//! 1. **更新优先**：`updateNewChat` / `updateChatTitle` / `updateUser` 一到，就把标题 /
//!    名字记进 [`Book`]。改名、改群名会实时刷新，而且**不用发请求**——这正是
//!    「TDLib 一时答不上 `getChat`，日志里就全是『未知聊天』」的根治办法；
//! 2. **私聊不查**：`chat_id > 0` 时聊天标签固定是 `Private`，省掉一次 `getChat`；
//! 3. **兜底直连**：`Book` 里没有（刚启动、还没收到 `updateNewChat`）才真的去问
//!    `getChat` / `getUser`，拿到的结果回填 `Book`。失败进 30 秒负缓存，避免 TDLib
//!    暂时不可用时每条消息都去撞 3 秒超时。
//!
//! 所有调用都在插件自己的工作线程里 `await`，不阻塞宿主的接收线程。

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use fuyubot_abi::{HostApi, ResponseRouter};
use serde_json::{Value, json};

use crate::log;

/// 单次请求的等待上限。正常 `getChat` / `getUser` 是本地库查询，毫秒级返回；
/// 3 秒是「TDLib 卡住」时的兜底，别把停机拖太久。
const CALL_TIMEOUT: Duration = Duration::from_secs(3);

/// 名字缓存有效期。改名 / 改群标题最多 10 分钟后才反映到日志里，够用。
const NAME_TTL: Duration = Duration::from_secs(10 * 60);

/// 查不到（TDLib 报错 / 超时）的负缓存有效期：短一点，恢复后很快能再试。
const MISSING_TTL: Duration = Duration::from_secs(30);

/// 缓存条目上限。超过就整表清空——日志插件不需要精确的 LRU，
/// 只要内存不随「见过多少个用户」无限涨。
const MAX_ENTRIES: usize = 4096;

/// [`Book`] 每张表的上限：超过就整表清空，理由同上。
pub const BOOK_CAPACITY: usize = 4096;

/// 属性缓存直接吃的更新类型（订阅侧也用它，别只改一边）。
pub const NAME_UPDATES: [&str; 3] = ["updateNewChat", "updateChatTitle", "updateUser"];

/// 工作线程与订阅闭包共用的 [`Book`]。
pub type SharedBook = Arc<Mutex<Book>>;

// ── 更新驱动的标题 / 名字 ──────────────────────────────────────────

/// 从更新里攒下来的标题 / 名字。
///
/// 只存正结果：更新说「叫什么」就记什么，改名由下一条更新覆盖，所以不需要 TTL。
/// 这张表是**先于** `getChat` / `getUser` 被查的——只要更新到过，就不会去发请求，
/// 也就不会因为一次超时把日志打成「未知聊天」。
#[derive(Default)]
pub struct Book {
    chats: HashMap<i64, String>,
    users: HashMap<i64, String>,
}

impl Book {
    pub fn new() -> Self {
        Self::default()
    }

    /// 吃一条 TDLib 更新；认识的类型返回 `true`（不认识的原样放过）。
    pub fn absorb(&mut self, update: &Value) -> bool {
        match kind(update) {
            "updateNewChat" => {
                if let Some(chat) = update.get("chat")
                    && let (Some(id), Some(title)) = (
                        chat.get("id").and_then(Value::as_i64),
                        clean(chat.get("title").and_then(Value::as_str)),
                    )
                {
                    self.set_chat(id, title);
                }
                true
            }
            "updateChatTitle" => {
                if let (Some(id), Some(title)) = (
                    update.get("chat_id").and_then(Value::as_i64),
                    clean(update.get("title").and_then(Value::as_str)),
                ) {
                    self.set_chat(id, title);
                }
                true
            }
            "updateUser" => {
                if let Some(user) = update.get("user")
                    && let (Some(id), Some(name)) =
                        (user.get("id").and_then(Value::as_i64), display_name(user))
                {
                    self.set_user(id, &name);
                }
                true
            }
            _ => false,
        }
    }

    /// 更新里见过的聊天标题。
    pub fn chat_title(&self, chat_id: i64) -> Option<&str> {
        self.chats.get(&chat_id).map(String::as_str)
    }

    /// 更新里见过的用户名字。
    pub fn user_name(&self, user_id: i64) -> Option<&str> {
        self.users.get(&user_id).map(String::as_str)
    }

    #[cfg(test)]
    pub fn len(&self) -> (usize, usize) {
        (self.chats.len(), self.users.len())
    }

    fn set_chat(&mut self, chat_id: i64, title: &str) {
        if self.chats.len() >= BOOK_CAPACITY && !self.chats.contains_key(&chat_id) {
            self.chats.clear();
        }
        self.chats.insert(chat_id, title.to_owned());
    }

    fn set_user(&mut self, user_id: i64, name: &str) {
        if self.users.len() >= BOOK_CAPACITY && !self.users.contains_key(&user_id) {
            self.users.clear();
        }
        self.users.insert(user_id, name.to_owned());
    }
}

/// 拿 `Book` 里的聊天标题。锁只在取值期间持有，**不要**跨 `.await`。
fn book_chat(book: &SharedBook, chat_id: i64) -> Option<String> {
    let book = book.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    book.chat_title(chat_id).map(str::to_owned)
}

/// 拿 `Book` 里的用户名。锁只在取值期间持有。
fn book_user(book: &SharedBook, user_id: i64) -> Option<String> {
    let book = book.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    book.user_name(user_id).map(str::to_owned)
}

// ── TDLib 门面 ────────────────────────────────────────────────────

/// TDLib 门面：只暴露本插件用到的两种查询，并负责把结果回填 [`Book`]。
pub struct Td {
    host: HostApi,
    router: ResponseRouter,
    /// 第一次查询失败时提示一次。之后静默：塌了就是塌了，每 30 秒报一遍只是噪音。
    warned: AtomicBool,
}

impl Td {
    pub fn new(host: HostApi, router: ResponseRouter) -> Self {
        Self {
            host,
            router,
            warned: AtomicBool::new(false),
        }
    }

    /// 发一条 TDLib 方法并等响应；`error` 响应与超时都变成 `None`。
    async fn call(&self, method: &str, params: Value) -> Option<Value> {
        let Ok((_id, response)) = self.router.invoke_simplified(method, &params.to_string()) else {
            self.warn_once(method);
            return None;
        };
        match tokio::time::timeout(CALL_TIMEOUT, response).await {
            Ok(Ok(value)) if kind(&value) != "error" => Some(value),
            _ => {
                self.warn_once(method);
                None
            }
        }
    }

    /// 查询失败提示一次：让「未知聊天」在日志里留下能查的线索，而不是每条都静默。
    fn warn_once(&self, method: &str) {
        if !self.warned.swap(true, Ordering::Relaxed) {
            log::warn(
                &self.host,
                format!(
                    "{method} 超时或报错：标题 / 名字改用更新缓存（updateNewChat / updateChatTitle / updateUser）补齐，同类失败不再提示"
                ),
            );
        }
    }

    /// `getUser` → `first_name last_name`；没有名字时返回 `None`。
    pub async fn user_name(&self, user_id: i64) -> Option<String> {
        let user = self.call("getUser", json!({ "user_id": user_id })).await?;
        display_name(&user)
    }

    /// `getChat` → `title`；空标题当作查不到。
    pub async fn chat_title(&self, chat_id: i64) -> Option<String> {
        let chat = self.call("getChat", json!({ "chat_id": chat_id })).await?;
        clean(chat.get("title").and_then(Value::as_str)).map(str::to_owned)
    }
}

// ── 带 TTL 的名字缓存 ──────────────────────────────────────────────

/// 名字缓存：**先查 `Book`**，再查 TTL 表，最后才问 TDLib；查到的正结果回填 `Book`。
///
/// 只有工作线程会碰它，所以 TTL 表不需要锁。
pub struct Names {
    users: HashMap<i64, Entry>,
    chats: HashMap<i64, Entry>,
    book: SharedBook,
}

struct Entry {
    value: Option<String>,
    expires: Instant,
}

impl Names {
    pub fn new(book: SharedBook) -> Self {
        Self {
            users: HashMap::new(),
            chats: HashMap::new(),
            book,
        }
    }

    pub async fn user_name(&mut self, td: &Td, user_id: i64) -> Option<String> {
        if let Some(name) = book_user(&self.book, user_id) {
            return Some(name);
        }
        if let Some(value) = fresh(&self.users, user_id) {
            return value;
        }
        let value = td.user_name(user_id).await;
        if let Some(name) = &value {
            remember_book(&self.book, |book| book.set_user(user_id, name));
        }
        remember(&mut self.users, user_id, value.clone());
        value
    }

    pub async fn chat_title(&mut self, td: &Td, chat_id: i64) -> Option<String> {
        if let Some(title) = book_chat(&self.book, chat_id) {
            return Some(title);
        }
        if let Some(value) = fresh(&self.chats, chat_id) {
            return value;
        }
        let value = td.chat_title(chat_id).await;
        if let Some(title) = &value {
            remember_book(&self.book, |book| book.set_chat(chat_id, title));
        }
        remember(&mut self.chats, chat_id, value.clone());
        value
    }
}

/// 把一次成功的查询回填 `Book`（锁只在这一小段持有）。
fn remember_book(book: &SharedBook, write: impl FnOnce(&mut Book)) {
    let mut book = book.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    write(&mut book);
}

/// 命中且未过期时返回值（含「查过但没有」的负缓存）。
fn fresh(map: &HashMap<i64, Entry>, id: i64) -> Option<Option<String>> {
    let entry = map.get(&id)?;
    (entry.expires > Instant::now()).then(|| entry.value.clone())
}

fn remember(map: &mut HashMap<i64, Entry>, id: i64, value: Option<String>) {
    if map.len() >= MAX_ENTRIES {
        map.clear();
    }
    let ttl = if value.is_some() {
        NAME_TTL
    } else {
        MISSING_TTL
    };
    map.insert(
        id,
        Entry {
            value,
            expires: Instant::now() + ttl,
        },
    );
}

// ── 小工具 ────────────────────────────────────────────────────────

fn kind(value: &Value) -> &str {
    value.get("@type").and_then(Value::as_str).unwrap_or("")
}

/// 去掉首尾空白；空串当作没有。
fn clean(text: Option<&str>) -> Option<&str> {
    text.map(str::trim).filter(|text| !text.is_empty())
}

/// `first_name last_name`；两边都空时返回 `None`。
fn display_name(user: &Value) -> Option<String> {
    let name = ["first_name", "last_name"]
        .iter()
        .filter_map(|key| user.get(key).and_then(Value::as_str))
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    (!name.is_empty()).then_some(name)
}

#[cfg(test)]
#[path = "../tests/unit/td.rs"]
mod tests;
