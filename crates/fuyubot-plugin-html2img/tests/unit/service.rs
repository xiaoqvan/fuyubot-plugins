use super::*;

#[test]
fn ordinary_images_produce_no_warnings() {
    assert!(size_warnings(960, 496, 120_000).is_empty());
}

#[test]
fn oversize_bytes_are_reported() {
    let warnings = size_warnings(960, 496, WARN_BYTES + 1);
    assert_eq!(warnings.len(), 1);
    assert!(warnings[0].contains("10 MB"), "{}", warnings[0]);
    assert!(warnings[0].contains("Telegram"), "{}", warnings[0]);
}

#[test]
fn long_edge_sum_is_reported() {
    let warnings = size_warnings(6_000, 4_001, 1024);
    assert!(warnings.iter().any(|text| text.contains("宽 + 高")));
}

#[test]
fn extreme_aspect_ratio_is_reported() {
    let warnings = size_warnings(2_000, 90, 1024);
    assert!(
        warnings.iter().any(|text| text.contains("长宽比")),
        "{warnings:?}"
    );
    // 竖条同样算：90×2000
    assert!(
        size_warnings(90, 2_000, 1024)
            .iter()
            .any(|text| text.contains("长宽比"))
    );
}

#[test]
fn vtable_exposes_render() {
    let service = Service::new(
        SERVICE_NAME,
        vec![("render", svc_render as Handler)],
        std::ptr::null_mut(),
    );
    let vtable = service.vtable();
    assert_eq!(vtable.version, 1);
    assert_eq!(vtable.method_count, 1);
    // SAFETY: 服务名是注册期间一直有效的 NUL 结尾字符串。
    let name = unsafe { CStr::from_ptr(vtable.name) }.to_str().unwrap();
    assert_eq!(name, SERVICE_NAME);
    // SAFETY: 方法表指向 `method_count` 个连续项。
    let methods = unsafe { std::slice::from_raw_parts(vtable.methods, vtable.method_count) };
    let method = unsafe { CStr::from_ptr(methods[0].name) }.to_str().unwrap();
    assert_eq!(method, "render");
    assert!(methods[0].func.is_some());
}

#[test]
fn replies_are_valid_json() {
    let raw = ok(json!({ "path": "/tmp/x.png", "width": 640 }));
    let value: Value =
        serde_json::from_str(unsafe { CStr::from_ptr(raw) }.to_str().unwrap()).unwrap();
    assert_eq!(value["ok"], true);
    assert_eq!(value["path"], "/tmp/x.png");
    assert_eq!(value["width"], 640);

    let raw = error("HTML 太大");
    let value: Value =
        serde_json::from_str(unsafe { CStr::from_ptr(raw) }.to_str().unwrap()).unwrap();
    assert_eq!(value["ok"], false);
    assert_eq!(value["error"], "HTML 太大");
}

#[test]
fn parse_tolerates_empty_and_null() {
    assert_eq!(parse(std::ptr::null()), Ok(json!({})));
    let empty = CString::new("   ").unwrap();
    assert_eq!(parse(empty.as_ptr()), Ok(json!({})));
}

#[test]
fn required_fields_are_validated() {
    let input = json!({ "html": "  " });
    assert!(require_str(&input, "html").is_err());
    let input = json!({ "html": "<p>x</p>" });
    assert_eq!(require_str(&input, "html").unwrap(), "<p>x</p>");
}

#[test]
fn vue_template_is_rendered_with_data() {
    let input = RenderInput {
        html: "<p>{{ title }}</p>".to_owned(),
        vue: Some(true),
        data: Some(json!({ "title": "你好" })),
        ..Default::default()
    };
    let html = source_html(&input).expect("应能渲染模板");
    assert!(html.contains("你好"), "{html}");
}

#[test]
fn plain_html_is_left_untouched() {
    let input = RenderInput {
        html: "<p>{{ title }}</p>".to_owned(),
        ..Default::default()
    };
    assert_eq!(source_html(&input).unwrap(), "<p>{{ title }}</p>");
}

#[test]
fn render_without_state_reports_error() {
    let input = CString::new(r#"{"html":"<p>x</p>"}"#).unwrap();
    // SAFETY: 入参是 NUL 结尾字符串；null context 走「状态已失效」分支。
    let raw = unsafe { svc_render(std::ptr::null_mut(), input.as_ptr()) };
    let value: Value =
        serde_json::from_str(unsafe { CStr::from_ptr(raw) }.to_str().unwrap()).unwrap();
    assert_eq!(value["ok"], false);
    assert!(value["error"].as_str().unwrap().contains("失效"));
}
