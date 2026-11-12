//! `start` / `stop` 生命周期冒烟测试：**假宿主 VTable**，不连 TDLib、不联网。
//!
//! 它钉住两件事：
//!
//! 1. 在「没有 SQL 插件、没有 help 插件、没有 cookie、没接 TDLib」的最小环境里，
//!    `start` 也能成功（软依赖 + 缺 cookie 不致命）；
//! 2. `stop()` 快速返回——不能卡在「等在飞任务归零」/ `join` 上。
//!
//! 假宿主把服务判成「一个都没有」：`list_services` 回 `[]`、`call_service` 回 NULL
//! （ABI v6 的服务调用走宿主这条桥，插件不再直接调别人的 vtable），
//! `on` 返回一个有效句柄但从不投递事件——插件的订阅建得起来就够了。

use std::ffi::c_void;
use std::time::{Duration, Instant};

use fuyubot_abi::{
    ABI_VERSION, HostApi, HostApiVTable, MiddlewareHandle, Plugin, RequestId, ServiceHandle,
    ServiceVTable, SubscriptionHandle, ToolHandle, WaitHandle,
};
use fuyubot_plugin_douyin::DouyinPlugin;

unsafe extern "C" fn fake_on(
    _host: *mut c_void,
    _event: *const std::ffi::c_char,
    _filter: *const std::ffi::c_char,
    _callback: Option<unsafe extern "C-unwind" fn(*mut c_void, *const std::ffi::c_char) -> i32>,
    _user_data: *mut c_void,
) -> SubscriptionHandle {
    SubscriptionHandle { id: 1 }
}

unsafe extern "C" fn fake_off(_host: *mut c_void, _handle: SubscriptionHandle) {}

unsafe extern "C" fn noop_emit(
    _: *mut c_void,
    _: *const std::ffi::c_char,
    _: *const std::ffi::c_char,
) -> i32 {
    0
}

unsafe extern "C" fn noop_register_tool(
    _: *mut c_void,
    _: *const std::ffi::c_char,
    _: Option<fuyubot_abi::ToolHandlerFn>,
    _: *mut c_void,
) -> ToolHandle {
    ToolHandle::INVALID
}

unsafe extern "C" fn noop_request_id(_: *mut c_void, _: *const std::ffi::c_char) -> RequestId {
    RequestId::INVALID
}

unsafe extern "C" fn noop_request_id2(
    _: *mut c_void,
    _: *const std::ffi::c_char,
    _: *const std::ffi::c_char,
) -> RequestId {
    RequestId::INVALID
}

unsafe extern "C" fn noop_cstr(
    _: *mut c_void,
    _: *const std::ffi::c_char,
) -> *const std::ffi::c_char {
    std::ptr::null()
}

unsafe extern "C" fn noop_cstr2(
    _: *mut c_void,
    _: *const std::ffi::c_char,
    _: *const std::ffi::c_char,
) -> *const std::ffi::c_char {
    std::ptr::null()
}

unsafe extern "C" fn noop_plain(_: *mut c_void) -> *const std::ffi::c_char {
    std::ptr::null()
}

unsafe extern "C" fn noop_provide(
    _: *mut c_void,
    _: *const std::ffi::c_char,
    _: *const ServiceVTable,
) -> ServiceHandle {
    ServiceHandle::INVALID
}

/// 所有插件服务都不存在（= 没装 help / 没装 SQLite 的最小环境）。
unsafe extern "C" fn noop_get(_: *mut c_void, _: *const std::ffi::c_char) -> *const ServiceVTable {
    std::ptr::null()
}

/// ABI v6：宿主里没有任何服务时 `list_services` 回空串；解析不成 JSON 也按空处理。
unsafe extern "C" fn empty_services(_: *mut c_void) -> *const std::ffi::c_char {
    c"[]".as_ptr()
}

/// ABI v6：服务调用桥。没有服务时回 NULL（= 失败）。
unsafe extern "C" fn noop_call_service(
    _: *mut c_void,
    _: *const std::ffi::c_char,
    _: *const std::ffi::c_char,
    _: *const std::ffi::c_char,
) -> *const std::ffi::c_char {
    std::ptr::null()
}

