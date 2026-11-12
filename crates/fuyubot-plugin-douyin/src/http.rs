//! HTTP 层：抖音的两个请求 + 流式下载。
//!
//! TS 版用 `axios`，这里用 **`reqwest` 0.13 async**——与插件的执行模型一致：
//! 所有网络请求都在插件自有的 tokio 运行时里 `.await`，宿主的更新回调里只
//! `runtime.submit(...)`（见 [`crate::runtime`]），**没有任何线程在阻塞等网络**。
//!
//! 两个 Client，两种需要：
//!
//! | Client | 为什么要这么配 |
//! | --- | --- |
//! | [`Agent::no_redirect`] | 短链 `v.douyin.com/…` **必须关重定向**：我们要的是 302 的 `Location`（里面才有作品 id），跟过去就只剩落地页了 |
//! | [`Agent::follow`] | 分享页 `iesdouyin.com/share/…` 必须跟随跳转、必须带 cookie（`ttwid` 等身份），否则不渲染 `_ROUTER_DATA` |
//!
//! 下载走 [`reqwest::Response::chunk`] **分块流式落盘**：体积无上限、内存恒定，
//! 每个块之间检查取消标志（插件 `stop()` 置位后，在飞下载会尽快收手）。

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use tokio::io::AsyncWriteExt;

/// 抖音对 UA 敏感：不带 UA 或用常见的桌面 UA，分享页会返回风控页而不是作品数据。
///
/// TS 版 `getvideo.ts` 用的是同一个 iPhone UA 字符串，原样搬过来——
/// 它不是随便选的：移动端 UA 才能拿到 SSR 里的 `_ROUTER_DATA`。
pub const MOBILE_UA: &str = "Mozilla/5.0 (iPhone; CPU iPhone OS 17_0 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.0 Mobile/15E148 Safari/604.1";

/// 单次请求的耗时上限。两个请求串行跑，最坏情况是它的两倍。
const TIMEOUT: Duration = Duration::from_secs(15);

/// 建立连接（含 TLS 握手）的预算。它跟下载体积无关，卡住就说明对方不对。
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

/// 一个 round trip 的结果。刻意做成「状态码 + Location + 正文」三件套，
/// 而不是照抄 TS 那样的 `(status, headers, data)`——这里只用到这三个。
pub struct Raw {
    pub status: u16,
    pub location: Option<String>,
    pub body: String,
}

/// 本层的错误。`String` 而不是细分枚举：调用方只会把它塞进给用户看的文案里，
/// 不需要按变体分支处理。
#[derive(Debug)]
pub struct HttpError(pub String);

impl std::fmt::Display for HttpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for HttpError {}

/// 本层的客户端句柄：两个 `reqwest::Client`（连接池各一份）。
///
/// 每次重建 Client 会让抖音侧一直建新 TLS 连接，连着解析几条链接时能明显感到慢，
/// 所以 `start()` 建一次、放进 [`crate::runtime::Runtime`] 复用。
pub struct Agent {
    /// 跟随重定向（分享页用）。
    pub follow: reqwest::Client,
    /// 一律不跟随（短链看 302 用）。
    pub no_redirect: reqwest::Client,
}

impl std::fmt::Debug for Agent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Agent").finish_non_exhaustive()
    }
}

/// 全平台统一的 TLS 配置：**webpki 内置根**（Mozilla），不用 reqwest 默认的
/// `rustls-platform-verifier`。
///
/// ⚠️ 那个默认值在 Android 上是坑：它的证书校验走系统信任库，要求宿主先做
/// JNI 初始化（`rustls_platform_verifier::android::init_with_env(env, context)`，
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

/// 复用连接池的 [`Agent`]。
///
/// 建不出来只可能是 TLS 后端初始化失败——这时让 `start()` 报错比带着一个坏
/// 客户端跑下去好（每次请求都会失败，用户看到的却是「解析失败」）。
pub fn agent() -> Result<Agent, HttpError> {
    let build = |redirect: reqwest::redirect::Policy| {
        reqwest::Client::builder()
            .tls_backend_preconfigured(tls_config())
            .connect_timeout(CONNECT_TIMEOUT)
            .redirect(redirect)
            .build()
    };
    let follow = build(reqwest::redirect::Policy::limited(10)).map_err(|error| {
        crate::log::error(&format!("建 HTTP 客户端失败：{error}"));
        HttpError(format!("建 HTTP 客户端失败：{error}"))
    })?;
    // `Policy::none()` 正好是「一律不跟、原样返回响应」——302 本身也就不是错误，
    // 我们能直接读 `Location`。
    let no_redirect = build(reqwest::redirect::Policy::none()).map_err(|error| {
        crate::log::error(&format!("建短链 HTTP 客户端失败：{error}"));
        HttpError(format!("建 HTTP 客户端失败：{error}"))
    })?;
    Ok(Agent {
        follow,
        no_redirect,
    })
}

