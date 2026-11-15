//! 「自动点赞机器人」的连续回应统计。
//!
//! 每个群的最近 20 条消息已经在 [`crate::cache::Cache`] 里（`recent_ids`），
//! 这里不重复维护消息列表：只记「谁回应过哪几条」，再看刚回应的那条在最近
//! 消息里能不能凑出 [`CONSECUTIVE_MESSAGES`] 条连续。
//!
//! 被删除的消息会从 cache 的最近消息里摘掉（`forget_messages`），所以
//! 「A B C(已删) D E F」在判定时就是连续的 A B D E F，不会被删掉的那条打断。
//!
//! 命中后进入冷却，同一个账号不会被反复处置；状态只在本进程内存里，
//! 插件重启就清空——这是检测用的短期状态，不值得为它写库。

use std::collections::{HashMap, VecDeque};
use std::sync::{Mutex, MutexGuard};

/// 连续回应多少条消息算自动点赞机器人。
pub const CONSECUTIVE_MESSAGES: usize = 5;
/// 命中后的冷却：冷却期内不再处置同一个账号。
pub const COOLDOWN_MS: i64 = 30 * 60 * 1000;

/// 内存上限：最多跟踪多少个群、每个群多少个用户。
const MAX_CHATS: usize = 1024;
const MAX_USERS_PER_CHAT: usize = 2048;
/// 每个用户最多记多少条回应（最近窗口本身最多 20 条，留点余量）。
const MAX_REACTED: usize = 64;
/// 多久没动静的群 / 用户从状态里清掉。
const IDLE_MS: i64 = 24 * 60 * 60 * 1000;
/// 清理间隔（每来一条回应都扫一遍全表就太浪费了）。
const SWEEP_INTERVAL_MS: i64 = 60 * 1000;

/// 自动点赞检测器。
#[derive(Debug, Default)]
pub struct LikeGuard {
    state: Mutex<State>,
}

#[derive(Debug, Default)]
struct State {
    chats: HashMap<i64, Chat>,
    /// 上一次清理的时间；0 表示还没扫过。
    last_sweep: i64,
}

#[derive(Debug)]
struct Chat {
    users: HashMap<i64, User>,
    last_seen: i64,
}

#[derive(Debug)]
struct User {
    /// 回应过的消息 id（按观察顺序），只保留还在最近窗口里的。
    reacted: VecDeque<i64>,
    last_seen: i64,
    cooldown_until: i64,
}

/// 一次命中。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Detection {
    pub user_id: i64,
    /// 这次凑出来的连续回应条数。
    pub reactions: usize,
}

impl Detection {
    /// 写进日志 / 审计记录的说明。
    pub fn detail(&self) -> String {
        format!("连续回应 {} 条消息", self.reactions)
    }
}

impl LikeGuard {
    pub fn new() -> Self {
        Self::default()
    }

    /// 记一次「某个用户在某个群回应了某条消息」。
    ///
    /// `recent` 是该群最近的 message id（旧 → 新，来自 `Cache::recent_ids`，
    /// 已经剔除被删除的消息）。只有落在 `recent` 里的回应才参与连续判定；
    /// 刚回应的这条必须是其中之一，否则说明它已经滑出窗口，直接忽略。
    pub fn observe(
        &self,
        chat_id: i64,
        user_id: i64,
        message_id: i64,
        recent: &[i64],
        now_ms: i64,
    ) -> Option<Detection> {
        let center = recent.iter().position(|id| *id == message_id)?;
        let mut state = lock(&self.state);
        state.sweep(now_ms);
        if !state.chats.contains_key(&chat_id) {
            if state.chats.len() >= MAX_CHATS {
                state.evict_oldest_chat();
            }
            state.chats.insert(
                chat_id,
                Chat {
                    users: HashMap::new(),
                    last_seen: now_ms,
                },
            );
        }
        let chat = state.chats.get_mut(&chat_id)?;
        chat.last_seen = now_ms;
        if !chat.users.contains_key(&user_id) && chat.users.len() >= MAX_USERS_PER_CHAT {
            return None;
        }
        let user = chat.users.entry(user_id).or_insert_with(|| User {
            reacted: VecDeque::new(),
            last_seen: now_ms,
            cooldown_until: 0,
        });
        user.last_seen = now_ms;

        // 同一条消息的反应变化会把 recent_sender_ids 重发一遍：重复更新不重复记账，
        // 也不重新判定——删除消息后由「下一条新回应」把连续段闭合。
        if user.reacted.contains(&message_id) {
            return None;
        }
        user.reacted.push_back(message_id);
        while user.reacted.len() > MAX_REACTED {
            user.reacted.pop_front();
        }
        // 滑出窗口（或已经删除）的消息不再参与判定。
        user.reacted.retain(|id| recent.contains(id));

        // 以刚回应的这条为中心，向两边数连续段。
        let mut left = center;
        while left > 0 && user.reacted.contains(&recent[left - 1]) {
            left -= 1;
        }
        let mut right = center;
        while right + 1 < recent.len() && user.reacted.contains(&recent[right + 1]) {
            right += 1;
        }
        let reactions = right - left + 1;
        if reactions < CONSECUTIVE_MESSAGES {
            return None;
        }
        if now_ms < user.cooldown_until {
            return None;
        }
        user.cooldown_until = now_ms.saturating_add(COOLDOWN_MS);
        Some(Detection { user_id, reactions })
    }
}

impl State {
    /// 定期清理长时间没动静的群与用户，并给群数量兜底。
    fn sweep(&mut self, now_ms: i64) {
        if now_ms.saturating_sub(self.last_sweep) < SWEEP_INTERVAL_MS {
            return;
        }
        self.last_sweep = now_ms;
        self.chats.retain(|_, chat| {
            chat.users.retain(|_, user| {
                now_ms.saturating_sub(user.last_seen) < IDLE_MS || user.cooldown_until > now_ms
            });
            now_ms.saturating_sub(chat.last_seen) < IDLE_MS || !chat.users.is_empty()
        });
        while self.chats.len() > MAX_CHATS {
            self.evict_oldest_chat();
        }
    }

    /// 丢掉最久没动静的群（MAX_CHATS 兜底）。
    fn evict_oldest_chat(&mut self) {
        let oldest = self
            .chats
            .iter()
            .min_by_key(|(_, chat)| chat.last_seen)
            .map(|(chat_id, _)| *chat_id);
        if let Some(chat_id) = oldest {
            self.chats.remove(&chat_id);
        }
    }
}

/// 毒锁不该让检测崩掉：状态里只有计数，接着用就是。
fn lock(state: &Mutex<State>) -> MutexGuard<'_, State> {
    state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
#[path = "../tests/unit/like.rs"]
mod tests;
