//! 跨插件服务的 ABI 外壳 + `render` 方法。
//!
//! 服务在 ABI 层只有三样东西：**名字**、**方法表**、**JSON 进出**。指针生命周期、
//! 字符串归属这类容易抄错的地方都收在这里，业务逻辑只写在 [`svc_render`]。
//!
//! ## 服务契约
//!
//! - 服务名：`fuyubot.html2img`
//! - 方法：`render`
//!
//! 入参（JSON，只有 `html` 必填，其余省略就用插件配置里的默认值）：
//!
//! ```json
//! {
//!   "html": "<h1>{{ title }}</h1>",
//!   "vue": true,
//!   "data": { "title": "你好" },
//!   "width": 960,
//!   "height": 496,
//!   "scale": 1.0,
//!   "dark": false,
//!   "transparent": false,
//!   "auto_height": true,
//!   "format": "jpg"
//! }
//! ```
//!
//! `vue = true` 时 `html` 被当作 Vue 模板，先用 `data`（缺省 `{}`）渲染成 HTML 再画；
//! `format` 可取 `png` / `jpg`（省略则用配置的默认值，默认 `jpg`）。
//!
//! 成功：
//!
//! ```json
//! {"ok": true, "path": "/tmp/fuyubot-html2img/html2img_1234_0.jpg", "format": "jpg", "width": 960, "height": 496, "bytes": 12345}
//! ```
//!
//! 返回的是**本地文件路径**（PNG 太大，不适合塞进 JSON 当 base64）。文件保留 24 小时，
//! 过期由插件在启动时清理。
//!
//! 失败（错误信息是给调用方看的中文句子）：
//!
//! ```json
//! {"ok": false, "error": "HTML 太大（200000 字节，上限 131072）"}
//! ```
//!
//! ## 三条约定
//!
//! 1. 回复字符串由**提供方**持有（线程局部缓冲），调用方读完即弃——不要 free；
//! 2. [`ServiceMethodFn`] 是**同步**的：渲染就在宿主的调用线程上跑，所以单次请求
//!    受 HTML 字节数、宽高、scale 与总像素预算约束；
//! 3. 注册后 vtable 必须活到进程结束——[`Service::register_shared`] 会把服务表
//!    泄漏成 `'static`，`context` 里的 `Arc<Shared>` 同样活到进程结束。

use std::cell::RefCell;
use std::ffi::{CStr, CString, c_char, c_void};
use std::panic::{AssertUnwindSafe, catch_unwind};

use fuyubot_abi::{HostApi, ServiceMethod, ServiceVTable};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::config::ImageFormat;
use crate::log;
use crate::render::{self, Request};
use crate::state::Shared;

pub use fuyubot_abi::ServiceMethodFn;

/// 服务名：别的插件按这个名字 `call_service`。
pub const SERVICE_NAME: &str = "fuyubot.html2img";

/// 服务方法的实现类型（与 ABI 的 [`ServiceMethodFn`] 是同一个类型）。
pub type Handler = ServiceMethodFn;

/// 一个可注册的服务。
///
/// 字段用 `Box` / `CString` 保证地址稳定：vtable 里的裸指针指向它们，注册之后
/// 不能再移动或释放。
pub struct Service {
    name: CString,
    /// 方法名与方法指针。注册后一直有效，vtable 指向这块内存。
    #[allow(dead_code)]
    methods: Box<[ServiceMethod]>,
    vtable: Box<ServiceVTable>,
}

