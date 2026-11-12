//! 鉴权材料的读取：两家平台的 cookie / 设备指纹 / 应用签名头。
//!
//! ## 从 `fuyubot.toml` 的 `[plugin.fuyubot.music]` 读（推荐）
//!
//! **只用一个根段，所有键平铺**（没有二级子表）。宿主的 `get_config(section, key)`
//! 返回 JSON 文本；这里的值一律要求是**字符串**：
//!
//! ```toml
//! [plugin.fuyubot.music]
//! # 整段 Cookie 头字符串
//! netease = "MUSIC_U=…; os=pc; appver=3.1.28.205001"
//! soda = "sessionid=…; sid_tt=…"
//!
//! # 汽水的设备指纹 / 签名头（可选；匿名可不填，但要和抓包那台机器一致）
//! soda_device_id = "…"
//! soda_iid = "…"
//! soda_fp = "…"
//! soda_x_helios = "…"
//! soda_x_medusa = "…"
//! soda_user_agent = "LunaPC/3.3.0(359450208)"
//! soda_signer_url = "http://127.0.0.1:8899/sign"
//! soda_signer_token = "…"
//! ```
//!
//! - `netease` / `soda` → 当 **Cookie 头**（内部包成 `{"cookie": …}` 走旧解析）。
//! - `soda_*` → 汽水的设备指纹 / 签名头，组装成 `{"device_id": …, "iid": …}` 走
//!   旧的 `parse_soda`（默认值 / 别名规则完全复用）。
//! - 缺失 / 空串 / 坏 JSON / **非字符串** → 按**没配置**处理（非字符串与坏 JSON
//!   会 warn），不 panic；该平台再回退旧文件。
//!
//! ## 旧文件仍然兼容（没配才回退）
//!
//! 某个平台在配置段里没配时，才回退读插件目录下的旧文件：
//!
//! ```text
//! plugins/
//!   fuyubot_plugin_music.dll
//!   music/
//!     163cookie.json      ← 网易云（与 TS 版同格式）
//!     soda.json           ← 汽水（cookie + 设备指纹 + 抓包签名头）
//! ```
//!
//! **读不到不算故障**：插件照常加载，匿名也能搜索、也能拿免费曲目的明文流；
//! 真正用到时再把「缺哪个字段」报给用户。换 cookie 重启宿主即可。

use std::path::{Path, PathBuf};

use fuyubot_abi::HostApi;
use serde::Deserialize;

/// 本插件在 `fuyubot.toml` 里的配置段（插件名点分 → `[plugin.fuyubot.music]`）。
///
/// **只用一个根段、所有键平铺**；不认二级子表。
pub const CONFIG_SECTION: &str = "fuyubot.music";
/// 网易云：整段 Cookie 头字符串。
pub const CONFIG_NETEASE: &str = "netease";
/// 汽水：整段 Cookie 头字符串。
pub const CONFIG_SODA: &str = "soda";
/// 汽水设备指纹（可选）。
pub const CONFIG_SODA_DEVICE_ID: &str = "soda_device_id";
/// 汽水安装 id（可选）。
pub const CONFIG_SODA_IID: &str = "soda_iid";
/// 汽水指纹（可选，一般等于 device_id）。
pub const CONFIG_SODA_FP: &str = "soda_fp";
/// 汽水应用级签名头（可选）。
pub const CONFIG_SODA_X_HELIOS: &str = "soda_x_helios";
/// 汽水应用级签名头（可选）。
pub const CONFIG_SODA_X_MEDUSA: &str = "soda_x_medusa";
/// 汽水 UA（可选）。
pub const CONFIG_SODA_USER_AGENT: &str = "soda_user_agent";
/// 汽水逐请求签名器地址（可选）。
pub const CONFIG_SODA_SIGNER_URL: &str = "soda_signer_url";
/// 汽水签名器 Bearer token（可选）。
pub const CONFIG_SODA_SIGNER_TOKEN: &str = "soda_signer_token";

/// 网易云 cookie 的默认项（官方 PC 客户端形状）。
///
/// `MUSIC_U` 是登录态，缺了就是匿名：部分歌曲能拿到试听或免费流，VIP 曲目不行。
const NETEASE_DEFAULT_OS: &str = "pc";
const NETEASE_DEFAULT_APPVER: &str = "3.1.28.205001";

