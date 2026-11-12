//! TDLib 名字解析：`getChat` / `getUser` + 短期缓存。
//!
//! 消息本身只带 `chat_id` / `sender_id`，标题和名字要再问一次 TDLib。日志是热路径，
//! 所以：
//!
//! - **私聊不查**：`chat_id > 0` 时聊天标签固定是 `Private`，省掉一次 `getChat`；
//! - **按 id 缓存**：同一个群 / 用户 10 分钟内只查一次；查失败缓存 30 秒，
//!   避免 TDLib 暂时不可用时每条消息都去撞 3 秒超时；
//! - **不阻塞宿主的接收线程**：所有调用都在插件自己的工作线程里 `await`。

use std::collections::HashMap;
use std::time::{Duration, Instant};

use fuyubot_abi::ResponseRouter;
use serde_json::{Value, json};

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

/// TDLib 门面：只暴露本插件用到的两种查询。
pub struct Td {
    router: ResponseRouter,
}

impl Td {
    pub fn new(router: ResponseRouter) -> Self {
        Self { router }
    }

    /// 发一条 TDLib 方法并等响应；`error` 响应与超时都变成 `None`。
    async fn call(&self, method: &str, params: Value) -> Option<Value> {
        let (_id, response) = self
            .router
            .invoke_simplified(method, &params.to_string())
            .ok()?;
        match tokio::time::timeout(CALL_TIMEOUT, response).await {
            Ok(Ok(value)) if value.get("@type").and_then(Value::as_str) != Some("error") => {
                Some(value)
            }
            _ => None,
        }
    }

    /// `getUser` → `first_name last_name`；没有名字时返回 `None`。
    pub async fn user_name(&self, user_id: i64) -> Option<String> {
        let user = self.call("getUser", json!({ "user_id": user_id })).await?;
        let name = ["first_name", "last_name"]
            .iter()
            .filter_map(|key| user.get(key).and_then(Value::as_str))
            .map(str::trim)
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join(" ");
        (!name.is_empty()).then_some(name)
    }

    /// `getChat` → `title`；空标题当作查不到。
    pub async fn chat_title(&self, chat_id: i64) -> Option<String> {
        let chat = self.call("getChat", json!({ "chat_id": chat_id })).await?;
        let title = chat
            .get("title")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim();
        (!title.is_empty()).then(|| title.to_owned())
    }
}

/// 带 TTL 的名字缓存。只有工作线程会碰它，所以不需要锁。
#[derive(Default)]
pub struct Names {
    users: HashMap<i64, Entry>,
    chats: HashMap<i64, Entry>,
}

struct Entry {
    value: Option<String>,
    expires: Instant,
}

impl Names {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn user_name(&mut self, td: &Td, user_id: i64) -> Option<String> {
        if let Some(value) = fresh(&self.users, user_id) {
            return value;
        }
        let value = td.user_name(user_id).await;
        remember(&mut self.users, user_id, value.clone());
        value
    }

    pub async fn chat_title(&mut self, td: &Td, chat_id: i64) -> Option<String> {
        if let Some(value) = fresh(&self.chats, chat_id) {
            return value;
        }
        let value = td.chat_title(chat_id).await;
        remember(&mut self.chats, chat_id, value.clone());
        value
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresh_entries_are_returned_and_expired_ones_are_not() {
        let mut map = HashMap::new();
        map.insert(
            7,
            Entry {
                value: Some("张三".to_owned()),
                expires: Instant::now() + Duration::from_secs(60),
            },
        );
        assert_eq!(fresh(&map, 7), Some(Some("张三".to_owned())));
        assert_eq!(fresh(&map, 8), None, "没查过");

        map.insert(
            9,
            Entry {
                value: None,
                expires: Instant::now() + Duration::from_secs(60),
            },
        );
        assert_eq!(fresh(&map, 9), Some(None), "负缓存也算命中");
    }

    #[test]
    fn cache_is_cleared_instead_of_growing_without_bound() {
        let mut map = HashMap::new();
        remember(&mut map, 1, Some("a".to_owned()));
        assert_eq!(map.len(), 1);

        // 手工填到上限，再插一条应当触发整表清空。
        for id in 2..=(MAX_ENTRIES as i64) {
            map.insert(
                id,
                Entry {
                    value: None,
                    expires: Instant::now(),
                },
            );
        }
        assert_eq!(map.len(), MAX_ENTRIES);
        remember(&mut map, 9999, Some("b".to_owned()));
        assert_eq!(map.len(), 1);
    }
}
