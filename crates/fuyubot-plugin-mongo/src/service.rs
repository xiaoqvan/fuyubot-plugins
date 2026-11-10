//! 插件服务的公共外壳。
//!
//! 跨插件服务在 ABI 层只有三样东西：**名字**、**方法表**、**JSON 进出**。
//! 每个插件都要把这三样拼一遍，很容易抄错（指针生命周期、buffers 归属…）。
//! 这里把它们收成一处，插件只写业务函数。
//!
//! ```ignore
//! impl Plugin for MyPlugin {
//!     fn init(host: HostApi) -> Result<Self, String> {
//!         Service::new("storage.mongo", vec![
//!             ("ping", svc_ping),
//!             ("find", svc_find),
//!         ])
//!         .register_shared(&host)?;
//!         Ok(Self { host })
//!     }
//! }
//! ```
//!
//! **跨插件可见的服务必须用 `register_shared`**：ABI v4 起 `register`（=
//! `provide_service`）落在**插件自己的作用域**里，兄弟插件之间没有边、谁也看不见谁。
//! `storage.mongo` / `media.ffmpeg` 这类名字是给别的插件用的契约，只有 shared 才同时
//! 满足「调用方 `get_service` 拿得到」和「宿主判 `inject` 等得到」两件事。
//!
//! 三条约定（写新服务时别忘）：
//!
//! 1. 回复字符串由**提供方**持有，调用方读完即弃——所以用线程局部缓冲，不 free；
//!    **成功但没有内容也要回 `""` 或 `{}`**：ABI v6 起方法返回 `NULL` 表示**失败**，
//!    不再等于「成功但无输出」；
//! 2. `ServiceMethodFn` 是同步的，慢操作要自己开线程，别堵住宿主回调线程；
//! 3. 注册后 vtable 必须活到进程结束——[`Service::register`] 会把它泄漏成 `'static`。
//!
//! 调用**别人**的服务一律走 [`call_service`]（v6 起经宿主的 `call_service` 桥）：
//! 被调方用 `current_plugin()` 问得到调用者，失败原因也由宿主带回。
//! 自己 `get_service` 拿函数指针直调仍然能跑，但那次调用绕过了宿主，
//! 提供方只会看到 `None`。

use std::cell::RefCell;
use std::ffi::{CStr, CString, c_char, c_void};
use std::sync::Mutex;

use fuyubot_abi::{HostApi, LogLevel, ServiceMethod, ServiceVTable};
use serde_json::{Value, json};

/// 服务的实现函数签名（去掉 `Option`，注册时更好写）。
pub type Handler = unsafe extern "C" fn(*mut c_void, *const c_char) -> *const c_char;

/// `ServiceVTable` 里全是裸指针，不是 `Sync`；但建成后只读，跨线程安全。
pub struct SyncVt(pub ServiceVTable);
unsafe impl Sync for SyncVt {}
unsafe impl Send for SyncVt {}

/// 一个可注册的服务。字段用 `Box` 保证地址稳定（vtable 里的指针指向它们）。
pub struct Service {
    name: CString,
    methods: Box<[ServiceMethod]>,
    vtable: Box<ServiceVTable>,
}

impl Service {
    /// `name` 用点号分段的约定名（`storage.mongo` / `download.qbittorrent`）。
    pub fn new(name: &str, methods: Vec<(&str, Handler)>) -> Self {
        assert!(!methods.is_empty(), "服务 {name} 至少要有一个方法");

        let name = CString::new(name).expect("服务名不能含 NUL");
        let entries: Vec<ServiceMethod> = methods
            .into_iter()
            .map(|(method, func)| ServiceMethod {
                // 方法名活到进程结束：泄漏是有意的，vtable 要一直能读。
                name: CString::new(method).expect("方法名不能含 NUL").into_raw(),
                func: Some(func),
            })
            .collect();
        let methods: Box<[ServiceMethod]> = entries.into_boxed_slice();

        let vtable = Box::new(ServiceVTable {
            name: name.as_ptr(),
            version: 1,
            size: std::mem::size_of::<ServiceVTable>() as u32,
            context: std::ptr::null_mut(),
            methods: methods.as_ptr(),
            method_count: methods.len(),
        });

        Self {
            name,
            methods,
            vtable,
        }
    }

