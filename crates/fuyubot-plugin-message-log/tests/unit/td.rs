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

// ── 更新驱动缓存（Book） ──────────────────────────────────────────

#[test]
fn book_reads_titles_from_chat_updates() {
    let mut book = Book::new();

    assert!(book.absorb(&serde_json::json!({
        "@type": "updateNewChat",
        "chat": { "@type": "chat", "id": -1001, "title": "开发群" }
    })));
    assert_eq!(book.chat_title(-1001), Some("开发群"));

    // 改群名：同一条 id 被覆盖，不需要等 TTL。
    assert!(book.absorb(&serde_json::json!({
        "@type": "updateChatTitle", "chat_id": -1001, "title": "新开发群"
    })));
    assert_eq!(book.chat_title(-1001), Some("新开发群"));
    assert_eq!(book.chat_title(-1002), None, "没见过的聊天");
}

#[test]
fn book_reads_names_from_user_updates() {
    let mut book = Book::new();

    assert!(book.absorb(&serde_json::json!({
        "@type": "updateUser",
        "user": { "@type": "user", "id": 7, "first_name": "张", "last_name": "三" }
    })));
    assert_eq!(book.user_name(7), Some("张 三"));

    // 只有名 / 只有姓都只显示那一个；两边都空则不记（别把「未知用户」写死进缓存）。
    book.absorb(&serde_json::json!({
        "@type": "updateUser",
        "user": { "@type": "user", "id": 8, "first_name": "李", "last_name": "" }
    }));
    assert_eq!(book.user_name(8), Some("李"));

    book.absorb(&serde_json::json!({
        "@type": "updateUser",
        "user": { "@type": "user", "id": 9, "first_name": " ", "last_name": "" }
    }));
    assert_eq!(book.user_name(9), None);
}

#[test]
fn book_leaves_other_updates_alone() {
    let mut book = Book::new();
    assert!(!book.absorb(&serde_json::json!({"@type":"updateNewMessage"})));
    assert!(!book.absorb(&serde_json::json!({"@type":"updateUserStatus"})));
    assert_eq!(book.len(), (0, 0));
    assert_eq!(book.chat_title(0), None);
    assert_eq!(book.user_name(0), None);
}

#[test]
fn book_is_cleared_at_capacity() {
    let mut book = Book::new();
    for id in 1..=(BOOK_CAPACITY as i64) {
        book.set_chat(-id, "群");
    }
    assert_eq!(book.len().0, BOOK_CAPACITY);

    book.set_chat(-9999, "新群");
    assert_eq!(book.len().0, 1, "超上限整表清空，不无限涨");
    assert_eq!(book.chat_title(-9999), Some("新群"));
}

#[test]
fn shared_book_is_readable_from_the_worker_side() {
    let mut book = Book::new();
    book.absorb(&serde_json::json!({
        "@type": "updateNewChat",
        "chat": { "@type": "chat", "id": -1001, "title": "开发群" }
    }));
    let shared: SharedBook = Arc::new(Mutex::new(book));
    assert_eq!(book_chat(&shared, -1001).as_deref(), Some("开发群"));
    assert_eq!(book_chat(&shared, -1002), None);
}
