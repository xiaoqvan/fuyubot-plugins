//! `fuyubot.sqlite`：给**其它插件**用的进程级 SQLite 服务。
//!
//! 它是一个普通 cdylib 插件，本身不订阅任何命令；价值全在 `start()` 里注册的
//! 共享服务 `fuyubot.sqlite`。别的插件在自己的 `start()` 里这样用：
//! ```ignore
//! let reply = host.call_service(
//!     "fuyubot.sqlite",
//!     "migrate",
//!     r#"{"plugin":"my.plugin","templates":["kv"]}"#,
//! )?;
//! let rows = host.call_service(
//!     "fuyubot.sqlite",
//!     "query",
//!     r#"{"plugin":"my.plugin","sql":"SELECT value FROM kv WHERE key = ?1","params":["greeting"]}"#,
//! )?;
//! # Ok::<(), String>(())
//! ```
//!
//! ## 三条设计线
//!
//! 1. **防注入靠参数绑定**：`params` 里的值走 `sqlite3_bind_*`，永远不拼进 SQL；
//!    `sql` 里的 `?1` / `:name` 只是占位符。`query` 只收只读语句，
//!    `execute` 一次只收一条语句，多语句写入走 `batch`（服务包事务）。
//! 2. **两种隔离模式**：默认 `per_plugin`（一个插件一个 `.sqlite3` 文件）；
//!    配 `mode = "shared"` 则共用一个文件，模板建的表自动加 `<plugin>_` 前缀。
//! 3. **连接加固**：`defensive=ON`、`trusted_schema=OFF`、关闭双引号字符串兼容、
//!    `ATTACH` / `PRAGMA` / `VACUUM` / 事务控制语句一律拒绝。
//!
//! 注意：ABI 不告诉服务「调用者是谁」，所以每次调用都要显式给 `plugin`。
//! 同进程的原生插件本来就能绕过任何边界，这里的隔离是**协作式**的，
//! 目标是防「数据里的注入」，不是防「恶意插件」。

mod config;
mod db;
mod error;
mod service;
mod sql;
mod templates;

use std::cell::RefCell;
use std::ffi::{CStr, CString, c_char, c_void};
use std::sync::{Arc, Mutex, OnceLock};

use fuyubot_abi::{HostApi, LogLevel, Plugin, ServiceMethod, ServiceVTable, define_plugin};

use crate::config::Config;
use crate::db::Manager;
use crate::error::ServiceError;

/// 当前生效的数据库管理器。`None` = 还没 `start` 或已经 `stop`。
///
/// 用 `Arc` 是为了让服务调用「取一份引用就放锁」：真正的 SQL 执行不占全局锁。
static STATE: OnceLock<Mutex<Option<Arc<Manager>>>> = OnceLock::new();

fn state() -> &'static Mutex<Option<Arc<Manager>>> {
    STATE.get_or_init(|| Mutex::new(None))
}

fn set_manager(manager: Option<Arc<Manager>>) {
    let mut guard = state()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    *guard = manager;
}

/// 当前管理器的一份快照（服务方法用）。
fn manager_snapshot() -> Option<Arc<Manager>> {
    let guard = state()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    guard.clone()
}

struct SqlitePlugin {
    host: HostApi,
    /// 服务表由插件自己持有：里面的裸指针指向同一 `Box` 里的缓冲，`stop()` 前一直有效。
    service: Option<Box<SqliteService>>,
}

impl Plugin for SqlitePlugin {
    fn init(host: HostApi) -> Result<Self, String> {
        Ok(Self {
            host,
            service: None,
        })
    }

    fn start(&mut self, config_json: &str) -> Result<(), String> {
        let config = match Config::from_host(self.host, config_json) {
            Ok(config) => config,
            Err(error) => {
                self.host
                    .log(LogLevel::Error, &format!("sqlite 插件配置无效：{error}"));
                return Err(error);
            }
        };

        let mode = config.mode.as_str().to_owned();
        let dir = config.dir.display().to_string();
        let manager = Arc::new(Manager::new(config).map_err(|error| {
            self.host.log(
                LogLevel::Error,
                &format!("sqlite 插件初始化数据库失败：{error}"),
            );
            error.to_string()
        })?);
        set_manager(Some(Arc::clone(&manager)));

        let service = build_service();
        let handle = self
            .host
            .provide_service_shared(service::SERVICE_NAME, &service.vtable);
        if !handle.is_valid() {
            set_manager(None);
            let message = format!(
                "注册共享服务 {} 失败（可能已有同名服务）",
                service::SERVICE_NAME
            );
            self.host.log(LogLevel::Error, &message);
            return Err(message);
        }
        self.service = Some(service);

        self.host.log(
            LogLevel::Info,
            &format!("sqlite 服务已就绪：mode={mode} dir={dir}"),
        );
        Ok(())
    }

