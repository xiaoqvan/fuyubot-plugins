//! `Runtime`：插件 `start` 之后活到最后的那一份共享状态。
//!
//! 它把这几样东西钉在一起——宿主句柄、TDLib 响应路由、HTTP 客户端、鉴权材料、
//! 下载目录候选、SQL 缓存、**tokio 执行引擎**——所有回调拿到的都是它的 `Arc`，
//! 谁都不用再层层传参。
//!
//! ## 生命周期
//!
//! ```text
//! plugin_start → Runtime::new ─┬─ tokio 多线程 runtime（WORKERS 个 music-worker 线程）
//!                              ├─ ResponseRouter（async 等响应，不再有 ResponseHub）
//!                              ├─ reqwest::Client（真 async HTTP，复用连接池）
//!                              └─ Cache（软依赖 fuyubot.sqlite；没装就是 no-op）
//! plugin_stop  → ① 退订 → ② 置取消标志 → ③ Runtime::shutdown（关闸 → 等在飞归零 → 关 runtime）
//! ```
//!
//! ## 线程模型（先读这一段）
//!
//! 宿主的 `on` / `on_command` 回调跑在**接收循环线程**上，那里只做一件事：
//! 把负载克隆出来 `submit` 成 tokio 任务。所有 HTTP（reqwest async）、磁盘、
//! TDLib 往返（`ResponseRouter` + `tokio::time::timeout`）、SQL 缓存读写
//! （`spawn_blocking`）都在工作任务里跑。**没有任何固定 worker 线程 /
//! `JobQueue` / `Condvar`**。
//!
//! ## 停机为什么是这个顺序（动态库卸载安全）
//!
//! 任务里跑的全是本动态库里的代码，**卸载前不能有任何线程还在跑**。所以：
//!
//! 1. **退订**：先断输入，避免「正在停机、新任务又进来」；
//! 2. **置取消标志**：下载的 `chunk()` 循环、解密循环每一步都看它，尽快退出；
//! 3. **关闸**：`submit` 的新任务直接丢弃（future 就地 drop）；
//! 4. **等在飞归零**（限时 [`DRAIN_TIMEOUT`]）：每接受一个任务就 +1、任务结束或被
//!    取消时 -1（RAII，任务被 drop 也算）；
//! 5. **关 runtime**：`shutdown_timeout([`STOP_TIMEOUT`])` 收摊，纯 async 任务被 drop
//!    即取消（安全）。
//!
//! 唯一允许的阻塞任务是留在 `spawn_blocking` 里的**短操作**：SQL 缓存读写（单条
//! SQLite 语句）与汽水 CENC 解密（循环里逐样本检查取消标志，`stop` 后很快返回）。
//! 因此第 4 步的等待上限可以只给 30 秒，而不是「最长下载时间」。

use std::future::Future;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock, Weak};
use std::time::{Duration, Instant};

use fuyubot_abi::{HostApi, ResponseRouter};
use serde_json::Value;

use crate::auth::{self, Auth};
use crate::cache::Cache;
use crate::http;

/// 工作线程数。HTTP 请求与 TDLib 等待都是 async 的，真正的并行度来自
/// 同时在飞的 future；4 是「几个群同时点歌」时的经验值。
const WORKERS: usize = 4;
/// `shutdown` 等在飞任务归零的上限。
///
/// 覆盖最长的阻塞操作（汽水 CENC 解密）：它在循环里逐样本检查取消标志，
/// `stop` 之后最多几十毫秒就会返回，30 秒是宽裕的保险。
const DRAIN_TIMEOUT: Duration = Duration::from_secs(30);
/// tokio runtime 收摊的上限：此时剩下的只可能是纯 async 任务，drop 即取消（安全）。
const STOP_TIMEOUT: Duration = Duration::from_secs(5);

