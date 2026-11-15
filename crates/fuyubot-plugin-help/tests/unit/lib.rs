use super::*;

#[test]
fn html2img_is_a_hard_dependency() {
    assert_eq!(HelpPlugin::inject(), vec!["fuyubot.html2img"]);
}

#[test]
fn host_command_table_name_matches_the_contract() {
    // 跨仓契约：名字写错不会报错，只会让 /help 读到空表。
    assert_eq!(commands::HOST_COMMANDS, "fuyubot.host.commands");
}