impl Service {
    /// `name` 用点号分段的约定名（`fuyubot.html2img`）。
    ///
    /// `context` 会原样交给每个 [`Handler`]：这里放的是 `Arc<Shared>` 的裸指针
    /// （见 `lib.rs` 的注册处），handler 里再取回来。
    pub fn new(name: &str, methods: Vec<(&str, Handler)>, context: *mut c_void) -> Self {
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
            context,
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

    pub fn vtable(&self) -> &ServiceVTable {
        &self.vtable
    }

    /// 注册到**进程级**：所有插件都看得见。
    ///
    /// ABI v4 起 `provide_service` 落在调用者自己的作用域里，兄弟插件之间没有边；
    /// `fuyubot.html2img` 是给别的插件用的契约，必须走 shared。
    ///
    /// 内部把 `self` 泄漏成 `'static`：宿主只保存裸指针，服务表必须在进程剩余
    /// 生命周期内保持有效，drop 掉就是悬垂指针。
    pub fn register_shared(self, host: &HostApi) -> Result<(), String> {
        let name = self.name().to_owned();
        let leaked: &'static Service = Box::leak(Box::new(self));
        let handle = host.provide_service_shared(&name, leaked.vtable());
        if !handle.is_valid() {
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

/// 解析入参。空串 / null 当作 `{}`，让无参调用也能走同一条路。
///
/// `input` 指向宿主的临时字符串：调用方（`extern "C"` 的服务方法）保证它在本次
/// 调用期间有效，所以这里按 ABI 契约解引用，不再升成 `unsafe fn` 去污染每个调用点。
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

/// 建本插件对外的那一个服务：`fuyubot.html2img` 的 `render` 方法。
///
/// `context` 会原样放进 vtable，交给 [`svc_render`] 取回共享状态。
pub fn build(context: *mut c_void) -> Service {
    Service::new(
        SERVICE_NAME,
        vec![("render", svc_render as Handler)],
        context,
    )
}

/// `render` 方法的入参。
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct RenderInput {
    html: String,
    width: Option<u32>,
    height: Option<u32>,
    scale: Option<f32>,
    dark: Option<bool>,
    transparent: Option<bool>,
    auto_height: Option<bool>,
    /// `true` 时把 `html` 当 Vue 模板编译（用 `data` 渲染），默认 `false`。
    vue: Option<bool>,
    /// Vue 模板的数据；`vue = true` 且不传时按 `{}`。
    data: Option<Value>,
    /// 输出格式：`png` / `jpg`；不给就用配置里的默认值（默认 jpg）。
    format: Option<String>,
}

/// `render` 方法：把 HTML 渲染成 PNG，返回本地文件路径。
///
/// 渲染器内部有大量第三方代码，任何 panic 都在这里兜住——`extern "C"` 边界上
/// 逃出去的 panic 是 abort，会连宿主一起带走。
///
/// # Safety
/// ABI 调用约定：`context` 必须是 `lib.rs` 里 `Arc::into_raw` 出来的 [`Shared`]
/// （注册期间一直存活）；`input` 必须是宿主给的 NUL 结尾 UTF-8 字符串。
unsafe extern "C" fn svc_render(context: *mut c_void, input: *const c_char) -> *const c_char {
    let outcome = catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: 由调用方保证 `context` 指向存活的 `Shared`。
        let Some(shared) = (unsafe { (context as *const Shared).as_ref() }) else {
            return error("插件状态已失效，请重试或重新加载插件");
        };
        handle(shared, input)
    }));
    outcome.unwrap_or_else(|_| error("渲染时发生内部错误（已捕获 panic）"))
}

/// 解析入参并渲染。错误一律变成 `{"ok":false,...}`，不向上抛。
fn handle(shared: &Shared, input: *const c_char) -> *const c_char {
    let value = match parse(input) {
        Ok(value) => value,
        Err(message) => return error(message),
    };
    if let Err(message) = require_str(&value, "html") {
        return error(message);
    }
    let input: RenderInput = match serde_json::from_value(value) {
        Ok(input) => input,
        Err(err) => return error(format!("入参字段不合法：{err}")),
    };
    run_render(shared, input)
}

/// `vue = true` 时先把模板编译成 HTML；否则原样返回。
fn source_html(input: &RenderInput) -> Result<String, String> {
    if !input.vue.unwrap_or(false) {
        return Ok(input.html.clone());
    }
    let data = input.data.clone().unwrap_or_else(|| json!({}));
    let template = vue_template_html::Template::compile(&input.html)
        .map_err(|error| format!("Vue 模板编译失败：{error}"))?;
    template
        .render(&data)
        .map_err(|error| format!("Vue 渲染失败：{error}"))
}

/// 真正的渲染流程：格式 → 模板 → 限额检查 → 渲染 → 转码 → 写盘 → 回报路径与尺寸。
fn run_render(shared: &Shared, input: RenderInput) -> *const c_char {
    let format = match input
        .format
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
    {
        Some(text) => match ImageFormat::parse(text) {
            Some(format) => format,
            None => return error(format!("不支持的 format：{text}（可用 png / jpg）")),
        },
        None => shared.config.format,
    };

    let html = match source_html(&input) {
        Ok(html) => html,
        Err(message) => return error(message),
    };
    if html.len() > shared.config.max_html_bytes {
        return error(format!(
            "HTML 太大（{} 字节，上限 {}）",
            html.len(),
            shared.config.max_html_bytes
        ));
    }

    let mut request = Request::new(html, &shared.config);
    if let Some(width) = input.width {
        request.width = shared.config.clamp_width(width);
    }
    if let Some(height) = input.height {
        request.height = shared.config.clamp_height(height);
    }
    if let Some(scale) = input.scale {
        request.scale = shared.config.clamp_scale(scale);
    }
    if let Some(dark) = input.dark {
        request.dark = dark;
    }
    if let Some(transparent) = input.transparent {
        request.transparent = transparent;
    }
    if let Some(auto_height) = input.auto_height {
        request.auto_height = auto_height;
    }
    if request.width > shared.config.max_width || request.height > shared.config.max_height {
        return error("请求的尺寸超过插件配置的上限");
    }
    if !shared
        .config
        .within_budget(request.width, request.height, request.scale)
    {
        return error(format!(
            "尺寸太大（{}×{} @{}，像素上限 {}）",
            request.width, request.height, request.scale, shared.config.max_pixels
        ));
    }

    let rendered = match render::render(&request) {
        Ok(rendered) => rendered,
        Err(err) => return error(format!("渲染失败：{err}")),
    };
    // `auto_height` 时真实高度只有渲染完才知道；按实际输出再核一次像素预算。
    let css_height = (rendered.height as f32 / request.scale).round().max(1.0) as u32;
    if !shared
        .config
        .within_budget(request.width, css_height, request.scale)
    {
        return error("渲染结果超过像素上限，请调低 width / height / scale");
    }

    let bytes = match format {
        ImageFormat::Png => rendered.png,
        ImageFormat::Jpg => match render::to_jpeg(&rendered.png, shared.config.jpeg_quality) {
            Ok(bytes) => bytes,
            Err(message) => return error(message),
        },
    };

    let path = shared.next_path(format.as_str());
    if let Err(message) = shared.write_bytes(&path, &bytes) {
        return error(message);
    }
    // 图片照常返回；超出 Telegram 照片规格时只在控制台提示（发出去会被压缩）。
    for warning in size_warnings(rendered.width, rendered.height, bytes.len()) {
        log::warn(&shared.host, format!("html2img：{warning}"));
    }
    ok(json!({
        "path": path.to_string_lossy(),
        "format": format.as_str(),
        "width": rendered.width,
        "height": rendered.height,
        "bytes": bytes.len(),
    }))
}

/// Telegram 照片的体积上限（超过就会被当照片压缩）。
const WARN_BYTES: usize = 10 * 1024 * 1024;
/// Telegram 照片的宽 + 高上限。
const WARN_EDGE_SUM: u32 = 10_000;
/// Telegram 照片的长宽比上限。
const WARN_ASPECT: f32 = 20.0;

/// 按最终图片的尺寸 / 体积决定要打哪些警告（只提示，不拦截）。
fn size_warnings(width: u32, height: u32, bytes: usize) -> Vec<String> {
    let mut warnings = Vec::new();
    if bytes > WARN_BYTES {
        warnings.push(format!(
            "输出 {width}×{height} 的图片 {:.1} MB，超过 Telegram 照片的 10 MB 上限：当照片发会被压缩，要原图请按文件发送（或调低 scale / jpeg_quality）",
            bytes as f64 / (1024.0 * 1024.0)
        ));
    }
    let sum = width + height;
    if sum > WARN_EDGE_SUM {
        warnings.push(format!(
            "输出 {width}×{height}，宽 + 高 = {sum} 超过 Telegram 的 {WARN_EDGE_SUM} 上限：当照片发会被压缩"
        ));
    }
    let (long, short) = if width >= height {
        (width, height)
    } else {
        (height, width)
    };
    let ratio = long as f32 / short.max(1) as f32;
    if ratio > WARN_ASPECT {
        warnings.push(format!(
            "输出 {width}×{height}，长宽比 {ratio:.1}:1 超过 Telegram 的 {WARN_ASPECT:.0}:1 上限：当照片发会被压缩"
        ));
    }
    warnings
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordinary_images_produce_no_warnings() {
        assert!(size_warnings(960, 496, 120_000).is_empty());
    }

    #[test]
    fn oversize_bytes_are_reported() {
        let warnings = size_warnings(960, 496, WARN_BYTES + 1);
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("10 MB"), "{}", warnings[0]);
        assert!(warnings[0].contains("Telegram"), "{}", warnings[0]);
    }

