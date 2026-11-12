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
    let mut config = rustls::ClientConfig::builder()
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
            .gzip(true)
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

/// GET，带一组请求头，4xx/5xx 也当正常响应返回。
pub async fn get(agent: &Agent, url: &str, headers: &[(&str, &str)]) -> Result<Reply, String> {
    crate::log::debug(&format!("→ GET {}", crate::log::redact_url(url)));
    let started = std::time::Instant::now();
    let mut request = agent.api.get(url);
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    let response = request
        .send()
        .await
        .map_err(|error| format!("请求失败：{error}"))?;
    let reply = read(response).await;
    crate::log::debug(&format!(
        "← {}（{} ms，{} 字节）",
        reply.status,
        started.elapsed().as_millis(),
        reply.body.len()
    ));
    Ok(reply)
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
        .map_err(|error| format!("请求失败：{error}"))?;
    let reply = read(response).await;
    crate::log::debug(&format!(
        "← {}（{} ms，{} 字节）",
        reply.status,
        started.elapsed().as_millis(),
        reply.body.len()
    ));
    Ok(reply)
}

/// POST 原始 JSON 体。
pub async fn post_json(
    agent: &Agent,
    url: &str,
    body: &[u8],
    headers: &[(&str, &str)],
) -> Result<Reply, String> {
    crate::log::debug(&format!("→ POST(json) {}", crate::log::redact_url(url)));
    let started = std::time::Instant::now();
    let mut request = agent
        .api
        .post(url)
        .header("Content-Type", "application/json; charset=utf-8");
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    let response = request
        .body(body.to_vec())
        .send()
        .await
        .map_err(|error| format!("请求失败：{error}"))?;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_dir_sits_next_to_the_plugins_dir() {
        assert_eq!(
            cache_dir(Path::new("D:/bots/demo/plugins")),
            PathBuf::from("D:/bots/demo/cache")
        );
        assert_eq!(cache_dir(Path::new("/")), PathBuf::from("cache"));
    }

    #[test]
    fn cache_name_takes_the_url_basename_and_a_clean_extension() {
        let name = cache_name("https://x.com/a/b/song.mp3?token=1", "mp3");
        assert!(name.starts_with("song_"), "{name}");
        assert!(name.ends_with(".mp3"), "{name}");
        assert_eq!(name.matches('.').count(), 1, "扩展名只有一个点：{name}");

        let name = cache_name("https://x.com/a/b/no-extension", "flac");
        assert!(name.starts_with("no-extension_"), "{name}");
        assert!(name.ends_with(".flac"), "{name}");

        // 非法字符换掉，且空名字有兜底
        let name = cache_name("https://x.com/a/b/c:1", "jpg");
        assert!(name.starts_with("c_1_"), "{name}");
        let name = cache_name("https://x.com/", "mp3");
        assert!(name.starts_with("music_"), "{name}");
    }

    #[test]
    fn retry_suffix_goes_before_the_extension() {
        assert_eq!(with_suffix("a.mp3", "mp3", 2), "a-2.mp3");
        assert_eq!(with_suffix("weird", "mp3", 1), "weird-1");
    }

    #[test]
    fn candidate_dirs_end_with_the_temp_directory() {
        let dirs = candidate_dirs(Path::new("D:/bots/demo/plugins"));
        assert_eq!(dirs.first(), Some(&PathBuf::from("D:/bots/demo/cache")));
        assert_eq!(
            dirs.last(),
            Some(&std::env::temp_dir().join("fuyubot-music"))
        );
    }

    /// async 下载确实把响应体流式落盘（本地回环，不联网）。
    #[tokio::test]
    async fn download_streams_the_body_to_a_unique_file() {
        use std::io::{Read, Write};
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let address = listener.local_addr().expect("addr");
        let server = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().expect("accept");
            // 读到请求头结束即可（GET 没有 body）。
            let mut head = Vec::new();
            let mut byte = [0u8; 1];
            while socket.read(&mut byte).expect("read") == 1 {
                head.push(byte[0]);
                if head.ends_with(b"\r\n\r\n") {
                    break;
                }
            }
            let body = b"0123456789".repeat(1000);
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: audio/mpeg\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            socket.write_all(response.as_bytes()).expect("head");
            socket.write_all(&body).expect("body");
            let _ = socket.flush();
        });

        let dir =
            std::env::temp_dir().join(format!("fuyubot-music-http-test-{}", crate::now_millis()));
        let path = download(
            &agent(),
            &format!("http://{address}/a/song.mp3?token=1"),
            &[],
            &dir,
            "mp3",
            &AtomicBool::new(false),
        )
        .await
        .expect("下载应当成功");
        server.join().expect("server");
        assert_eq!(
            tokio::fs::metadata(&path).await.expect("metadata").len(),
            10_000
        );
        assert!(
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("song_")),
            "文件名应当来自 URL 末段：{}",
            path.display()
        );
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// 取消标志置位时下载立刻放弃并删掉半成品。
    #[tokio::test]
    async fn a_cancelled_download_leaves_no_partial_file() {
        let cancel = AtomicBool::new(true);
        let dir =
            std::env::temp_dir().join(format!("fuyubot-music-http-cancel-{}", crate::now_millis()));
        // 端口 1 不会有人监听：这里只验证「取消优先于连接」之前的路径不会误建文件。
        // 真正的分块取消由 `write_unique` 的循环保证，单测不依赖网络。
        let error = download(
            &agent(),
            "http://127.0.0.1:1/none.mp3",
            &[],
            &dir,
            "mp3",
            &cancel,
        )
        .await
        .unwrap_err();
        assert!(!error.is_empty());
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }
}