/// **不跟随重定向**地取一次，只为读 `Location`。
///
/// 用 `no_redirect` 那个 Client：3xx 原样返回，`Location` 头还在。
pub async fn head_no_redirect(agent: &Agent, url: &str) -> Result<Raw, HttpError> {
    crate::log::debug(&format!("→ 短链 GET {}", crate::log::redact_url(url)));
    let started = std::time::Instant::now();
    let response = agent
        .no_redirect
        .get(url)
        .header("User-Agent", MOBILE_UA)
        .timeout(TIMEOUT)
        .send()
        .await
        .map_err(|error| {
            crate::log::error(&format!(
                "← 短链请求失败（{} ms）：{error}",
                started.elapsed().as_millis()
            ));
            HttpError(format!("请求失败：{error}"))
        })?;

    let status = response.status().as_u16();
    let location = header_of(&response, "location");
    let body = read_body(response).await;
    let raw = Raw {
        status,
        location,
        body,
    };
    crate::log::debug(&format!(
        "← 短链 {}（{} ms）→ Location: {}",
        raw.status,
        started.elapsed().as_millis(),
        raw.location.as_deref().unwrap_or("（无）")
    ));
    Ok(raw)
}

/// 带 cookie 取分享页正文（`_ROUTER_DATA` 里才有作品数据）。
pub async fn get_page(agent: &Agent, url: &str, cookie: &str) -> Result<Raw, HttpError> {
    crate::log::debug(&format!(
        "→ 分享页 GET {}（Cookie {}）",
        crate::log::redact_url(url),
        crate::log::cookie_summary(cookie)
    ));
    let started = std::time::Instant::now();
    let response = agent
        .follow
        .get(url)
        .header("User-Agent", MOBILE_UA)
        .header("Cookie", cookie)
        .header(
            "Accept",
            "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8",
        )
        .header("Accept-Language", "zh-CN,zh;q=0.9")
        .timeout(TIMEOUT)
        .send()
        .await
        .map_err(|error| {
            crate::log::error(&format!(
                "← 分享页请求失败（{} ms）：{error}",
                started.elapsed().as_millis()
            ));
            HttpError(format!("请求失败：{error}"))
        })?;

    let status = response.status().as_u16();
    let body = read_body(response).await;
    let raw = Raw {
        status,
        location: None,
        body,
    };
    // 正文只打开头一小段：整页 HTML 几百 KB，会把宿主日志冲掉，
    // 而且里面带着用户身份。够看出「是风控页还是真数据」就行。
    crate::log::info(&format!(
        "← 分享页 {}，正文 {} 字节（{} ms）\n  开头：{}",
        raw.status,
        raw.body.len(),
        started.elapsed().as_millis(),
        crate::log::shorten(&raw.body, 200)
    ));
    Ok(raw)
}

/// 带**自定义请求头**取一次正文（detail API 路线用）。
///
/// API 路线的画像与分享页不同：桌面 Chrome UA + 签名三件套 + Referer，
/// 全部由调用方（[`crate::sign::detail`]）算好传进来；Cookie 照带——
/// 同一份浏览器身份，签名和 cookie 必须自洽。
pub async fn get_api(
    agent: &Agent,
    url: &str,
    cookie: &str,
    extra_headers: &[(String, String)],
) -> Result<Raw, HttpError> {
    crate::log::debug(&format!("→ detail API GET {}", crate::log::redact_url(url)));
    let started = std::time::Instant::now();
    let mut request = agent
        .follow
        .get(url)
        .header("Cookie", cookie)
        .header("Accept", "application/json, text/plain, */*")
        .header("Referer", "https://www.douyin.com/")
        .timeout(TIMEOUT);
    for (name, value) in extra_headers {
        request = request.header(name, value);
    }
    let response = request.send().await.map_err(|error| {
        crate::log::error(&format!(
            "← detail API 请求失败（{} ms）：{error}",
            started.elapsed().as_millis()
        ));
        HttpError(format!("请求失败：{error}"))
    })?;
    let status = response.status().as_u16();
    let body = read_body(response).await;
    crate::log::debug(&format!(
        "← detail API {status}，正文 {} 字节（{} ms）",
        body.len(),
        started.elapsed().as_millis()
    ));
    Ok(Raw {
        status,
        location: None,
        body,
    })
}

