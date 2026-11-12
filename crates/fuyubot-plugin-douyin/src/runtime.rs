//! `Runtime`：插件 `start` 之后活到最后的那一份共享状态 + tokio 执行引擎。
//!
//! 它把这几样东西钉在一起——宿主句柄、响应路由器、HTTP 客户端、cookie、
//! 下载目录、SQL 缓存、**tokio 多线程执行引擎**——所有回调拿到的都是它的 `Arc`，
//! 谁都不用再层层传参。
//!
//! ## 线程模型（先读这一段）
//!
//! 宿主的事件 / 命令回调跑在**接收循环线程**上，那里只做一件事：
//! 克隆参数 + [`Runtime::submit`] 一个任务。所有网络请求（`reqwest`）、
//! TDLib 往返（`ResponseRouter` 的 future + `tokio::time::timeout`）、
//! 缓存读写（`spawn_blocking`，SQLite 是同步的）都在 tokio 工作任务里跑。
//!
//! 这取代了旧模型「每条命令 `std::thread::spawn` 一个线程、`stop()` 挨个 join」：
//! 线程数固定（[`WORKERS`] 个），而且 await 不占线程。
//!
//! ## 生命周期与停机顺序（动态库卸载安全）
//!
//! ```text
//! plugin_start → Runtime::new ─┬─ tokio 多线程 runtime（WORKERS 个 douyin-worker 线程）
//!                              ├─ ResponseRouter（响应按 request_id 路由回 future）
//!                              ├─ HTTP Agent（两个 reqwest Client 复用连接池）
//!                              └─ Cache（软依赖 fuyubot.sqlite，异步 init）
//! plugin_stop  → drop 订阅守卫 → 置取消标志 → 关闸并等在飞归零 → rt.shutdown_timeout
//! ```
//!
//! 任务里跑的全是本动态库里的代码，**卸载前不能有任何线程还在跑**。所以 `stop()`：
//!
//! 1. 先 drop 订阅守卫（不再收新事件）；
//! 2. 置 [`Runtime::cancel`]：在飞下载在**块边界**看到它就删掉半截文件收手；
//! 3. [`Engine::shutdown`] 关闸（新任务直接丢弃）+ 等在飞任务归零（[`DRAIN_TIMEOUT`]）；
//! 4. `shutdown_timeout(STOP_TIMEOUT)` 收掉 runtime。
//!
//! 在飞等待有上限：正常停机在飞为 0，立刻返回；最坏情况是有一个卡住的下载，
//! 等到 [`DRAIN_TIMEOUT`] 之后 runtime 被 drop——纯 async 任务 drop 即取消（安全），
//! 剩下的只可能是极短的 SQLite `spawn_blocking`。

use std::future::Future;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock, Weak};
use std::time::{Duration, Instant};

use fuyubot_abi::{HostApi, ResponseRouter};
use serde_json::{Value, json};

use crate::cache::Cache;
use crate::http;

/// 工作线程数。解析一条作品最多两个 HTTP 往返 + 一次发送等待，
/// 4 个够「几群人同时贴链接」时并行推进，又不会把抖音侧打得太狠。
const WORKERS: usize = 4;

/// `shutdown` 等在飞任务归零的上限。
///
/// 正常停机时在飞为 0，立刻返回；有卡住的下载时最多等这么久。
/// 超过之后 runtime 被 drop：纯 async 任务 drop 即取消，不会把线程留在库里。
const DRAIN_TIMEOUT: Duration = Duration::from_secs(30);

/// tokio runtime 收摊的上限：此时剩下的只可能是纯 async 任务，drop 即取消（安全）。
const STOP_TIMEOUT: Duration = Duration::from_secs(5);

/// 一次 op / TDLib 调用的错误。文案已经能直接进日志或给用户看。
#[derive(Debug)]
pub struct CallError(pub String);

impl std::fmt::Display for CallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for CallError {}

