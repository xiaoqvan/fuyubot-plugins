use super::*;

/// 17 条，与 TS `cmdHandlers` 一一对应——少一条就是少一个功能。
#[test]
fn registry_covers_every_ts_command() {
    let names: Vec<&str> = COMMANDS.iter().map(|spec| spec.name).collect();
    assert_eq!(names.len(), 17, "TS 版是 17 条命令");
    for expected in [
        "adconfig",
        "spam",
        "setlogchannel",
        "whitelist",
        "rmwhitelist",
        "qadid",
        "delad",
        "qban",
        "addfwdbl",
        "rmfwdbl",
        "addkw",
        "editkw",
        "adkew",
        "admatch",
        "adaiadd",
        "adaiset",
        "start",
    ] {
        assert!(names.contains(&expected), "缺命令 /{expected}");
    }
}

/// 表里没有的名字不是命令——检测流水线要继续扫它。
#[test]
fn unknown_names_are_not_commands() {
    assert!(find("接单").is_none());
    assert!(find("").is_none());
    // 大小写不敏感（`command_of` 已小写化，这里再兜一层）。
    assert!(find("AdConfig").is_some());
}

/// scope/permission 与 TS 的 `cmdHandlers` 一致：11 条私聊、6 条 admin、4 条 owner。
#[test]
fn scopes_and_permissions_match_the_ts_table() {
    let private = COMMANDS
        .iter()
        .filter(|spec| spec.scope == Scope::Private)
        .count();
    let admin = COMMANDS
        .iter()
        .filter(|spec| spec.permission == Permission::Admin)
        .count();
    let owner = COMMANDS
        .iter()
        .filter(|spec| spec.permission == Permission::Owner)
        .count();
    assert_eq!(private, 11);
    assert_eq!(admin, 6);
    assert_eq!(owner, 4);
}
