use super::*;

#[test]
fn like_guard_has_a_short_alias() {
    assert_eq!(config_key("like_guard"), "like_guard_enabled");
    assert_eq!(config_key("like_guard_enabled"), "like_guard_enabled");
    assert_eq!(config_key("delay"), "delay");
    assert_eq!(config_key(""), "");
}

#[test]
fn like_guard_toggle_is_registered() {
    assert!(
        TOGGLES
            .iter()
            .any(|(name, ..)| *name == "like_guard_enabled")
    );
    // 新增开关别漏改这里：状态页与 `/adconfig` 都按 TOGGLES 遍历。
    assert_eq!(TOGGLES.len(), 4);
}