/// 网易云客户端 UA（eapi 认这个，换成普通浏览器 UA 会拿不到数据）。
pub const NETEASE_USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; WOW64) AppleWebKit/537.36 (KHTML, like Gecko) Safari/537.36 Chrome/91.0.4472.164 NeteaseMusicDesktop/2.10.2.200154";

/// 网易云鉴权材料。
#[derive(Debug, Clone, Default)]
pub struct NeteaseAuth {
    /// 原样保留文件里的键值对（顺序也保留），方便原样回写 `Cookie` 头。
    pub cookies: Vec<(String, String)>,
    /// 由 `cookies` 拼出来的 `Cookie` 头；空表示匿名。
    pub cookie_header: String,
}

impl NeteaseAuth {
    /// 有 `MUSIC_U` 才算登录态（与参考实现的判断一致）。
    pub fn is_logged_in(&self) -> bool {
        self.cookies
            .iter()
            .any(|(name, value)| name == "MUSIC_U" && !value.trim().is_empty())
    }
}

/// 汽水的鉴权材料。
///
/// * `cookie`：登录态（`sessionid` / `sid_tt` 等）。
/// * `device_id` / `iid` / `fp`：设备指纹，必须和 `x-helios` / `x-medusa` 出自同一台机器。
/// * `x_helios` / `x_medusa`：官方客户端原生安全组件产出的**应用级签名头**。
///   只有「整曲 / 会员音质」的 `POST /luna/pc/track_v2` 需要它；缺了只能拿 Web 侧
///   下发的流（免费曲目可得整曲，会员曲目通常只有几十秒试听）。
#[derive(Debug, Clone, Default)]
pub struct SodaAuth {
    pub cookie: String,
    pub device_id: String,
    pub iid: String,
    pub fp: String,
    pub x_helios: String,
    pub x_medusa: String,
    pub user_agent: String,
    /// 应用签名器地址（可选）。配了它就用逐请求签名，静态头只作兜底。
    pub signer_url: String,
    /// 签名器的 Bearer token（可选）。
    pub signer_token: String,
}

impl SodaAuth {
    /// 有没有应用级签名头（两个都要有才完整）。
    pub fn has_signature(&self) -> bool {
        !self.x_helios.trim().is_empty() && !self.x_medusa.trim().is_empty()
    }

    /// 有没有配置逐请求签名器。
    ///
    /// 只认完整的 `http(s)://` 地址：配置里写了一段说明文字（示例文件里很常见）
    /// 时当没配，避免每次取流都拿一个非法 URI 去发请求。
    pub fn has_signer(&self) -> bool {
        let url = self.signer_url.trim();
        url.starts_with("http://") || url.starts_with("https://")
    }

    pub fn fp_or_device_id(&self) -> String {
        let fp = self.fp.trim();
        if fp.is_empty() {
            self.device_id.trim().to_owned()
        } else {
            fp.to_owned()
        }
    }
}

/// 一次加载的全部鉴权材料。
#[derive(Debug, Clone, Default)]
pub struct Auth {
    pub netease: Option<NeteaseAuth>,
    pub soda: Option<SodaAuth>,
    pub dir: PathBuf,
}

/// 从 `fuyubot.toml` 的 `[plugin.fuyubot.music]` 加载；没配才回退旧文件。
///
/// 配置优先（推荐），旧文件只是兼容已有部署。读不到 / 配置为空都只记日志，不返回错误。
pub fn load(host: HostApi, plugins_dir: &Path) -> Auth {
    let dir = plugins_dir.join("music");
    let netease = load_netease(host, &dir);
    let soda = load_soda(host, &dir);

    match &netease {
        Some(auth) if auth.is_logged_in() => {
            crate::log::info("网易云：已读取登录态（MUSIC_U）");
        }
        Some(_) => crate::log::warn(
            "网易云：鉴权材料里没有 MUSIC_U，只能拿匿名/免费曲目（VIP 曲目会失败）",
        ),
        None => crate::log::warn(&format!(
            "网易云：fuyubot.toml 的 [plugin.{CONFIG_SECTION}] 没配 {CONFIG_NETEASE}，\
             也没有旧文件 {}，/music163 只走匿名",
            dir.join("163cookie.json").display()
        )),
    }
    if let Some(auth) = &soda
        && !auth.signer_url.trim().is_empty()
        && !auth.has_signer()
    {
        crate::log::warn(&format!(
            "汽水：signer_url 不是 http(s) 地址，已忽略：{}",
            crate::log::shorten(auth.signer_url.trim(), 80)
        ));
    }
    match &soda {
        Some(auth) if auth.has_signer() => {
            crate::log::info("汽水：已配置应用签名器（整曲取流走逐请求签名）")
        }
        Some(auth) if auth.has_signature() => {
            crate::log::info("汽水：已读取 cookie 与应用签名头（整曲取流可用）")
        }
        Some(auth) if !auth.cookie.trim().is_empty() => {
            crate::log::warn("汽水：有 cookie 但没有 x-helios / x-medusa，会员曲目只能拿试听")
        }
        Some(_) => crate::log::warn("汽水：匿名模式（免费曲目可播，会员曲目只能试听）"),
        None => crate::log::warn(&format!(
            "汽水：fuyubot.toml 的 [plugin.{CONFIG_SECTION}] 没配 {CONFIG_SODA}，\
             也没有旧文件 {}，/soda 只走匿名",
            dir.join("soda.json").display()
        )),
    }

    Auth { netease, soda, dir }
}