// 下载的落点：**`cache/` 目录**，返回路径。
//
// 落点照 TS 版 `downloadFile.ts`：`path.resolve(process.cwd(), "cache")`，
// 也就是**宿主工作目录下的 `cache/`**。
//
// ⚠️ 插件是 cdylib，`current_exe()` 拿到的是 `fuyubot.exe`，算不出宿主的工作目录；
// 宿主也**不会**把 cwd 传给插件（`plugin_start` 的 config 里只有 `plugins_dir`）。
// 好在插件目录就是项目里的 `plugins/`，它的上一级正是那个「工作目录」——
// 所以 cache 落在 `plugins_dir/../cache`（见 [`cache_dir`]）。
//
// ⚠️ **为什么必须有下载这一步**：抖音的视频直链带防盗链（Referer / 时效签名），
// TDLib 按远程文件（`inputFileRemote`）去拉时拿不到正确上下文，发送会失败。
// 请求头必须照搬解析时那套（UA + cookie + Referer），否则拿到的是风控页。

/// 落盘的目录名。与 TS 版保持一致，方便对照和复用。
pub const CACHE_DIR: &str = "cache";

/// `download` 的落盘扩展名。TDLib 靠它判断 MIME，所以它得**真的反映内容**。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Video,
    Image,
}

impl Kind {
    fn extension(self) -> &'static str {
        match self {
            Self::Video => "mp4",
            Self::Image => "jpg",
        }
    }
}

/// 由宿主给的插件目录推出项目目录，再拼出 `cache/`。
///
/// `plugins_dir` 是绝对路径（宿主保证），它的父目录就是项目根。
/// 拿不到父目录（比如它本身就是根）时退回 `cache/`（相对当前目录）。
pub fn cache_dir(plugins_dir: &Path) -> PathBuf {
    plugins_dir
        .parent()
        .map(|parent| parent.join(CACHE_DIR))
        .unwrap_or_else(|| PathBuf::from(CACHE_DIR))
}

/// 按优先级列出候选落点：**先试理想的，写不动就往下退**。
///
/// 为什么需要多个：
///
/// 1. `<项目>/cache`（TS 版那处，也是首选）——但宿主跑在受限环境里时
///    可能写不进（表现为 `拒绝访问 (os error 5)`）；
/// 2. 插件目录下的 `douyin/cache`——`plugins_dir` 是宿主**自己**给的绝对路径，
///    它既然能把插件 dlopen 进来，这个目录多半可写；
/// 3. 系统临时目录——最后兜底，几乎总能写。
///
/// ⚠️ 别被「目录上带 ReadOnly 属性」误导：Windows 给含 `desktop.ini` 的
/// 「自定义文件夹」打 ReadOnly 只是个显示标记（而且你取消它、资源管理器会再打回去），
/// **对目录不产生写入限制**。真正写不进时，靠换目录解决，不是改属性。
pub fn candidate_dirs(plugins_dir: &Path) -> Vec<PathBuf> {
    let mut dirs = vec![cache_dir(plugins_dir), plugins_dir.join(CACHE_DIR)];
    let temp = std::env::temp_dir().join("fuyubot-douyin");
    if !dirs.contains(&temp) {
        dirs.push(temp);
    }
    dirs
}

/// 下载**视频**（`.mp4`）。
///
/// `cancel` 是插件的取消标志：置位后在下一个块边界放弃并删掉半截文件
/// （`stop()` 不能等一个几百 MB 的下载跑完才返回）。
pub async fn download(
    agent: &Agent,
    url: &str,
    cookie: &str,
    dir: &Path,
    cancel: &AtomicBool,
) -> Result<PathBuf, HttpError> {
    download_as(agent, url, cookie, Kind::Video, dir, cancel).await
}

/// 下载的**总**预算。TDLib 自己的 `inputFileLocal` 上传另算，这里只覆盖把字节拿到手。
///
/// ⚠️ 这不是「单次读」的上限，而是整次下载的上限：抖音的 1080p 长视频上百 MB、
/// 到国内 CDN 的实际吞吐又常常只有几百 KB/s，给 3 分钟会误杀正常下载。
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(600);

