//! HTTP 层：`reqwest` 0.13 **async** + 流式下载落盘。
//!
//! 所有请求都在插件自己的 tokio 工作任务里 `.await`，**没有任何同步阻塞调用**
//! （旧版是 `ureq` + 固定 worker 线程）。这样宿主回调只需 `submit` 就返回，
//! 下载几十 MB 的大文件也不会占住一条线程。
//!
//! 这一层只做「发请求 / 收字节 / 落盘」，不认识任何平台。
//!
//! 下载刻意**流式**：`response.chunk().await` 一块一块写 `tokio::fs` 文件，
//! 块与块之间检查取消标志；整份读进内存只适用于小 JSON，绝不用于音频 / 视频。

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use reqwest::Client;
use tokio::io::AsyncWriteExt;

/// 普通 API 请求的耗时上限。搜索、取流、详情都是小 JSON。
const TIMEOUT: Duration = Duration::from_secs(20);

/// 下载的建连超时（真正的「卡死」通常发生在这一步）。
const DOWNLOAD_CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
/// 下载响应体的单次读超时：大文件可能是 100MB~2GB，不能用一个总预算把整次传输卡死。
/// 真正的中断靠 `cancel` 标志按块检查；这里只是最后一道保险。
const DOWNLOAD_READ_TIMEOUT: Duration = Duration::from_secs(2 * 60 * 60);

/// 一个 round trip 的结果。状态码留着给调用方判断（4xx 有时是「没版权」而不是故障）。
pub struct Reply {
    pub status: u16,
    /// 跟随重定向后的最终 URL（汽水火山的短链要靠它拿 track id）。
    pub final_url: String,
    pub body: Vec<u8>,
}

impl Reply {
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).to_string()
    }

    /// 当 JSON 解；解析失败时把正文开头带进错误里（接口改版时最容易看出来）。
    pub fn json(&self) -> Result<serde_json::Value, String> {
        serde_json::from_slice(&self.body).map_err(|error| {
            format!(
                "接口返回的不是 JSON（HTTP {}）：{error}；正文开头：{}",
                self.status,
                crate::log::shorten(&self.text(), 160)
            )
        })
    }
}

/// 复用连接池的 HTTP 客户端。
///
/// 两个 `Client`：小 JSON 带 20 秒总超时；下载只限制建连与单次读，
/// 靠取消标志中断（总超时会误伤慢速链路上的大文件）。
pub struct Agent {
    api: Client,
    download: Client,
}

/// 全平台统一的 TLS 配置：**webpki 内置根**（Mozilla），不用 reqwest 默认的
/// `rustls-platform-verifier`。
///
/// ⚠️ 那个默认值在 Android 上是坑：证书校验走系统信任库，要求宿主先做 JNI
/// 初始化（`rustls_platform_verifier::android::init_with_env(env, context)`，
/// 要一个 Android 应用 `Context`）——插件是 `dlopen` 进来的动态库，拿不到也
/// 不该拿，结果第一次 TLS 握手就 panic
/// （`Expect rustls-platform-verifier to be initialized`）。
/// 换成内置根后全平台行为一致，也正是本仓「不依赖系统证书栈」的本意。
fn tls_config() -> rustls::ClientConfig {
    let mut roots = rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    // 显式点名加密后端：workspace 一起构建时 `ring`（adblock 的 ureq）和
    // `aws-lc-rs`（reqwest）会同时编进同一份 rustls，`ClientConfig::builder()`
    // 的 feature 自动探测会 panic（Could not automatically determine the
    // process-level CryptoProvider）；点名后与 feature 合并无关，也不写进程级全局值。
    let provider = std::sync::Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    let mut config = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .expect("rustls：默认协议版本")
        .with_root_certificates(roots)
        .with_no_client_auth();
    // 与 reqwest 默认对齐：`http2` feature 没开，只谈 http/1.1。
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    config
}

impl Agent {
    /// 建一个客户端。失败在 `agent()` 里转成默认（不 panic）。
    fn build() -> Result<Self, reqwest::Error> {
        let api = Client::builder()
            .tls_backend_preconfigured(tls_config())
            .timeout(TIMEOUT)
            .gzip(true)
            .build()?;
        let download = Client::builder()
            .tls_backend_preconfigured(tls_config())
            // 不用总超时：它把 DNS + 建连 + 整份响应体算作一个预算，
            // 大文件在慢速链路上会被硬切断。分项限制见常量。
            .connect_timeout(DOWNLOAD_CONNECT_TIMEOUT)
            .read_timeout(DOWNLOAD_READ_TIMEOUT)
            // 媒体下载不开 gzip：Range 的字节偏移必须和服务器一致，
            // 而且 googlevideo 偶尔会给一个解不开的 Content-Encoding。
            .gzip(false)
            .build()?;
        Ok(Self { api, download })
    }
}