/// 网易云：配置优先，没配才回退 `163cookie.json`。
fn load_netease(host: HostApi, dir: &Path) -> Option<NeteaseAuth> {
    if let Some(cookie) = config_string(host, CONFIG_NETEASE) {
        crate::log::info(&format!(
            "网易云：鉴权材料来自 fuyubot.toml 的 [plugin.{CONFIG_SECTION}].{CONFIG_NETEASE}"
        ));
        return Some(parse_netease(&serde_json::json!({ "cookie": cookie })));
    }
    let path = dir.join("163cookie.json");
    let value = read_json(&path)?;
    crate::log::warn(&format!(
        "网易云：配置段没有 {CONFIG_NETEASE}（或不是字符串），回退旧文件 {}（建议迁移到 fuyubot.toml）",
        path.display()
    ));
    Some(parse_netease(&value))
}

/// 汽水：配置优先，没配才回退 `soda.json`。
///
/// 平铺 key 组装成旧 `soda.json` 的形状：`soda` → cookie，`soda_*` → 对应字段。
/// 只配了 `soda_device_id` 这类指纹也算「配置了」（匿名可用）。
fn load_soda(host: HostApi, dir: &Path) -> Option<SodaAuth> {
    let mut fields = serde_json::Map::new();
    if let Some(cookie) = config_string(host, CONFIG_SODA) {
        fields.insert("cookie".to_owned(), serde_json::json!(cookie));
    }
    for (key, field) in [
        (CONFIG_SODA_DEVICE_ID, "device_id"),
        (CONFIG_SODA_IID, "iid"),
        (CONFIG_SODA_FP, "fp"),
        (CONFIG_SODA_X_HELIOS, "x_helios"),
        (CONFIG_SODA_X_MEDUSA, "x_medusa"),
        (CONFIG_SODA_USER_AGENT, "user_agent"),
        (CONFIG_SODA_SIGNER_URL, "signer_url"),
        (CONFIG_SODA_SIGNER_TOKEN, "signer_token"),
    ] {
        if let Some(value) = config_string(host, key) {
            fields.insert(field.to_owned(), serde_json::json!(value));
        }
    }
    if !fields.is_empty() {
        crate::log::info(&format!(
            "汽水：鉴权材料来自 fuyubot.toml 的 [plugin.{CONFIG_SECTION}] 平铺 key（soda / soda_*）"
        ));
        return Some(parse_soda(&serde_json::Value::Object(fields)));
    }
    let path = dir.join("soda.json");
    let value = read_json(&path)?;
    crate::log::warn(&format!(
        "汽水：配置段没有 soda / soda_*（或不是字符串），回退旧文件 {}（建议迁移到 fuyubot.toml）",
        path.display()
    ));
    Some(parse_soda(&value))
}

/// 读一个平铺配置项：宿主返回 JSON 文本，值**必须是字符串**。
///
/// 缺失 / 空串 / 坏 JSON / 非字符串 → `None`（非字符串与坏 JSON 会 warn）；
/// 调用方按「没配置」处理并回退旧文件。
fn config_string(host: HostApi, key: &str) -> Option<String> {
    normalize_config_string(host.get_config(CONFIG_SECTION, key), key)
}

/// [`config_string`] 的纯函数部分（单测直接喂 `Option<String>`）。
fn normalize_config_string(raw: Option<String>, key: &str) -> Option<String> {
    let raw = raw?;
    let value: serde_json::Value = match serde_json::from_str(&raw) {
        Ok(value) => value,
        Err(error) => {
            crate::log::warn(&format!(
                "配置 [plugin.{CONFIG_SECTION}].{key} 不是合法 JSON（{error}），按未配置处理"
            ));
            return None;
        }
    };
    let serde_json::Value::String(text) = value else {
        crate::log::warn(&format!(
            "配置项 [plugin.{CONFIG_SECTION}].{key} 必须是字符串，已忽略"
        ));
        return None;
    };
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_owned())
}

