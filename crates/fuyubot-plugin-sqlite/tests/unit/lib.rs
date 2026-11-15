use super::*;

#[test]
fn service_name_matches_the_manifest_name() {
    assert_eq!(service::SERVICE_NAME, "fuyubot.sqlite");
}

#[test]
fn response_buffer_round_trips() {
    let pointer = reply(r#"{"ok":true}"#);
    let text = unsafe { CStr::from_ptr(pointer) }.to_str().unwrap();
    assert_eq!(text, r#"{"ok":true}"#);
}