/// 插件运行期共享状态。
pub struct Runtime {
    pub host: HostApi,
    /// 宿主的 async 响应路由（`start` 里建，归属本插件）。
    pub router: ResponseRouter,
    /// reqwest 客户端（连接池复用）。
    pub http: http::Agent,
    pub auth: Auth,
    /// 可写的缓存目录候选（按优先级）。
    pub dirs: Vec<PathBuf>,
    /// SQL 文件 id 缓存（软依赖；没装 sqlite 插件时所有方法都是 no-op）。
    pub cache: Cache,
    /// 缓存初始化的串行锁 + 「已经尝试过」标志。
    ///
    /// `start()` 里的 init 是异步任务，而**第一条命令可能比它先到**；不串行化的话
    /// 第一次查缓存会因为「表还没建好」而必然 miss（白丢一次命中）。这把锁保证
    /// 并发调用只有一个真跑 migrate，另一个等它跑完再查。
    cache_init_lock: tokio::sync::Mutex<()>,
    cache_tried: AtomicBool,
    /// `stop` 置位后，下载 / 解密 / 循环每一步都会尽快退出。
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
    /// `config_json` 由宿主给，形状是 `{"plugins_dir": "..."}`。插件是动态库，
    /// `current_exe()` 拿到的是宿主 exe，**算不出自己旁边有什么**，只能靠它。
    pub fn new(host: HostApi, config_json: &str) -> Result<Arc<Self>, String> {
        // 宿主可能给相对路径（`--plugin-dir plugins`），先按**插件进程的当前目录**
        // 落成绝对路径：工作任务里再拼缓存目录，不能再依赖 cwd 没变过。
        let mut plugins_dir = plugins_dir_from(config_json);
        if plugins_dir.is_relative()
            && let Ok(cwd) = std::env::current_dir()
        {
            plugins_dir = cwd.join(plugins_dir);
        }
        crate::log::debug(&format!("插件目录：{}", plugins_dir.display()));

        // 鉴权材料：优先 `fuyubot.toml` 的 `[plugin.fuyubot.music]`，没配才回退旧文件。
        let auth = auth::load(host, &plugins_dir);

        // 响应路由器必须在插件 `start` 里建：宿主作用域栈顶此刻是本插件，
        // `invoke` 发出的请求才能归属到本插件名下。
        let router =
            ResponseRouter::new(host).map_err(|error| format!("建响应路由器失败：{error}"))?;

        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(WORKERS)
            .thread_name("music-worker")
            .enable_all()
            .build()
            .map_err(|error| format!("起 tokio 运行时失败：{error}"))?;
        let handle = rt.handle().clone();
        let engine = Engine::wrap(rt);

        let runtime = Arc::new(Self {
            host,
            router,
            http: http::agent(),
            auth,
            dirs: http::candidate_dirs(&plugins_dir),
            cache: Cache::new(host),
            cache_init_lock: tokio::sync::Mutex::new(()),
            cache_tried: AtomicBool::new(false),
            cancel: Arc::new(AtomicBool::new(false)),
            handle,
            engine,
            me: OnceLock::new(),
        });
        // 弱引用只在这里填一次；此后 `submit` 都能从 `&self` 升级出 Arc。
        let _ = runtime.me.set(Arc::downgrade(&runtime));
        Ok(runtime)
    }

    /// `&self` → `Arc<Runtime>`（`new` 之后一定成功）。
    pub(crate) fn arc(&self) -> Option<Arc<Runtime>> {
        self.me.get().and_then(Weak::upgrade)
    }

    /// 把一个异步任务丢进执行引擎。闸门已关（停机中）就直接丢弃。
    ///
    /// 调用点形如 `rt.submit(move |rt| async move { … })`；宿主的事件回调里
    /// 只该做「克隆更新 + submit」，真正的活全在任务里。
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

    /// 插件是否正在停止（下载 / 解密循环里按块检查）。
    pub fn stopping(&self) -> bool {
        self.cancel.load(Ordering::SeqCst)
    }

    /// 在插件自己的 tokio runtime 上跑一个 future 并等它结束。
    ///
    /// 只给**同步入口 / 测试**用（生产路径全在 `submit` 的任务里）；不能在
    /// tokio 上下文里调用（`Handle::block_on` 会 panic）。
    pub fn block_on<F: Future>(&self, future: F) -> F::Output {
        self.handle.block_on(future)
    }

    /// 第一个能建出来的缓存目录。
    ///
    /// `create_dir_all` 是短磁盘操作，留在调用方任务里直接做；真正的长 IO
    /// （下载 / 解密）才需要 async / `spawn_blocking`。
    pub fn cache_dir(&self) -> Result<PathBuf, String> {
        let mut last = String::from("没有候选目录");
        for dir in &self.dirs {
            match std::fs::create_dir_all(dir) {
                Ok(()) => {
                    crate::log::debug(&format!("缓存目录：{}", dir.display()));
                    return Ok(dir.clone());
                }
                Err(error) => {
                    crate::log::debug(&format!("缓存目录不可用 {}：{error}", dir.display()));
                    last = format!("{}：{error}", dir.display());
                }
            }
        }
        Err(format!("没有可写的缓存目录（{last}）"))
    }

    /// 下发一条 simplify op 并等 `simplifyResult`。
    ///
    /// `ResponseRouter` **先发后登记**：响应比 `invoke_op` 返回还早也塞进缓冲，
    /// 不会丢；超时用 `tokio::time::timeout`，`stop` 之后路由关闭会立刻返回。
    pub async fn send_op(&self, op_json: &str, timeout_ms: u64) -> Result<Value, String> {
        let (request, future) = self
            .router
            .invoke_op(op_json)
            .map_err(|error| format!("下发失败：{error}"))?;
        if !request.is_valid() {
            return Err("宿主返回无效请求号".to_owned());
        }
        let budget = Duration::from_millis(timeout_ms + 5_000);
        let result = tokio::time::timeout(budget, future)
            .await
            .map_err(|_| format!("等了 {} ms 没等到结果", budget.as_millis()))?
            .map_err(|error| format!("响应通道已关闭：{error}"))?;
        if result.get("ok").and_then(Value::as_bool) == Some(true) {
            return Ok(result.get("result").cloned().unwrap_or(Value::Null));
        }
        let code = result
            .pointer("/error/code")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        let message = result
            .pointer("/error/message")
            .and_then(Value::as_str)
            .unwrap_or("");
        Err(format!("发送失败（{code}）：{message}"))
    }

