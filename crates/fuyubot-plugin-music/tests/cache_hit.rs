//! 端到端：**SQL 缓存命中 → 不访问网络、直接用 `inputFileId` 发送**。
//!
//! 这是用户核心诉求（「下次同一个音乐 id + 音质直接用文件 id 发送，不再拉接口、
//! 不再下载」）的唯一端到端证据。做法：
//!
//! 1. 假 `HostApiVTable`：`get_service("fuyubot.sqlite")` 返回一个静态
//!    `ServiceVTable`（migrate / query / execute），数据放进程内的
//!    `Mutex<Vec<(key, doc)>>`；预置 `netease:186016:lossless` 一行。
//! 2. 假 `simplify_invoke`：把 op JSON 记下来，**同步** fire `simplifyResult`
//!    （router 的「先发后登记」缓冲会兜住，早到也不会丢）。
//! 3. `MusicPlugin::start` → 等日志里出现「SQL 缓存已启用」（说明异步 `cache_init`
//!    真的完成了）→ fire 一条 `/music163 186016 lossless` 的 `updateNewMessage`。
//! 4. 断言发出的 `send_message` op 里音频文件是 `inputFileId` 4242、封面是 7777，
//!    且没有 `inputFileLocal` / `inputFileRemote` / `inputFileURL` 回退。
//! 5. `stop()` 必须 5 秒内返回。
//!
//! 订阅/记录都用**全局** `Mutex`（不是 thread_local）：插件的处理任务跑在它自己的
//! tokio worker 线程上，`simplify_invoke` 也在那里被调用。

use std::cell::RefCell;
use std::ffi::{CStr, CString, c_char, c_void};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use fuyubot_abi::{
    ABI_VERSION, EventCallbackFn, HostApi, HostApiVTable, MiddlewareCallbackFn, MiddlewareHandle,
    Plugin, RequestId, ServiceHandle, ServiceMethod, ServiceVTable, SubscriptionHandle, ToolHandle,
    ToolHandlerFn, WaitHandle,
};
use fuyubot_plugin_music::MusicPlugin;
use serde_json::{Value, json};

// ── 假宿主的全局状态 ────────────────────────────────────────────────

#[derive(Clone)]
struct Subscription {
    event: String,
    callback: EventCallbackFn,
    user_data: usize,
}

static SUBSCRIPTIONS: Mutex<Vec<Subscription>> = Mutex::new(Vec::new());
static NEXT_HANDLE: AtomicU64 = AtomicU64::new(1);
static NEXT_REQUEST: AtomicU64 = AtomicU64::new(1000);

/// 插件通过 `simplify_invoke` 发下来的 op（按顺序）。
static CAPTURED_OPS: Mutex<Vec<Value>> = Mutex::new(Vec::new());
/// 插件通过 `execute` 写库的记录（命中路径不该有写）。
static EXECUTED_SQL: Mutex<Vec<String>> = Mutex::new(Vec::new());
/// 插件通过 `query` 读过的键（证明命中真的来自 SQL 缓存）。
static QUERIED_KEYS: Mutex<Vec<String>> = Mutex::new(Vec::new());
/// 插件记的日志（用来确定 `cache_init` 完成）。
static LOGS: Mutex<Vec<String>> = Mutex::new(Vec::new());
/// 假 sqlite 的 key → doc 文本。
static CACHE_DB: Mutex<Vec<(String, String)>> = Mutex::new(Vec::new());
/// `td_invoke_simplified` 被调用次数（发送链路不该再发任何 TDLib 方法请求）。
static TD_METHOD_CALLS: AtomicU64 = AtomicU64::new(0);
/// `migrate` 是否被调用过（备用信号，主信号是日志）。
static MIGRATED: Mutex<bool> = Mutex::new(false);
/// 让假 `migrate` 慢多少毫秒（模拟启动期异步 init 还没跑完的竞态窗口）。
static MIGRATE_DELAY_MS: AtomicU64 = AtomicU64::new(0);
/// 同一测试二进制里的用例共享全局静态状态（假宿主 / 假 sqlite / 日志），必须串行。
static TEST_LOCK: Mutex<()> = Mutex::new(());

