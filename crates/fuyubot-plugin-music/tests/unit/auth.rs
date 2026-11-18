use super::*;

#[test]
fn flat_netease_cookie_file_is_kept_verbatim() {
    let auth = parse_netease(&serde_json::json!({
        "MUSIC_U": "token",
        "os": "pc",
        "appver": "3.1.0",
        "deviceId": "abc",
    }));
    assert!(auth.is_logged_in());
    assert!(auth.cookie_header.contains("MUSIC_U=token"));
    assert!(auth.cookie_header.contains("os=pc"));
    assert!(auth.cookie_header.contains("deviceId=abc"));
    assert!(
        !auth.cookie_header.contains("appver=3.1.28"),
        "文件里的值不能被默认值覆盖"
    );
}

#[test]
fn missing_defaults_are_filled_but_not_duplicated() {
    let auth = parse_netease(&serde_json::json!({"MUSIC_U": "t"}));
    assert_eq!(
        auth.cookies.iter().filter(|(name, _)| name == "os").count(),
        1
    );
    assert!(auth.cookie_header.contains("os=pc"));
    assert!(auth.cookie_header.contains("appver="));
}

#[test]
fn a_single_cookie_string_is_accepted() {
    let auth = parse_netease(&serde_json::json!({"cookie": "MUSIC_U=abc; os=pc; junk"}));
    assert_eq!(
        auth.cookies
            .iter()
            .filter(|(name, _)| name == "MUSIC_U")
            .count(),
        1
    );
    assert!(auth.cookie_header.contains("MUSIC_U=abc"));
    assert!(!auth.cookie_header.contains("junk"));
}

#[test]
fn anonymous_netease_has_no_login_state() {
    let auth = parse_netease(&serde_json::json!({}));
    assert!(!auth.is_logged_in());
    assert!(auth.cookie_header.contains("os=pc"));
}

#[test]
fn soda_file_accepts_captured_aliases() {
    let auth = parse_soda(&serde_json::json!({
        "cookie": "sessionid=abc",
        "deviceId": "111",
        "install_id": "222",
        "X-Helios": "h",
        "X-Medusa": "m",
        "ua": "LunaPC/3.3.0(1)",
    }));
    assert_eq!(auth.device_id, "111");
    assert_eq!(auth.iid, "222");
    assert_eq!(auth.fp, "111");
    assert!(auth.has_signature());
    assert_eq!(auth.user_agent, "LunaPC/3.3.0(1)");
}

#[test]
fn soda_without_fingerprint_gets_generated_ids() {
    let auth = parse_soda(&serde_json::json!({}));
    assert!(!auth.device_id.is_empty());
    assert!(!auth.iid.is_empty());
    assert_eq!(auth.fp, auth.device_id);
    assert!(!auth.has_signature());
}

#[test]
fn signer_settings_are_read_with_aliases() {
    let auth = parse_soda(&serde_json::json!({
        "signerUrl": "http://127.0.0.1:8899/sign",
        "signerToken": "secret",
    }));
    assert!(auth.has_signer());
    assert_eq!(auth.signer_url, "http://127.0.0.1:8899/sign");
    assert_eq!(auth.signer_token, "secret");
    // 只填 token 不算配置了签名器
    assert!(!parse_soda(&serde_json::json!({"signer_token": "x"})).has_signer());
    // 说明文字 / 非 http(s) 地址不算签名器
    assert!(!parse_soda(&serde_json::json!({"signer_url": "可选：http://x"})).has_signer());
    assert!(!parse_soda(&serde_json::json!({"signer_url": "127.0.0.1:8899"})).has_signer());
}

#[test]
fn half_a_signature_is_not_a_signature() {
    let auth = parse_soda(&serde_json::json!({"x_helios": "h"}));
    assert!(!auth.has_signature(), "两个头都齐才算");
}

#[test]
fn cookie_values_cannot_break_the_header() {
    let auth = parse_netease(&serde_json::json!({"MUSIC_U": "a;b\r\nc"}));
    assert!(!auth.cookie_header.contains('\r'));
    assert!(!auth.cookie_header.contains('\n'));
    assert_eq!(
        auth.cookie_header.matches(';').count(),
        2,
        "分隔符只有补的两个默认值"
    );
}

