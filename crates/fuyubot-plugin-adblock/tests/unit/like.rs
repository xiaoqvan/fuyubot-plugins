use super::*;

fn now() -> i64 {
    1_700_000_000_000
}

fn ids(from: i64, to: i64) -> Vec<i64> {
    (from..=to).collect()
}

#[test]
fn five_consecutive_reactions_are_detected() {
    let guard = LikeGuard::new();
    let recent = ids(1, 10);
    let at = now();
    // 前 4 条都还不够。
    for id in 1..=4 {
        assert!(
            guard.observe(-100, 7, id, &recent, at).is_none(),
            "{id} 条还不算连续 5 条"
        );
    }
    let hit = guard
        .observe(-100, 7, 5, &recent, at)
        .expect("连续 5 条应命中");
    assert_eq!(hit.user_id, 7);
    assert_eq!(hit.reactions, 5);
    assert!(
        hit.detail().contains("连续回应 5 条消息"),
        "{}",
        hit.detail()
    );
}

#[test]
fn gaps_break_the_run() {
    let guard = LikeGuard::new();
    let recent = ids(1, 10);
    let at = now();
    // 1,2,4,5,6,7：中间缺 3，最长只有 4 条，不命中。
    for id in [1, 2, 4, 5, 6, 7] {
        assert!(guard.observe(-100, 9, id, &recent, at).is_none(), "{id}");
    }
}

#[test]
fn deleted_message_does_not_break_the_run() {
    let guard = LikeGuard::new();
    let at = now();
    // 回应 1,2,4,5（3 没回应）——最长连续 2。
    let before_delete = ids(1, 6);
    for id in [1, 2, 4, 5] {
        assert!(guard.observe(-100, 9, id, &before_delete, at).is_none());
    }
    // 3 被删除，cache 的最近消息变成 1,2,4,5,6：这时回应 6 就连成 5 条。
    let after_delete = vec![1, 2, 4, 5, 6];
    let hit = guard
        .observe(-100, 9, 6, &after_delete, at)
        .expect("删除 3 之后 1,2,4,5,6 应连成 5 条");
    assert_eq!(hit.reactions, 5);
}

#[test]
fn duplicate_updates_do_not_count_twice() {
    let guard = LikeGuard::new();
    let recent = ids(1, 8);
    let at = now();
    for id in 1..=5 {
        guard.observe(-100, 9, id, &recent, at);
    }
    // 同一条消息的反应变化会重发 recent_sender_ids，不能重复计数。
    assert!(guard.observe(-100, 9, 5, &recent, at).is_none());
}

#[test]
fn cooldown_blocks_repeat() {
    let guard = LikeGuard::new();
    let at = now();
    let first = ids(1, 10);
    for id in 1..=5 {
        guard.observe(-100, 9, id, &first, at);
    }

    // 冷却期内又连续回应 5 条：只记账，不重复处置。
    let cooling = ids(6, 10);
    let mut hits = 0;
    for id in 6..=10 {
        if guard.observe(-100, 9, id, &cooling, at + 60_000).is_some() {
            hits += 1;
        }
    }
    assert_eq!(hits, 0, "冷却期内不该再命中");

    // 冷却结束后再连续回应 5 条，重新命中一次。
    let later = at + COOLDOWN_MS + 1;
    let second = ids(11, 15);
    let mut hits = 0;
    for id in 11..=15 {
        if guard.observe(-100, 9, id, &second, later).is_some() {
            hits += 1;
        }
    }
    assert_eq!(hits, 1);
}

#[test]
fn chats_are_isolated() {
    let guard = LikeGuard::new();
    let recent = ids(1, 6);
    let at = now();
    for id in 1..=4 {
        guard.observe(-100, 9, id, &recent, at);
    }
    // 同一个账号在另一个群只回应了 1 条，不命中。
    assert!(guard.observe(-200, 9, 1, &recent, at).is_none());
    // 原群第 5 条命中。
    assert!(guard.observe(-100, 9, 5, &recent, at).is_some());
}

#[test]
fn messages_outside_recent_are_ignored() {
    let guard = LikeGuard::new();
    let recent = ids(1, 5);
    // 消息 99 不在最近窗口里：不记账、也不判定。
    assert!(guard.observe(-100, 9, 99, &recent, now()).is_none());
}

#[test]
fn reactions_outside_window_are_pruned() {
    let guard = LikeGuard::new();
    let at = now();
    let first = ids(1, 5);
    for id in 1..=4 {
        guard.observe(-100, 9, id, &first, at);
    }
    // 窗口滑到 6..10：1..4 全部滑出，不能和新的回应拼成连续段。
    let second = ids(6, 10);
    for id in 6..=9 {
        assert!(guard.observe(-100, 9, id, &second, at).is_none(), "{id}");
    }
}