/// 缓存里预置的那一行。
const CACHED_KEY: &str = "netease:186016:lossless";
const CACHED_DOC: &str = r#"{"v":1,"kind":"audio","file_id":4242,"cover_id":7777,"title":"晴天","performer":"周杰伦","album":"叶惠美","link":"https://music.163.com/song?id=186016","quality_label":"无损（lossless）","bitrate":999000,"size":1000,"duration":269,"trial":false}"#;

fn reset_state() {
    SUBSCRIPTIONS.lock().unwrap().clear();
    CAPTURED_OPS.lock().unwrap().clear();
    EXECUTED_SQL.lock().unwrap().clear();
    QUERIED_KEYS.lock().unwrap().clear();
    LOGS.lock().unwrap().clear();
    CACHE_DB.lock().unwrap().clear();
    *MIGRATED.lock().unwrap() = false;
    TD_METHOD_CALLS.store(0, Ordering::SeqCst);
    MIGRATE_DELAY_MS.store(0, Ordering::SeqCst);
    CACHE_DB
        .lock()
        .unwrap()
        .push((CACHED_KEY.to_owned(), CACHED_DOC.to_owned()));
}

fn fire(event: &str, payload: &str) {
    let targets: Vec<(EventCallbackFn, usize)> = {
        let subs = SUBSCRIPTIONS.lock().unwrap();
        subs.iter()
            .filter(|subscription| subscription.event == event)
            .map(|subscription| (subscription.callback, subscription.user_data))
            .collect()
    };
    for (callback, user_data) in targets {
        let text = CString::new(payload).expect("payload");
        // SAFETY: 回调与 user_data 由 fake_on 存下；payload 在调用期间存活。
        unsafe { callback(user_data as *mut c_void, text.as_ptr()) };
    }
}

// ── 假 sqlite 服务 ──────────────────────────────────────────────────

thread_local! {
    /// 宿主契约：服务返回值由提供方持有，只到本线程下一次调用前有效。
    static SERVICE_REPLY: RefCell<CString> = RefCell::new(CString::new("").expect("empty"));
}

fn reply(text: &str) -> *const c_char {
    SERVICE_REPLY.with(|cell| {
        *cell.borrow_mut() = CString::new(text).expect("reply");
        cell.borrow().as_ptr()
    })
}

fn input_value(input_json: *const c_char) -> Value {
    if input_json.is_null() {
        return Value::Null;
    }
    // SAFETY: 宿主契约保证入参是 NUL 结尾字符串。
    let text = unsafe { CStr::from_ptr(input_json) }.to_string_lossy();
    serde_json::from_str(&text).unwrap_or(Value::Null)
}

unsafe extern "C" fn service_migrate(
    _context: *mut c_void,
    _input: *const c_char,
) -> *const c_char {
    // 慢 migrate：模拟「启动期异步 init 还没跑完，第一条命令已经到了」的竞态窗口。
    let delay = MIGRATE_DELAY_MS.load(Ordering::SeqCst);
    if delay > 0 {
        std::thread::sleep(Duration::from_millis(delay));
    }
    *MIGRATED.lock().unwrap() = true;
    reply(
        r#"{"ok":true,"plugin":"fuyubot.music","applied":[{"template":"document","table":"media_cache","created":true,"statements":1}]}"#,
    )
}

