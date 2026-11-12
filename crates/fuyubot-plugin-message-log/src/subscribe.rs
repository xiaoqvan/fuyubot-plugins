//! 事件订阅守卫：`drop` 就是退订。
//!
//! 宿主的 `on` 是裸 C 回调 + `user_data` 指针，`off` 要配对调用。这里把它包成
//! RAII：闭包装在 `Box` 里（堆地址稳定，指针不会在移动时失效），句柄 drop 时退订。
//! 与仓库里 adblock / help 的订阅守卫是同一个套路。

use std::ffi::{CStr, c_char, c_void};

use fuyubot_abi::{HostApi, SubscriptionHandle};
use serde_json::Value;

struct Handler {
    f: Box<dyn Fn(&Value) + Send + Sync>,
}

/// 一个活着的订阅。
pub struct EventSubscription {
    host: HostApi,
    handle: SubscriptionHandle,
    /// 只负责**持有**闭包；退订之前它的地址必须稳定。
    _handler: Box<Handler>,
}

impl Drop for EventSubscription {
    fn drop(&mut self) {
        if self.handle.is_valid() {
            self.host.off(self.handle);
        }
    }
}

/// 订阅一个事件通道；`filter` 是宿主的过滤器 JSON（`{"字段路径": 期望值}`）。
///
/// 回调里**只该做很快的事**（这里只做一次 `try_send`）——它跑在宿主的接收循环线程上。
pub fn subscribe(
    host: HostApi,
    event: &str,
    filter: Option<&str>,
    f: impl Fn(&Value) + Send + Sync + 'static,
) -> Option<EventSubscription> {
    let mut handler: Box<Handler> = Box::new(Handler { f: Box::new(f) });
    // SAFETY: 指针指向堆上的 `Handler`；`EventSubscription` 持有这个 Box，
    // 保证退订之前地址一直有效。
    let user_data = (&raw mut *handler).cast::<c_void>();
    let handle = host.on(event, filter, trampoline, user_data);
    handle.is_valid().then(|| EventSubscription {
        host,
        handle,
        _handler: handler,
    })
}

extern "C-unwind" fn trampoline(user_data: *mut c_void, payload: *const c_char) -> i32 {
    if user_data.is_null() || payload.is_null() {
        return -1;
    }
    // SAFETY: `user_data` 是 `subscribe` 里那个 Box，订阅活多久它就活多久。
    let handler = unsafe { &*(user_data as *const Handler) };
    // SAFETY: 宿主保证 `payload` 是 NUL 结尾的临时字符串。
    let text = unsafe { CStr::from_ptr(payload) }.to_string_lossy();
    let Ok(value) = serde_json::from_str::<Value>(&text) else {
        return 0;
    };
    (handler.f)(&value);
    0
}