// ── 配置段 `[plugin.xiaoqvan.music]` ──────────────────────────────
//
// 用假 `HostApi`：`get_config` 从进程内表里取「宿主返回的 JSON 文本」，
// 其余函数一律 noop。所有配置测试共用一份表，必须串行。

use std::cell::RefCell;
use std::ffi::{CStr, CString, c_char, c_void};
use std::sync::Mutex;

use fuyubot_abi::{
    ABI_VERSION, EventCallbackFn, HostApiVTable, MiddlewareCallbackFn, MiddlewareHandle, RequestId,
    ServiceHandle, ServiceVTable, SubscriptionHandle, ToolHandle, ToolHandlerFn, WaitHandle,
};

/// 串行化配置测试（共用 [`CONFIG`]）。
static AUTH_TEST_LOCK: Mutex<()> = Mutex::new(());
/// `section.key` → 宿主 `get_config` 应当返回的 JSON 文本。
static CONFIG: Mutex<Vec<(String, String)>> = Mutex::new(Vec::new());

thread_local! {
    static CONFIG_REPLY: RefCell<CString> = RefCell::new(CString::new("").expect("empty"));
}

fn set_config(key: &str, json: &str) {
    let full = format!("{CONFIG_SECTION}.{key}");
    let mut config = CONFIG.lock().unwrap();
    config.retain(|(candidate, _)| candidate != &full);
    config.push((full, json.to_owned()));
}

fn clear_config() {
    CONFIG.lock().unwrap().clear();
}

unsafe extern "C" fn fake_get_config(
    _host: *mut c_void,
    section: *const c_char,
    key: *const c_char,
) -> *const c_char {
    if section.is_null() || key.is_null() {
        return std::ptr::null();
    }
    // SAFETY: 宿主契约保证 section / key 是 NUL 结尾字符串。
    let section = unsafe { CStr::from_ptr(section) }.to_string_lossy();
    let key = unsafe { CStr::from_ptr(key) }.to_string_lossy();
    let full = format!("{section}.{key}");
    let value = CONFIG
        .lock()
        .unwrap()
        .iter()
        .find(|(candidate, _)| candidate == &full)
        .map(|(_, value)| value.clone());
    match value {
        Some(text) => CONFIG_REPLY.with(|cell| {
            *cell.borrow_mut() = CString::new(text).expect("config");
            cell.borrow().as_ptr()
        }),
        None => std::ptr::null(),
    }
}

