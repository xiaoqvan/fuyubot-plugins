//! **端到端**缓存命中测试（用户核心诉求的证据）。
//!
//! 用假宿主 + 假 `fuyubot.sqlite` 服务，把整条链路真跑一遍：
//!
//! ```text
//! updateNewMessage（含抖音链接）
//!   → 自动识别 → 认出作品 id（本地路径解析，零网络）
//!   → 缓存命中（假 SQLite 的 SELECT 返回预置文档）
//!   → 直接发 inputFileId（不解析分享页、不下载、不先试远程直链）
//! ```
//!
//! 钉住的断言：
//!
//! 1. 全程**只有一条**发送 op——没有「先试远程直链」那条回退；
//! 2. 视频本体是 `inputFileId 4242`、封面是 `inputFileId 7777`；
//! 3. op JSON 里不出现任何 URL（`http` / `inputFileRemote`），也**没有 `reply_to`**；
//! 4. 命中路径不写缓存（假 SQLite 的 execute 一次都没被调）；
//! 5. **没有 `getChat`**（引用行为删掉后不再判群聊；`td_invoke_simplified` 一次都没被调）；
//! 6. `stop()` 5 秒内返回。
//!
//! 另外两条是**回归**（Lead 在真实宿主回放里发现并修掉的问题）：
//!
//! - `foreign_commands_are_not_answered`：`on_command` 对**所有**斜杠命令都会回调，
//!   本插件只认 `/dy`；`/music163 …` 之类的命令不能让它发「用法」。
//! - `cache_still_hits_when_migrate_is_slow`：启动期的 cache init 是异步的，
//!   第一条命令比 migrate 更快到达时也必须**命中缓存**（靠 `ensure_cache` 补一次 init），
//!   不能因为 init 没跑完就 miss 掉、跑去解析/下载。

use std::collections::HashMap;
use std::ffi::{CStr, CString, c_char, c_void};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

use fuyubot_abi::{
    ABI_VERSION, HostApi, HostApiVTable, MiddlewareHandle, Plugin, RESPONSE_EVENT, RequestId,
    ServiceHandle, ServiceMethod, ServiceVTable, SubscriptionHandle, ToolHandle, WaitHandle,
};
use fuyubot_plugin_douyin::{DouyinPlugin, cache};
use serde_json::{Value, json};

/// 测试用的作品链接（作品 id 只在路径里，本地就能认出来——这一步不联网）。
const LINK: &str = "https://www.douyin.com/video/7300000000000000001";

// ── 全局假状态（这个测试是单用例，静态量够了）────────────────────────

/// 假宿主收到的全部 simplify op JSON。
static OPS: Mutex<Vec<String>> = Mutex::new(Vec::new());
/// 假宿主收到的全部 `td_invoke_simplified` 方法名（`getChat` 当年就在这条路上）。
static TD_CALLS: Mutex<Vec<String>> = Mutex::new(Vec::new());
/// 假 SQLite 收到的全部 `execute` 请求（缓存命中路径不该有写入）。
static EXECUTED: Mutex<Vec<String>> = Mutex::new(Vec::new());
/// `migrate` 被调过没有（= 缓存 init 完成没完成）。
static MIGRATED: AtomicBool = AtomicBool::new(false);
/// 假 SQLite 的 `migrate` 人为延迟（毫秒）。用来复现「init 还没跑完，第一条命令就到了」。
static MIGRATE_DELAY_MS: AtomicU64 = AtomicU64::new(0);
/// 假请求号。
static NEXT_ID: AtomicU64 = AtomicU64::new(7001);
/// 假订阅号。
static NEXT_SUB: AtomicU64 = AtomicU64::new(1);
/// 假 SQLite 表数据：key → doc 文本。
static ROWS: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();
/// 假宿主的订阅表（`on` 存进来、`fire` 按事件名找人）。
static SUBSCRIPTIONS: OnceLock<Mutex<Vec<Subscription>>> = OnceLock::new();
/// 泄漏出去的假服务 vtable 地址（`OnceLock` 存地址避免 `*const` 的 Sync 问题）。
static SERVICE_PTR: OnceLock<usize> = OnceLock::new();
/// 泄漏出去的假宿主 vtable 地址。
static HOST_VTABLE: OnceLock<usize> = OnceLock::new();

