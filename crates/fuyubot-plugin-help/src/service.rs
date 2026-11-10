//! 命令注册表 + 进程级共享服务 `fuyubot.help.commands`。
//!
//! 这是本插件与「框架内置 help」保持兼容的那一半：服务名、方法名（`register` /
//! `list`）、入参 / 出参形状都与内置插件一致，所以 `/start`、`/plugins`、adblock
//! 这些插件**不用改一行代码**就能继续把命令登记进来。
//!
//! 命令表按**登记它的插件**分组：`/help` 出图时一个插件一张卡片，组标题用宿主
//! 报告里的展示名（`广告拦截（adblock）`）；没有归属的收在最后的「其它」里。
//! `plugin` 字段由宿主判定（ABI v6 的调用者栈），插件伪造不了。
//!
//! `list` 方法仍回**扁平数组**——与内置 help 一致，调用方不用改；分组只发生在
//! `/help` 的渲染里。
//!
//! ## 服务契约
//!
//! | 项 | 内容 |
//! | --- | --- |
//! | 服务名 | `fuyubot.help.commands`（`provide_service_shared`） |
//! | `register` | 入参 `{"name":"…","description":"…"}`；成功 `{"ok":true,"plugin":"…"}` |
//! | `list` | `[{"name":"…","description":"…","plugin":"…"}, …]` |
//!
//! `plugin` 来自 `HostApi::current_plugin()`：ABI v6 起读的是宿主维护的**调用者栈**
//! （与线程无关，调用方在自己的线程上登记也认得），插件伪造不了。

use std::cell::RefCell;
use std::ffi::{CStr, CString, c_char, c_void};
use std::sync::{Mutex, OnceLock};

use fuyubot_abi::{HostApi, ServiceMethod, ServiceVTable};
use serde_json::{Value, json};

/// 共享服务名：其它插件按这个名字 `call_service`。
pub const SERVICE_NAME: &str = "fuyubot.help.commands";

/// 动态命令注册表。`/help` 出图时与静态基线合并、去重后展示。
///
/// 它藏在本插件的动态库里，只通过上面的共享服务暴露出去。
static REGISTRY: OnceLock<Mutex<Vec<CommandEntry>>> = OnceLock::new();

/// 注册表里的一条：命令名、说明，以及**登记它的插件名**。
#[derive(Debug, Clone)]
pub struct CommandEntry {
    pub name: String,
    pub description: String,
    /// `None` 表示宿主自己 / 身份未知，不是「插件自报为空」。
    pub plugin: Option<String>,
}

fn registry() -> &'static Mutex<Vec<CommandEntry>> {
    REGISTRY.get_or_init(|| Mutex::new(Vec::new()))
}

/// 把一条命令塞进注册表（按名字去重：已存在就更新说明与归属）。
pub fn register_command(name: &str, description: &str, plugin: Option<&str>) {
    let Some(name) = normalize_name(name) else {
        return;
    };
    let description = description.trim().to_owned();
    let mut guard = registry()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    match guard.iter_mut().find(|entry| entry.name == name) {
        Some(entry) => {
            entry.description = description;
            entry.plugin = plugin.map(str::to_owned);
        }
        None => guard.push(CommandEntry {
            name,
            description,
            plugin: plugin.map(str::to_owned),
        }),
    }
}

/// 命令名规范化：去首尾空白、去掉开头的一个或多个 `/`，空名返回 `None`。
///
/// 内置 help 不剥 `/`，但很多插件会顺手写成 `/foo`；这里统一成不带斜杠，
/// 出图时再统一加，避免出现 `//foo`。
pub fn normalize_name(name: &str) -> Option<String> {
    let trimmed = name.trim();
    let without_slash = trimmed.trim_start_matches('/').trim();
    (!without_slash.is_empty()).then(|| without_slash.to_owned())
}

/// 静态兜底的命令（内置三件套），只在对应插件**没有**动态登记时补位。
///
/// 基线里没有来源（`plugin`），所以 `list` 里也不带 `plugin`——不知道就别编。
pub const BUILTIN_COMMANDS: &[(&str, &str)] = &[
    ("start", "开始使用"),
    ("help", "这条帮助"),
    ("plugins", "看看装了哪些插件"),
];