/// 复用连接池的 Agent。每次重建会让服务端一直看到新 TLS 连接。
pub fn agent() -> Agent {
    match Agent::build() {
        Ok(agent) => agent,
        Err(error) => {
            // TLS 后端初始化失败极少见（rustls 是纯 Rust 的），真遇到也不该 panic：
            // 退回一个只带 TLS 根配置的客户端——注意不能 `Client::new()`，
            // 它会走 reqwest 默认的平台校验器（Android 上一握手就 panic）。
            crate::log::warn(&format!("HTTP 客户端初始化失败，退回裸配置：{error}"));
            Agent {
                api: Client::builder()
                    .tls_backend_preconfigured(tls_config())
                    .build()
                    .unwrap_or_else(|_| Client::new()),
                download: Client::builder()
                    .tls_backend_preconfigured(tls_config())
                    .build()
                    .unwrap_or_else(|_| Client::new()),
            }
        }
    }
}

/// GET 的总尝试次数（含首次）。
///
/// 只对**建连阶段**的失败重试：DNS 抖动、建连超时、TLS 握手被掐断。
/// 短链解析整条链路上就这一次请求，一次抖动就让用户只看到「❌ 请求失败」太脆。
const GET_ATTEMPTS: u32 = 2;

/// GET，带一组请求头，4xx/5xx 也当正常响应返回。
pub async fn get(agent: &Agent, url: &str, headers: &[(&str, &str)]) -> Result<Reply, String> {
    let mut attempt = 1;
    loop {
        match get_once(agent, url, headers).await {
            Ok(reply) => return Ok(reply),
            Err(error) => {
                // 已经收到响应之后的失败不重放：body 读了一半没法重来。
                let retryable = error.is_connect() || error.is_timeout();
                if !retryable || attempt >= GET_ATTEMPTS {
                    return Err(format!("请求失败：{}", describe(&error)));
                }
                crate::log::warn(&format!(
                    "← GET 失败（第 {attempt} 次），重试：{}",
                    describe(&error)
                ));
                tokio::time::sleep(Duration::from_millis(300 * u64::from(attempt))).await;
                attempt += 1;
            }
        }
    }
}

/// 发一次 GET（不重试）。
async fn get_once(
    agent: &Agent,
    url: &str,
    headers: &[(&str, &str)],
) -> Result<Reply, reqwest::Error> {
    crate::log::debug(&format!("→ GET {}", crate::log::redact_url(url)));
    let started = std::time::Instant::now();
    let mut request = agent.api.get(url);
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    let response = request.send().await?;
    let reply = read(response).await;
    crate::log::debug(&format!(
        "← {}（{} ms，{} 字节）",
        reply.status,
        started.elapsed().as_millis(),
        reply.body.len()
    ));
    Ok(reply)
}

/// 把 `reqwest::Error` 的整条 `source()` 链摊平。
///
/// `Display` 只有「error sending request for url (…)」这一句，真正的失败原因
/// （DNS 解析失败 / 建连超时 / TLS 握手失败 / 连接被重置）全都藏在 `source()` 里。
/// 不摊开的话，用户看到的和日志里留的都只是一句没有信息量的话。
fn describe(error: &reqwest::Error) -> String {
    let mut text = error.to_string();
    let mut cause = std::error::Error::source(error);
    while let Some(inner) = cause {
        let detail = inner.to_string();
        if !detail.is_empty() && !text.contains(detail.as_str()) {
            text.push('：');
            text.push_str(&detail);
        }
        cause = inner.source();
    }
    crate::log::shorten(&text, 280)
}

