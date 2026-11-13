//! 用假宿主验证异步响应的关键契约：**响应比 `invoke_op` 返回还早**时也不能丢，
//! 以及 `plugin start/stop` 的生命周期能跑通、`stop` 快速返回。
//!
//! 真实宿主里这条路径难得一撞（TDLib 往返有网络延迟），但失败路径（参数错 /
//! unknown_op）是立刻返回的。这里让假宿主在 `simplify_invoke` 里**同步**回调，
//! 复现最坏情况。
//!
//! 旧的同步 `ResponseHub` 已删除（改成 `fuyubot-abi` 的 `ResponseRouter`），
//! 所以这里断言的是 router 的「先发后登记」缓冲，而不是旧的 Condvar 等待。

use std::cell::RefCell;
use std::ffi::{CStr, CString, c_char, c_void};
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use fuyubot_abi::{
    ABI_VERSION, EventCallbackFn, HostApi, HostApiVTable, MiddlewareCallbackFn, MiddlewareHandle,
    Plugin, RequestId, ResponseRouter, ServiceHandle, ServiceVTable, SubscriptionHandle,
    ToolHandle, ToolHandlerFn, WaitHandle,
};
use fuyubot_plugin_music::MusicPlugin;
use fuyubot_plugin_music::runtime::Runtime;
use fuyubot_plugin_music::send;

struct Subscription {
    event: String,
    callback: EventCallbackFn,
    user_data: *mut c_void,
}

thread_local! {
    static SUBSCRIPTIONS: RefCell<Vec<Subscription>> = const { RefCell::new(Vec::new()) };
    static NEXT: RefCell<u64> = const { RefCell::new(1) };
}

fn fire(event: &str, payload: &str) {
    let targets = SUBSCRIPTIONS.with(|subs| {
        subs.borrow()
            .iter()
            .filter(|subscription| subscription.event == event)
            .map(|subscription| (subscription.callback, subscription.user_data))
            .collect::<Vec<_>>()
    });
    for (callback, user_data) in targets {
        let text = CString::new(payload).expect("payload");
        // SAFETY: 回调与 user_data 由 fake_on 存下；payload 在调用期间存活。
        unsafe { callback(user_data, text.as_ptr()) };
    }
}

unsafe extern "C" fn fake_on(
    _host: *mut c_void,
    event: *const c_char,
    _filter: *const c_char,
    callback: Option<EventCallbackFn>,
    user_data: *mut c_void,
) -> SubscriptionHandle {
    let Some(callback) = callback else {
        return SubscriptionHandle::INVALID;
    };
    // SAFETY: 宿主契约保证 event 是 NUL 结尾字符串。
    let event = unsafe { CStr::from_ptr(event) }
        .to_string_lossy()
        .into_owned();
    let id = NEXT.with(|next| {
        let mut next = next.borrow_mut();
        let id = *next;
        *next += 1;
        id
    });
    SUBSCRIPTIONS.with(|subs| {
        subs.borrow_mut().push(Subscription {
            event,
            callback,
            user_data,
        })
    });
    SubscriptionHandle { id }
}

unsafe extern "C" fn fake_off(_host: *mut c_void, _handle: SubscriptionHandle) {}

/// 最坏情况：响应在 `simplify_invoke` 返回之前就投出来。
unsafe extern "C" fn fake_simplify_invoke(_host: *mut c_void, _op: *const c_char) -> RequestId {
    fire(
        "response",
        r#"{"@type":"simplifyResult","op":"send_message","request_id":101,"ok":true,"result":{"id":1,"content":{"@type":"messageText"}}}"#,
    );
    RequestId { id: 101 }
}

