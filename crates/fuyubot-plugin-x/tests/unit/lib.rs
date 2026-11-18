use super::*;

#[test]
fn metadata_is_static_and_readable() {
    assert_eq!(XPlugin::display_name(), "X 解析");
    assert!(!XPlugin::description().is_empty());
}

#[test]
fn html2img_is_a_soft_dependency_only() {
    // 软依赖：没装也照常激活，只是纯文本文章退化成文字。
    assert!(XPlugin::inject().is_empty());
    assert_eq!(
        XPlugin::soft_inject(),
        vec![config::HTML2IMG_SERVICE.to_owned()]
    );
}

#[test]
fn command_name_and_scope_are_stable() {
    assert_eq!(commands::COMMAND, "x");
    assert!(!commands::COMMAND_DESCRIPTION.is_empty());
}