/// 合并动态注册表与静态基线，按名字去重、**动态优先**，最后排序。
///
/// 排序规则：先把内置三件套（`/start`、`/help`、`/plugins`）放前面，其余按命令名
/// 排。这样图片里第一眼看到的是最常用的入口，插件启动顺序也不会决定先后。
fn merged_entries(entries: &[CommandEntry]) -> Vec<Value> {
    let mut seen = std::collections::HashSet::new();
    let mut merged: Vec<CommandEntry> = Vec::with_capacity(entries.len() + BUILTIN_COMMANDS.len());

    for entry in entries {
        if !seen.insert(entry.name.clone()) {
            continue;
        }
        merged.push(entry.clone());
    }
    for (name, description) in BUILTIN_COMMANDS {
        if seen.insert((*name).to_owned()) {
            merged.push(CommandEntry {
                name: (*name).to_owned(),
                description: (*description).to_owned(),
                plugin: None,
            });
        }
    }
    merged.sort_by(|left, right| {
        order_key(&left.name)
            .cmp(&order_key(&right.name))
            .then_with(|| left.name.cmp(&right.name))
    });

    merged
        .into_iter()
        .map(|entry| {
            let mut value = json!({
                "name": entry.name,
                "description": entry.description,
            });
            if let Some(plugin) = entry.plugin {
                value["plugin"] = json!(plugin);
            }
            value
        })
        .collect()
}

/// 排序键：内置三件套优先，其余按小写命令名。
fn order_key(name: &str) -> (u8, String) {
    let lower = name.to_ascii_lowercase();
    let rank = match lower.as_str() {
        "start" => 0,
        "help" => 1,
        "plugins" => 2,
        _ => 3,
    };
    (rank, lower)
}

/// 当前 `/help` 实际要展示的命令（动态优先 + 静态补缺，按名字排序）。
pub fn collect_commands() -> Vec<Value> {
    let guard = registry()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    merged_entries(&guard)
}

// 服务方法返回给调用方的字符串由**提供方持有**：这里用线程局部缓冲，调用方读完即弃。
thread_local! {
    static RESPONSE: RefCell<CString> = RefCell::new(CString::new("").expect("常量"));
}

/// 把一段文本放进线程局部缓冲并返回指针。
fn reply(text: &str) -> *const c_char {
    let cstring = CString::new(text).unwrap_or_else(|_| CString::new("").expect("常量"));
    RESPONSE.with(|buffer| {
        *buffer.borrow_mut() = cstring;
        buffer.borrow().as_ptr()
    })
}

/// 问宿主「现在是谁在调我」：`context` 是 `HelpService` 自持的 `Box<HostApi>`。
///
/// ABI v6 起这条回答的始终是**调用者**：宿主在服务调用期间压调用者栈（与线程无关），
/// 所以插件在自己的 tokio task / `thread::spawn` 里登记也拿得到；宿主自己调时是 `None`。
/// 身份查的是宿主那份 `owner → 插件名` 映射，入参里自报的名字一概不看。
fn caller(context: *mut c_void) -> Option<String> {
    if context.is_null() {
        return None;
    }
    // SAFETY: `HelpService::new` 把 `vtable.context` 指向它自己持有的 `Box<HostApi>`，
    // 只要 `HelpService` 活着就一直有效（宿主回滚注册先于它被放下）。
    let host = unsafe { &*(context as *const HostApi) };
    host.current_plugin()
}