/// ABI v6：`set_config_section`。假宿主一律拒绝写。
unsafe extern "C" fn noop_set_config_section(
    _: *mut c_void,
    _: *const std::ffi::c_char,
    _: *const std::ffi::c_char,
    _: *const std::ffi::c_char,
) -> i32 {
    -1
}

unsafe extern "C" fn noop_middleware(
    _: *mut c_void,
    _: u32,
    _: Option<unsafe extern "C-unwind" fn(*mut c_void, u32, *const std::ffi::c_char) -> i32>,
    _: *mut c_void,
) -> MiddlewareHandle {
    MiddlewareHandle::INVALID
}

unsafe extern "C" fn noop_wait(_: *mut c_void, _: *const std::ffi::c_char, _: u64) -> WaitHandle {
    WaitHandle::INVALID
}

unsafe extern "C" fn noop_cancel(_: *mut c_void, _: WaitHandle) {}

unsafe extern "C" fn noop_log(_: *mut c_void, _: u32, _: *const std::ffi::c_char) {}

unsafe extern "C" fn noop_set_config(
    _: *mut c_void,
    _: *const std::ffi::c_char,
    _: *const std::ffi::c_char,
) -> i32 {
    -1
}

fn fake_vtable() -> HostApiVTable {
    HostApiVTable {
        version: ABI_VERSION,
        size: std::mem::size_of::<HostApiVTable>() as u32,
        host_ctx: std::ptr::null_mut(),
        on: Some(fake_on),
        off: Some(fake_off),
        emit: Some(noop_emit),
        register_tool: Some(noop_register_tool),
        call_tool: Some(noop_request_id2),
        call_tool_result: Some(noop_cstr2),
        list_tools: Some(noop_plain),
        list_plugins: Some(noop_plain),
        provide_service: Some(noop_provide),
        provide_service_shared: Some(noop_provide),
        get_service: Some(noop_get),
        td_invoke: Some(noop_request_id),
        td_invoke_simplified: Some(noop_request_id2),
        td_execute: Some(noop_cstr),
        use_middleware: Some(noop_middleware),
        wait_for_update: Some(noop_wait),
        cancel_wait: Some(noop_cancel),
        log: Some(noop_log),
        get_config: Some(noop_cstr2),
        set_config: Some(noop_set_config),
        simplify_invoke: Some(noop_request_id),
        simplify_invoke_result: Some(noop_cstr),
        // ABI v6 新增：调用者身份 / 服务调用桥 / 失败原因 / 服务清单 / 跨插件写配置。
        current_plugin: Some(noop_plain),
        call_service: Some(noop_call_service),
        last_error: Some(noop_plain),
        list_services: Some(empty_services),
        set_config_section: Some(noop_set_config_section),
    }
}

#[test]
fn start_and_stop_survive_a_minimal_host() {
    let vtable = fake_vtable();
    // SAFETY: vtable 在整个测试期间存活，且所有必需函数都已填。
    let host = unsafe { HostApi::from_raw(&vtable) }.expect("假宿主应当能构造 HostApi");

    let mut plugin = <DouyinPlugin as Plugin>::init(host).expect("init 不该失败");
    // 目录不存在 / 没有 cookies.json：只该 warn，不该让 start 失败。
    let config = r#"{"plugins_dir":"target/lifecycle-test/plugins"}"#;
    plugin
        .start(config)
        .expect("没有 SQL / help / cookie 时 start 也必须成功");

    // 让「缓存初始化」那个异步任务跑完（它只查一次 service，立刻返回）。
    std::thread::sleep(Duration::from_millis(100));

    let started = Instant::now();
    plugin.stop();
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "stop 必须快速返回，实际 {:?}",
        started.elapsed()
    );
}

/// `stop` 重复调用不能崩（宿主探测插件时也会调，那时 `start` 没跑过）。
#[test]
fn stop_without_start_is_a_no_op() {
    let vtable = fake_vtable();
    // SAFETY: 同上。
    let host = unsafe { HostApi::from_raw(&vtable) }.expect("假宿主");
    let mut plugin = <DouyinPlugin as Plugin>::init(host).expect("init");
    plugin.stop();
    plugin.stop();
}