    // ── SQL 缓存（软依赖；读写在 spawn_blocking 里做）────────────────

    /// 初始化缓存（幂等）。`start` 里异步调一次；**第一次用到缓存时也会补一次**——
    /// 命令可能比 start 的 init 任务先到，不补的话第一次命中会被白白错过。
    pub async fn cache_init(&self) -> bool {
        // 双重检查 + 串行锁：并发调用里只有第一个真跑 migrate，其余等它跑完再看结果。
        let _guard = self.cache_init_lock.lock().await;
        if self.cache_tried.load(Ordering::SeqCst) {
            return self.cache.available();
        }
        let cache = self.cache.clone();
        let enabled = tokio::task::spawn_blocking(move || cache.init())
            .await
            .unwrap_or(false);
        self.cache_tried.store(true, Ordering::SeqCst);
        if enabled {
            crate::log::info("SQL 缓存已启用（fuyubot.sqlite）：命中后直接用文件 id 发送");
        } else {
            crate::log::info("未装 fuyubot.sqlite 或它不可用，缓存禁用（只影响加速，不影响发送）");
        }
        enabled
    }

    /// 第一次用到缓存时，确保启动期的异步 init 已经跑过（幂等、已试过就直接返回）。
    async fn ensure_cache(&self) {
        if self.cache.available() || self.cache_tried.load(Ordering::SeqCst) {
            return;
        }
        let _ = self.cache_init().await;
    }

    /// 读一条缓存文档；不可用 / 没命中 / 坏 JSON 都是 `None`。
    pub async fn cache_get(&self, key: &str) -> Option<Value> {
        self.ensure_cache().await;
        if !self.cache.available() {
            return None;
        }
        let cache = self.cache.clone();
        let key = key.to_owned();
        tokio::task::spawn_blocking(move || cache.get(&key))
            .await
            .ok()
            .flatten()
    }

    /// 写一条缓存文档。失败只记 debug——**缓存写入失败不能影响发送结果**。
    pub async fn cache_put(&self, key: &str, doc: &Value) {
        self.ensure_cache().await;
        if !self.cache.available() {
            return;
        }
        let cache = self.cache.clone();
        let key = key.to_owned();
        let doc = doc.clone();
        let written = tokio::task::spawn_blocking(move || {
            cache.put(&key, &doc);
            true
        })
        .await
        .unwrap_or(false);
        if !written {
            crate::log::debug("缓存写入任务没能完成（不影响发送结果）");
        }
    }

    /// 删一条缓存文档（命中但发送失败时回落用）。
    pub async fn cache_remove(&self, key: &str) {
        self.ensure_cache().await;
        if !self.cache.available() {
            return;
        }
        let cache = self.cache.clone();
        let key = key.to_owned();
        let _ = tokio::task::spawn_blocking(move || cache.remove(&key)).await;
    }

    /// 停机：关闸 → 等在飞归零 → 关 runtime（见模块文档的顺序说明）。
    pub fn shutdown(&self) {
        self.engine.shutdown();
    }
}

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
    /// 在飞任务数（`submit` 每接受一个 +1）。
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

/// 宿主给的插件目录；没给就退回 `plugins/`。
pub fn plugins_dir_from(config_json: &str) -> PathBuf {
    serde_json::from_str::<Value>(config_json)
        .ok()
        .and_then(|config| config.get("plugins_dir")?.as_str().map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("plugins"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plugins_dir_comes_from_the_config_or_defaults() {
        assert_eq!(
            plugins_dir_from(r#"{"plugins_dir":"D:/bots/demo/plugins"}"#),
            PathBuf::from("D:/bots/demo/plugins")
        );
        assert_eq!(plugins_dir_from(""), PathBuf::from("plugins"));
        assert_eq!(plugins_dir_from("{}"), PathBuf::from("plugins"));
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
        let done = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        for _ in 0..8 {
            let ticket = engine.enter().expect("闸门应该是开的");
            let done = Arc::clone(&done);
            spawn_for_test(&engine, async move {
                tokio::time::sleep(Duration::from_millis(20)).await;
                done.fetch_add(1, Ordering::SeqCst);
                drop(ticket);
            });
        }
        engine.shutdown();
        assert_eq!(done.load(Ordering::SeqCst), 8, "8 个在飞任务都该跑完");
    }

    /// 关闸之后：新任务进不来。
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
    fn spawn_for_test(engine: &Arc<Engine>, fut: impl Future<Output = ()> + Send + 'static) {
        let state = lock(&engine.state);
        let rt = state.rt.as_ref().expect("runtime 还活着");
        rt.spawn(fut);
    }
}