/// POST 表单（网易云 eapi 固定是 `params=<hex>`）。
pub async fn post_form(
    agent: &Agent,
    url: &str,
    form: &[(&str, &str)],
    headers: &[(&str, &str)],
) -> Result<Reply, String> {
    crate::log::debug(&format!("→ POST(form) {}", crate::log::redact_url(url)));
    let started = std::time::Instant::now();
    let mut request = agent.api.post(url);
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    let response = request
        .form(form)
        .send()
        .await
        .map_err(|error| format!("请求失败：{}", describe(&error)))?;
    let reply = read(response).await;
    crate::log::debug(&format!(
        "← {}（{} ms，{} 字节）",
        reply.status,
        started.elapsed().as_millis(),
        reply.body.len()
    ));
    Ok(reply)
}

/// POST 原始 JSON 体（20 秒总超时）。
pub async fn post_json(
    agent: &Agent,
    url: &str,
    body: &[u8],
    headers: &[(&str, &str)],
) -> Result<Reply, String> {
    post_json_timeout(agent, url, body, headers, TIMEOUT).await
}

/// POST 原始 JSON 体，可覆盖总超时。
///
/// 需要长超时的 JSON POST（例如外部进程要下载几十秒到几分钟）。
pub async fn post_json_timeout(
    agent: &Agent,
    url: &str,
    body: &[u8],
    headers: &[(&str, &str)],
    timeout: Duration,
) -> Result<Reply, String> {
    crate::log::debug(&format!("→ POST(json) {}", crate::log::redact_url(url)));
    let started = std::time::Instant::now();
    let mut request = agent
        .api
        .post(url)
        .timeout(timeout)
        .header("Content-Type", "application/json; charset=utf-8");
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    let response = request
        .body(body.to_vec())
        .send()
        .await
        .map_err(|error| format!("请求失败：{}", describe(&error)))?;
    let reply = read(response).await;
    crate::log::debug(&format!(
        "← {}（{} ms，{} 字节）",
        reply.status,
        started.elapsed().as_millis(),
        reply.body.len()
    ));
    Ok(reply)
}

/// 把响应读成 [`Reply`]（只用于小 JSON；下载走流式）。
async fn read(response: reqwest::Response) -> Reply {
    let status = response.status().as_u16();
    let final_url = response.url().to_string();
    let body = response
        .bytes()
        .await
        .map(|bytes| bytes.to_vec())
        .unwrap_or_default();
    Reply {
        status,
        final_url,
        body,
    }
}

/// 下载一个媒体文件到 `dir`，返回落盘路径。扩展名由调用方给（TDLib 靠它判 MIME）。
pub async fn download(
    agent: &Agent,
    url: &str,
    headers: &[(&str, &str)],
    dir: &Path,
    extension: &str,
    cancel: &AtomicBool,
) -> Result<PathBuf, String> {
    crate::log::debug(&format!("→ 下载 {}", crate::log::redact_url(url)));
    let started = std::time::Instant::now();
    let mut request = agent.download.get(url);
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    let response = request
        .send()
        .await
        .map_err(|error| format!("下载失败：{error}"))?;
    let status = response.status().as_u16();
    if !(200..300).contains(&status) {
        return Err(format!("下载被拒：HTTP {status}"));
    }

    tokio::fs::create_dir_all(dir)
        .await
        .map_err(|error| format!("建不了缓存目录 {}：{error}", dir.display()))?;
    let (path, error) = write_unique(response, dir, url, extension, cancel).await;
    let path = path.ok_or_else(|| format!("写缓存文件失败：{error}"))?;
    crate::log::debug(&format!(
        "← 已落盘 {}（{} ms）",
        path.display(),
        started.elapsed().as_millis()
    ));
    Ok(path)
}

/// 撞名时最多再试几次。
const UNIQUE_ATTEMPTS: usize = 8;