fn read_json(path: &Path) -> Option<serde_json::Value> {
    let raw = match std::fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return None,
        Err(error) => {
            crate::log::warn(&format!("读不了 {}：{error}", path.display()));
            return None;
        }
    };
    match serde_json::from_str::<serde_json::Value>(&raw) {
        Ok(value) => Some(value),
        Err(error) => {
            crate::log::warn(&format!("{} 不是合法 JSON：{error}", path.display()));
            None
        }
    }
}

/// 解析 `163cookie.json`。
///
/// 支持两种写法：
/// * 参考实现那种**扁平对象**：`{"MUSIC_U":"…","os":"pc","appver":"…"}`；
/// * 单字段：`{"cookie":"MUSIC_U=…; os=pc"}`（从浏览器直接复制的情况）。
pub fn parse_netease(value: &serde_json::Value) -> NeteaseAuth {
    let mut cookies: Vec<(String, String)> = Vec::new();

    if let Some(cookie) = value.get("cookie").and_then(|item| item.as_str()) {
        for part in cookie.split(';') {
            let Some((name, value)) = part.split_once('=') else {
                continue;
            };
            let name = name.trim();
            let value = value.trim();
            if !name.is_empty() && !value.is_empty() {
                cookies.push((name.to_owned(), value.to_owned()));
            }
        }
    } else if let Some(object) = value.as_object() {
        for (name, item) in object {
            let value = match item {
                serde_json::Value::String(text) => text.trim().to_owned(),
                serde_json::Value::Number(number) => number.to_string(),
                serde_json::Value::Bool(flag) => flag.to_string(),
                _ => continue,
            };
            if !value.is_empty() {
                cookies.push((name.clone(), value));
            }
        }
    }

    // 默认项只补缺，不覆盖文件里显式写下的值。
    for (name, value) in [
        ("os", NETEASE_DEFAULT_OS),
        ("appver", NETEASE_DEFAULT_APPVER),
    ] {
        if !cookies.iter().any(|(key, _)| key == name) {
            cookies.push((name.to_owned(), value.to_owned()));
        }
    }

    let cookie_header = cookies
        .iter()
        .map(|(name, value)| {
            // 值里的分号/换行会把 header 截断（也可能是恶意的）。
            format!("{name}={}", value.replace([';', '\r', '\n'], ""))
        })
        .collect::<Vec<_>>()
        .join("; ");

    NeteaseAuth {
        cookies,
        cookie_header,
    }
}

/// `soda.json` 的字段别名：抓包工具导出的常见写法都认。
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct SodaFile {
    #[serde(alias = "cookie_string", alias = "COOKIE")]
    cookie: String,
    #[serde(alias = "deviceId", alias = "DEVICE_ID")]
    device_id: String,
    #[serde(alias = "install_id", alias = "installId", alias = "IID")]
    iid: String,
    #[serde(alias = "FP")]
    fp: String,
    #[serde(alias = "xHelios", alias = "X-Helios", alias = "helios")]
    x_helios: String,
    #[serde(alias = "xMedusa", alias = "X-Medusa", alias = "medusa")]
    x_medusa: String,
    #[serde(alias = "userAgent", alias = "ua")]
    user_agent: String,
    #[serde(alias = "signerUrl", alias = "QISHUI_SIGNER_URL")]
    signer_url: String,
    #[serde(alias = "signerToken", alias = "QISHUI_SIGNER_TOKEN")]
    signer_token: String,
}

/// 解析 `soda.json`；设备指纹缺失时生成一个（匿名可用的形态）。
pub fn parse_soda(value: &serde_json::Value) -> SodaAuth {
    let parsed: SodaFile = serde_json::from_value(value.clone()).unwrap_or_default();
    let now = crate::now_millis();
    let device_id = non_empty(parsed.device_id).unwrap_or_else(|| now.to_string());
    let iid = non_empty(parsed.iid).unwrap_or_else(|| (now + 1).to_string());
    let fp = non_empty(parsed.fp).unwrap_or_else(|| device_id.clone());

    SodaAuth {
        cookie: parsed.cookie.trim().to_owned(),
        device_id,
        iid,
        fp,
        x_helios: parsed.x_helios.trim().to_owned(),
        x_medusa: parsed.x_medusa.trim().to_owned(),
        user_agent: parsed.user_agent.trim().to_owned(),
        signer_url: parsed.signer_url.trim().to_owned(),
        signer_token: parsed.signer_token.trim().to_owned(),
    }
}

