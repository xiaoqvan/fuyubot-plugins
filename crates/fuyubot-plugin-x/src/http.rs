//! HTTP 层：解析接口的 JSON + 媒体下载。
//!
//! 只有两件事要联网：
//!
//! | 用途 | 端点 | 结果 |
//! | --- | --- | --- |
//! | 解析 | api.fxtwitter.com/status/ID | 一份 JSON（含 Article 的 Draft.js 文档） |
//! | 兜底下载 | pbs.twimg.com / video.twimg.com | 本地文件（远程直链发不出去时才用） |
//!
//! TLS 用 **webpki 内置根**（Mozilla），不用 reqwest 默认的
//! rustls-platform-verifier：后者在 Android 上要求宿主先做 JNI 初始化，插件是
//! dlopen 进来的动态库，拿不到 application context，第一次握手就 panic。

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde_json::Value;
use tokio::io::AsyncWriteExt;

/// 单次 API 请求的耗时上限。
const TIMEOUT: Duration = Duration::from_secs(20);

/// 建立连接（含 TLS 握手）的预算。
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

/// 下载的耗时上限：视频动辄几十 MB，给得宽一点。
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(600);

/// 解析接口用的 UA。带上插件名，出问题时对方能从日志里认出是谁在请求。
pub const UA: &str = "FuyuBot-Plugin-x/0.1 (+https://github.com/CatMoeCircle/fuyu-bot)";

/// 一次 GET 的结果。
pub struct Raw {
    pub status: u16,
    pub body: String,
}

impl Raw {
    pub fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }
}

/// 本层的错误。文案已经能直接进日志或给用户看。
#[derive(Debug)]
pub struct HttpError(pub String);

impl std::fmt::Display for HttpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for HttpError {}

fn tls_config() -> rustls::ClientConfig {
    let mut roots = rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    // 显式点名加密后端：workspace 里 ring（adblock 的 ureq）与 aws-lc-rs
    // （reqwest）会同时编进同一份 rustls，靠 feature 自动探测会 panic。
    let provider = std::sync::Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    let mut config = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .expect("rustls：默认协议版本")
        .with_root_certificates(roots)
        .with_no_client_auth();
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    config
}

/// 复用连接池的客户端句柄。
pub struct Agent {
    client: reqwest::Client,
}

impl std::fmt::Debug for Agent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Agent").finish_non_exhaustive()
    }
}

pub fn agent() -> Result<Agent, HttpError> {
    let client = reqwest::Client::builder()
        .tls_backend_preconfigured(tls_config())
        .connect_timeout(CONNECT_TIMEOUT)
        .redirect(reqwest::redirect::Policy::limited(10))
        .user_agent(UA)
        .build()
        .map_err(|error| {
            crate::log::error(&format!("建 HTTP 客户端失败：{error}"));
            HttpError(format!("建 HTTP 客户端失败：{error}"))
        })?;
    Ok(Agent { client })
}

impl Agent {
    /// GET 一份文本（保留状态码）。
    pub async fn get(&self, url: &str) -> Result<Raw, HttpError> {
        crate::log::debug(&format!("→ GET {}", crate::log::redact_url(url)));
        let response = self
            .client
            .get(url)
            .timeout(TIMEOUT)
            .send()
            .await
            .map_err(|error| HttpError(format!("请求失败：{error}")))?;
        let status = response.status().as_u16();
        let body = response
            .text()
            .await
            .map_err(|error| HttpError(format!("读取响应失败：{error}")))?;
        crate::log::debug(&format!("← {} {} 字符", status, body.chars().count()));
        Ok(Raw { status, body })
    }

    /// GET 一份 JSON。
    pub async fn get_json(&self, url: &str) -> Result<Value, HttpError> {
        let raw = self.get(url).await?;
        if !raw.is_success() {
            return Err(HttpError(format!("解析接口返回 HTTP {}", raw.status)));
        }
        serde_json::from_str(&raw.body)
            .map_err(|error| HttpError(format!("解析接口返回的不是 JSON：{error}")))
    }

    /// 流式下载到本地。块与块之间检查取消标志，插件停止时尽快收手。
    pub async fn download(
        &self,
        url: &str,
        path: &Path,
        cancel: &AtomicBool,
    ) -> Result<u64, HttpError> {
        crate::log::debug(&format!("→ 下载 {}", crate::log::redact_url(url)));
        let mut response = self
            .client
            .get(url)
            .timeout(DOWNLOAD_TIMEOUT)
            .send()
            .await
            .map_err(|error| HttpError(format!("下载失败：{error}")))?;
        if !response.status().is_success() {
            return Err(HttpError(format!("下载失败：HTTP {}", response.status())));
        }
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|error| HttpError(format!("建下载目录失败：{error}")))?;
        }
        let mut file = tokio::fs::File::create(path)
            .await
            .map_err(|error| HttpError(format!("建下载文件失败：{error}")))?;
        let mut written = 0u64;
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|error| HttpError(format!("下载中断：{error}")))?
        {
            if cancel.load(Ordering::SeqCst) {
                drop(file);
                let _ = tokio::fs::remove_file(path).await;
                return Err(HttpError("已取消".to_owned()));
            }
            file.write_all(&chunk)
                .await
                .map_err(|error| HttpError(format!("写文件失败：{error}")))?;
            written += chunk.len() as u64;
        }
        file.flush()
            .await
            .map_err(|error| HttpError(format!("刷盘失败：{error}")))?;
        crate::log::debug(&format!("← 下载完成 {written} 字节"));
        Ok(written)
    }
}