unsafe extern "C" fn noop_on(
    _: *mut c_void,
    _: *const c_char,
    _: *const c_char,
    _: Option<EventCallbackFn>,
    _: *mut c_void,
) -> SubscriptionHandle {
    SubscriptionHandle::INVALID
}
unsafe extern "C" fn noop_off(_: *mut c_void, _: SubscriptionHandle) {}
unsafe extern "C" fn noop_emit(_: *mut c_void, _: *const c_char, _: *const c_char) -> i32 {
    0
}
unsafe extern "C" fn noop_register_tool(
    _: *mut c_void,
    _: *const c_char,
    _: Option<ToolHandlerFn>,
    _: *mut c_void,
) -> ToolHandle {
    ToolHandle::INVALID
}
unsafe extern "C" fn noop_call_tool(
    _: *mut c_void,
    _: *const c_char,
    _: *const c_char,
) -> RequestId {
    RequestId::INVALID
}
unsafe extern "C" fn noop_call_tool_result(
    _: *mut c_void,
    _: *const c_char,
    _: *const c_char,
) -> *const c_char {
    std::ptr::null()
}
unsafe extern "C" fn noop_list(_: *mut c_void) -> *const c_char {
    std::ptr::null()
}
unsafe extern "C" fn noop_provide(
    _: *mut c_void,
    _: *const c_char,
    _: *const ServiceVTable,
) -> ServiceHandle {
    ServiceHandle::INVALID
}
unsafe extern "C" fn noop_get(_: *mut c_void, _: *const c_char) -> *const ServiceVTable {
    std::ptr::null()
}
unsafe extern "C" fn noop_td(_: *mut c_void, _: *const c_char) -> RequestId {
    RequestId::INVALID
}
unsafe extern "C" fn noop_td_simplified(
    _: *mut c_void,
    _: *const c_char,
    _: *const c_char,
) -> RequestId {
    RequestId::INVALID
}
unsafe extern "C" fn noop_execute(_: *mut c_void, _: *const c_char) -> *const c_char {
    std::ptr::null()
}
unsafe extern "C" fn noop_middleware(
    _: *mut c_void,
    _: u32,
    _: Option<MiddlewareCallbackFn>,
    _: *mut c_void,
) -> MiddlewareHandle {
    MiddlewareHandle::INVALID
}
unsafe extern "C" fn noop_wait(_: *mut c_void, _: *const c_char, _: u64) -> WaitHandle {
    WaitHandle::INVALID
}
unsafe extern "C" fn noop_cancel(_: *mut c_void, _: WaitHandle) {}
unsafe extern "C" fn noop_log(_: *mut c_void, _: u32, _: *const c_char) {}
unsafe extern "C" fn noop_set_config(_: *mut c_void, _: *const c_char, _: *const c_char) -> i32 {
    -1
}
unsafe extern "C" fn noop_simplify_invoke(_: *mut c_void, _: *const c_char) -> RequestId {
    RequestId::INVALID
}
unsafe extern "C" fn noop_simplify_invoke_result(
    _: *mut c_void,
    _: *const c_char,
) -> *const c_char {
    std::ptr::null()
}
/// ABI v6：服务调用桥。假宿主里没有服务，一律 NULL（= 失败）。
unsafe extern "C" fn noop_call_service(
    _: *mut c_void,
    _: *const c_char,
    _: *const c_char,
    _: *const c_char,
) -> *const c_char {
    std::ptr::null()
}
/// ABI v6：`set_config_section`。假宿主一律拒绝写。
unsafe extern "C" fn noop_set_config_section(
    _: *mut c_void,
    _: *const c_char,
    _: *const c_char,
    _: *const c_char,
) -> i32 {
    -1
}

fn fake_host() -> HostApi {
    let vtable = HostApiVTable {
        version: ABI_VERSION,
        size: std::mem::size_of::<HostApiVTable>() as u32,
        host_ctx: std::ptr::null_mut(),
        on: Some(noop_on),
        off: Some(noop_off),
        emit: Some(noop_emit),
        register_tool: Some(noop_register_tool),
        call_tool: Some(noop_call_tool),
        call_tool_result: Some(noop_call_tool_result),
        list_tools: Some(noop_list),
        list_plugins: Some(noop_list),
        provide_service: Some(noop_provide),
        get_service: Some(noop_get),
        td_invoke: Some(noop_td),
        td_invoke_simplified: Some(noop_td_simplified),
        td_execute: Some(noop_execute),
        use_middleware: Some(noop_middleware),
        wait_for_update: Some(noop_wait),
        cancel_wait: Some(noop_cancel),
        log: Some(noop_log),
        get_config: Some(fake_get_config),
        set_config: Some(noop_set_config),
        provide_service_shared: Some(noop_provide),
        simplify_invoke: Some(noop_simplify_invoke),
        simplify_invoke_result: Some(noop_simplify_invoke_result),
        // ABI v6 新增：调用者身份 / 服务调用桥 / 失败原因 / 服务清单 / 跨插件写配置。
        current_plugin: Some(noop_list),
        call_service: Some(noop_call_service),
        last_error: Some(noop_list),
        list_services: Some(noop_list),
        set_config_section: Some(noop_set_config_section),
    };
    // SAFETY: 所有必需函数都已填；`from_raw` 会拷走函数指针。
    unsafe { HostApi::from_raw(&vtable) }.expect("fake host")
}

/// 建一个空的临时 `plugins/` 目录（每个用例独立，跑完删）。
fn temp_plugins_dir(name: &str) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("fuyubot-music-auth-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("music")).expect("temp dir");
    dir
}

fn write_legacy(dir: &Path, file: &str, json: &str) {
    std::fs::write(dir.join("music").join(file), json).expect("legacy file");
}