fn non_empty(value: String) -> Option<String> {
    let value = value.trim().to_owned();
    (!value.is_empty()).then_some(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flat_netease_cookie_file_is_kept_verbatim() {
        let auth = parse_netease(&serde_json::json!({
            "MUSIC_U": "token",
            "os": "pc",
            "appver": "3.1.0",
            "deviceId": "abc",
        }));
        assert!(auth.is_logged_in());
        assert!(auth.cookie_header.contains("MUSIC_U=token"));
        assert!(auth.cookie_header.contains("os=pc"));
        assert!(auth.cookie_header.contains("deviceId=abc"));
        assert!(
            !auth.cookie_header.contains("appver=3.1.28"),
            "文件里的值不能被默认值覆盖"
        );
    }

    #[test]
    fn missing_defaults_are_filled_but_not_duplicated() {
        let auth = parse_netease(&serde_json::json!({"MUSIC_U": "t"}));
        assert_eq!(
            auth.cookies.iter().filter(|(name, _)| name == "os").count(),
            1
        );
        assert!(auth.cookie_header.contains("os=pc"));
        assert!(auth.cookie_header.contains("appver="));
    }

    #[test]
    fn a_single_cookie_string_is_accepted() {
        let auth = parse_netease(&serde_json::json!({"cookie": "MUSIC_U=abc; os=pc; junk"}));
        assert_eq!(
            auth.cookies
                .iter()
                .filter(|(name, _)| name == "MUSIC_U")
                .count(),
            1
        );
        assert!(auth.cookie_header.contains("MUSIC_U=abc"));
        assert!(!auth.cookie_header.contains("junk"));
    }

    #[test]
    fn anonymous_netease_has_no_login_state() {
        let auth = parse_netease(&serde_json::json!({}));
        assert!(!auth.is_logged_in());
        assert!(auth.cookie_header.contains("os=pc"));
    }

    #[test]
    fn soda_file_accepts_captured_aliases() {
        let auth = parse_soda(&serde_json::json!({
            "cookie": "sessionid=abc",
            "deviceId": "111",
            "install_id": "222",
            "X-Helios": "h",
            "X-Medusa": "m",
            "ua": "LunaPC/3.3.0(1)",
        }));
        assert_eq!(auth.device_id, "111");
        assert_eq!(auth.iid, "222");
        assert_eq!(auth.fp, "111");
        assert!(auth.has_signature());
        assert_eq!(auth.user_agent, "LunaPC/3.3.0(1)");
    }

    #[test]
    fn soda_without_fingerprint_gets_generated_ids() {
        let auth = parse_soda(&serde_json::json!({}));
        assert!(!auth.device_id.is_empty());
        assert!(!auth.iid.is_empty());
        assert_eq!(auth.fp, auth.device_id);
        assert!(!auth.has_signature());
    }

    #[test]
    fn signer_settings_are_read_with_aliases() {
        let auth = parse_soda(&serde_json::json!({
            "signerUrl": "http://127.0.0.1:8899/sign",
            "signerToken": "secret",
        }));
        assert!(auth.has_signer());
        assert_eq!(auth.signer_url, "http://127.0.0.1:8899/sign");
        assert_eq!(auth.signer_token, "secret");
        // 只填 token 不算配置了签名器
        assert!(!parse_soda(&serde_json::json!({"signer_token": "x"})).has_signer());
        // 说明文字 / 非 http(s) 地址不算签名器
        assert!(!parse_soda(&serde_json::json!({"signer_url": "可选：http://x"})).has_signer());
        assert!(!parse_soda(&serde_json::json!({"signer_url": "127.0.0.1:8899"})).has_signer());
    }

    #[test]
    fn half_a_signature_is_not_a_signature() {
        let auth = parse_soda(&serde_json::json!({"x_helios": "h"}));
        assert!(!auth.has_signature(), "两个头都齐才算");
    }

    #[test]
    fn cookie_values_cannot_break_the_header() {
        let auth = parse_netease(&serde_json::json!({"MUSIC_U": "a;b\r\nc"}));
        assert!(!auth.cookie_header.contains('\r'));
        assert!(!auth.cookie_header.contains('\n'));
        assert_eq!(
            auth.cookie_header.matches(';').count(),
            2,
            "分隔符只有补的两个默认值"
        );
    }

    // ── 配置段 `[plugin.fuyubot.music]` ──────────────────────────────
    //
    // 用假 `HostApi`：`get_config` 从进程内表里取「宿主返回的 JSON 文本」，
    // 其余函数一律 noop。所有配置测试共用一份表，必须串行。

    use std::cell::RefCell;
    use std::ffi::{CStr, CString, c_char, c_void};
    use std::sync::Mutex;

    use fuyubot_abi::{
        ABI_VERSION, EventCallbackFn, HostApiVTable, MiddlewareCallbackFn, MiddlewareHandle,
        RequestId, ServiceHandle, ServiceVTable, SubscriptionHandle, ToolHandle, ToolHandlerFn,
        WaitHandle,
    };

    /// 串行化配置测试（共用 [`CONFIG`]）。
    static AUTH_TEST_LOCK: Mutex<()> = Mutex::new(());
    /// `section.key` → 宿主 `get_config` 应当返回的 JSON 文本。
    static CONFIG: Mutex<Vec<(String, String)>> = Mutex::new(Vec::new());

    thread_local! {
        static CONFIG_REPLY: RefCell<CString> = RefCell::new(CString::new("").expect("empty"));
    }

    fn set_config(key: &str, json: &str) {
        let full = format!("{CONFIG_SECTION}.{key}");
        let mut config = CONFIG.lock().unwrap();
        config.retain(|(candidate, _)| candidate != &full);
        config.push((full, json.to_owned()));
    }

    fn clear_config() {
        CONFIG.lock().unwrap().clear();
    }

    unsafe extern "C" fn fake_get_config(
        _host: *mut c_void,
        section: *const c_char,
        key: *const c_char,
    ) -> *const c_char {
        if section.is_null() || key.is_null() {
            return std::ptr::null();
        }
        // SAFETY: 宿主契约保证 section / key 是 NUL 结尾字符串。
        let section = unsafe { CStr::from_ptr(section) }.to_string_lossy();
        let key = unsafe { CStr::from_ptr(key) }.to_string_lossy();
        let full = format!("{section}.{key}");
        let value = CONFIG
            .lock()
            .unwrap()
            .iter()
            .find(|(candidate, _)| candidate == &full)
            .map(|(_, value)| value.clone());
        match value {
            Some(text) => CONFIG_REPLY.with(|cell| {
                *cell.borrow_mut() = CString::new(text).expect("config");
                cell.borrow().as_ptr()
            }),
            None => std::ptr::null(),
        }
    }

    unsafe extern "C" fn noop_on(
        _: *mut c_void,
        _: *const c_char,
        _: *const c_char,
        _: Option<EventCallbackFn>,
        _: *mut c_void,
    ) -> SubscriptionHandle {
        SubscriptionHandle::INVALID
    }
    unsafe extern "C" fn noop_off(_: *mut c_void, _: SubscriptionHandle) {}
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
    unsafe extern "C" fn noop_td_simplified(
        _: *mut c_void,
        _: *const c_char,
        _: *const c_char,
    ) -> RequestId {
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
    unsafe extern "C" fn noop_set_config(
        _: *mut c_void,
        _: *const c_char,
        _: *const c_char,
    ) -> i32 {
        -1
    }
    unsafe extern "C" fn noop_simplify_invoke(_: *mut c_void, _: *const c_char) -> RequestId {
        RequestId::INVALID
    }
    unsafe extern "C" fn noop_simplify_invoke_result(
        _: *mut c_void,
        _: *const c_char,
    ) -> *const c_char {
        std::ptr::null()
    }
    /// ABI v6：服务调用桥。假宿主里没有服务，一律 NULL（= 失败）。
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
            on: Some(noop_on),
            off: Some(noop_off),
            emit: Some(noop_emit),
            register_tool: Some(noop_register_tool),
            call_tool: Some(noop_call_tool),
            call_tool_result: Some(noop_call_tool_result),
            list_tools: Some(noop_list),
            list_plugins: Some(noop_list),
            provide_service: Some(noop_provide),
            get_service: Some(noop_get),
            td_invoke: Some(noop_td),
            td_invoke_simplified: Some(noop_td_simplified),
            td_execute: Some(noop_execute),
            use_middleware: Some(noop_middleware),
            wait_for_update: Some(noop_wait),
            cancel_wait: Some(noop_cancel),
            log: Some(noop_log),
            get_config: Some(fake_get_config),
            set_config: Some(noop_set_config),
            provide_service_shared: Some(noop_provide),
            simplify_invoke: Some(noop_simplify_invoke),
            simplify_invoke_result: Some(noop_simplify_invoke_result),
            // ABI v6 新增：调用者身份 / 服务调用桥 / 失败原因 / 服务清单 / 跨插件写配置。
            current_plugin: Some(noop_list),
            call_service: Some(noop_call_service),
            last_error: Some(noop_list),
            list_services: Some(noop_list),
            set_config_section: Some(noop_set_config_section),
        };
        // SAFETY: 所有必需函数都已填；`from_raw` 会拷走函数指针。
        unsafe { HostApi::from_raw(&vtable) }.expect("fake host")
    }

    /// 建一个空的临时 `plugins/` 目录（每个用例独立，跑完删）。
    fn temp_plugins_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("fuyubot-music-auth-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("music")).expect("temp dir");
        dir
    }

    fn write_legacy(dir: &Path, file: &str, json: &str) {
        std::fs::write(dir.join("music").join(file), json).expect("legacy file");
    }

    #[test]
    fn config_string_is_used_as_the_cookie() {
        let _guard = AUTH_TEST_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        clear_config();
        set_config(CONFIG_NETEASE, r#""MUSIC_U=from-config; os=pc""#);
        let cookie = config_string(fake_host(), CONFIG_NETEASE).expect("配置应当生效");
        assert_eq!(cookie, "MUSIC_U=from-config; os=pc");
        let auth = parse_netease(&serde_json::json!({ "cookie": cookie }));
        assert!(auth.is_logged_in());
        assert!(auth.cookie_header.contains("MUSIC_U=from-config"));
    }

    #[test]
    fn non_string_config_is_ignored_and_falls_back() {
        let _guard = AUTH_TEST_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        clear_config();
        let dir = temp_plugins_dir("non-string");
        write_legacy(&dir, "163cookie.json", r#"{"MUSIC_U":"from-file"}"#);
        // 旧版子表写法（对象）不再支持：按未配置处理 → 回退旧文件。
        set_config(
            CONFIG_NETEASE,
            r#"{"MUSIC_U":"obj-token","deviceId":"abc"}"#,
        );
        assert!(config_string(fake_host(), CONFIG_NETEASE).is_none());
        let netease = load(fake_host(), &dir).netease.expect("回退旧文件");
        assert!(
            netease.cookie_header.contains("from-file"),
            "非字符串配置应当回退旧文件：{}",
            netease.cookie_header
        );
        // 数字 / null / 数组 / 空对象同样按未配置处理，不 panic。
        for bad in ["42", "null", "[1,2]", "{}"] {
            set_config(CONFIG_SODA, bad);
            assert!(
                config_string(fake_host(), CONFIG_SODA).is_none(),
                "{bad} 不该被当成配置"
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn empty_or_bad_config_is_unconfigured_without_panicking() {
        let _guard = AUTH_TEST_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        // 纯函数：没配置 / 空串 / 坏 JSON / 空字符串值 / null / 空对象 / 其它类型。
        assert!(normalize_config_string(None, CONFIG_NETEASE).is_none());
        assert!(normalize_config_string(Some(String::new()), CONFIG_NETEASE).is_none());
        assert!(normalize_config_string(Some("not json".to_owned()), CONFIG_NETEASE).is_none());
        assert!(normalize_config_string(Some("\"\"".to_owned()), CONFIG_NETEASE).is_none());
        assert!(normalize_config_string(Some("\"   \"".to_owned()), CONFIG_NETEASE).is_none());
        assert!(normalize_config_string(Some("null".to_owned()), CONFIG_NETEASE).is_none());
        assert!(normalize_config_string(Some("{}".to_owned()), CONFIG_NETEASE).is_none());
        assert!(normalize_config_string(Some("42".to_owned()), CONFIG_NETEASE).is_none());
        assert!(normalize_config_string(Some("[1]".to_owned()), CONFIG_NETEASE).is_none());
        // 合法字符串：去首尾空白后非空才算配置。
        assert_eq!(
            normalize_config_string(Some("\"token\"".to_owned()), CONFIG_NETEASE),
            Some("token".to_owned())
        );
        assert_eq!(
            normalize_config_string(Some("\"  token  \"".to_owned()), CONFIG_NETEASE),
            Some("token".to_owned())
        );
        // 宿主里根本没有这个键：不 panic，返回 None。
        clear_config();
        assert!(config_string(fake_host(), CONFIG_NETEASE).is_none());
    }

    #[test]
    fn config_takes_priority_over_the_legacy_file() {
        let _guard = AUTH_TEST_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        clear_config();
        let dir = temp_plugins_dir("priority");
        write_legacy(&dir, "163cookie.json", r#"{"MUSIC_U":"from-file"}"#);
        set_config(CONFIG_NETEASE, r#""MUSIC_U=from-config""#);

        let auth = load(fake_host(), &dir).netease.expect("配置应当生效");
        assert!(
            auth.cookie_header.contains("from-config"),
            "配置优先：{}",
            auth.cookie_header
        );
        assert!(
            !auth.cookie_header.contains("from-file"),
            "配置存在时不该读旧文件：{}",
            auth.cookie_header
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_config_falls_back_to_the_legacy_file() {
        let _guard = AUTH_TEST_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        clear_config();
        let dir = temp_plugins_dir("fallback");
        write_legacy(&dir, "163cookie.json", r#"{"MUSIC_U":"from-file"}"#);

        let auth = load(fake_host(), &dir).netease.expect("回退旧文件");
        assert!(
            auth.cookie_header.contains("from-file"),
            "应当读旧文件：{}",
            auth.cookie_header
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn neither_config_nor_file_is_anonymous() {
        let _guard = AUTH_TEST_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        clear_config();
        let dir = temp_plugins_dir("anonymous");
        let auth = load(fake_host(), &dir);
        assert!(auth.netease.is_none(), "没配置也没文件 → 匿名");
        assert!(auth.soda.is_none(), "没配置也没文件 → 匿名");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn flat_soda_keys_are_all_read() {
        let _guard = AUTH_TEST_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        clear_config();
        // 单层平铺：soda 是 Cookie 头，其余是 soda_ 前缀的指纹 / 签名头。
        set_config(CONFIG_SODA, r#""sessionid=abc""#);
        set_config(CONFIG_SODA_DEVICE_ID, r#""111""#);
        set_config(CONFIG_SODA_IID, r#""222""#);
        set_config(CONFIG_SODA_FP, r#""333""#);
        set_config(CONFIG_SODA_X_HELIOS, r#""h""#);
        set_config(CONFIG_SODA_X_MEDUSA, r#""m""#);
        set_config(CONFIG_SODA_USER_AGENT, r#""LunaPC/3.3.0(1)""#);
        set_config(CONFIG_SODA_SIGNER_URL, r#""http://127.0.0.1:8899/sign""#);
        set_config(CONFIG_SODA_SIGNER_TOKEN, r#""secret""#);
        let dir = temp_plugins_dir("soda-flat");
        let soda = load(fake_host(), &dir).soda.expect("配置应当生效");
        assert_eq!(soda.cookie, "sessionid=abc");
        assert_eq!(soda.device_id, "111");
        assert_eq!(soda.iid, "222");
        assert_eq!(soda.fp, "333");
        assert!(soda.has_signature());
        assert!(soda.has_signer());
        assert_eq!(soda.user_agent, "LunaPC/3.3.0(1)");
        assert_eq!(soda.signer_token, "secret");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn flat_soda_fingerprint_without_cookie_is_still_configured() {
        let _guard = AUTH_TEST_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        clear_config();
        // 只配设备指纹（匿名但固定指纹）也算配置了，不该回退旧文件。
        set_config(CONFIG_SODA_DEVICE_ID, r#""111""#);
        let dir = temp_plugins_dir("soda-fingerprint");
        write_legacy(&dir, "soda.json", r#"{"cookie":"from-file"}"#);
        let soda = load(fake_host(), &dir).soda.expect("配置应当生效");
        assert_eq!(soda.device_id, "111");
        assert!(soda.cookie.is_empty(), "没配 cookie 就是匿名");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn soda_config_string_is_used_as_the_cookie() {
        let _guard = AUTH_TEST_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        clear_config();
        set_config(CONFIG_SODA, r#""sessionid=xyz; sid_tt=1""#);
        let dir = temp_plugins_dir("soda-string");
        let soda = load(fake_host(), &dir).soda.expect("配置应当生效");
        assert_eq!(soda.cookie, "sessionid=xyz; sid_tt=1");
        // 设备指纹没配时按现有行为生成。
        assert!(!soda.device_id.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
