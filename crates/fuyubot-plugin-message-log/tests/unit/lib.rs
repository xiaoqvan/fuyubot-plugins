use super::*;

#[test]
fn plugin_id_matches_the_config_section() {
    // 跨仓契约：`define_plugin!` 的名字、`[plugin.<名>]` 段、日志来源词缀是同一个。
    assert_eq!(config::PLUGIN_NAME, "xiaoqvan.message-log");
}

#[test]
fn plugin_is_loaded_without_hard_or_soft_dependencies() {
    assert!(MessageLogPlugin::inject().is_empty());
    assert!(MessageLogPlugin::soft_inject().is_empty());
}
