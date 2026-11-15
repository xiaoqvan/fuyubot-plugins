//! 进程内缓存：最近的群消息、私聊 AI 配置状态机。
//!
//! 两样东西都不落盘——它们只服务「当下这一轮判断」：
//!
//! | 缓存 | 为什么需要 | 失效 |
//! | --- | --- | --- |
//! | 最近消息 | `/spam` 引用可能指向刚被删的广告，`getMessage` 拿不到时靠它兜底 | 每群 20 条，先进先出 |
//! | AI 配置流程 | 私聊里 `/start aiConfig_…` 之后要跨多条消息记住进度 | 10 分钟 |
//!
//! 用户历史文本**不在这里**：它落库（`message_texts`，48 小时 TTL），见
//! [`crate::store::Db::recent_texts`]——进程重启不丢，也不占常驻内存。

use std::collections::HashMap;
use std::sync::Mutex;

use serde_json::{Value, json};

use crate::util::now_ms;

/// 每群保留的最近消息条数。
const RECENT_LIMIT: usize = 20;
/// 私聊配置流程每步的存活时间。
pub const PENDING_TIMEOUT_MS: i64 = 10 * 60 * 1000;

#[derive(Debug, Clone)]
pub struct PendingAiConfig {
    pub chat_id: i64,
    pub step: Step,
    pub api_key: String,
    pub base_url: String,
    pub created_at: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    ApiKey,
    BaseUrl,
    Model,
}

#[derive(Default)]
struct Inner {
    recent: HashMap<i64, Vec<(i64, Value)>>,
    pending: HashMap<i64, PendingAiConfig>,
}

/// 内存缓存句柄。
///
/// 「群类型 / 我的管理员权限」不归这里管：直接读宿主缓存（`fuyubot.host.cache`），
/// 那张表由 TDLib 更新实时维护，插件不再自己攒一份 TTL 缓存。
#[derive(Default)]
pub struct Cache {
    inner: Mutex<Inner>,
}

impl Cache {
    pub fn new() -> Self {
        Self::default()
    }

    // ── 最近消息 ──────────────────────────────────────────────────

    /// 记一条最近消息（**只留兜底路径用得到的字段**，不存整条 TDLib 消息）。
    pub fn remember_message(&self, chat_id: i64, message: &Value) {
        let Some(message_id) = message.get("id").and_then(Value::as_i64) else {
            return;
        };
        let Ok(mut inner) = self.inner.lock() else {
            return;
        };
        let list = inner.recent.entry(chat_id).or_default();
        list.retain(|(id, _)| *id != message_id);
        list.push((message_id, slim_message(message)));
        if list.len() > RECENT_LIMIT {
            let overflow = list.len() - RECENT_LIMIT;
            list.drain(0..overflow);
        }
    }

    /// 某群最近消息的 id（旧 → 新）。
    ///
    /// 自动点赞检测拿它当「连续回应」的基准：删除消息时由
    /// [`Self::forget_messages`] 从里面摘掉，被删的消息不会打断连续性。
    pub fn recent_ids(&self, chat_id: i64) -> Vec<i64> {
        let Ok(inner) = self.inner.lock() else {
            return Vec::new();
        };
        inner
            .recent
            .get(&chat_id)
            .map(|list| list.iter().map(|(id, _)| *id).collect())
            .unwrap_or_default()
    }

    /// 消息被删除时把它从最近消息里摘掉（否则它会挡住「连续」判定）。
    pub fn forget_messages(&self, chat_id: i64, message_ids: &[i64]) {
        let Ok(mut inner) = self.inner.lock() else {
            return;
        };
        let Some(list) = inner.recent.get_mut(&chat_id) else {
            return;
        };
        list.retain(|(id, _)| !message_ids.contains(id));
    }

    pub fn recent_message(&self, chat_id: i64, message_id: i64) -> Option<Value> {
        let inner = self.inner.lock().ok()?;
        inner
            .recent
            .get(&chat_id)?
            .iter()
            .find(|(id, _)| *id == message_id)
            .map(|(_, message)| message.clone())
    }