/// 下载并按 `kind` 给一个**干净**的扩展名。
///
/// ## 为什么是**流式写盘**，而不是先读进内存
///
/// 旧实现用 `ureq` 的 `read_to_vec()`，而 ureq 的便捷方法带着一个**写死的 10 MiB
/// 上限**——超过就报 `larger than request limit`。于是**任何超过 10 MB 的视频都下不下来**，
/// 而且报错看着像「HTTP 出问题了」，实际跟 HTTP 一点关系都没有。
///
/// 就算把上限抬到 500 MB 也不该走那条路：整份读进内存再整块写盘，峰值内存是文件大小
/// 的两倍。现在用 [`reqwest::Response::chunk`] 一块一块写：**体积无上限、内存恒定**
/// （一个块 + 一个 BufWriter 缓冲），读到一半断了能直接删掉半截文件，
/// 不会留下一个「看着像成了」的空壳。
pub async fn download_as(
    agent: &Agent,
    url: &str,
    cookie: &str,
    kind: Kind,
    dir: &Path,
    cancel: &AtomicBool,
) -> Result<PathBuf, HttpError> {
    let extension = kind.extension();
    crate::log::debug(&format!("→ 下载 {}", crate::log::redact_url(url)));
    let started = std::time::Instant::now();
    let mut response = agent
        .follow
        .get(url)
        .header("User-Agent", MOBILE_UA)
        .header("Cookie", cookie)
        .header("Referer", "https://www.douyin.com/")
        // 视频可能上百 MB：**总预算**给足（连接超时在 Client 上，15 秒）。
        .timeout(DOWNLOAD_TIMEOUT)
        .send()
        .await
        .map_err(|error| {
            crate::log::error(&format!("← 下载请求失败：{error}"));
            HttpError(format!("下载失败：{error}"))
        })?;

    if !response.status().is_success() {
        crate::log::error(&format!("← 下载被拒：HTTP {}", response.status()));
        return Err(HttpError(format!("下载被拒：HTTP {}", response.status())));
    }

    // 落盘：TDLib 的 `inputFileLocal` 需要一个真路径。
    //
    // ⚠️ **扩展名必须是真的 `.mp4` 结尾**：TDLib 靠它判断 MIME 类型。
    // 早先为了「文件名不冲突」写成 `set_extension("mp4.<时间戳>")`，
    // 结果文件名是 `fuyubot-douyin-1234.mp4.1793299949`——TDLib 认不出这是视频，
    // 表现就是「下载成功 6 MB，然后发送照样失败」。
    // 去重改成放在**主干**里，扩展名保持干净。
    tokio::fs::create_dir_all(dir)
        .await
        .map_err(|error| HttpError(format!("建不了缓存目录 {}: {error}", dir.display())))?;

    // `Content-Length` 只用来记日志（有的 CDN 用 chunked，拿不到就是 `None`）。
    let expected = response.content_length();

    let (path, written) = write_stream(dir, url, extension, &mut Body(&mut response), cancel)
        .await
        .inspect_err(|error| {
            crate::log::error(&format!("← 下载失败：{}", error.0));
        })?;
    crate::log::debug(&format!(
        "← 下载 {} 字节（Content-Length {}，{} ms）→ {}",
        written,
        expected
            .map(|n| n.to_string())
            .unwrap_or_else(|| "未知".to_owned()),
        started.elapsed().as_millis(),
        path.display()
    ));
    Ok(path)
}

/// 分块来源：生产路径是 [`reqwest::Response`]，单测里换成各种假流
/// （空的 / 中途断的 / 超大的），这样流式落盘逻辑本身不联网也能测。
trait ChunkSource {
    /// 下一块正文；`Ok(None)` 表示正常结束。
    async fn next_chunk(&mut self) -> Result<Option<Vec<u8>>, String>;
}

/// 生产路径的 [`ChunkSource`]：`reqwest::Response::chunk()`。
struct Body<'a>(&'a mut reqwest::Response);

impl ChunkSource for Body<'_> {
    async fn next_chunk(&mut self) -> Result<Option<Vec<u8>>, String> {
        self.0
            .chunk()
            .await
            .map(|chunk| chunk.map(|bytes| bytes.to_vec()))
            .map_err(|error| error.to_string())
    }
}

/// 撞名时最多再试几次（`cache_name` 里带时间戳，每次调用都会变）。
const UNIQUE_ATTEMPTS: usize = 8;

