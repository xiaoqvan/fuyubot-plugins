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
/// YouTube：整段 Cookie 头字符串（可选；遇到「Sign in to confirm you're not a bot」时配）。
pub const CONFIG_YOUTUBE: &str = "youtube";

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

/// YouTube 的鉴权材料（可选）。
///
/// YouTube 的公开接口不需要登录；配置 cookie 只为了绕开数据中心 IP 上常见的
/// 「Sign in to confirm you're not a bot」验证。
#[derive(Debug, Clone, Default)]
pub struct YoutubeAuth {
    /// 整段 Cookie 头（浏览器里复制即可）。
    pub cookie: String,
}

impl YoutubeAuth {
    pub fn is_configured(&self) -> bool {
        !self.cookie.trim().is_empty()
    }
}

/// 一次加载的全部鉴权材料。
#[derive(Debug, Clone, Default)]
pub struct Auth {
    pub netease: Option<NeteaseAuth>,
    pub soda: Option<SodaAuth>,
    /// YouTube cookie（可选）。
    pub youtube: Option<YoutubeAuth>,
    pub dir: PathBuf,
}

/// 从 `fuyubot.toml` 的 `[plugin.fuyubot.music]` 加载；没配才回退旧文件。
///
/// 配置优先（推荐），旧文件只是兼容已有部署。读不到 / 配置为空都只记日志，不返回错误。
pub fn load(host: HostApi, plugins_dir: &Path) -> Auth {
    let dir = plugins_dir.join("music");
    let netease = load_netease(host, &dir);
    let soda = load_soda(host, &dir);
    let youtube = load_youtube(host);

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

    match &youtube {
        Some(auth) if auth.is_configured() => {
            crate::log::info("YouTube：已读取 cookie（取流遇到机器人校验时会带上）")
        }
        _ => crate::log::info("YouTube：匿名取流（数据中心 IP 上可能触发登录验证）"),
    }

    Auth {
        netease,
        soda,
        youtube,
        dir,
    }
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

/// YouTube：只从配置段读 cookie（没有旧文件）。
fn load_youtube(host: HostApi) -> Option<YoutubeAuth> {
    let cookie = config_string(host, CONFIG_YOUTUBE)?;
    Some(YoutubeAuth { cookie })
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
#[path = "../tests/unit/auth.rs"]
mod tests;