fn rows() -> &'static Mutex<HashMap<String, String>> {
    ROWS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn subscriptions() -> &'static Mutex<Vec<Subscription>> {
    SUBSCRIPTIONS.get_or_init(|| Mutex::new(Vec::new()))
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// 一条假订阅（与宿主 `on` 的契约同形）。
struct Subscription {
    id: u64,
    event: String,
    callback: Option<unsafe extern "C-unwind" fn(*mut c_void, *const c_char) -> i32>,
    user_data: *mut c_void,
}

// SAFETY: 测试里所有访问都在同一进程、且只在持有锁时读指针；回调本身由插件保证
// （订阅活多久 `user_data` 就活多久）。
unsafe impl Send for Subscription {}
unsafe impl Sync for Subscription {}

/// 把一段文本放进线程局部缓冲并返回指针：宿主契约是「读方在下一次宿主调用前读完」。
fn reply(text: String) -> *const c_char {
    thread_local! {
        static BUFFER: std::cell::RefCell<CString> =
            std::cell::RefCell::new(CString::new("{}").expect("字面量没有 NUL"));
    }
    let text = CString::new(text).expect("回复里不能有 NUL");
    BUFFER.with(|buffer| {
        *buffer.borrow_mut() = text;
        buffer.borrow().as_ptr()
    })
}

fn reply_json(value: Value) -> *const c_char {
    reply(value.to_string())
}

fn request_of(input: *const c_char) -> Value {
    if input.is_null() {
        return Value::Null;
    }
    // SAFETY: 宿主保证入参是 NUL 结尾的临时字符串。
    let text = unsafe { CStr::from_ptr(input) }.to_string_lossy();
    serde_json::from_str(&text).unwrap_or(Value::Null)
}

// ── 假 `fuyubot.sqlite` 服务：migrate / query / execute ──────────────

unsafe extern "C" fn service_migrate(
    _context: *mut c_void,
    _input: *const c_char,
) -> *const c_char {
    MIGRATED.store(true, Ordering::SeqCst);
    // 与真 sqlite 插件的 `migrate` 回复同形；`applied[0].table` 是缓存真正使用的表名。
    reply_json(json!({
        "ok": true,
        "plugin": "fuyubot.douyin",
        "applied": [{
            "template": "document",
            "table": "media_cache",
            "created": true,
            "statements": 1,
        }],
    }))
}

unsafe extern "C" fn service_query(_context: *mut c_void, input: *const c_char) -> *const c_char {
    let request = request_of(input);
    let key = request["params"][0].as_str().unwrap_or_default();
    let doc = lock(rows()).get(key).cloned();
    match doc {
        Some(doc) => reply_json(json!({
            "ok": true,
            "columns": ["doc"],
            "rows": [[doc]],
            "row_count": 1,
            "truncated": false,
        })),
        None => reply_json(json!({
            "ok": true,
            "columns": ["doc"],
            "rows": [],
            "row_count": 0,
            "truncated": false,
        })),
    }
}

unsafe extern "C" fn service_execute(_context: *mut c_void, input: *const c_char) -> *const c_char {
    let request = request_of(input);
    lock(&EXECUTED).push(request.to_string());
    reply_json(json!({
        "ok": true,
        "changes": 1,
        "last_insert_rowid": 1,
        "database": "mem",
    }))
}

