//! 入群 24 小时内的资料变更复检。
//!
//! 对应 TS 版 `utils/profileChangeWatch.ts`。`updateUser` 对**所有**用户都会触发，
//! 所以第一件事是用数据库里的「有效监视记录」做快速过滤；复检本身还有 30 秒节流，
//! 避免有人反复改简介把 AI 接口打爆。
//!
//! 与入群检查共用同一套阈值与处置（[`super::new_member`]）。

use serde_json::Value;

use crate::ai::{self, AiMeta, ModerationInput};
use crate::flow;
use crate::log;
use crate::runtime::Runtime;
use crate::store::doc_id;
use crate::util::now_ms;

/// 复检最小间隔。
const RECHECK_THROTTLE_MS: i64 = 30_000;

/// 处理 `updateUser`。
pub async fn handle_update_user(rt: &Runtime, update: &Value) {
    let Some(user) = update.get("user") else {
        return;
    };
    let Some(user_id) = user.get("id").and_then(Value::as_i64) else {
        return;
    };
    if user
        .get("type")
        .and_then(|kind| kind.get("@type"))
        .and_then(Value::as_str)
        == Some("userTypeBot")
    {
        return;
    }
    // 快速过滤：这个用户在任何群都没有有效监视记录。
    if !rt.db.has_any_active_watch(user_id).await {
        return;
    }
    let watches = rt.db.watches_by_user(user_id).await;
    if watches.is_empty() {
        return;
    }

    let name = format!(
        "{}{}",
        user.get("first_name").and_then(Value::as_str).unwrap_or(""),
        user.get("last_name").and_then(Value::as_str).unwrap_or("")
    );
    // bio 不在 updateUser 里，得单独问一次。
    let bio = rt
        .td
        .user_full(user_id)
        .await
        .ok()
        .and_then(|full| {
            full.get("bio")
                .and_then(|bio| bio.get("text"))
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .unwrap_or_default();
    let now = now_ms();

    for watch in watches {
        // 节流：刚复检过就跳过。
        if let Some(last) = watch.last_recheck_at
            && now - last < RECHECK_THROTTLE_MS
        {
            continue;
        }
        let name_changed = name != watch.join_name;
        let bio_changed = bio != watch.join_bio;
        if !name_changed && !bio_changed {
            continue;
        }
        let config = rt.db.config_or_default(watch.chat_id).await;
        if !config.check_new_member {
            continue;
        }
        log::info(
            &rt.host,
            format!(
                "[入群监视] 用户({user_id}) 在群({}) 入群{}分钟内变更资料 name={name_changed} bio={bio_changed}，触发AI复检",
                watch.chat_id,
                (now - watch.join_at).max(0) / 60_000
            ),
        );

        // 复审已经通过的用户：不再因为资料变更的弱信号重新禁言/封禁。
        if watch.review_approved {
            log::info(
                &rt.host,
                format!(
                    "[入群监视] 用户({user_id}) 群({}) 已复审通过，跳过资料变更复检处置",
                    watch.chat_id
                ),
            );
            rt.db
                .mark_profile_changed(watch.chat_id, user_id, &name, &bio)
                .await;
            continue;
        }

        // 先更新快照，避免同一份内容重复触发。
        rt.db
            .mark_profile_changed(watch.chat_id, user_id, &name, &bio)
            .await;
        recheck_profile(rt, watch.chat_id, user_id, &name, &bio).await;
        rt.db.mark_profile_rechecked(watch.chat_id, user_id).await;
    }
}

/// 对变更后的资料做一次 AI 复检（阈值与入群检查一致）。
async fn recheck_profile(rt: &Runtime, chat_id: i64, user_id: i64, name: &str, bio: &str) {
    // 黑名单优先
    if let Some(record) = rt.db.match_name(name).await {
        flow::new_member::ban_new_member(
            rt,
            chat_id,
            user_id,
            &format!("入群后资料变更命中名称黑名单 (ID: {})", doc_id(&record)),
            1.0,
        )
        .await;
        return;
    }
    if !bio.trim().is_empty()
        && let Some(record) = rt.db.match_bio(bio).await
    {
        flow::new_member::ban_new_member(
            rt,
            chat_id,
            user_id,
            &format!(
                "入群后资料变更命中简介黑名单关键词 (ID: {})",
                doc_id(&record)
            ),
            1.0,
        )
        .await;
        return;
    }

    let channel = flow::personal_channel(rt, user_id).await;
    let mut input = ModerationInput::profile(name, bio);
    if let Some((channel_name, description)) = &channel {
        input = input.with_personal_channel(channel_name.clone(), description.clone());
    }
    let result = ai::review(
        rt,
        &input,
        None,
        Some(chat_id),
        &AiMeta {
            scene: Some("profile_change"),
            message_type: None,
            force_default: false,
        },
    )
    .await;

    if !result.profile_hit() || result.confidence <= 0.0 {
        log::info(
            &rt.host,
            format!(
                "[入群监视] 用户({user_id}) 群({chat_id}) 资料变更复检通过 (confidence={})",
                result.confidence
            ),
        );
        return;
    }
    let reason = if result.reason.is_empty() {
        "入群后资料变更检测到广告".to_owned()
    } else {
        result.reason.clone()
    };

    // 已通过禁言复审的用户：AI 的弱信号不得再把人打回禁言/封禁。
    if rt
        .db
        .mute_review(chat_id, user_id)
        .await
        .is_some_and(|review| review.status == "approved")
    {
        log::info(
            &rt.host,
            format!(
                "[入群监视] 用户({user_id}) 群({chat_id}) 复审已通过，忽略资料变更弱广告信号 (confidence={})",
                result.confidence
            ),
        );
        return;
    }

    if result.confidence >= 0.95 {
        flow::new_member::ban_new_member(rt, chat_id, user_id, &reason, result.confidence).await;
    } else {
        flow::new_member::mute_new_member(rt, chat_id, user_id, name, &reason, result.confidence)
            .await;
    }
}