    #[test]
    fn long_edge_sum_is_reported() {
        let warnings = size_warnings(6_000, 4_001, 1024);
        assert!(warnings.iter().any(|text| text.contains("宽 + 高")));
    }

    #[test]
    fn extreme_aspect_ratio_is_reported() {
        let warnings = size_warnings(2_000, 90, 1024);
        assert!(
            warnings.iter().any(|text| text.contains("长宽比")),
            "{warnings:?}"
        );
        // 竖条同样算：90×2000
        assert!(
            size_warnings(90, 2_000, 1024)
                .iter()
                .any(|text| text.contains("长宽比"))
        );
    }

    #[test]
    fn vtable_exposes_render() {
        let service = Service::new(
            SERVICE_NAME,
            vec![("render", svc_render as Handler)],
            std::ptr::null_mut(),
        );
        let vtable = service.vtable();
        assert_eq!(vtable.version, 1);
        assert_eq!(vtable.method_count, 1);
        // SAFETY: 服务名是注册期间一直有效的 NUL 结尾字符串。
        let name = unsafe { CStr::from_ptr(vtable.name) }.to_str().unwrap();
        assert_eq!(name, SERVICE_NAME);
        // SAFETY: 方法表指向 `method_count` 个连续项。
        let methods = unsafe { std::slice::from_raw_parts(vtable.methods, vtable.method_count) };
        let method = unsafe { CStr::from_ptr(methods[0].name) }.to_str().unwrap();
        assert_eq!(method, "render");
        assert!(methods[0].func.is_some());
    }

