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