unsafe extern "C" fn service_query(_context: *mut c_void, input: *const c_char) -> *const c_char {
    let request = input_value(input);
    let key = request
        .get("params")
        .and_then(Value::as_array)
        .and_then(|params| params.first())
        .and_then(Value::as_str)
        .unwrap_or_default();
    let doc = CACHE_DB
        .lock()
        .unwrap()
        .iter()
        .find(|(candidate, _)| candidate == key)
        .map(|(_, doc)| doc.clone());
    QUERIED_KEYS.lock().unwrap().push(key.to_owned());
    match doc {
        Some(doc) => reply(
            &json!({
                "ok": true,
                "columns": ["doc"],
                "rows": [[doc]],
                "row_count": 1,
                "truncated": false,
            })
            .to_string(),
        ),
        None => reply(r#"{"ok":true,"columns":[],"rows":[],"row_count":0,"truncated":false}"#),
    }
}

unsafe extern "C" fn service_execute(_context: *mut c_void, input: *const c_char) -> *const c_char {
    let request = input_value(input);
    let sql = request
        .get("sql")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let params: Vec<Value> = request
        .get("params")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    EXECUTED_SQL.lock().unwrap().push(sql.clone());
    if sql.starts_with("INSERT") && params.len() >= 2 {
        let key = params[0].as_str().unwrap_or_default().to_owned();
        let doc = params[1].as_str().unwrap_or_default().to_owned();
        let mut db = CACHE_DB.lock().unwrap();
        db.retain(|(candidate, _)| candidate != &key);
        db.push((key, doc));
    } else if sql.starts_with("DELETE") && !params.is_empty() {
        let key = params[0].as_str().unwrap_or_default();
        CACHE_DB
            .lock()
            .unwrap()
            .retain(|(candidate, _)| candidate != key);
    }
    reply(r#"{"ok":true,"changes":1,"last_insert_rowid":1}"#)
}

/// 假 `fuyubot.sqlite` 的 vtable（进程内只建一次，泄漏到进程结束）。
fn service_vtable() -> *const ServiceVTable {
    static TABLE: OnceLock<usize> = OnceLock::new();
    let table = *TABLE.get_or_init(|| {
        let methods: &'static [ServiceMethod] = Box::leak(
            vec![
                ServiceMethod {
                    name: c"migrate".as_ptr(),
                    func: Some(service_migrate),
                },
                ServiceMethod {
                    name: c"query".as_ptr(),
                    func: Some(service_query),
                },
                ServiceMethod {
                    name: c"execute".as_ptr(),
                    func: Some(service_execute),
                },
            ]
            .into_boxed_slice(),
        );
        let vtable = Box::leak(Box::new(ServiceVTable {
            name: c"fuyubot.sqlite".as_ptr(),
            version: 1,
            size: std::mem::size_of::<ServiceVTable>() as u32,
            context: std::ptr::null_mut(),
            methods: methods.as_ptr(),
            method_count: methods.len(),
        }));
        vtable as *const ServiceVTable as usize
    });
    table as *const ServiceVTable
}

/// ABI v6：服务调用桥。宿主把 `(服务名, 方法名, 入参)` 交过来，插件不再直接
/// 取别人的 vtable 调函数指针——那条路提供方看不到调用者。
unsafe extern "C" fn fake_call_service(
    _host: *mut c_void,
    name: *const c_char,
    method: *const c_char,
    input: *const c_char,
) -> *const c_char {
    if name.is_null() || method.is_null() {
        return std::ptr::null();
    }
    // SAFETY: 宿主契约保证名字是 NUL 结尾字符串。
    let service = unsafe { CStr::from_ptr(name) }.to_string_lossy();
    if service != "fuyubot.sqlite" {
        return std::ptr::null();
    }
    let method = unsafe { CStr::from_ptr(method) }.to_string_lossy();
    // SAFETY: 上下文在假服务里不用，入参由宿主传、调用期间存活。
    unsafe {
        match method.as_ref() {
            "migrate" => service_migrate(std::ptr::null_mut(), input),
            "query" => service_query(std::ptr::null_mut(), input),
            "execute" => service_execute(std::ptr::null_mut(), input),
            _ => std::ptr::null(),
        }
    }
}

/// ABI v6：本测试里能看见的服务清单（可用性探测走它，不再 `get_service`）。
unsafe extern "C" fn fake_list_services(_: *mut c_void) -> *const c_char {
    static SERVICES: OnceLock<CString> = OnceLock::new();
    SERVICES
        .get_or_init(|| {
            CString::new(
                r#"[{"name":"fuyubot.sqlite","version":1,"methods":["migrate","query","execute"],"plugin":"fuyubot.sqlite"}]"#,
            )
            .expect("清单里没有 NUL")
        })
        .as_ptr()
}

// ── 假宿主 vtable ───────────────────────────────────────────────────

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
    let id = NEXT_HANDLE.fetch_add(1, Ordering::SeqCst);
    SUBSCRIPTIONS.lock().unwrap().push(Subscription {
        event,
        callback,
        user_data: user_data as usize,
    });
    SubscriptionHandle { id }
}

unsafe extern "C" fn fake_off(_host: *mut c_void, _handle: SubscriptionHandle) {}