/// 插件运行期共享状态。
pub struct Runtime {
    pub host: HostApi,
    /// 宿主 ABI v5 的 async 响应路由：只用来 `invoke_op` 发 simplify op 并 await 结果。
    ///
    /// 它还有 `invoke_simplified`（直接发 TDLib 方法）的能力，但本插件**不用**了——
    /// 那个入口当年只为 `getChat` 判群聊服务；引用行为删掉后就不需要它了。
    pub router: ResponseRouter,
    pub http: http::Agent,
    /// cookie 整串（`cookies.json` 里那个 `cookies` 字段）；没有就是空串。
    pub cookie: String,
    /// 下载落盘目录：宿主工作目录下的 `cache/`（由 `plugins_dir` 推出）。
    pub cache_dir: PathBuf,
    /// SQL 缓存（软依赖 `fuyubot.sqlite`；没装时永远未命中）。
    pub cache: Cache,
    /// 缓存初始化的串行锁 + 「已经尝试过」标志。
    ///
    /// `start()` 里的 init 是异步任务，而**第一条命令可能比它先到**；不串行化的话
    /// 第一次查缓存会因为「表还没建好」而必然 miss（白丢一次命中）。这把锁保证
    /// 并发调用只有一个真跑 migrate，另一个等它跑完再查。
    cache_init_lock: tokio::sync::Mutex<()>,
    cache_tried: AtomicBool,
    /// 取消标志：`stop()` 置位，流式下载在块间检查。
    pub cancel: Arc<AtomicBool>,
    /// spawn 用的句柄（`Handle::spawn` 在同步上下文里也能用）。
    handle: tokio::runtime::Handle,
    engine: Arc<Engine>,
    /// 指向自己的弱引用：`submit` 只需要 `&self` 就能升级出 `Arc<Runtime>`。
    me: OnceLock<Weak<Runtime>>,
}

impl Runtime {
    /// 从 `plugin_start` 的 `config_json` 起一份运行时。
    ///
    /// `config_json` 由宿主给，形状是 `{"plugins_dir": "...", "scripts_dir": "...", "spawn": true}`。
    /// 插件是动态库，`current_exe()` 拿到的是宿主 exe，**算不出自己旁边有什么**，只能靠它。
    pub fn new(host: HostApi, config_json: &str) -> Result<Arc<Self>, String> {
        let plugins_dir = plugins_dir_from(config_json);
        // cookie 读不到不该让整个插件起不来——照常加载，用到时再报给用户。
        // 用户换 cookie 只要重启宿主，不必重编译、也不必改配置。
        let cookie = match crate::parse::read_cookies(host, &plugins_dir) {
            Ok(cookie) => cookie,
            Err(error) => {
                crate::log::warn(&format!("暂时没有 cookie，用到时会报错：{error}"));
                String::new()
            }
        };
        let cache_dir = http::cache_dir(&plugins_dir);
        crate::log::debug(&format!("缓存目录 {}", cache_dir.display()));

        // 响应路由器必须在插件 `start` 里建：宿主作用域栈顶此刻是本插件，
        // `invoke*` 发出的请求才能归属到本插件名下。
        let router =
            ResponseRouter::new(host).map_err(|error| format!("建响应路由器失败：{error}"))?;

        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(WORKERS)
            .thread_name("douyin-worker")
            .enable_all()
            .build()
            .map_err(|error| format!("起 tokio 运行时失败：{error}"))?;
        let handle = rt.handle().clone();
        let engine = Engine::wrap(rt);

        let http = http::agent().map_err(|error| error.0)?;

        let runtime = Arc::new(Self {
            host,
            router,
            http,
            cookie,
            cache_dir,
            cache: Cache::new(host),
            cache_init_lock: tokio::sync::Mutex::new(()),
            cache_tried: AtomicBool::new(false),
            cancel: Arc::new(AtomicBool::new(false)),
            handle,
            engine,
            me: OnceLock::new(),
        });
        // 弱引用只在这里填一次；此后 `submit` 能从 `&self` 升级出 Arc。
        let _ = runtime.me.set(Arc::downgrade(&runtime));
        Ok(runtime)
    }

    /// `&self` → `Arc<Runtime>`（`new` 之后一定成功）。
    fn arc(&self) -> Option<Arc<Runtime>> {
        self.me.get().and_then(Weak::upgrade)
    }