/// 边读边写：把 `source` 流进一个**新建**的缓存文件，返回 `(路径, 字节数)`。
///
/// 失败时会把半截文件删掉——缓存目录里留一个截断的 `.mp4`，
/// 下一次发送只会拿到「文件损坏」，比直接报错更难查。
///
/// `cancel` 在**块之间**检查：在飞下载不必等整份文件下来就能放弃。
async fn write_stream<S>(
    dir: &Path,
    url: &str,
    extension: &str,
    source: &mut S,
    cancel: &AtomicBool,
) -> Result<(PathBuf, u64), HttpError>
where
    S: ChunkSource + ?Sized,
{
    let (path, file) = reserve_unique(dir, url, extension).await.map_err(|error| {
        HttpError(format!(
            "写缓存文件失败（试了 {UNIQUE_ATTEMPTS} 个名字）：{error}"
        ))
    })?;

    let mut writer = tokio::io::BufWriter::new(file);
    let mut written = 0u64;
    loop {
        if cancel.load(Ordering::Relaxed) {
            // 先把句柄关掉再删：Windows 上开着句柄的删除会失败（错误 32）。
            drop(writer);
            let _ = tokio::fs::remove_file(&path).await;
            crate::log::warn("← 下载已取消（插件正在停止）");
            return Err(HttpError("下载已取消".to_owned()));
        }
        match source.next_chunk().await {
            Ok(None) => break,
            Ok(Some(chunk)) => {
                if let Err(error) = writer.write_all(&chunk).await {
                    drop(writer);
                    let _ = tokio::fs::remove_file(&path).await;
                    return Err(HttpError(format!("写缓存文件失败：{error}")));
                }
                written += chunk.len() as u64;
            }
            Err(message) => {
                drop(writer);
                let _ = tokio::fs::remove_file(&path).await;
                crate::log::error(&format!("← 下载中断，已删半截文件：{message}"));
                return Err(HttpError(format!("下载中断：{message}")));
            }
        }
    }
    // ⚠️ `BufWriter` 的 `Drop` **忽略** flush 的错误——磁盘满 / 配额到头只会静默丢数据，
    // 所以必须显式 flush 并处理它。
    if let Err(error) = writer.flush().await {
        drop(writer);
        let _ = tokio::fs::remove_file(&path).await;
        return Err(HttpError(format!("写缓存文件失败：{error}")));
    }
    drop(writer);

    if written == 0 {
        let _ = tokio::fs::remove_file(&path).await;
        crate::log::error("← 下载下来是空文件");
        return Err(HttpError("下载下来是空文件".to_owned()));
    }
    Ok((path, written))
}

/// 建一个**全新的**缓存文件（不覆盖任何已存在的），撞名就换名字重来。
///
/// 用 `create_new`：文件已存在就直接失败，不覆盖别人正在读的那个。
/// 覆盖一个 TDLib 正打开的文件在 Windows 上正是 ACCESS_DENIED 的来源。
async fn reserve_unique(
    dir: &Path,
    url: &str,
    extension: &str,
) -> Result<(PathBuf, tokio::fs::File), String> {
    let mut last = String::from("未知错误");
    for attempt in 0..UNIQUE_ATTEMPTS {
        // 第一次用 `cache_name` 的原样；之后追加一个序号，保证真的换了个名字。
        let name = match attempt {
            0 => cache_name(url, extension),
            n => with_suffix(&cache_name(url, extension), extension, n),
        };
        let path = dir.join(&name);
        match tokio::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .await
        {
            Ok(file) => return Ok((path, file)),
            Err(error) => last = format!("建 {} 失败：{error}", path.display()),
        }
        // 换个名字再试（时间戳会变，序号也会变）
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    Err(last)
}

/// 在扩展名之前插入 `-<序号>`：`file_123.mp4` → `file_123-2.mp4`。
fn with_suffix(name: &str, extension: &str, index: usize) -> String {
    match name.strip_suffix(&format!(".{extension}")) {
        Some(stem) => format!("{stem}-{index}.{extension}"),
        None => format!("{name}-{index}"),
    }
}

/// 缓存文件名：照 TS 版 `downloadFile.ts` 的规则。
///
/// 1. 取 URL **路径**的最后一段（`new URL(url).pathname` 的 basename）；
/// 2. Windows 文件名非法字符替换成 `_`（抖音的图片 URL 里带 `:540:q75`
///    这种变换后缀，不换会写盘失败）；
/// 3. 末尾的点和空格去掉；
/// 4. 名字空了就用时间戳兜底。
///
/// 扩展名**另给**（TS 版直接用 URL 里的，而抖音的视频 URL 未必带 `.mp4`，
/// 且 TDLib 要靠它判 MIME）。
fn cache_name(url: &str, extension: &str) -> String {
    let path = url.split("://").nth(1).unwrap_or(url);
    let path = path.split('?').next().unwrap_or(path);
    let path = path.split('#').next().unwrap_or(path);
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
    // TS 版：`replace(/[. ]+$/g, "")` —— 去掉末尾的点和空格。
    let trimmed = cleaned.trim_end_matches(['.', ' ']);

    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    if trimmed.is_empty() {
        return format!("file_{stamp}.{extension}");
    }
    // 名字里已经有这个扩展名就别再追加一层：`v0200fg.mp4` 不该变成
    // `v0200fg.mp4.mp4`（TS 版直接用 URL 里的扩展名，天然不会重）。
    if trimmed
        .rsplit('.')
        .next()
        .is_some_and(|tail| tail.eq_ignore_ascii_case(extension))
    {
        return trimmed.to_owned();
    }
    format!("{trimmed}.{extension}")
}

/// 取一个响应头（`reqwest::header::HeaderMap` 自己大小写不敏感）。
fn header_of(response: &reqwest::Response, name: &str) -> Option<String> {
    response
        .headers()
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned)
}