/// 假服务 vtable；泄漏出去让它活到进程结束（`get_service` 返回裸指针）。
fn service_vtable() -> *const ServiceVTable {
    let address = *SERVICE_PTR.get_or_init(|| {
        // 名字必须是 NUL 结尾且活得够久：`Box::leak` 进 'static。
        let names: Vec<&'static CString> = ["fuyubot.sqlite", "migrate", "query", "execute"]
            .iter()
            .map(|name| {
                Box::leak(Box::new(CString::new(*name).expect("方法名没有 NUL")))
                    as &'static CString
            })
            .collect();
        let methods: &'static [ServiceMethod] = Box::leak(
            vec![
                ServiceMethod {
                    name: names[1].as_ptr(),
                    func: Some(service_migrate),
                },
                ServiceMethod {
                    name: names[2].as_ptr(),
                    func: Some(service_query),
                },
                ServiceMethod {
                    name: names[3].as_ptr(),
                    func: Some(service_execute),
                },
            ]
            .into_boxed_slice(),
        );
        let table: &'static ServiceVTable = Box::leak(Box::new(ServiceVTable {
            name: names[0].as_ptr(),
            version: 1,
            size: std::mem::size_of::<ServiceVTable>() as u32,
            context: std::ptr::null_mut(),
            methods: methods.as_ptr(),
            method_count: methods.len(),
        }));
        table as *const ServiceVTable as usize
    });
    address as *const ServiceVTable
}