/// 边收边写。写完才发现 0 字节的一律当失败（否则 TDLib 会发一条空音频）。
async fn write_unique(
    mut response: reqwest::Response,
    dir: &Path,
    url: &str,
    extension: &str,
    cancel: &AtomicBool,
) -> (Option<PathBuf>, String) {
    let mut last = String::from("未知错误");
    for attempt in 0..UNIQUE_ATTEMPTS {
        let name = match attempt {
            0 => cache_name(url, extension),
            n => with_suffix(&cache_name(url, extension), extension, n),
        };
        let path = dir.join(&name);
        // `create_new`：不覆盖别的请求（或 TDLib）正在读的文件。
        let mut file = match tokio::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .await
        {
            Ok(file) => file,
            Err(error) => {
                last = format!("建 {} 失败：{error}", path.display());
                continue;
            }
        };
        // 按块读 / 写：`stop` 置位后能在一次分块内退出，而不是卡满整份响应体。
        let mut written = 0u64;
        loop {
            if cancel.load(Ordering::SeqCst) {
                let _ = tokio::fs::remove_file(&path).await;
                return (None, "插件正在停止，下载已取消".to_owned());
            }
            match response.chunk().await {
                Ok(Some(chunk)) => {
                    if let Err(error) = file.write_all(&chunk).await {
                        let _ = tokio::fs::remove_file(&path).await;
                        return (None, format!("写 {} 失败：{error}", path.display()));
                    }
                    written += chunk.len() as u64;
                }
                Ok(None) => break,
                Err(error) => {
                    let _ = tokio::fs::remove_file(&path).await;
                    // 读到一半断了就没法重放同一个 body，直接放弃。
                    crate::log::error(&format!("← 下载中断：{error}"));
                    return (None, format!("读下载流失败：{error}"));
                }
            }
        }
        // 先关文件再判空删：Windows 上打开着的文件删不掉。
        drop(file);
        if written == 0 {
            let _ = tokio::fs::remove_file(&path).await;
            return (None, "下载下来是空文件".to_owned());
        }
        return (Some(path), String::new());
    }
    (None, last)
}

/// 在扩展名之前插入 `-<序号>`：`a.mp3` → `a-2.mp3`（扩展名必须保持在最后）。
fn with_suffix(name: &str, extension: &str, index: usize) -> String {
    match name.strip_suffix(&format!(".{extension}")) {
        Some(stem) => format!("{stem}-{index}.{extension}"),
        None => format!("{name}-{index}"),
    }
}

/// 缓存文件名：URL 路径最后一段 + 干净扩展名；非法字符换 `_`。
fn cache_name(url: &str, extension: &str) -> String {
    let path = url.split("://").nth(1).unwrap_or(url);
    let path = path.split(['?', '#']).next().unwrap_or(path);
    let base = path.rsplit('/').next().unwrap_or("").to_owned();
    let cleaned: String = base
        .chars()
        .map(|ch| {
            if matches!(ch, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*') || ch.is_control()
            {
                '_'
            } else {
                ch
            }
        })
        .collect();
    let trimmed = cleaned.trim_end_matches(['.', ' ']);
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0);
    if trimmed.is_empty() {
        return format!("music_{stamp}.{extension}");
    }
    // 底下永远带时间戳：同一首歌重复点两次、或两首歌 URL 末段同名时都不会撞名。
    let stem = match trimmed.strip_suffix(&format!(".{extension}")) {
        Some(stem) if !stem.is_empty() => stem,
        _ => trimmed,
    };
    format!("{stem}_{stamp}.{extension}")
}

/// 由宿主给的插件目录推出 `<项目>/cache`。
///
/// 插件是 cdylib，`current_exe()` 拿到的是 `fuyubot.exe`，算不出宿主工作目录；
/// 而 `plugins_dir` 是宿主自己给的绝对路径，它的父目录正是项目根。
pub fn cache_dir(plugins_dir: &Path) -> PathBuf {
    plugins_dir
        .parent()
        .map(|parent| parent.join("cache"))
        .unwrap_or_else(|| PathBuf::from("cache"))
}

/// 下载目录的候选：项目 `cache/` → 插件目录下 `music/cache` → 系统临时目录。
///
/// 宿主可能跑在写不进项目目录的环境里（表现为 `拒绝访问 (os error 5)`），
/// 换目录比改权限可靠。
pub fn candidate_dirs(plugins_dir: &Path) -> Vec<PathBuf> {
    let mut dirs = vec![
        cache_dir(plugins_dir),
        plugins_dir.join("music").join("cache"),
    ];
    let temp = std::env::temp_dir().join("fuyubot-music");
    if !dirs.contains(&temp) {
        dirs.push(temp);
    }
    dirs
}

/// googlevideo 的整段请求会被限速到 ~10 KB/s，Range 分段则正常（实测 250 KB/s）。
/// 每段 2 MiB：3 分钟的 m4a 大约 3~4 个请求，既快又不会把单请求拖太久。
const RANGE_CHUNK: u64 = 2 * 1024 * 1024;