    pub fn name(&self) -> &str {
        self.name.to_str().unwrap_or("")
    }

    /// 方法表（保活用的那份）。测试与排障会读它。
    pub fn methods(&self) -> &[ServiceMethod] {
        &self.methods
    }

    pub fn vtable(&self) -> &ServiceVTable {
        &self.vtable
    }

    /// 注册到宿主。成功返回后服务即可被其它插件按名字取到。
    ///
    /// 内部把 `self` 泄漏成 `'static`：宿主只保存裸指针，服务表必须在进程剩余
    /// 生命周期内保持有效，drop 掉就是悬垂指针。
    pub fn register(self, host: &HostApi) -> Result<(), String> {
        let name = self.name().to_owned();
        let leaked: &'static Service = Box::leak(Box::new(self));
        // SAFETY: `leaked` 是 `'static`，满足 provide_service 的生命周期要求。
        let handle = unsafe { host.provide_service(&name, leaked.vtable.as_ref()) };
        if handle.id == 0 {
            return Err(format!("宿主拒绝注册服务 {name}"));
        }
        Ok(())
    }

    /// 注册到**进程级**：所有插件都看得见。
    ///
    /// ABI v4 起 `provide_service` 落在**调用者自己的作用域**里——兄弟插件之间没有边，
    /// 谁也看不见谁的私有服务。基础设施插件提供的 `storage.mongo` / `media.ffmpeg`
    /// 这类名字是**跨插件契约**，必须走这条；否则业务插件 `get_service` 拿到 null，
    /// 宿主判硬依赖（`inject`）时也看不见这个名字，插件会永远停在 Pending。
    ///
    /// 同名会撞（宿主报 `ServiceNameExists`），所以只在真的共享时用。
    pub fn register_shared(self, host: &HostApi) -> Result<(), String> {
        let name = self.name().to_owned();
        let leaked: &'static Service = Box::leak(Box::new(self));
        let handle = host.provide_service_shared(&name, leaked.vtable.as_ref());
        if handle.id == 0 {
            return Err(format!("宿主拒绝注册共享服务 {name}"));
        }
        Ok(())
    }
}

thread_local! {
    /// 回复缓冲：宿主约定「返回的字符串由提供方持有，读完即弃（不要 free）」。
    static REPLY: RefCell<CString> = RefCell::new(CString::new("{}").expect("常量"));
}

/// 把 JSON 放进线程局部缓冲并交出指针。
pub fn reply(value: Value) -> *const c_char {
    let text = value.to_string();
    let cstring = CString::new(text).unwrap_or_else(|_| {
        CString::new(r#"{"ok":false,"error":"响应里出现 NUL"}"#).expect("常量")
    });
    REPLY.with(|slot| {
        *slot.borrow_mut() = cstring;
        slot.borrow().as_ptr()
    })
}

/// 成功回复：`{"ok":true, ...}`。
pub fn ok(fields: Value) -> *const c_char {
    let mut object = match fields {
        Value::Object(map) => map,
        Value::Null => serde_json::Map::new(),
        other => {
            let mut map = serde_json::Map::new();
            map.insert("value".to_owned(), other);
            map
        }
    };
    object.insert("ok".to_owned(), Value::Bool(true));
    reply(Value::Object(object))
}

/// 失败回复：`{"ok":false,"error":"..."}`。
pub fn error(message: impl Into<String>) -> *const c_char {
    reply(json!({ "ok": false, "error": message.into() }))
}

/// 解析入参。空串 / null 当作 `{}`，让无参方法也能被空调用。
///
/// `input` 指向宿主的临时字符串：调用方（`extern "C"` 的服务方法）保证它在本次调用
/// 期间有效，所以这里按 ABI 契约解引用，不再升成 `unsafe fn` 去污染每个调用点。
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub fn parse(input: *const c_char) -> Result<Value, String> {
    if input.is_null() {
        return Ok(json!({}));
    }
    let text = unsafe { CStr::from_ptr(input) }
        .to_str()
        .map_err(|err| format!("入参不是 UTF-8：{err}"))?;
    if text.trim().is_empty() {
        return Ok(json!({}));
    }
    serde_json::from_str(text).map_err(|err| format!("入参不是合法 JSON：{err}"))
}

/// 取必填字符串字段。
pub fn require_str<'a>(input: &'a Value, key: &str) -> Result<&'a str, String> {
    input
        .get(key)
        .and_then(Value::as_str)
        .filter(|text| !text.trim().is_empty())
        .ok_or_else(|| format!("缺少字段 {key}"))
}

