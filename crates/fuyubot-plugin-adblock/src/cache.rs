//! 进程内缓存：最近的群消息、用户历史文本、私聊 AI 配置状态机。
//!
//! 三样东西都不落盘——它们只服务「当下这一轮判断」：
//!
//! | 缓存 | 为什么需要 | 失效 |
//! | --- | --- | --- |
//! | 最近消息 | `/spam` 引用可能指向刚被删的广告，`getMessage` 拿不到时靠它兜底 | 每群 10 条，先进先出 |
//! | 用户历史 | AI 审核时附上「这个人刚才在聊什么」，降低正常聊天误封 | 每人每群 6 条 |
//! | AI 配置流程 | 私聊里 `/start aiConfig_…` 之后要跨多条消息记住进度 | 30 分钟 |

use std::collections::HashMap;
use std::sync::Mutex;

use serde_json::Value;

use crate::util::now_ms;

/// 每群保留的最近消息条数。
const RECENT_LIMIT: usize = 10;
/// 每人每群保留的历史文本条数（多存 1 条，排除当前消息后仍能给出 5 条）。
const HISTORY_LIMIT: usize = 6;
/// 私聊配置流程的存活时间。
pub const PENDING_TIMEOUT_MS: i64 = 30 * 60 * 1000;

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
    history: HashMap<(i64, i64), Vec<(i64, String)>>,
    pending: HashMap<i64, PendingAiConfig>,
    /// `chat_id → (是不是群, 采集时间)`
    chat_kind: HashMap<i64, (bool, i64)>,
    /// `chat_id → (我是不是管理员, 采集时间)`
    admin: HashMap<i64, (bool, i64)>,
}

/// 「群类型 / 我的管理员权限」这两项缓存多久。
///
/// 它们每条消息都要问一次，而 TDLib 本地库虽然快，4 个 worker 也架不住每群每消息
/// 各跑两趟。60 秒的窗口对「刚把 Bot 提成管理员」这种操作足够短。
const PERMISSION_TTL_MS: i64 = 60_000;

/// 内存缓存句柄。
#[derive(Default)]
pub struct Cache {
    inner: Mutex<Inner>,
}

impl Cache {
    pub fn new() -> Self {
        Self::default()
    }

    // ── 最近消息 ──────────────────────────────────────────────────

    pub fn remember_message(&self, chat_id: i64, message: &Value) {
        let Some(message_id) = message.get("id").and_then(Value::as_i64) else {
            return;
        };
        let Ok(mut inner) = self.inner.lock() else {
            return;
        };
        let list = inner.recent.entry(chat_id).or_default();
        list.retain(|(id, _)| *id != message_id);
        list.push((message_id, message.clone()));
        if list.len() > RECENT_LIMIT {
            let overflow = list.len() - RECENT_LIMIT;
            list.drain(0..overflow);
        }
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

    // ── 用户历史文本 ──────────────────────────────────────────────

    pub fn remember_text(&self, chat_id: i64, user_id: i64, message_id: i64, text: &str) {
        let text = text.trim();
        if text.is_empty() {
            return;
        }
        let Ok(mut inner) = self.inner.lock() else {
            return;
        };
        let list = inner.history.entry((chat_id, user_id)).or_default();
        list.retain(|(id, _)| *id != message_id);
        list.push((message_id, text.to_owned()));
        if list.len() > HISTORY_LIMIT {
            let overflow = list.len() - HISTORY_LIMIT;
            list.drain(0..overflow);
        }
    }

    /// 最近 `limit` 条文本（按时间从旧到新，排除 `exclude` 那条）。
    pub fn recent_texts(
        &self,
        chat_id: i64,
        user_id: i64,
        exclude: Option<i64>,
        limit: usize,
    ) -> Vec<String> {
        let Ok(inner) = self.inner.lock() else {
            return Vec::new();
        };
        let Some(list) = inner.history.get(&(chat_id, user_id)) else {
            return Vec::new();
        };
        let mut texts: Vec<String> = list
            .iter()
            .filter(|(id, _)| Some(*id) != exclude)
            .map(|(_, text)| text.clone())
            .collect();
        let limit = limit.max(1);
        if texts.len() > limit {
            texts.drain(0..texts.len() - limit);
        }
        texts
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
    /// 过期条目在这里顺手清掉——流程本来就是「30 分钟内不输入就作废」。
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

    // ── 群类型 / 管理员权限 ──────────────────────────────────────

    pub fn chat_is_group(&self, chat_id: i64) -> Option<bool> {
        let inner = self.inner.lock().ok()?;
        let (value, at) = inner.chat_kind.get(&chat_id)?;
        (now_ms() - at <= PERMISSION_TTL_MS).then_some(*value)
    }

    pub fn store_chat_is_group(&self, chat_id: i64, value: bool) {
        if let Ok(mut inner) = self.inner.lock() {
            inner.chat_kind.insert(chat_id, (value, now_ms()));
        }
    }

    pub fn admin_ok(&self, chat_id: i64) -> Option<bool> {
        let inner = self.inner.lock().ok()?;
        let (value, at) = inner.admin.get(&chat_id)?;
        (now_ms() - at <= PERMISSION_TTL_MS).then_some(*value)
    }

    pub fn store_admin_ok(&self, chat_id: i64, value: bool) {
        if let Ok(mut inner) = self.inner.lock() {
            inner.admin.insert(chat_id, (value, now_ms()));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn recent_messages_are_capped() {
        let cache = Cache::new();
        for id in 0..20 {
            cache.remember_message(-1, &json!({"id": id}));
        }
        assert!(cache.recent_message(-1, 19).is_some());
        assert!(cache.recent_message(-1, 0).is_none());
    }

    #[test]
    fn history_excludes_current() {
        let cache = Cache::new();
        cache.remember_text(-1, 7, 1, "a");
        cache.remember_text(-1, 7, 2, "b");
        let texts = cache.recent_texts(-1, 7, Some(2), 5);
        assert_eq!(texts, vec!["a"]);
    }

    #[test]
    fn pending_expires() {
        let cache = Cache::new();
        cache.begin_pending(7, -100);
        assert!(cache.pending_snapshot(7).is_some());
        cache.clear_pending(7);
        assert!(cache.pending_snapshot(7).is_none());
    }
}