/// ABI v6：服务调用桥。宿主把 `(服务名, 方法名, 入参)` 交过来，插件不再直接
/// 取别人的 vtable 调函数指针——那条路提供方看不到调用者。
unsafe extern "C" fn fake_call_service(
    _host: *mut c_void,
    name: *const c_char,
    method: *const c_char,
    input: *const c_char,
) -> *const c_char {
    // SAFETY: 宿主契约保证三个入参都是 NUL 结尾字符串。
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

// ── 假宿主 VTable ───────────────────────────────────────────────────

unsafe extern "C" fn fake_on(
    _host: *mut c_void,
    event: *const c_char,
    _filter: *const c_char,
    callback: Option<unsafe extern "C-unwind" fn(*mut c_void, *const c_char) -> i32>,
    user_data: *mut c_void,
) -> SubscriptionHandle {
    // SAFETY: 宿主保证事件名是 NUL 结尾字符串。
    let event = unsafe { CStr::from_ptr(event) }
        .to_string_lossy()
        .into_owned();
    let id = NEXT_SUB.fetch_add(1, Ordering::SeqCst);
    lock(subscriptions()).push(Subscription {
        id,
        event,
        callback,
        user_data,
    });
    SubscriptionHandle { id }
}

unsafe extern "C" fn fake_off(_host: *mut c_void, handle: SubscriptionHandle) {
    lock(subscriptions()).retain(|subscription| subscription.id != handle.id);
}

/// 把一条事件投给所有匹配的订阅（**不**持锁跑回调）。
fn fire(event: &str, payload: &Value) {
    let text = CString::new(payload.to_string()).expect("负载里不能有 NUL");
    let targets: Vec<(
        Option<unsafe extern "C-unwind" fn(*mut c_void, *const c_char) -> i32>,
        *mut c_void,
    )> = {
        let subscriptions = lock(subscriptions());
        subscriptions
            .iter()
            .filter(|subscription| subscription.event == event)
            .map(|subscription| (subscription.callback, subscription.user_data))
            .collect()
    };
    for (callback, user_data) in targets {
        if let Some(callback) = callback {
            // SAFETY: 回调与 `user_data` 的存活由订阅持有者保证（契约同真宿主）。
            unsafe { callback(user_data, text.as_ptr()) };
        }
    }
}

/// 假 `simplify_invoke`：记录 op、分配请求号，并**同步**把 `simplifyResult` 投回去。
///
/// 同步投递正好覆盖 `ResponseRouter` 的「响应比登记先到」竞态缓冲。
unsafe extern "C" fn fake_simplify_invoke(_host: *mut c_void, op_json: *const c_char) -> RequestId {
    // SAFETY: 宿主保证入参是 NUL 结尾的临时字符串。
    let op = unsafe { CStr::from_ptr(op_json) }
        .to_string_lossy()
        .into_owned();
    lock(&OPS).push(op);
    let id = NEXT_ID.fetch_add(1, Ordering::SeqCst);
    let message = json!({
        "@type": "message",
        "id": 900 + id,
        "chat_id": -100123,
        "content": { "@type": "messageText" },
    });
    fire(
        RESPONSE_EVENT,
        &json!({
            "@type": "simplifyResult",
            "op": "send_message",
            "ok": true,
            "result": message,
            "request_id": id,
        }),
    );
    RequestId { id }
}

unsafe extern "C" fn noop_emit(_: *mut c_void, _: *const c_char, _: *const c_char) -> i32 {
    0
}

unsafe extern "C" fn noop_register_tool(
    _: *mut c_void,
    _: *const c_char,
    _: Option<fuyubot_abi::ToolHandlerFn>,
    _: *mut c_void,
) -> ToolHandle {
    ToolHandle::INVALID
}

unsafe extern "C" fn noop_request_id(_: *mut c_void, _: *const c_char) -> RequestId {
    RequestId::INVALID
}

unsafe extern "C" fn noop_request_id2(
    _: *mut c_void,
    _: *const c_char,
    _: *const c_char,
) -> RequestId {
    RequestId::INVALID
}

/// `td_invoke_simplified`：本测试里**不该**被调到（引用行为删掉后不再有 getChat）。
unsafe extern "C" fn fake_td_invoke_simplified(
    _host: *mut c_void,
    method: *const c_char,
    _params: *const c_char,
) -> RequestId {
    // SAFETY: 宿主保证方法名是 NUL 结尾字符串。
    let method = unsafe { CStr::from_ptr(method) }
        .to_string_lossy()
        .into_owned();
    lock(&TD_CALLS).push(method);
    RequestId::INVALID
}

unsafe extern "C" fn noop_cstr(_: *mut c_void, _: *const c_char) -> *const c_char {
    std::ptr::null()
}

unsafe extern "C" fn noop_cstr2(
    _: *mut c_void,
    _: *const c_char,
    _: *const c_char,
) -> *const c_char {
    std::ptr::null()
}

unsafe extern "C" fn noop_plain(_: *mut c_void) -> *const c_char {
    std::ptr::null()
}

unsafe extern "C" fn noop_provide(
    _: *mut c_void,
    _: *const c_char,
    _: *const ServiceVTable,
) -> ServiceHandle {
    ServiceHandle::INVALID
}

unsafe extern "C" fn fake_get_service(
    _host: *mut c_void,
    name: *const c_char,
) -> *const ServiceVTable {
    // SAFETY: 宿主保证服务名是 NUL 结尾字符串。
    let name = unsafe { CStr::from_ptr(name) }.to_string_lossy();
    if name == "fuyubot.sqlite" {
        service_vtable()
    } else {
        std::ptr::null()
    }
}

unsafe extern "C" fn noop_middleware(
    _: *mut c_void,
    _: u32,
    _: Option<unsafe extern "C-unwind" fn(*mut c_void, u32, *const c_char) -> i32>,
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

/// ABI v6：`set_config_section`。假宿主一律拒绝写。
unsafe extern "C" fn noop_set_config_section(
    _: *mut c_void,
    _: *const c_char,
    _: *const c_char,
    _: *const c_char,
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
        get_service: Some(fake_get_service),
        td_invoke: Some(noop_request_id),
        td_invoke_simplified: Some(fake_td_invoke_simplified),
        td_execute: Some(noop_cstr),
        use_middleware: Some(noop_middleware),
        wait_for_update: Some(noop_wait),
        cancel_wait: Some(noop_cancel),
        log: Some(noop_log),
        get_config: Some(noop_cstr2),
        set_config: Some(noop_set_config),
        simplify_invoke: Some(fake_simplify_invoke),
        simplify_invoke_result: Some(noop_cstr),
        // ABI v6 新增：调用者身份 / 服务调用桥 / 失败原因 / 服务清单 / 跨插件写配置。
        current_plugin: Some(noop_plain),
        call_service: Some(fake_call_service),
        last_error: Some(noop_plain),
        list_services: Some(fake_list_services),
        set_config_section: Some(noop_set_config_section),
    }
}

fn fake_host() -> HostApi {
    let address = *HOST_VTABLE.get_or_init(|| {
        let table: &'static HostApiVTable = Box::leak(Box::new(fake_vtable()));
        table as *const HostApiVTable as usize
    });
    // SAFETY: vtable 泄漏到进程结束、所有必需函数都已填。
    unsafe { HostApi::from_raw(address as *const HostApiVTable) }.expect("假宿主应当能构造 HostApi")
}

// ── 测试本体 ────────────────────────────────────────────────────────

/// 这个二进制里的用例共用全局假宿主状态，必须**串行**跑（cargo 默认并行）。
static TEST_LOCK: Mutex<()> = Mutex::new(());

/// 拿串行锁；毒锁也继续用（一次 panic 不该让后面的用例全部挂掉）。
fn serial() -> MutexGuard<'static, ()> {
    TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// 每个用例开头清一遍全局状态，避免上一个用例的残留造成 flaky。
fn reset_globals() {
    lock(&OPS).clear();
    lock(&TD_CALLS).clear();
    lock(&EXECUTED).clear();
    lock(rows()).clear();
    MIGRATED.store(false, Ordering::SeqCst);
    MIGRATE_DELAY_MS.store(0, Ordering::SeqCst);
}

/// 轮询等一个条件；超时 panic（测试里比 sleep 稳）。
fn wait_until(what: &str, timeout: Duration, mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if condition() {
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("等不到：{what}（{timeout:?}）");
}

/// 假 SQLite 里预置的缓存文档（kind=video，文件 id 4242、封面 7777）。
fn preset_document() -> Value {
    json!({
        "v": 1,
        "kind": "video",
        "file_id": 4242,
        "cover_id": 7777,
        "title": "标题 #A",
        "author": "作者",
        "width": 1080,
        "height": 1920,
        "duration": 12,
        "caption": "标题 #A\n\nBy @作者",
    })
}

/// 一个干净的插件目录（带 `cookies.json`——没有 cookie 处理入口就早退，走不到发送）。
fn plugin_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("douyin-cache-hit-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("douyin")).expect("建插件目录");
    std::fs::write(
        dir.join("douyin").join("cookies.json"),
        r#"{"cookies":"ttwid=1; sid=2"}"#,
    )
    .expect("写 cookies.json");
    dir
}

/// `init` + `start`（假宿主 + 假 SQLite）。
fn start_plugin(dir: &Path) -> DouyinPlugin {
    let mut plugin = <DouyinPlugin as Plugin>::init(fake_host()).expect("init 不该失败");
    let config = json!({ "plugins_dir": dir.to_string_lossy() }).to_string();
    plugin.start(&config).expect("start 不该失败");
    plugin
}

/// 投一条文本消息（非 `/` 开头走自动识别，`/` 开头走命令路径）。
fn fire_text(chat_id: i64, message_id: i64, text: &str) {
    fire(
        "updateNewMessage",
        &json!({
            "@type": "updateNewMessage",
            "message": {
                "@type": "message",
                "id": message_id,
                "chat_id": chat_id,
                "is_outgoing": false,
                "sender_id": { "@type": "messageSenderUser", "user_id": 42 },
                "content": {
                    "@type": "messageText",
                    "text": {
                        "@type": "formattedText",
                        "text": text,
                        "entities": [],
                    },
                },
            },
        }),
    );
}

/// 第一条已捕获的发送 op（解析成 JSON）。
fn first_op() -> Value {
    let ops = lock(&OPS);
    let first = ops.first().expect("至少应当有一条发送 op");
    serde_json::from_str(first).expect("op JSON 必须合法")
}

/// 预置一行「单视频，file 4242 / cover 7777」的缓存。
fn preset_cache_row() {
    lock(rows()).insert(
        cache::key(&["douyin", "7300000000000000001"]),
        preset_document().to_string(),
    );
}

/// 主要用例：缓存命中 → 全程只有一条 `send_message`，视频/封面都是 `inputFileId`。
#[test]
fn cache_hit_sends_by_file_id_without_parsing_or_downloading() {
    let _serial = serial();
    reset_globals();
    preset_cache_row();

    let dir = plugin_dir("hit");
    let mut plugin = start_plugin(&dir);
    // 等缓存初始化完成（migrate 被真的调到）。
    wait_until("SQL 缓存 migrate", Duration::from_secs(5), || {
        MIGRATED.load(Ordering::SeqCst)
    });

    let chat_id: i64 = -1001234567890;
    fire_text(chat_id, 22, LINK);

    // 等发送 op 出现（假 simplify_invoke 同步回结果）。
    wait_until("发送 op", Duration::from_secs(5), || {
        !lock(&OPS).is_empty()
    });
    // 再等一小会儿确认没有第二条（「先试远程直链」那条回退）。
    std::thread::sleep(Duration::from_millis(100));

    let ops = lock(&OPS).clone();
    assert_eq!(
        ops.len(),
        1,
        "缓存命中只该发一条 send_message（不该先试远程直链）：{ops:#?}"
    );

    let op: Value = serde_json::from_str(&ops[0]).expect("op JSON 必须合法");
    assert_eq!(op["op"], "send_message", "缓存命中的发送 op：{op}");
    let content = &op["args"]["input_message_content"];
    assert_eq!(
        content["@type"], "inputMessageVideo",
        "应当是视频消息：{op}"
    );
    assert_eq!(
        content["video"]["video"],
        json!({ "@type": "inputFileId", "id": 4242 }),
        "视频本体必须是缓存里的文件 id：{op}"
    );
    // 封面走 `inputVideo.cover`（发的时候 cover 传的那张图）。
    assert_eq!(
        content["video"]["cover"],
        json!({ "@type": "inputFileId", "id": 7777 }),
        "封面必须是缓存里的文件 id：{op}"
    );
    assert_eq!(
        content["caption"]["text"], "标题 #A\n\nBy @作者",
        "文案必须是缓存里存的最终 markdown 原样：{op}"
    );
    assert!(
        op["args"].get("reply_to").is_none(),
        "不该引用/回复原消息（用户要求去掉 reply_to）：{op}"
    );
    assert_eq!(op["args"]["chat_id"], chat_id);

    // 零网络零下载的证据：op 里没有任何 URL / 远程文件。
    assert!(
        !ops[0].contains("inputFileRemote"),
        "缓存命中不该退回远程直链：{}",
        ops[0]
    );
    assert!(
        !ops[0].contains("http"),
        "缓存命中的 op 里不该出现任何 URL：{}",
        ops[0]
    );
    // 命中路径不写缓存（写缓存只在完整解析成功后）。
    assert!(
        lock(&EXECUTED).is_empty(),
        "缓存命中不该再写缓存：{:?}",
        lock(&EXECUTED)
    );
    // 不再判群聊：`getChat` 走的是 `td_invoke_simplified`，这次一次都不该出现。
    assert!(
        lock(&TD_CALLS).is_empty(),
        "不该再调 getChat（引用行为已删）：{:?}",
        lock(&TD_CALLS)
    );

    // 停机要快（不能卡在等在飞任务上）。
    let started = Instant::now();
    plugin.stop();
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "stop 必须 5 秒内返回，实际 {:?}",
        started.elapsed()
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// **回归（Lead 回放发现）**：`on_command` 对**所有**斜杠命令都会回调，
/// 本插件只认 `/dy`——别的插件命令（`/music163 …`）不能让它发「用法：`/dy 抖音链接`」。
///
/// 这条同时是个正向对照：随后投一条 `/dy`（无链接）必须仍然回用法提示，
/// 证明「没动静」是过滤对了，而不是插件根本没在听。
#[test]
fn foreign_commands_are_not_answered() {
    let _serial = serial();
    reset_globals();

    let dir = plugin_dir("foreign");
    let mut plugin = start_plugin(&dir);
    wait_until("SQL 缓存 migrate", Duration::from_secs(5), || {
        MIGRATED.load(Ordering::SeqCst)
    });

    // 外来命令：音乐插件的 `/music163` 与内置 `/start`。
    fire_text(-1001, 30, "/music163 186016 lossless");
    fire_text(-1001, 31, "/start");
    // 给插件一个「如果要发消息」的窗口。
    std::thread::sleep(Duration::from_millis(300));
    assert!(
        lock(&OPS).is_empty(),
        "外来命令不该触发任何发送 op：{:?}",
        lock(&OPS)
    );
    assert!(
        lock(&TD_CALLS).is_empty(),
        "外来命令不该触发任何 TDLib 调用：{:?}",
        lock(&TD_CALLS)
    );

    // 对照：`/dy`（无链接）→ 只回用法提示，不联网。
    fire_text(-1001, 32, "/dy");
    wait_until("对 /dy 的用法提示", Duration::from_secs(5), || {
        !lock(&OPS).is_empty()
    });
    let op = first_op();
    assert_eq!(op["op"], "send_message");
    assert_eq!(
        op["args"]["input_message_content"]["@type"], "inputMessageText",
        "`/dy` 无链接应当回一条文本：{op}"
    );
    assert!(
        op["args"]["input_message_content"]["text"]["text"]
            .as_str()
            .is_some_and(|text| text.contains("用法")),
        "`/dy` 无链接应当回用法提示：{op}"
    );

    plugin.stop();
    let _ = std::fs::remove_dir_all(&dir);
}

/// **回归（Lead 回放发现）**：启动期 cache init 是异步的。
///
/// 第一条命令比 `migrate` 更快到达时，`cache_get` 里的 `ensure_cache` 必须补一次 init——
/// 不能因为「init 还没跑完」就 miss 掉缓存、跑去解析分享页（真网络）或下载媒体。
#[test]
fn cache_still_hits_when_migrate_is_slow() {
    let _serial = serial();
    reset_globals();
    // migrate 慢 200ms：模拟真 SQLite 插件冷启动 / 磁盘慢。
    MIGRATE_DELAY_MS.store(200, Ordering::SeqCst);
    preset_cache_row();

    let dir = plugin_dir("slow-migrate");
    let mut plugin = start_plugin(&dir);
    // **不等** migrate：`start` 一返回就投递——竞态窗口就在这里。
    fire_text(-1002, 40, LINK);

    wait_until(
        "缓存命中后的发送 op",
        Duration::from_secs(5),
        || !lock(&OPS).is_empty(),
    );
    let ops = lock(&OPS).clone();
    assert_eq!(
        ops.len(),
        1,
        "慢 init 不该让第一条命令 miss 去解析/下载：{ops:#?}"
    );

    let op: Value = serde_json::from_str(&ops[0]).expect("op JSON 必须合法");
    assert_eq!(op["op"], "send_message", "应当是发送 op：{op}");
    assert_eq!(
        op["args"]["input_message_content"]["video"]["video"],
        json!({ "@type": "inputFileId", "id": 4242 }),
        "必须命中缓存的文件 id，而不是走去解析/下载：{op}"
    );
    assert!(
        !ops[0].contains("http") && !ops[0].contains("inputFileRemote"),
        "命中路径的 op 里不该出现 URL / 远程文件：{}",
        ops[0]
    );
    assert!(
        MIGRATED.load(Ordering::SeqCst),
        "migrate 最终应该跑过（ensure_cache 补的那次）"
    );

    plugin.stop();
    let _ = std::fs::remove_dir_all(&dir);
}