    fn stop(&mut self) {
        // 先摘服务再关库：宿主在 plugin_stop 之后才回滚服务注册，
        // 这期间的调用会拿到 `not_ready`，而不是摸到一个已经关掉的连接。
        self.service = None;
        set_manager(None);
    }
}

/// 服务表 + 它指向的方法名缓冲。字段从不被读，但必须活在 `vtable` 的裸指针期间。
struct SqliteService {
    vtable: ServiceVTable,
    #[allow(dead_code)]
    methods: Vec<ServiceMethod>,
    #[allow(dead_code)]
    names: Vec<CString>,
}

// `ServiceVTable` 含裸指针，默认 `!Send + !Sync`；这里它只是「只读字符串缓冲 +
// 函数指针」的打包，方法本身只碰进程级 `Mutex`，跨线程共享是安全的。
unsafe impl Send for SqliteService {}
unsafe impl Sync for SqliteService {}

fn build_service() -> Box<SqliteService> {
    let service_name = CString::new(service::SERVICE_NAME).expect("服务名不该含 NUL");
    let mut names = Vec::with_capacity(service::METHODS.len() + 1);
    names.push(service_name);
    for method in service::METHODS {
        names.push(CString::new(*method).expect("方法名不该含 NUL"));
    }

    let functions: [unsafe extern "C" fn(*mut c_void, *const c_char) -> *const c_char;
        service::METHODS.len()] = [
        svc_info,
        svc_query,
        svc_execute,
        svc_batch,
        svc_templates,
        svc_migrate,
        svc_tables,
    ];
    let methods: Vec<ServiceMethod> = functions
        .into_iter()
        .enumerate()
        .map(|(index, func)| ServiceMethod {
            name: names[index + 1].as_ptr(),
            func: Some(func),
        })
        .collect();

    let vtable = ServiceVTable {
        name: names[0].as_ptr(),
        version: 1,
        size: core::mem::size_of::<ServiceVTable>() as u32,
        context: core::ptr::null_mut(),
        methods: methods.as_ptr(),
        method_count: methods.len(),
    };
    Box::new(SqliteService {
        vtable,
        methods,
        names,
    })
}

macro_rules! service_method {
    ($name:ident, $method:literal) => {
        unsafe extern "C" fn $name(
            _context: *mut c_void,
            input_json: *const c_char,
        ) -> *const c_char {
            handle($method, input_json)
        }
    };
}

service_method!(svc_info, "info");
service_method!(svc_query, "query");
service_method!(svc_execute, "execute");
service_method!(svc_batch, "batch");
service_method!(svc_templates, "templates");
service_method!(svc_migrate, "migrate");
service_method!(svc_tables, "tables");

/// 所有服务方法的公共入口：读入参、取管理器、分发、把 panic 挡在 `extern "C"` 内。
fn handle(method: &str, input_json: *const c_char) -> *const c_char {
    let output = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let input = if input_json.is_null() {
            String::new()
        } else {
            // SAFETY: ABI 契约要求提供方传 NUL 结尾字符串。
            match unsafe { CStr::from_ptr(input_json) }.to_str() {
                Ok(text) => text.to_owned(),
                Err(_) => {
                    return reply(&error_json(&ServiceError::bad_request(
                        "入参不是合法 UTF-8",
                    )));
                }
            }
        };
        match manager_snapshot() {
            Some(manager) => reply(&service::dispatch(&manager, method, &input)),
            None => reply(&error_json(&ServiceError::new(
                "not_ready",
                "sqlite 服务还没就绪（插件 start 没跑或已经 stop）",
            ))),
        }
    }));
    match output {
        Ok(pointer) => pointer,
        Err(_) => reply(&error_json(&ServiceError::new(
            "panic",
            "sqlite 服务内部 panic（已拦截，详见宿主日志）",
        ))),
    }
}

fn error_json(error: &ServiceError) -> String {
    service::error_json(error)
}

thread_local! {
    /// 服务方法返回的字符串由**提供方**持有；一次调用读走前不会被覆盖。
    static RESPONSE: RefCell<CString> =
        RefCell::new(CString::new("").expect("空串不含 NUL"));
}

fn reply(text: &str) -> *const c_char {
    let value = CString::new(text).unwrap_or_else(|_| {
        CString::new(r#"{"ok":false,"error":{"code":"internal_nul","message":"响应里有 NUL"}}"#)
            .expect("兜底 JSON 不含 NUL")
    });
    RESPONSE.with(|slot| {
        *slot.borrow_mut() = value;
        slot.borrow().as_ptr()
    })
}

define_plugin!(SqlitePlugin, "fuyubot.sqlite");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn service_name_matches_the_manifest_name() {
        assert_eq!(service::SERVICE_NAME, "fuyubot.sqlite");
    }

    #[test]
    fn response_buffer_round_trips() {
        let pointer = reply(r#"{"ok":true}"#);
        let text = unsafe { CStr::from_ptr(pointer) }.to_str().unwrap();
        assert_eq!(text, r#"{"ok":true}"#);
    }
}