    /// 把一个异步任务丢进执行引擎。闸门已关（停机中）就直接丢弃。
    ///
    /// 调用点形如 `rt.submit(move |rt| async move { … })`；宿主的事件回调里
    /// 只该做「克隆参数 + submit」，真正的活全在任务里。
    pub fn submit<Fut>(&self, job: impl FnOnce(Arc<Runtime>) -> Fut + Send + 'static)
    where
        Fut: Future<Output = ()> + Send + 'static,
    {
        let Some(runtime) = self.arc() else {
            return;
        };
        let Some(ticket) = self.engine.enter() else {
            return;
        };
        let handle = self.handle.clone();
        handle.spawn(async move {
            let _ticket = ticket;
            job(runtime).await;
        });
    }

    /// 停机：取消标志 → 关闸 → 等在飞归零 → 关 runtime（见模块文档的顺序说明）。
    pub fn shutdown(&self) {
        self.cancel.store(true, Ordering::SeqCst);
        self.engine.shutdown();
    }

    // ── TDLib / simplify 调用 ───────────────────────────────────────

    /// 下发一条 **simplify op** 并等它的 `simplifyResult`（async，不占线程）。
    ///
    /// 走 `ResponseRouter::invoke_op`：内部先发后登记（响应比登记快的时候靠小缓冲兜住），
    /// 并在插件自己的 executor 线程上自动切回本插件的 owner——所以从 tokio 任务里发
    /// 也能把结果收回来。
    ///
    /// 成功返回 `result` 字段；`ok:false` 变成 [`CallError`]（`code: message`）。
    /// 等待预算比 op 自己的 `timeout_ms` 宽 5 秒，确保能接住「宿主侧超时」那个结果
    /// 而不是自己先超时。
    pub async fn invoke_op(
        &self,
        op: &str,
        args: Value,
        timeout_ms: u64,
    ) -> Result<Value, CallError> {
        let op_json = json!({ "op": op, "args": args }).to_string();
        let (_id, future) = self
            .router
            .invoke_op(&op_json)
            .map_err(|error| CallError(format!("下发 {op} 失败：{error}")))?;
        let response = wait(future, timeout_ms.saturating_add(5_000)).await?;
        if response.get("ok").and_then(Value::as_bool) == Some(true) {
            return Ok(response.get("result").cloned().unwrap_or(Value::Null));
        }
        let code = response
            .pointer("/error/code")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        let message = response
            .pointer("/error/message")
            .and_then(Value::as_str)
            .unwrap_or("");
        Err(CallError(format!("{code}: {message}")))
    }

    /// 发一条纯文本（报错误用）。失败只记日志：这是**已经出错之后**的通知，
    /// 再抛错也没有人接。
    ///
    /// ⚠️ **不引用任何消息**（用户要求去掉引用行为）：只发 `chat_id` + 文本。
    pub async fn say(&self, chat_id: i64, text: &str) {
        let args = json!({
            "chat_id": chat_id,
            "input_message_content": {
                "@type": "inputMessageText",
                "text": crate::simplify::text_json(&crate::simplify::Text::markdown(text)),
            },
        });
        if let Err(error) = self
            .invoke_op("send_message", args, crate::send::TIMEOUT_MS)
            .await
        {
            crate::log::error(&format!("回复失败：{error}"));
        }
    }

    // ── SQL 缓存（软依赖，全部丢 spawn_blocking）────────────────────

    /// 初始化缓存（幂等）：探测 `fuyubot.sqlite` + 幂等建表。
    ///
    /// `start` 里异步调一次；**第一次用到缓存时也会补一次**——命令可能比 start 的
    /// init 任务先到，不补的话第一次命中会被白白错过。并发调用只有第一个真跑 migrate。
    /// 返回是否可用；**失败不影响插件启动**——只是没有加速。
    pub async fn init_cache(&self) -> bool {
        let _guard = self.cache_init_lock.lock().await;
        if self.cache_tried.load(Ordering::SeqCst) {
            return self.cache.available();
        }
        let cache = self.cache.clone();
        let available = match tokio::task::spawn_blocking(move || cache.init()).await {
            Ok(available) => available,
            Err(error) => {
                crate::log::warn(&format!("缓存初始化任务被打断：{error}"));
                false
            }
        };
        self.cache_tried.store(true, Ordering::SeqCst);
        if available {
            crate::log::info("SQL 缓存已启用（命中后按文件 id 直接发送，不再解析/下载）");
        } else {
            crate::log::info("未装 fuyubot.sqlite，缓存禁用（不影响解析，只是没有加速）");
        }
        available
    }

    /// 第一次用到缓存时，确保启动期的异步 init 已经跑过（幂等、已试过就直接返回）。
    async fn ensure_cache(&self) {
        if self.cache.available() || self.cache_tried.load(Ordering::SeqCst) {
            return;
        }
        let _ = self.init_cache().await;
    }

    /// 读一条缓存；没有 / 不可用 / 失败都是 `None`。
    pub async fn cache_get(&self, key: &str) -> Option<Value> {
        self.ensure_cache().await;
        if !self.cache.available() {
            return None;
        }
        let cache = self.cache.clone();
        let key = key.to_owned();
        match tokio::task::spawn_blocking(move || cache.get(&key)).await {
            Ok(value) => value,
            Err(error) => {
                crate::log::warn(&format!("读缓存任务被打断：{error}"));
                None
            }
        }
    }

    /// 写一条缓存。失败只记 Debug：缓存是加速，写不进去不该影响发送结果。
    pub async fn cache_put(&self, key: &str, doc: &Value) {
        self.ensure_cache().await;
        if !self.cache.available() {
            return;
        }
        let cache = self.cache.clone();
        let key = key.to_owned();
        let doc = doc.clone();
        let logged = key.clone();
        match tokio::task::spawn_blocking(move || cache.put(&key, &doc)).await {
            Ok(()) => crate::log::debug(&format!("缓存已写入 {logged}")),
            Err(error) => crate::log::debug(&format!("写缓存任务被打断：{error}")),
        }
    }

    /// 删一条缓存（命中的文件 id 失效时用）。
    pub async fn cache_remove(&self, key: &str) {
        self.ensure_cache().await;
        if !self.cache.available() {
            return;
        }
        let cache = self.cache.clone();
        let key = key.to_owned();
        if let Err(error) = tokio::task::spawn_blocking(move || cache.remove(&key)).await {
            crate::log::debug(&format!("删缓存任务被打断：{error}"));
        }
    }
}

/// 等一个 `ResponseFuture`，超时统一成 [`CallError`]。
async fn wait(
    future: impl Future<Output = Result<Value, fuyubot_abi::AbiError>>,
    timeout_ms: u64,
) -> Result<Value, CallError> {
    match tokio::time::timeout(Duration::from_millis(timeout_ms), future).await {
        Err(_elapsed) => Err(CallError("等不到 TDLib 响应（超时）".to_owned())),
        Ok(Err(error)) => Err(CallError(format!("响应通道已关闭：{error}"))),
        Ok(Ok(response)) => Ok(response),
    }
}

/// 宿主给的插件目录；没给就退回 `plugins/`（与内置 `manage` 插件同款约定）。
pub fn plugins_dir_from(config_json: &str) -> PathBuf {
    serde_json::from_str::<Value>(config_json)
        .ok()
        .and_then(|config| config.get("plugins_dir")?.as_str().map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("plugins"))
}

// ── 执行引擎（照搬 `fuyubot-plugin-adblock` 的 Engine）────────────

/// 执行引擎：tokio runtime 的宿主 + 「闸门 + 在飞计数」。
struct Engine {
    state: Mutex<State>,
    /// `inflight` 归零时唤醒 [`Engine::shutdown`] 的等待。
    idle: Condvar,
}

struct State {
    /// `shutdown` 时 `take()` 出来收摊。
    rt: Option<tokio::runtime::Runtime>,
    /// 闸门：关了以后新任务直接丢弃。
    open: bool,
    /// 在飞任务数（`submit` 各计一个）。
    inflight: usize,
}

/// 在飞计数的 RAII：持有期间 `inflight` 不归零；drop（含任务被取消）时减一。
struct Ticket(Arc<Engine>);

impl Drop for Ticket {
    fn drop(&mut self) {
        let mut state = lock(&self.0.state);
        state.inflight = state.inflight.saturating_sub(1);
        if state.inflight == 0 {
            self.0.idle.notify_all();
        }
    }
}

impl Engine {
    fn wrap(rt: tokio::runtime::Runtime) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(State {
                rt: Some(rt),
                open: true,
                inflight: 0,
            }),
            idle: Condvar::new(),
        })
    }

    /// 进闸门（闸门 + 在飞计数一步做完，与 `shutdown` 的关闸在同一把锁里互斥）。
    /// 闸门已关返回 `None`：调用方把 future 丢掉就是「取消」。
    fn enter(self: &Arc<Self>) -> Option<Ticket> {
        let mut state = lock(&self.state);
        if !state.open {
            return None;
        }
        state.inflight += 1;
        Some(Ticket(Arc::clone(self)))
    }

    /// 关闸 → 等在飞归零（限时）→ 关 runtime。
    fn shutdown(&self) {
        let mut state = lock(&self.state);
        state.open = false;
        let deadline = Instant::now() + DRAIN_TIMEOUT;
        while state.inflight > 0 {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                crate::log::warn(&format!(
                    "停机时有 {} 个任务没跑完，等满 {:?} 后强制收尾",
                    state.inflight, DRAIN_TIMEOUT
                ));
                break;
            }
            let (guard, _) = self
                .idle
                .wait_timeout(state, remaining)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            state = guard;
        }
        if let Some(rt) = state.rt.take() {
            drop(state);
            rt.shutdown_timeout(STOP_TIMEOUT);
        }
    }
}