unsafe extern "C" fn fake_get_service(
    _host: *mut c_void,
    name: *const c_char,
) -> *const ServiceVTable {
    if name.is_null() {
        return std::ptr::null();
    }
    // SAFETY: 宿主契约保证 name 是 NUL 结尾字符串。
    let name = unsafe { CStr::from_ptr(name) }.to_string_lossy();
    if name == "fuyubot.sqlite" {
        service_vtable()
    } else {
        std::ptr::null()
    }
}

/// 记录 op + 同步 fire `simplifyResult`（响应比返回早，走 router 缓冲）。
unsafe extern "C" fn fake_simplify_invoke(_host: *mut c_void, op: *const c_char) -> RequestId {
    let op_json = if op.is_null() {
        String::new()
    } else {
        // SAFETY: 宿主契约保证 op 是 NUL 结尾字符串。
        unsafe { CStr::from_ptr(op) }.to_string_lossy().into_owned()
    };
    let value: Value = serde_json::from_str(&op_json).unwrap_or(Value::Null);
    CAPTURED_OPS.lock().unwrap().push(value.clone());
    let id = NEXT_REQUEST.fetch_add(1, Ordering::SeqCst);
    let op_name = value
        .get("op")
        .and_then(Value::as_str)
        .unwrap_or("send_message");
    let payload = json!({
        "@type": "simplifyResult",
        "op": op_name,
        "request_id": id,
        "ok": true,
        "result": {
            "@type": "message",
            "id": 555,
            "chat_id": -1001234567890i64,
            "content": {
                "@type": "messageText",
                "text": {"@type": "formattedText", "text": "ok", "entities": []}
            }
        }
    });
    fire("response", &payload.to_string());
    RequestId { id }
}

/// 插件不该再发任何 TDLib 方法请求（旧版为判群聊发过一个，已删除）。计数器钉住这一点。
unsafe extern "C" fn fake_td_invoke_simplified(
    _host: *mut c_void,
    _method: *const c_char,
    _params: *const c_char,
) -> RequestId {
    TD_METHOD_CALLS.fetch_add(1, Ordering::SeqCst);
    RequestId::INVALID
}

unsafe extern "C" fn fake_log(_host: *mut c_void, _level: u32, message: *const c_char) {
    if message.is_null() {
        return;
    }
    // SAFETY: 宿主契约保证 message 是 NUL 结尾字符串。
    let text = unsafe { CStr::from_ptr(message) }
        .to_string_lossy()
        .into_owned();
    LOGS.lock().unwrap().push(text);
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
        get_service: Some(fake_get_service),
        td_invoke: Some(noop_td),
        td_invoke_simplified: Some(fake_td_invoke_simplified),
        td_execute: Some(noop_execute),
        use_middleware: Some(noop_middleware),
        wait_for_update: Some(noop_wait),
        cancel_wait: Some(noop_cancel),
        log: Some(fake_log),
        get_config: Some(noop_get_config),
        set_config: Some(noop_set_config),
        provide_service_shared: Some(noop_provide),
        simplify_invoke: Some(fake_simplify_invoke),
        simplify_invoke_result: Some(noop_simplify_invoke_result),
        // ABI v6 新增：调用者身份 / 服务调用桥 / 失败原因 / 服务清单 / 跨插件写配置。
        current_plugin: Some(noop_list),
        call_service: Some(fake_call_service),
        last_error: Some(noop_list),
        list_services: Some(fake_list_services),
        set_config_section: Some(noop_set_config_section),
    };
    // SAFETY: vtable 的所有必需函数都已填；`from_raw` 会拷走函数指针。
    unsafe { HostApi::from_raw(&vtable) }.expect("fake host")
}

// ── 测试 ────────────────────────────────────────────────────────────

fn wait_until(timeout: Duration, mut condition: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if condition() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    condition()
}

/// 一条 `/music163 186016 lossless` 的 `updateNewMessage`（群里发的文本命令）。
fn command_update() -> Value {
    json!({
        "@type": "updateNewMessage",
        "message": {
            "@type": "message",
            "id": 42,
            "chat_id": -1001234567890i64,
            "is_outgoing": false,
            "content": {
                "@type": "messageText",
                "text": {
                    "@type": "formattedText",
                    "text": "/music163 186016 lossless",
                    "entities": []
                }
            }
        }
    })
}