pub fn opt_str<'a>(input: &'a Value, key: &str) -> Option<&'a str> {
    input.get(key).and_then(Value::as_str)
}

pub fn opt_u64(input: &Value, key: &str) -> Option<u64> {
    input.get(key).and_then(Value::as_u64)
}

pub fn opt_bool(input: &Value, key: &str) -> Option<bool> {
    input.get(key).and_then(Value::as_bool)
}

/// 发事件。宿主没实现事件总线时安静失败——业务不该因为发不出进度就崩。
pub fn emit(host: &HostApi, event_type: &str, payload: Value) {
    let _ = host.emit(event_type, &payload.to_string());
}

/// 插件日志的统一前缀，避免多插件混在一起时分不清来源。
pub fn log(host: &HostApi, level: LogLevel, plugin: &str, message: &str) {
    host.log(level, &format!("[{plugin}] {message}"));
}

// ─── 调用别人的服务 ────────────────────────────────────────────────────────

/// 宿主让本插件看见的服务清单（ABI v6 的 `list_services`）；解析不出来就当空表。
///
/// 每一项：`{"name":"…","version":1,"methods":["…"],"plugin":"提供方插件名"}`，
/// `plugin` 为 `null` 表示宿主预置的服务。
fn services(host: &HostApi) -> Vec<Value> {
    serde_json::from_str(&host.list_services()).unwrap_or_default()
}

/// 服务是否在线：名字在清单里，且方法表里有这个方法。
///
/// 比 `get_service(..).is_null()` 多问一句「有没有这个方法」，也顺带知道是谁提供的。
/// 只用宿主给的清单，不碰服务表指针。
pub fn service_available(host: &HostApi, service: &str, method: &str) -> bool {
    services(host).iter().any(|entry| {
        entry.get("name").and_then(Value::as_str) == Some(service)
            && entry
                .get("methods")
                .and_then(Value::as_array)
                .is_some_and(|methods| methods.iter().any(|name| name.as_str() == Some(method)))
    })
}

/// 调一次别人的服务，入参 / 出参都是 JSON。
///
/// ABI v6 起服务调用**经过宿主**（[`HostApi::call_service`]）：被调方用
/// `current_plugin()` 问出来的就是**本插件**，失败原因也由宿主带回。
/// 别自己 `get_service` 拿函数指针直调——那条路绕过宿主，提供方只会看到 `None`，
/// 而且 `context` 得自己编（各插件的 context 语义并不相同）。
pub fn call_service(
    host: &HostApi,
    service: &str,
    method: &str,
    input: &Value,
) -> Result<Value, String> {
    let payload = input.to_string();
    let reply = host
        .call_service(service, method, &payload)
        .map_err(|error| format!("服务 {service}.{method} 调用失败：{error}"))?;
    let value: Value = serde_json::from_str(&reply).map_err(|error| {
        format!("服务 {service}.{method} 的响应不是 JSON：{error}（原文：{reply}）")
    })?;

    if value.get("ok").and_then(Value::as_bool) != Some(true) {
        let message = value
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("未知错误")
            .to_owned();
        return Err(message);
    }
    Ok(value)
}

/// 常驻配置槽：`start()` 写、方法读。
pub struct Settings<T> {
    slot: Mutex<Option<T>>,
}

impl<T: Clone> Settings<T> {
    pub const fn new() -> Self {
        Self {
            slot: Mutex::new(None),
        }
    }

    pub fn set(&self, value: T) {
        let mut guard = self
            .slot
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *guard = Some(value);
    }

    pub fn get(&self) -> Option<T> {
        self.slot
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    /// 没配置就报错——比用默认值静默连错库好。
    pub fn require(&self) -> Result<T, String> {
        self.get().ok_or_else(|| "插件尚未初始化配置".to_owned())
    }
}

impl<T: Clone> Default for Settings<T> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
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
}