/// 锁拿手顺：毒锁不值得让停机崩掉，接着用就行（状态是单调的计数与开关）。
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn plugins_dir_parsing() {
        assert_eq!(
            plugins_dir_from(r#"{"plugins_dir":"D:\\demo\\plugins"}"#),
            PathBuf::from(r"D:\demo\plugins")
        );
        assert_eq!(plugins_dir_from("{}"), PathBuf::from("plugins"));
        assert_eq!(plugins_dir_from("not json"), PathBuf::from("plugins"));
        assert_eq!(
            plugins_dir_from(r#"{"plugins_dir":""}"#),
            PathBuf::from(""),
            "空串也照收（宿主不该给，但别 panic）"
        );
    }

    /// 在飞任务必须跑完才许停机完（卸载安全的核心语义）。
    #[test]
    fn shutdown_waits_for_inflight_jobs() {
        let engine = Engine::wrap(
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_time()
                .build()
                .expect("tokio runtime"),
        );
        let done = Arc::new(AtomicUsize::new(0));
        for _ in 0..8 {
            let ticket = engine.enter().expect("闸门应该是开的");
            let done = Arc::clone(&done);
            engine_handle_spawn(&engine, async move {
                tokio::time::sleep(Duration::from_millis(20)).await;
                done.fetch_add(1, Ordering::SeqCst);
                drop(ticket);
            });
        }
        engine.shutdown();
        assert_eq!(done.load(Ordering::SeqCst), 8, "8 个在飞任务都该跑完");
    }

    /// 关闸之后新任务进不来。
    #[test]
    fn gate_drops_new_work_after_shutdown() {
        let engine = Engine::wrap(
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_time()
                .build()
                .expect("tokio runtime"),
        );
        engine.shutdown();
        assert!(engine.enter().is_none(), "关闸后不许再进");
    }

    /// 测试里借个 spawn 入口（生产路径走 `Runtime::handle`）。
    fn engine_handle_spawn(engine: &Arc<Engine>, fut: impl Future<Output = ()> + Send + 'static) {
        let state = lock(&engine.state);
        let rt = state.rt.as_ref().expect("runtime 还活着");
        rt.spawn(fut);
    }
}