#[test]
fn config_string_is_used_as_the_cookie() {
    let _guard = AUTH_TEST_LOCK
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    clear_config();
    set_config(CONFIG_NETEASE, r#""MUSIC_U=from-config; os=pc""#);
    let cookie = config_string(fake_host(), CONFIG_NETEASE).expect("配置应当生效");
    assert_eq!(cookie, "MUSIC_U=from-config; os=pc");
    let auth = parse_netease(&serde_json::json!({ "cookie": cookie }));
    assert!(auth.is_logged_in());
    assert!(auth.cookie_header.contains("MUSIC_U=from-config"));
}

#[test]
fn youtube_cookie_is_read() {
    let _guard = AUTH_TEST_LOCK
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    clear_config();
    let dir = temp_plugins_dir("third-platforms");

    set_config(CONFIG_YOUTUBE, r#""SAPISID=abc; SID=def""#);
    let auth = load(fake_host(), &dir);
    let youtube = auth.youtube.expect("youtube cookie 应当读出来");
    assert!(youtube.is_configured());
    assert_eq!(youtube.cookie, "SAPISID=abc; SID=def");

    // 旧文件不影响这几家：没配就是匿名。
    clear_config();
    let auth = load(fake_host(), &dir);
    assert!(auth.youtube.is_none());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn non_string_config_is_ignored_and_falls_back() {
    let _guard = AUTH_TEST_LOCK
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    clear_config();
    let dir = temp_plugins_dir("non-string");
    write_legacy(&dir, "163cookie.json", r#"{"MUSIC_U":"from-file"}"#);
    // 旧版子表写法（对象）不再支持：按未配置处理 → 回退旧文件。
    set_config(
        CONFIG_NETEASE,
        r#"{"MUSIC_U":"obj-token","deviceId":"abc"}"#,
    );
    assert!(config_string(fake_host(), CONFIG_NETEASE).is_none());
    let netease = load(fake_host(), &dir).netease.expect("回退旧文件");
    assert!(
        netease.cookie_header.contains("from-file"),
        "非字符串配置应当回退旧文件：{}",
        netease.cookie_header
    );
    // 数字 / null / 数组 / 空对象同样按未配置处理，不 panic。
    for bad in ["42", "null", "[1,2]", "{}"] {
        set_config(CONFIG_SODA, bad);
        assert!(
            config_string(fake_host(), CONFIG_SODA).is_none(),
            "{bad} 不该被当成配置"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn empty_or_bad_config_is_unconfigured_without_panicking() {
    let _guard = AUTH_TEST_LOCK
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    // 纯函数：没配置 / 空串 / 坏 JSON / 空字符串值 / null / 空对象 / 其它类型。
    assert!(normalize_config_string(None, CONFIG_NETEASE).is_none());
    assert!(normalize_config_string(Some(String::new()), CONFIG_NETEASE).is_none());
    assert!(normalize_config_string(Some("not json".to_owned()), CONFIG_NETEASE).is_none());
    assert!(normalize_config_string(Some("\"\"".to_owned()), CONFIG_NETEASE).is_none());
    assert!(normalize_config_string(Some("\"   \"".to_owned()), CONFIG_NETEASE).is_none());
    assert!(normalize_config_string(Some("null".to_owned()), CONFIG_NETEASE).is_none());
    assert!(normalize_config_string(Some("{}".to_owned()), CONFIG_NETEASE).is_none());
    assert!(normalize_config_string(Some("42".to_owned()), CONFIG_NETEASE).is_none());
    assert!(normalize_config_string(Some("[1]".to_owned()), CONFIG_NETEASE).is_none());
    // 合法字符串：去首尾空白后非空才算配置。
    assert_eq!(
        normalize_config_string(Some("\"token\"".to_owned()), CONFIG_NETEASE),
        Some("token".to_owned())
    );
    assert_eq!(
        normalize_config_string(Some("\"  token  \"".to_owned()), CONFIG_NETEASE),
        Some("token".to_owned())
    );
    // 宿主里根本没有这个键：不 panic，返回 None。
    clear_config();
    assert!(config_string(fake_host(), CONFIG_NETEASE).is_none());
}

#[test]
fn config_takes_priority_over_the_legacy_file() {
    let _guard = AUTH_TEST_LOCK
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    clear_config();
    let dir = temp_plugins_dir("priority");
    write_legacy(&dir, "163cookie.json", r#"{"MUSIC_U":"from-file"}"#);
    set_config(CONFIG_NETEASE, r#""MUSIC_U=from-config""#);

    let auth = load(fake_host(), &dir).netease.expect("配置应当生效");
    assert!(
        auth.cookie_header.contains("from-config"),
        "配置优先：{}",
        auth.cookie_header
    );
    assert!(
        !auth.cookie_header.contains("from-file"),
        "配置存在时不该读旧文件：{}",
        auth.cookie_header
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn missing_config_falls_back_to_the_legacy_file() {
    let _guard = AUTH_TEST_LOCK
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    clear_config();
    let dir = temp_plugins_dir("fallback");
    write_legacy(&dir, "163cookie.json", r#"{"MUSIC_U":"from-file"}"#);

    let auth = load(fake_host(), &dir).netease.expect("回退旧文件");
    assert!(
        auth.cookie_header.contains("from-file"),
        "应当读旧文件：{}",
        auth.cookie_header
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn neither_config_nor_file_is_anonymous() {
    let _guard = AUTH_TEST_LOCK
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    clear_config();
    let dir = temp_plugins_dir("anonymous");
    let auth = load(fake_host(), &dir);
    assert!(auth.netease.is_none(), "没配置也没文件 → 匿名");
    assert!(auth.soda.is_none(), "没配置也没文件 → 匿名");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn flat_soda_keys_are_all_read() {
    let _guard = AUTH_TEST_LOCK
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    clear_config();
    // 单层平铺：soda 是 Cookie 头，其余是 soda_ 前缀的指纹 / 签名头。
    set_config(CONFIG_SODA, r#""sessionid=abc""#);
    set_config(CONFIG_SODA_DEVICE_ID, r#""111""#);
    set_config(CONFIG_SODA_IID, r#""222""#);
    set_config(CONFIG_SODA_FP, r#""333""#);
    set_config(CONFIG_SODA_X_HELIOS, r#""h""#);
    set_config(CONFIG_SODA_X_MEDUSA, r#""m""#);
    set_config(CONFIG_SODA_USER_AGENT, r#""LunaPC/3.3.0(1)""#);
    set_config(CONFIG_SODA_SIGNER_URL, r#""http://127.0.0.1:8899/sign""#);
    set_config(CONFIG_SODA_SIGNER_TOKEN, r#""secret""#);
    let dir = temp_plugins_dir("soda-flat");
    let soda = load(fake_host(), &dir).soda.expect("配置应当生效");
    assert_eq!(soda.cookie, "sessionid=abc");
    assert_eq!(soda.device_id, "111");
    assert_eq!(soda.iid, "222");
    assert_eq!(soda.fp, "333");
    assert!(soda.has_signature());
    assert!(soda.has_signer());
    assert_eq!(soda.user_agent, "LunaPC/3.3.0(1)");
    assert_eq!(soda.signer_token, "secret");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn flat_soda_fingerprint_without_cookie_is_still_configured() {
    let _guard = AUTH_TEST_LOCK
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    clear_config();
    // 只配设备指纹（匿名但固定指纹）也算配置了，不该回退旧文件。
    set_config(CONFIG_SODA_DEVICE_ID, r#""111""#);
    let dir = temp_plugins_dir("soda-fingerprint");
    write_legacy(&dir, "soda.json", r#"{"cookie":"from-file"}"#);
    let soda = load(fake_host(), &dir).soda.expect("配置应当生效");
    assert_eq!(soda.device_id, "111");
    assert!(soda.cookie.is_empty(), "没配 cookie 就是匿名");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn soda_config_string_is_used_as_the_cookie() {
    let _guard = AUTH_TEST_LOCK
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    clear_config();
    set_config(CONFIG_SODA, r#""sessionid=xyz; sid_tt=1""#);
    let dir = temp_plugins_dir("soda-string");
    let soda = load(fake_host(), &dir).soda.expect("配置应当生效");
    assert_eq!(soda.cookie, "sessionid=xyz; sid_tt=1");
    // 设备指纹没配时按现有行为生成。
    assert!(!soda.device_id.is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}