unsafe extern "C" fn fake_td_invoke_simplified(
    _host: *mut c_void,
    _method: *const c_char,
    _params: *const c_char,
) -> RequestId {
    fire("response", r#"{"@type":"user","request_id":202,"id":1}"#);
    RequestId { id: 202 }
}

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
unsafe extern "C" fn noop_get_config(
    _: *mut c_void,
    _: *const c_char,
    _: *const c_char,
) -> *const c_char {
    std::ptr::null()
}
unsafe extern "C" fn noop_set_config(_: *mut c_void, _: *const c_char, _: *const c_char) -> i32 {
    -1
}
unsafe extern "C" fn noop_simplify_invoke_result(
    _: *mut c_void,
    _: *const c_char,
) -> *const c_char {
    std::ptr::null()
}
/// ABI v6：服务调用桥。这个测试不装任何插件服务，一律 NULL（= 失败）。
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
        on: Some(fake_on),
        off: Some(fake_off),
        emit: Some(noop_emit),
        register_tool: Some(noop_register_tool),
        call_tool: Some(noop_call_tool),
        call_tool_result: Some(noop_call_tool_result),
        list_tools: Some(noop_list),
        list_plugins: Some(noop_list),
        provide_service: Some(noop_provide),
        get_service: Some(noop_get),
        td_invoke: Some(noop_td),
        td_invoke_simplified: Some(fake_td_invoke_simplified),
        td_execute: Some(noop_execute),
        use_middleware: Some(noop_middleware),
        wait_for_update: Some(noop_wait),
        cancel_wait: Some(noop_cancel),
        log: Some(noop_log),
        get_config: Some(noop_get_config),
        set_config: Some(noop_set_config),
        provide_service_shared: Some(noop_provide),
        simplify_invoke: Some(fake_simplify_invoke),
        simplify_invoke_result: Some(noop_simplify_invoke_result),
        // ABI v6 新增：调用者身份 / 服务调用桥 / 失败原因 / 服务清单 / 跨插件写配置。
        current_plugin: Some(noop_list),
        call_service: Some(noop_call_service),
        last_error: Some(noop_list),
        list_services: Some(noop_list),
        set_config_section: Some(noop_set_config_section),
    };
    // SAFETY: vtable 的所有必需函数都已填；测试进程里它活得够久。
    unsafe { HostApi::from_raw(&vtable) }.expect("fake host")
}

fn clear_subscriptions() {
    SUBSCRIPTIONS.with(|subs| subs.borrow_mut().clear());
}

/// ① 响应比 `invoke_op` 返回还早：router 的缓冲必须兜住。
#[tokio::test]
async fn a_response_that_beats_the_invoke_call_is_not_lost() {
    clear_subscriptions();
    let router = ResponseRouter::new(fake_host()).expect("router");
    let (request, future) = router
        .invoke_op(
            r#"{"op":"send_message","args":{"chat_id":1,"input_message_content":{"@type":"inputMessageText","text":{"@type":"formattedText","text":"hi","entities":[]}}}}"#,
        )
        .expect("invoke_op");
    assert_eq!(request.id, 101);
    let result = tokio::time::timeout(Duration::from_secs(5), future)
        .await
        .expect("早到的响应不能丢，不该超时")
        .expect("响应通道不该关");
    assert_eq!(result["ok"], true);
    assert_eq!(result["result"]["id"], 1);
}

/// 同一套竞态在真正的发送路径上（`Runtime` + `send_text`）也要成立。
#[test]
fn send_text_waits_through_the_runtime() {
    clear_subscriptions();
    let runtime = Runtime::new(fake_host(), r#"{"plugins_dir":"plugins"}"#).expect("runtime");
    let target = send::Target::send(1);
    let outcome = runtime.block_on(send::send_text(&runtime, &target, "hi"));
    assert!(outcome.is_ok(), "早到的 simplifyResult 不能丢：{outcome:?}");
    runtime.cancel.store(true, Ordering::SeqCst);
    runtime.shutdown();
}

/// TDLib 方法响应（不是 update 通道）也能被 router 的 future 接住。
#[tokio::test]
async fn td_method_responses_are_awaited_too() {
    clear_subscriptions();
    let router = ResponseRouter::new(fake_host()).expect("router");
    let (request, future) = router.invoke_simplified("getMe", "{}").expect("getMe");
    assert_eq!(request.id, 202);
    let me = tokio::time::timeout(Duration::from_secs(5), future)
        .await
        .expect("不该超时")
        .expect("响应通道不该关");
    assert_eq!(me["@type"], "user");
}

/// ② `plugin start/stop` 生命周期：start 用最小 config，stop 必须快速返回。
#[test]
fn plugin_start_and_stop_finish_quickly() {
    clear_subscriptions();
    let mut plugin = MusicPlugin::init(fake_host()).expect("init");
    plugin
        .start(r#"{"plugins_dir":"D:/nonexistent/fuyubot-music-test/plugins"}"#)
        .expect("start");
    let started = Instant::now();
    plugin.stop();
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "stop 必须快速返回（实际 {:?}）",
        started.elapsed()
    );
}

/// 宿主探测插件时也会调 `stop`（那时 start 没跑过），不能 panic。
#[test]
fn stop_without_start_is_harmless() {
    clear_subscriptions();
    let mut plugin = MusicPlugin::init(fake_host()).expect("init");
    plugin.stop();
}