    #[test]
    fn replies_are_valid_json() {
        let raw = ok(json!({ "path": "/tmp/x.png", "width": 640 }));
        let value: Value =
            serde_json::from_str(unsafe { CStr::from_ptr(raw) }.to_str().unwrap()).unwrap();
        assert_eq!(value["ok"], true);
        assert_eq!(value["path"], "/tmp/x.png");
        assert_eq!(value["width"], 640);

        let raw = error("HTML 太大");
        let value: Value =
            serde_json::from_str(unsafe { CStr::from_ptr(raw) }.to_str().unwrap()).unwrap();
        assert_eq!(value["ok"], false);
        assert_eq!(value["error"], "HTML 太大");
    }

    #[test]
    fn parse_tolerates_empty_and_null() {
        assert_eq!(parse(std::ptr::null()), Ok(json!({})));
        let empty = CString::new("   ").unwrap();
        assert_eq!(parse(empty.as_ptr()), Ok(json!({})));
    }

    #[test]
    fn required_fields_are_validated() {
        let input = json!({ "html": "  " });
        assert!(require_str(&input, "html").is_err());
        let input = json!({ "html": "<p>x</p>" });
        assert_eq!(require_str(&input, "html").unwrap(), "<p>x</p>");
    }

    #[test]
    fn vue_template_is_rendered_with_data() {
        let input = RenderInput {
            html: "<p>{{ title }}</p>".to_owned(),
            vue: Some(true),
            data: Some(json!({ "title": "你好" })),
            ..Default::default()
        };
        let html = source_html(&input).expect("应能渲染模板");
        assert!(html.contains("你好"), "{html}");
    }

    #[test]
    fn plain_html_is_left_untouched() {
        let input = RenderInput {
            html: "<p>{{ title }}</p>".to_owned(),
            ..Default::default()
        };
        assert_eq!(source_html(&input).unwrap(), "<p>{{ title }}</p>");
    }

    #[test]
    fn render_without_state_reports_error() {
        let input = CString::new(r#"{"html":"<p>x</p>"}"#).unwrap();
        // SAFETY: 入参是 NUL 结尾字符串；null context 走「状态已失效」分支。
        let raw = unsafe { svc_render(std::ptr::null_mut(), input.as_ptr()) };
        let value: Value =
            serde_json::from_str(unsafe { CStr::from_ptr(raw) }.to_str().unwrap()).unwrap();
        assert_eq!(value["ok"], false);
        assert!(value["error"].as_str().unwrap().contains("失效"));
    }
}
