use super::*;

unsafe extern "C" fn echo(_ctx: *mut c_void, input: *const c_char) -> *const c_char {
    match parse(input) {
        Ok(value) => ok(value),
        Err(err) => error(err),
    }
}

#[test]
fn service_exposes_stable_pointer() {
    let service = Service::new("test.echo", vec![("echo", echo)]);
    let vtable = service.vtable();
    assert_eq!(vtable.method_count, 1);
    assert_eq!(vtable.version, 1);
    assert!(!vtable.name.is_null());
    let name = unsafe { CStr::from_ptr(vtable.name) }.to_str().unwrap();
    assert_eq!(name, "test.echo");
    // 方法表里的指针必须指向有效内存。
    let methods = unsafe { std::slice::from_raw_parts(vtable.methods, vtable.method_count) };
    let method = unsafe { CStr::from_ptr(methods[0].name) }.to_str().unwrap();
    assert_eq!(method, "echo");
}

#[test]
fn reply_is_valid_json_and_reusable() {
    let first = ok(json!({ "n": 1 }));
    let text = unsafe { CStr::from_ptr(first) }.to_str().unwrap();
    let value: Value = serde_json::from_str(text).unwrap();
    assert_eq!(value["ok"], true);
    assert_eq!(value["n"], 1);

    // 同线程再调一次，旧指针内容会被覆盖（契约允许）。
    let second = error("boom");
    let text = unsafe { CStr::from_ptr(second) }.to_str().unwrap();
    let value: Value = serde_json::from_str(text).unwrap();
    assert_eq!(value["ok"], false);
    assert_eq!(value["error"], "boom");
}

#[test]
fn parse_tolerates_empty_and_null() {
    assert_eq!(parse(std::ptr::null()), Ok(json!({})));
    let empty = CString::new("   ").unwrap();
    assert_eq!(parse(empty.as_ptr()), Ok(json!({})));
}

#[test]
fn required_fields_are_validated() {
    let input = json!({ "db": "  ", "collection": "anime" });
    assert!(require_str(&input, "db").is_err());
    assert_eq!(require_str(&input, "collection").unwrap(), "anime");
    assert!(require_str(&input, "missing").is_err());
}