/// 把正文读成 `String`。分享页是 UTF-8 的 HTML，读不成串就没有意义了。
///
/// ⚠️ 这里**必须**是「读完整页再还」而不是流式：`_ROUTER_DATA` 在 `<script>` 里，
/// 只读半页可能正好切在 JSON 中间。15 秒的超时已经把体积上限兜住了。
async fn read_body(response: reqwest::Response) -> String {
    // 正文读失败不该让整条消息发不出去——退成空串，让上层报「没有数据」。
    response.text().await.unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 缓存目录 = 插件目录的上一级下的 `cache/`。
    /// 插件目录就是项目里的 `plugins/`，所以上一级正是宿主的工作目录——
    /// 这与 TS 版 `path.resolve(process.cwd(), "cache")` 是同一处。
    #[test]
    fn cache_dir_sits_next_to_the_plugins_dir() {
        assert_eq!(
            cache_dir(Path::new("D:/bots/demo/plugins")),
            PathBuf::from("D:/bots/demo/cache")
        );
        // 没有父目录（根）时退回相对路径，不能 panic
        assert_eq!(cache_dir(Path::new("/")), PathBuf::from("cache"));
    }

    /// 文件名规则照搬 TS 版：取 URL 路径最后一段，非法字符换 `_`。
    #[test]
    fn cache_name_takes_the_url_basename() {
        assert_eq!(
            cache_name("https://x.com/a/b/v0200fg.mp4?x=1", "mp4"),
            "v0200fg.mp4"
        );
        assert_eq!(
            cache_name("https://x.com/a/b/cover.jpeg", "jpg"),
            "cover.jpeg.jpg"
        );
    }

    /// ⚠️ 抖音的视频 URL 是 `…/play/?video_id=…`——** `play` 后面还有个斜杠**，
    /// 所以 URL 路径的最后一段其实是**空的**（`path.posix.basename("/aweme/v1/play/")`
    /// 得到 `""`）。TS 版这时同样会走 `|| file_${Date.now()}` 兜底。
    /// 这个行为看着奇怪，但两边一致，别为了「好看」去改实现——改了就跟 TS 版对不上了。
    #[test]
    fn a_trailing_slash_leaves_no_basename() {
        let name = cache_name(
            "https://aweme.snssdk.com/aweme/v1/play/?video_id=abc",
            "mp4",
        );
        assert!(name.starts_with("file_"), "{name}");
        assert!(name.ends_with(".mp4"), "{name}");
    }

    /// 抖音的图片 URL 带 `:540:q75` 这种变换后缀——冒号在 Windows 文件名里非法，
    /// 不换会写盘失败（TS 版就是为这个才加的替换）。
    ///
    /// 注意 `~` **不在**替换表里：TS 版只换 `<>:"/\\|?*` 和控制字符，`~` 是合法的。
    #[test]
    fn illegal_windows_characters_are_replaced() {
        assert_eq!(
            cache_name("https://p3.douyinpic.com/img/obj~540:q75.jpeg", "jpg"),
            "obj~540_q75.jpeg.jpg",
            "冒号换掉，波浪号保留（TS 版同）"
        );
        // 反斜杠在 Rust 字符字面量里要写成 '\\'
        for bad in ['<', '>', '"', '/', '\\', '|', '?', '*'] {
            let name = cache_name(&format!("https://x.com/a{bad}b"), "mp4");
            assert!(!name.contains(bad), "{bad} 应被替换：{name}");
        }
    }

    /// TS 版末尾那条 `replace(/[. ]+$/g, "")`：末尾的点和空格要去掉，
    /// 否则 `abc..mp4` 这种名字在 Windows 上会被当成怪扩展名。
    #[test]
    fn trailing_dots_and_spaces_are_stripped() {
        assert_eq!(cache_name("https://x.com/abc..", "mp4"), "abc.mp4");
        assert_eq!(cache_name("https://x.com/abc  ", "mp4"), "abc.mp4");
    }

    /// 路径里取不出名字时（比如 URL 以 `/` 结尾）用时间戳兜底，不能叫 `.mp4`。
    #[test]
    fn an_empty_basename_falls_back_to_a_timestamp() {
        let name = cache_name("https://x.com/", "mp4");
        assert!(name.starts_with("file_"), "{name}");
        assert!(name.ends_with(".mp4"), "{name}");
        assert!(name.len() > "file_.mp4".len(), "时间戳不能是空的：{name}");
    }

    /// 扩展名必须干净地落在末尾——TDLib 靠它判 MIME，
    /// 之前 `set_extension("mp4.<时间戳>")` 让发送失败过。
    #[test]
    fn the_extension_is_clean_and_last() {
        for (url, ext) in [
            ("https://x.com/a/b/play", "mp4"),
            ("https://x.com/a/b/cover", "jpg"),
        ] {
            let name = cache_name(url, ext);
            assert!(
                name.ends_with(&format!(".{ext}")),
                "{name} 应以 .{ext} 结尾"
            );
            // 末尾扩展名之后不能再有别的点
            let after = name.rsplit_once('.').map(|(_, tail)| tail).unwrap_or("");
            assert_eq!(after, ext, "{name} 的扩展名应该就是 {ext}");
        }
    }

    #[test]
    fn kind_maps_to_a_real_extension() {
        assert_eq!(Kind::Video.extension(), "mp4");
        assert_eq!(Kind::Image.extension(), "jpg");
    }

    /// 撞名重试的序号插在扩展名**之前**：`file_1.mp4` → `file_1-2.mp4`。
    /// 顺序反了（变成 `.mp4-2`）TDLib 就认不出这是视频了。
    #[test]
    fn the_retry_suffix_goes_before_the_extension() {
        assert_eq!(with_suffix("file_1.mp4", "mp4", 2), "file_1-2.mp4");
        assert_eq!(with_suffix("cover.jpg", "jpg", 3), "cover-3.jpg");
        // 没有预期扩展名时至少别把名字搞坏
        assert_eq!(with_suffix("weird", "mp4", 1), "weird-1");
    }

    // ── 流式落盘的假流（不联网）──────────────────────────────────

    /// 一次吐出 `total` 字节（分 `chunk` 大小的块），内容全是 `byte`。
    struct Feed {
        left: u64,
        chunk: usize,
        byte: u8,
    }

    impl ChunkSource for Feed {
        async fn next_chunk(&mut self) -> Result<Option<Vec<u8>>, String> {
            if self.left == 0 {
                return Ok(None);
            }
            let n = (self.left as usize).min(self.chunk);
            self.left -= n as u64;
            Ok(Some(vec![self.byte; n]))
        }
    }

    /// 一个字都不吐。
    struct Empty;

    impl ChunkSource for Empty {
        async fn next_chunk(&mut self) -> Result<Option<Vec<u8>>, String> {
            Ok(None)
        }
    }

    /// 先吐 1 MiB，然后报错——比真的掐网络好造。
    struct Broken(u64);

    impl ChunkSource for Broken {
        async fn next_chunk(&mut self) -> Result<Option<Vec<u8>>, String> {
            if self.0 == 0 {
                return Err("模拟 CDN 掐连接".to_owned());
            }
            let n = (self.0 as usize).min(64 * 1024);
            self.0 -= n as u64;
            Ok(Some(vec![b'x'; n]))
        }
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("dy-{tag}-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        dir
    }

    fn leftover(dir: &Path) -> Vec<PathBuf> {
        std::fs::read_dir(dir)
            .map(|entries| entries.flatten().map(|e| e.path()).collect())
            .unwrap_or_default()
    }

    /// 12 MiB 的流必须完整落盘：**这条是「大视频下不下来」的核心回归测试**
    /// （旧实现走 ureq 的 `read_to_vec`，写死 10 MiB 上限）。
    #[tokio::test]
    async fn a_12_mib_stream_lands_on_disk_completely() {
        const SIZE: u64 = 12 * 1024 * 1024;
        let dir = temp_dir("big");
        let cancel = AtomicBool::new(false);
        let mut source = Feed {
            left: SIZE,
            chunk: 64 * 1024,
            byte: 0,
        };
        let (path, written) =
            write_stream(&dir, "https://x.com/a/big.mp4", "mp4", &mut source, &cancel)
                .await
                .expect("大文件必须写得下来");
        assert_eq!(written, SIZE, "写入字节数要跟源一样多");
        let on_disk = std::fs::metadata(&path).expect("文件应当真的存在").len();
        assert_eq!(on_disk, SIZE, "落盘大小必须完整，不能被截断");
        assert!(path.extension().is_some_and(|ext| ext == "mp4"));

        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir(&dir);
    }

    /// 空的响应体不能留下一个 0 字节的 `.mp4`：那会让 TDLib 报「文件损坏」，
    /// 比直接说「下载下来是空文件」难查得多。
    #[tokio::test]
    async fn an_empty_stream_leaves_no_file_behind() {
        let dir = temp_dir("empty");
        let cancel = AtomicBool::new(false);
        let error = write_stream(
            &dir,
            "https://x.com/a/empty.mp4",
            "mp4",
            &mut Empty,
            &cancel,
        )
        .await
        .expect_err("空正文必须报错");
        assert!(error.0.contains("空文件"), "{}", error.0);
        assert!(leftover(&dir).is_empty(), "空下载不该留下文件");
        let _ = std::fs::remove_dir(&dir);
    }

    /// 读到一半断了（模拟 CDN 掐连接）：半截文件必须删掉，不能留在缓存里。
    ///
    /// 留着的后果是下一次发送拿到一个「长度不对的 mp4」，
    /// 报错变成 TDLib 的「文件损坏」，与本来的网络问题对不上。
    #[tokio::test]
    async fn a_broken_stream_deletes_the_partial_file() {
        let dir = temp_dir("broken");
        let cancel = AtomicBool::new(false);
        let error = write_stream(
            &dir,
            "https://x.com/a/broken.mp4",
            "mp4",
            &mut Broken(1024 * 1024),
            &cancel,
        )
        .await
        .expect_err("中断的流必须报错");
        assert!(error.0.contains("下载中断"), "{}", error.0);
        assert!(leftover(&dir).is_empty(), "半截文件必须被删掉");
        let _ = std::fs::remove_dir(&dir);
    }

    /// 取消标志置位后，下载必须在**块边界**收手并删掉半截文件——
    /// `stop()` 不能等一个几百 MB 的下载跑完才返回。
    #[tokio::test]
    async fn a_cancelled_download_stops_between_chunks() {
        let dir = temp_dir("cancel");
        let cancel = AtomicBool::new(true);
        let error = write_stream(
            &dir,
            "https://x.com/a/cancel.mp4",
            "mp4",
            &mut Feed {
                left: 30 * 1024 * 1024,
                chunk: 64 * 1024,
                byte: b'z',
            },
            &cancel,
        )
        .await
        .expect_err("取消后必须立刻放弃");
        assert!(error.0.contains("取消"), "{}", error.0);
        assert!(leftover(&dir).is_empty(), "取消的半截文件必须删掉");
        let _ = std::fs::remove_dir(&dir);
    }

    /// 撞名时换名字重试，而不是覆盖别人正在读的文件。
    #[tokio::test]
    async fn a_second_download_does_not_overwrite_the_first() {
        let dir = temp_dir("collide");
        let cancel = AtomicBool::new(false);
        let mut first_src = Feed {
            left: 3,
            chunk: 8,
            byte: b'a',
        };
        let (first, _) = write_stream(
            &dir,
            "https://x.com/a/v0200.mp4",
            "mp4",
            &mut first_src,
            &cancel,
        )
        .await
        .expect("第一次应当写成功");
        let mut second_src = Feed {
            left: 2,
            chunk: 8,
            byte: b'b',
        };
        // 形状完全相同的 URL：文件名会撞（`cache_name` 带毫秒时间戳，这里刻意
        // 用同一秒内的两次调用来逼近撞名场景；真撞了就必须换名字重试）。
        let (second, _) = write_stream(
            &dir,
            "https://x.com/a/v0200.mp4",
            "mp4",
            &mut second_src,
            &cancel,
        )
        .await
        .expect("撞名后应当换名重试成功");
        assert_ne!(first, second, "不能写回同一个文件");
        assert_eq!(std::fs::read(&first).unwrap(), b"aaa");
        assert_eq!(std::fs::read(&second).unwrap(), b"bb");

        let _ = std::fs::remove_file(&first);
        let _ = std::fs::remove_file(&second);
        let _ = std::fs::remove_dir(&dir);
    }
}