    /// 某条已缓存消息所在的话题（论坛群组）。
    ///
    /// 发送提示 / 处置通知时用它把消息送回**同一条话题**，而不是 General。
    pub fn topic_of(&self, chat_id: i64, message_id: i64) -> Option<crate::simplify::Topic> {
        let inner = self.inner.lock().ok()?;
        let message = inner
            .recent
            .get(&chat_id)?
            .iter()
            .find(|(id, _)| *id == message_id)?
            .1
            .clone();
        crate::text::topic_of(&message)
    }

    // ── 私聊 AI 配置流程 ──────────────────────────────────────────

    pub fn begin_pending(&self, user_id: i64, chat_id: i64) {
        let Ok(mut inner) = self.inner.lock() else {
            return;
        };
        inner
            .pending
            .retain(|_, pending| now_ms() - pending.created_at <= PENDING_TIMEOUT_MS);
        inner.pending.insert(
            user_id,
            PendingAiConfig {
                chat_id,
                step: Step::ApiKey,
                api_key: String::new(),
                base_url: String::new(),
                created_at: now_ms(),
            },
        );
    }

    /// 取一份可变引用（配置流程要一步一步往前推）。
    ///
    /// 过期条目在这里顺手清掉——流程本来就是「10 分钟内不输入就作废」。
    pub fn with_pending<R>(
        &self,
        user_id: i64,
        f: impl FnOnce(&mut PendingAiConfig) -> R,
    ) -> Option<R> {
        let mut inner = self.inner.lock().ok()?;
        let expired = inner
            .pending
            .get(&user_id)
            .is_some_and(|pending| now_ms() - pending.created_at > PENDING_TIMEOUT_MS);
        if expired {
            inner.pending.remove(&user_id);
            return None;
        }
        inner.pending.get_mut(&user_id).map(f)
    }

    /// 取一份快照（不在锁里做事：发消息 / 调 AI 都可能很慢）。
    pub fn pending_snapshot(&self, user_id: i64) -> Option<PendingAiConfig> {
        self.with_pending(user_id, |pending| pending.clone())
    }

    /// 写回快照；`None` 表示清空。
    pub fn store_pending(&self, user_id: i64, pending: Option<PendingAiConfig>) {
        if let Ok(mut inner) = self.inner.lock() {
            match pending {
                Some(pending) => {
                    inner.pending.insert(user_id, pending);
                }
                None => {
                    inner.pending.remove(&user_id);
                }
            }
        }
    }

    pub fn clear_pending(&self, user_id: i64) {
        if let Ok(mut inner) = self.inner.lock() {
            inner.pending.remove(&user_id);
        }
    }
}

/// 把一条 TDLib 消息裁成「兜底路径真正会读的字段」。
///
/// 两个调用方（`/spam` 的引用兜底、访客机器人连带封禁）只用到：
/// - `id` / `chat_id` / `sender_id`：认人、认消息；
/// - `content`：`/spam` 要按类型重新组装 AI 入参（图片还要从 `sizes` 里取文件 id）；
/// - `reply_to`：引用来源与引文；
/// - `topic_id`：论坛群组里把回复送回同一条话题；
/// - `forward_info.origin`：只读 `@type` / `chat_id` / `sender_chat_id` 三个键。
///
/// 其余字段（发送状态、编辑时间、话题、相册 id、作者签名……）一律不留：
/// 每群 20 条、每条都是整段 JSON 的话，转发消息带 origin 的聊天对象会白占一大截。
fn slim_message(message: &Value) -> Value {
    let mut slim = json!({});
    for key in [
        "id",
        "chat_id",
        "sender_id",
        "content",
        "reply_to",
        "topic_id",
    ] {
        if let Some(value) = message.get(key) {
            slim[key] = value.clone();
        }
    }
    if let Some(origin) = message.pointer("/forward_info/origin") {
        let mut kept = json!({});
        for key in ["@type", "chat_id", "sender_chat_id"] {
            if let Some(value) = origin.get(key) {
                kept[key] = value.clone();
            }
        }
        slim["forward_info"] = json!({ "origin": kept });
    }
    slim
}

#[cfg(test)]
#[path = "../tests/unit/cache.rs"]
mod tests;
