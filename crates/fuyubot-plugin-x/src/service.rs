//! 外部渲染服务：其他插件通过 `xiaoqvan.x.render` 复用同款卡片版式。
//!
//! 方法：
//! - `html`：入参 `{"api": <FxTwitter JSON>}`，返回 `{"ok":true,"html":"..."}`；
//! - `render`：同上，调 html2img 出图，返回 `{"ok":true,"path":"..."}`。
//!
//! 入参也可以直接是 FxTwitter 根对象，或 `{"api":"<JSON 字符串>"}`。

use std::cell::RefCell;
use std::ffi::{CStr, CString, c_char, c_void};
use std::panic::{self, AssertUnwindSafe};
use std::sync::Arc;

use fuyubot_abi::{ServiceMethod, ServiceVTable};
use serde_json::{Value, json};

use crate::model::Post;
use crate::runtime::Runtime;

const SERVICE_NAME: &CStr = c"xiaoqvan.x.render";

struct MethodTable([ServiceMethod; 2]);

// SAFETY: 表里只有静态 C 字符串和函数指针，注册后不再改。
unsafe impl Sync for MethodTable {}

static METHODS: MethodTable = MethodTable([
    ServiceMethod {
        name: c"render".as_ptr(),
        func: Some(render_method),
    },
    ServiceMethod {
        name: c"html".as_ptr(),
        func: Some(html_method),
    },
]);

/// 服务注册的持有者：vtable 和它引用的 Runtime 都必须活到插件卸载。
pub struct Registration {
    _vtable: Box<ServiceVTable>,
    _runtime: Arc<Runtime>,
}

// SAFETY: vtable 注册后只读，Raw 指针由宿主持有；Registration 只随 XPlugin 移动。
unsafe impl Send for Registration {}

pub fn register(runtime: Arc<Runtime>) -> Result<Registration, String> {
    let vtable = Box::new(ServiceVTable {
        name: SERVICE_NAME.as_ptr(),
        version: 1,
        size: std::mem::size_of::<ServiceVTable>() as u32,
        context: Arc::as_ptr(&runtime) as *mut c_void,
        methods: METHODS.0.as_ptr(),
        method_count: METHODS.0.len(),
    });
    let handle = runtime
        .host
        .provide_service_shared("xiaoqvan.x.render", &*vtable);
    if !handle.is_valid() {
        return Err("provide_service_shared 返回无效句柄（可能重名）".to_owned());
    }
    Ok(Registration {
        _vtable: vtable,
        _runtime: runtime,
    })
}

#[derive(Clone, Copy)]
enum Mode {
    Html,
    Render,
}

unsafe extern "C" fn render_method(context: *mut c_void, input: *const c_char) -> *const c_char {
    respond(context, input, Mode::Render)
}

unsafe extern "C" fn html_method(context: *mut c_void, input: *const c_char) -> *const c_char {
    respond(context, input, Mode::Html)
}

fn respond(context: *mut c_void, input: *const c_char, mode: Mode) -> *const c_char {
    let outcome = panic::catch_unwind(AssertUnwindSafe(|| unsafe { handle(context, input, mode) }));
    let payload = match outcome {
        Ok(Ok(value)) => value,
        Ok(Err(error)) => json!({ "ok": false, "error": error }),
        Err(_) => json!({ "ok": false, "error": "渲染服务内部 panic" }),
    };
    reply(payload)
}

unsafe fn handle(context: *mut c_void, input: *const c_char, mode: Mode) -> Result<Value, String> {
    if context.is_null() {
        return Err("缺少运行时上下文".to_owned());
    }
    if input.is_null() {
        return Err("缺少入参".to_owned());
    }
    let raw = unsafe { CStr::from_ptr(input) }
        .to_str()
        .map_err(|error| format!("入参不是 UTF-8：{error}"))?;
    let post = parse_post(raw)?;

    // context 指向 Arc 里的 Runtime；先加计数，保证调用期间 runtime 不被卸载。
    let ptr = context as *const Runtime;
    unsafe { Arc::increment_strong_count(ptr) };
    let runtime = unsafe { Arc::from_raw(ptr) };
    let html = crate::render::card_html(&post, &runtime.config);
    match mode {
        Mode::Html => Ok(json!({ "ok": true, "html": html })),
        Mode::Render => {
            let path = runtime.render_card_blocking(html)?;
            Ok(json!({ "ok": true, "path": path }))
        }
    }
}

/// 解析入参：`{"api": {...}}`、`{"api": "..."}`，或者整个 FxTwitter 根对象。
pub fn parse_post(input: &str) -> Result<Post, String> {
    let value: Value =
        serde_json::from_str(input).map_err(|error| format!("入参不是 JSON：{error}"))?;
    let api = match value.get("api") {
        Some(Value::String(text)) => {
            serde_json::from_str(text).map_err(|error| format!("api 字符串不是 JSON：{error}"))?
        }
        Some(other) => other.clone(),
        None => value,
    };
    crate::model::from_api(&api)
}

thread_local! {
    static REPLY: RefCell<CString> = RefCell::new(CString::new("{}").unwrap());
}

/// Host 会立刻拷贝返回值；指针活到本线程下一次调用。
fn reply(value: Value) -> *const c_char {
    let text = CString::new(value.to_string())
        .unwrap_or_else(|_| CString::new("{\"ok\":false,\"error\":\"响应含 NUL\"}").unwrap());
    REPLY.with(|slot| {
        *slot.borrow_mut() = text;
        slot.borrow().as_ptr()
    })
}

#[cfg(test)]
#[path = "../tests/unit/service.rs"]
mod tests;