/// 缓存命中：`/music163 186016 lossless` 直接用 `inputFileId` 发送，不下载、不解析。
#[test]
fn a_cache_hit_sends_the_stored_file_ids_without_any_download() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    reset_state();

    let dir = std::env::temp_dir().join(format!("fuyubot-music-cache-hit-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);

    let mut plugin = MusicPlugin::init(fake_host()).expect("init");
    let config = json!({ "plugins_dir": dir.to_string_lossy() }).to_string();
    plugin.start(&config).expect("start");

    // `cache_init` 是异步任务：等日志确认它真的完成（此时 cache.available()==true）。
    let enabled = wait_until(Duration::from_secs(5), || {
        LOGS.lock()
            .unwrap()
            .iter()
            .any(|line| line.contains("SQL 缓存已启用"))
    });
    assert!(
        enabled,
        "SQL 缓存没有初始化成功；日志：{:?}",
        LOGS.lock().unwrap()
    );

    // 模拟用户在群里发 `/music163 186016 lossless`。
    fire("updateNewMessage", &command_update().to_string());

    // 等插件把 send_message op 发下来。
    let captured = wait_until(Duration::from_secs(5), || {
        CAPTURED_OPS
            .lock()
            .unwrap()
            .iter()
            .any(|op| op.get("op").and_then(Value::as_str) == Some("send_message"))
    });
    let ops = CAPTURED_OPS.lock().unwrap().clone();
    assert!(
        captured,
        "缓存命中后应当发出 send_message；captured={ops:?}"
    );

    // 只有这一条 op：命中路径不该再发「解析 / 搜索」之类的请求，也不该发错误文案。
    assert_eq!(ops.len(), 1, "只该记录一条 op；captured={ops:?}");
    let sends: Vec<&Value> = ops
        .iter()
        .filter(|op| op.get("op").and_then(Value::as_str) == Some("send_message"))
        .collect();
    assert_eq!(sends.len(), 1, "只该发一条消息；captured={ops:?}");
    let op = sends[0];

    // 音频本体与封面都必须是 TDLib 已知文件 id（inputFileId），不是本地路径 / URL。
    let content = &op["args"]["input_message_content"];
    assert_eq!(content["@type"], "inputMessageAudio", "{content}");
    assert_eq!(
        content["audio"]["audio"],
        json!({ "@type": "inputFileId", "id": 4242 }),
        "音频应当用缓存里的 file id：{content}"
    );
    assert_eq!(
        content["audio"]["album_cover_thumbnail"]["thumbnail"],
        json!({ "@type": "inputFileId", "id": 7777 }),
        "封面应当用缓存里的 file id：{content}"
    );

    // 文本信息也来自缓存（不是重新解析接口）。
    assert_eq!(content["audio"]["title"], "晴天");
    assert_eq!(content["audio"]["performer"], "周杰伦");
    assert_eq!(content["audio"]["duration"], 269);
    assert!(
        content["caption"]["text"]
            .as_str()
            .is_some_and(|caption| caption.contains("无损（lossless）")),
        "文案应当带缓存的音质：{content}"
    );

    // 没有任何「回退到下载」的痕迹。
    let serialised = op.to_string();
    for forbidden in ["inputFileLocal", "inputFileRemote", "inputFileURL"] {
        assert!(
            !serialised.contains(forbidden),
            "缓存命中不该出现 {forbidden}：{serialised}"
        );
    }

    // 不引用原消息：sendMessage 参数里没有 reply_to。
    assert!(
        op["args"].get("reply_to").is_none(),
        "本插件不引用原消息：{serialised}"
    );
    assert!(!serialised.contains("reply_to"), "{serialised}");

    // 发送链路不该再发任何 TDLib 方法请求（判群聊那次已删除）。
    assert_eq!(
        TD_METHOD_CALLS.load(Ordering::SeqCst),
        0,
        "发送链路不该再调用 TDLib 方法"
    );

    // 命中路径只读缓存，不该写库（写库只发生在完整流程发送成功之后）。
    assert!(
        EXECUTED_SQL.lock().unwrap().is_empty(),
        "缓存命中不该写库：{:?}",
        EXECUTED_SQL.lock().unwrap()
    );
    // 而且读的正是「网易云 186016 + 用户请求的 lossless」这个键。
    let queried = QUERIED_KEYS.lock().unwrap().clone();
    assert!(
        queried.iter().any(|key| key == CACHED_KEY),
        "缓存命中必须来自 SQL 查询 {CACHED_KEY}；实际查询：{queried:?}"
    );

    // 停机必须快速返回（没有卡在下载 / 上传预算里）。
    let started = Instant::now();
    plugin.stop();
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "stop 必须 5 秒内返回（实际 {:?}）",
        started.elapsed()
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// 回归：启动期 cache init 很慢（`migrate` 睡 200ms）时，**第一条命令也不能 miss**。
///
/// `start()` 里的 init 是异步任务。旧实现里命令若比它快，`cache_get` 看到
/// `available()==false` 就直接 miss → 走网络（或发错误文案）。现在 `cache_get`
/// 会先 `ensure_cache()` 等 init 完成（幂等 + 串行锁），所以第一条 op 仍然是
/// `inputFileId` 命中。
#[test]
fn a_slow_cache_init_still_lets_the_first_command_hit() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    reset_state();
    // 模拟「init 比命令慢」：migrate 睡 200ms。
    MIGRATE_DELAY_MS.store(200, Ordering::SeqCst);

    let dir = std::env::temp_dir().join(format!("fuyubot-music-cache-race-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);

    let mut plugin = MusicPlugin::init(fake_host()).expect("init");
    let config = json!({ "plugins_dir": dir.to_string_lossy() }).to_string();
    plugin.start(&config).expect("start");

    // 测试前提：`start()` 返回时 init 还没完成（migrate 要睡 200ms）。
    assert!(
        !LOGS
            .lock()
            .unwrap()
            .iter()
            .any(|line| line.contains("SQL 缓存已启用")),
        "测试前提不成立：init 不该在 start() 返回前完成"
    );

    // **立即**发命令，不等 init 完成。
    let fired_at = Instant::now();
    fire("updateNewMessage", &command_update().to_string());

    // 第一条 op 必须是命中后的音频发送（不是搜索 / 错误文案 / 本地文件回退）。
    let captured = wait_until(Duration::from_secs(5), || {
        !CAPTURED_OPS.lock().unwrap().is_empty()
    });
    // 证明命令确实等到了慢 init（migrate 睡 200ms；下限留足余量防抖动）。
    assert!(
        fired_at.elapsed() >= Duration::from_millis(150),
        "命令应当等 init 完成后再命中（实测 {:?}）",
        fired_at.elapsed()
    );
    let ops = CAPTURED_OPS.lock().unwrap().clone();
    assert!(
        captured,
        "5s 内没有捕获到任何 op；日志：{:?}",
        LOGS.lock().unwrap()
    );
    assert_eq!(ops.len(), 1, "只该有一条 op；captured={ops:?}");
    let op = &ops[0];
    assert_eq!(op["op"], "send_message", "第一条 op 必须是发送：{op}");
    let content = &op["args"]["input_message_content"];
    assert_eq!(content["@type"], "inputMessageAudio", "{content}");
    assert_eq!(
        content["audio"]["audio"],
        json!({ "@type": "inputFileId", "id": 4242 }),
        "init 慢时第一条命令也必须命中缓存：{content}"
    );
    assert_eq!(
        content["audio"]["album_cover_thumbnail"]["thumbnail"],
        json!({ "@type": "inputFileId", "id": 7777 }),
        "{content}"
    );
    let serialised = op.to_string();
    for forbidden in ["inputFileLocal", "inputFileRemote", "inputFileURL"] {
        assert!(
            !serialised.contains(forbidden),
            "init 慢时不该回退到下载：{serialised}"
        );
    }
    // 确实读到了预置的缓存键（证明是「等 init 完成后命中」，不是碰巧）。
    assert!(
        QUERIED_KEYS
            .lock()
            .unwrap()
            .iter()
            .any(|key| key == CACHED_KEY),
        "第一条命令应当查过缓存键 {CACHED_KEY}"
    );

    let started = Instant::now();
    plugin.stop();
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "stop 必须 5 秒内返回（实际 {:?}）",
        started.elapsed()
    );

    let _ = std::fs::remove_dir_all(&dir);
}