/// 服务方法 `register`：入参 `{"name": "...", "description": "..."}`。
///
/// 其它插件在自己的 `start()` 里调它登记命令；成功回
/// `{"ok":true,"plugin":"登记者插件名"}`（宿主问不到身份时 `plugin` 是 `null`），
/// 入参非法回 `{"error":"…"}`。
unsafe extern "C" fn svc_register(
    context: *mut c_void,
    input_json: *const c_char,
) -> *const c_char {
    if input_json.is_null() {
        return reply(r#"{"error":"缺入参"}"#);
    }
    // SAFETY: ABI 契约：`input_json` 是宿主传的 NUL 结尾字符串。
    let input = match unsafe { CStr::from_ptr(input_json) }.to_str() {
        Ok(text) => text,
        Err(_) => return reply(r#"{"error":"非 UTF-8 入参"}"#),
    };
    let value: Value = match serde_json::from_str(input) {
        Ok(value) => value,
        Err(_) => return reply(r#"{"error":"不是合法 JSON"}"#),
    };
    let Some(name) = value.get("name").and_then(Value::as_str) else {
        return reply(r#"{"error":"缺 name 或为空"}"#);
    };
    let Some(name) = normalize_name(name) else {
        return reply(r#"{"error":"缺 name 或为空"}"#);
    };
    let description = value
        .get("description")
        .and_then(Value::as_str)
        .unwrap_or("");
    // 归属由宿主判，不看入参——插件没法伪造成别人。
    let plugin = caller(context);
    register_command(&name, description, plugin.as_deref());
    reply(
        &json!({
            "ok": true,
            "plugin": match &plugin {
                Some(plugin) => Value::String(plugin.clone()),
                None => Value::Null,
            },
        })
        .to_string(),
    )
}

/// 服务方法 `list`：回 `[{"name":"…","description":"…","plugin":"…"}, …]`。
unsafe extern "C" fn svc_list(_context: *mut c_void, _input_json: *const c_char) -> *const c_char {
    let commands = collect_commands();
    reply(&serde_json::to_string(&commands).unwrap_or_else(|_| "[]".to_owned()))
}

/// `fuyubot.help.commands` 的服务表。
///
/// `host` 单独 `Box` 一层是为了地址稳定：`HelpService` 本身会被放进 `Box`，但内部
/// 这个 `Box` 指向的堆地址不随外层移动改变。
pub struct HelpService {
    #[allow(dead_code)]
    host: Box<HostApi>,
    vtable: ServiceVTable,
    // 下面两个字段从不被代码「读」，但它们的缓冲被 `vtable` 的裸指针引用，
    // 必须随 `HelpService` 一起活着。
    #[allow(dead_code)]
    methods: Box<[ServiceMethod]>,
    #[allow(dead_code)]
    names: Vec<CString>,
}

// `ServiceVTable` / `ServiceMethod` 含裸指针，Rust 默认判为 `!Send + !Sync`。
// 但这里只是「指针 + 进程级只读数据（CString 缓冲、函数代码）」的打包，
// 在进程内任意线程间移动 / 共享都安全。手动标注。
unsafe impl Send for HelpService {}
unsafe impl Sync for HelpService {}

impl HelpService {
    /// 构造服务表。`vtable.context` 指向自持的 `Box<HostApi>`。
    pub fn new(host: HostApi) -> Box<Self> {
        let name_service = CString::new(SERVICE_NAME).expect("服务名不该含 NUL");
        let name_register = CString::new("register").expect("方法名不该含 NUL");
        let name_list = CString::new("list").expect("方法名不该含 NUL");
        // 顺序固定：names[0]=服务名，names[1]=register，names[2]=list。
        let names = vec![name_service, name_register, name_list];
        let methods: Box<[ServiceMethod]> = vec![
            ServiceMethod {
                name: names[1].as_ptr(),
                func: Some(svc_register),
            },
            ServiceMethod {
                name: names[2].as_ptr(),
                func: Some(svc_list),
            },
        ]
        .into_boxed_slice();

        let host = Box::new(host);
        let vtable = ServiceVTable {
            name: names[0].as_ptr(),
            version: 1,
            size: std::mem::size_of::<ServiceVTable>() as u32,
            context: (&*host as *const HostApi).cast_mut().cast::<c_void>(),
            methods: methods.as_ptr(),
            method_count: methods.len(),
        };

        Box::new(Self {
            host,
            vtable,
            methods,
            names,
        })
    }

    pub fn vtable(&self) -> &ServiceVTable {
        &self.vtable
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, description: &str, plugin: Option<&str>) -> CommandEntry {
        CommandEntry {
            name: name.to_owned(),
            description: description.to_owned(),
            plugin: plugin.map(str::to_owned),
        }
    }

    #[test]
    fn names_are_normalized() {
        assert_eq!(normalize_name("  /help ").as_deref(), Some("help"));
        assert_eq!(normalize_name("//help").as_deref(), Some("help"));
        assert_eq!(normalize_name("start").as_deref(), Some("start"));
        assert_eq!(normalize_name(" / ").as_deref(), None);
        assert_eq!(normalize_name("   ").as_deref(), None);
    }

    #[test]
    fn dynamic_entries_win_and_builtins_fill_gaps() {
        let merged = merged_entries(&[
            entry("help", "动态的 help", Some("fuyubot.help")),
            entry("zeta", "自定义命令", Some("fuyubot.demo")),
        ]);
        let names: Vec<&str> = merged
            .iter()
            .filter_map(|value| value.get("name").and_then(Value::as_str))
            .collect();
        // 动态优先 + 静态补缺 + 内置三件套排前面。
        assert_eq!(names, vec!["start", "help", "plugins", "zeta"]);
        assert_eq!(merged[1]["description"], "动态的 help");
        assert_eq!(merged[1]["plugin"], "fuyubot.help");
        // 静态补位的那条没有来源。
        assert!(merged[2].get("plugin").is_none());
    }

    #[test]
    fn duplicate_dynamic_entries_are_deduped_first_one_wins() {
        let merged = merged_entries(&[
            entry("dup", "第一条", Some("a")),
            entry("dup", "第二条", Some("b")),
        ]);
        let dup = merged
            .iter()
            .find(|value| value["name"] == "dup")
            .expect("dup 应在结果里");
        assert_eq!(dup["description"], "第一条");
        assert_eq!(dup["plugin"], "a");
    }

    #[test]
    fn caller_without_context_is_none() {
        assert!(caller(std::ptr::null_mut()).is_none());
    }

    /// 走一遍真正的 C ABI 服务方法：`register` → `list`。
    #[test]
    fn service_register_and_list_roundtrip() {
        let input = CString::new(r#"{"name":"/svc_demo","description":"服务测试"}"#).unwrap();
        // SAFETY: 入参是 NUL 结尾字符串；null context 走「问不到调用者」分支。
        let raw = unsafe { svc_register(std::ptr::null_mut(), input.as_ptr()) };
        // SAFETY: 返回值由本线程的 RESPONSE 缓冲持有，下一次 reply 之前有效。
        let reply: Value =
            serde_json::from_str(unsafe { CStr::from_ptr(raw) }.to_str().unwrap()).unwrap();
        assert_eq!(reply["ok"], true);
        assert_eq!(reply["plugin"], Value::Null);

        // 名字里的 `/` 被剥掉，说明来自注册表而不是静态基线。
        let commands = collect_commands();
        let entry = commands
            .iter()
            .find(|command| command["name"] == "svc_demo")
            .expect("svc_demo 应出现在命令表里");
        assert_eq!(entry["description"], "服务测试");
        assert!(entry.get("plugin").is_none());

        // SAFETY: `svc_list` 不读入参；null 是允许的。
        let raw = unsafe { svc_list(std::ptr::null_mut(), std::ptr::null()) };
        // SAFETY: 同上，返回值由本线程缓冲持有。
        let listed: Value =
            serde_json::from_str(unsafe { CStr::from_ptr(raw) }.to_str().unwrap()).unwrap();
        assert!(
            listed
                .as_array()
                .unwrap()
                .iter()
                .any(|command| command["name"] == "svc_demo")
        );
    }

    #[test]
    fn service_register_rejects_bad_input() {
        for body in [
            r#"{"name":"  "}"#,
            r#"{"description":"缺 name"}"#,
            "not json",
        ] {
            let input = CString::new(body).unwrap();
            // SAFETY: 入参是 NUL 结尾字符串。
            let raw = unsafe { svc_register(std::ptr::null_mut(), input.as_ptr()) };
            // SAFETY: 返回值由本线程缓冲持有。
            let reply: Value =
                serde_json::from_str(unsafe { CStr::from_ptr(raw) }.to_str().unwrap()).unwrap();
            assert!(reply.get("error").is_some(), "{body} 应被拒绝：{reply}");
        }
    }
}