/// 分段下载：按 Range 一块一块拉，边拉边写。
///
/// YouTube 的音频直链没有 ratebypass，一次要完整文件会被限速；普通音频
/// 普通链接用 download 就行。总大小由 player 的 contentLength 给（0 表示未知，
/// 这时退回整段下载）。
pub async fn download_ranged(
    agent: &Agent,
    url: &str,
    headers: &[(&str, &str)],
    dir: &Path,
    extension: &str,
    total: u64,
    cancel: &AtomicBool,
) -> Result<PathBuf, String> {
    if total == 0 {
        return download(agent, url, headers, dir, extension, cancel).await;
    }
    crate::log::debug(&format!(
        "→ 分段下载 {}（{total} 字节）",
        crate::log::redact_url(url)
    ));
    let started = std::time::Instant::now();
    tokio::fs::create_dir_all(dir)
        .await
        .map_err(|error| format!("建不了缓存目录 {}：{error}", dir.display()))?;
    let (path, mut file) = create_unique(dir, url, extension).await?;
    let mut written = 0u64;
    while written < total {
        if cancel.load(Ordering::SeqCst) {
            drop(file);
            let _ = tokio::fs::remove_file(&path).await;
            return Err("插件正在停止，下载已取消".to_owned());
        }
        let end = written.saturating_add(RANGE_CHUNK - 1).min(total - 1);
        let range = format!("bytes={written}-{end}");
        let mut request = agent.download.get(url);
        for (name, value) in headers {
            request = request.header(*name, *value);
        }
        let response = match request.header("Range", range.as_str()).send().await {
            Ok(response) => response,
            Err(error) => {
                drop(file);
                let _ = tokio::fs::remove_file(&path).await;
                return Err(format!("分段请求失败：{error}"));
            }
        };
        let status = response.status().as_u16();
        if status != 206 && status != 200 {
            drop(file);
            let _ = tokio::fs::remove_file(&path).await;
            return Err(format!("分段下载被拒：HTTP {status}"));
        }
        if status == 200 && written > 0 {
            drop(file);
            let _ = tokio::fs::remove_file(&path).await;
            return Err("服务器忽略了 Range 请求，无法续传".to_owned());
        }
        let mut response = response;
        let before = written;
        loop {
            if cancel.load(Ordering::SeqCst) {
                drop(file);
                let _ = tokio::fs::remove_file(&path).await;
                return Err("插件正在停止，下载已取消".to_owned());
            }
            match response.chunk().await {
                Ok(Some(chunk)) => {
                    if let Err(error) = file.write_all(&chunk).await {
                        drop(file);
                        let _ = tokio::fs::remove_file(&path).await;
                        return Err(format!("写 {} 失败：{error}", path.display()));
                    }
                    written += chunk.len() as u64;
                }
                Ok(None) => break,
                Err(error) => {
                    drop(file);
                    let _ = tokio::fs::remove_file(&path).await;
                    return Err(format!("读分段流失败：{error}"));
                }
            }
        }
        if written == before {
            drop(file);
            let _ = tokio::fs::remove_file(&path).await;
            return Err("分段下载返回了空响应".to_owned());
        }
        // 服务器不接受 Range、直接回整份文件时，这一轮已经写完了。
        if status == 200 {
            break;
        }
    }
    drop(file);
    let size = std::fs::metadata(&path).map(|meta| meta.len()).unwrap_or(0);
    if size != total {
        let _ = tokio::fs::remove_file(&path).await;
        return Err(format!("下载不完整：{size}/{total} 字节"));
    }
    crate::log::debug(&format!(
        "← 分段下载完成 {}（{} ms）",
        path.display(),
        started.elapsed().as_millis()
    ));
    Ok(path)
}

/// 在 dir 下建一个不撞名的文件。
async fn create_unique(
    dir: &Path,
    url: &str,
    extension: &str,
) -> Result<(PathBuf, tokio::fs::File), String> {
    let mut last = String::from("未知错误");
    for attempt in 0..UNIQUE_ATTEMPTS {
        let name = match attempt {
            0 => cache_name(url, extension),
            n => with_suffix(&cache_name(url, extension), extension, n),
        };
        let path = dir.join(name);
        match tokio::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .await
        {
            Ok(file) => return Ok((path, file)),
            Err(error) => last = format!("建 {} 失败：{error}", path.display()),
        }
    }
    Err(last)
}

#[cfg(test)]
#[path = "../tests/unit/http.rs"]
mod tests;
