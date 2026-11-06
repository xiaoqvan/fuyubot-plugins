//! `Runtime`：插件 `start` 之后活到最后的那一份共享状态。
//!
//! 它把这几样东西钉在一起——宿主句柄、TDLib 门面、Mongo 数据层、插件配置、
//! HTTP 客户端、**tokio 执行引擎**——所有回调拿到的都是它的 `Arc`，谁都不用再层层传参。
//!
//! ## 生命周期
//!
//! ```text
//! plugin_start → Runtime::new ─┬─ tokio 多线程 runtime（WORKERS 个 adblock-worker 线程）
//!                              ├─ 延时 / 周期任务（tokio::time::sleep 排期，同上）
//!                              ├─ Db（走 storage.mongo 服务，硬依赖）
//!                              └─ HTTP agent（AI 接口复用连接）
//! plugin_stop  → Runtime::shutdown ─ ① 关闸 → ② 等在飞任务归零 → ③ 关 runtime
//! ```
//!
//! ## 停机为什么是这个顺序（动态库卸载安全）
//!
//! 任务里跑的全是本动态库里的代码，**卸载前不能有任何线程还在跑**。所以：
//!
//! 1. **关闸**：`submit` / `after` / `every` 的新任务直接丢弃（future 就地 drop，
//!    等价于旧调度器「shutdown 丢弃未到期任务」的语义）；
//! 2. **等在飞归零**：每接受一个任务就 +1、任务结束或被取消时 -1（RAII，
//!    任务被 drop 也算）。没到期的延时/周期任务此刻只是在 `sleep`，不占计数；
//! 3. **关 runtime**：`shutdown_timeout` 收摊，睡着的定时任务在这里被 drop（取消）。
//!
//! 在飞等待有上限（[`DRAIN_TIMEOUT`]）：**必须覆盖最长的阻塞操作**（AI 接口的
//! 90 秒）。tokio 的 `shutdown_timeout` 只等 blocking 任务到超时，之后那些线程
//! 会继续跑——宿主在 `plugin_stop` 返回后就卸载动态库，残线程再跑一步就是未定义
//! 行为。所以宁可最坏等 95 秒（正常停机在飞为 0，立刻返回）。

use std::future::Future;
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock, Weak};
use std::time::{Duration, Instant};

use fuyubot_abi::HostApi;
use fuyubot_abi::ResponseRouter;
use serde_json::Value;

use crate::api::Td;
use crate::cache::Cache;
use crate::config::PluginConfig;
use crate::store::Db;

/// 工作线程数。AI 审核一次要几秒，太少了会把后面排队的消息压住；
/// 太多了又会同时打爆接口限流——4 是「几个群同时出广告」时的经验值。
const WORKERS: usize = 4;
/// AI 接口的整体超时。
const HTTP_TIMEOUT: Duration = Duration::from_secs(90);
/// `shutdown` 等在飞任务归零的上限。
///
/// **必须覆盖最长的阻塞操作**（AI 接口的 [`HTTP_TIMEOUT`]）：tokio 的
/// `shutdown_timeout` 只等 blocking 任务到超时为止，之后那些线程**会继续跑**
/// （tokio 不取消 `spawn_blocking`）。宿主在 `plugin_stop` 返回后就卸载动态库，
/// 残线程再跑一步就是未定义行为——所以宁可停机最坏等 95 秒（正常停机在飞为 0，
/// 立刻返回），也不能让 blocking 线程活过卸载。
const DRAIN_TIMEOUT: Duration = Duration::from_secs(95);
/// tokio runtime 收摊的上限：此时剩下的只可能是纯 async 任务，drop 即取消（安全）。
const STOP_TIMEOUT: Duration = Duration::from_secs(5);

/// 插件运行期共享状态。
pub struct Runtime {
    pub host: HostApi,
    pub td: Td,
    pub db: Db,
    pub cfg: PluginConfig,
    pub http: ureq::Agent,
    /// 宿主的插件目录（排障时打日志用）。
    pub plugins_dir: PathBuf,
    pub cache: Cache,
    /// spawn 用的句柄（`Handle::spawn` 在同步上下文里也能用）。
    handle: tokio::runtime::Handle,
    engine: Arc<Engine>,
    /// 指向自己的弱引用：`submit` / `after` / `every` 只需要 `&self` 就能升级出
    /// `Arc<Runtime>` 交给任务，调用方不必把 `Arc` 层层传下去。
    me: OnceLock<Weak<Runtime>>,
}

impl Runtime {
    /// 从 `plugin_start` 的 `config_json` 起一份运行时。
    ///
    /// `config_json` 由宿主给，形状是 `{"plugins_dir": "...", "scripts_dir": "...", "spawn": true}`。
    /// 插件是动态库，`current_exe()` 拿到的是宿主 exe，**算不出自己旁边有什么**，只能靠它。
    pub fn new(host: HostApi, config_json: &str) -> Result<Arc<Self>, String> {
        let plugins_dir = plugins_dir_of(config_json);
        let cfg = PluginConfig::load(&host);

        // 响应路由器必须在插件 `start` 里建：宿主作用域栈顶此刻是本插件，
        // `invoke` 发出的请求才能归属到本插件名下。
        let router =
            ResponseRouter::new(host).map_err(|error| format!("建响应路由器失败：{error}"))?;

        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(WORKERS)
            .thread_name("adblock-worker")
            .enable_time()
            .build()
            .map_err(|error| format!("起 tokio 运行时失败：{error}"))?;
        let handle = rt.handle().clone();
        let engine = Engine::wrap(rt);

        let runtime = Arc::new(Self {
            host,
            td: Td::new(host, router),
            db: Db::new(host, cfg.db.clone()),
            cfg,
            http: http_agent(),
            plugins_dir,
            cache: Cache::new(),
            handle,
            engine,
            me: OnceLock::new(),
        });
        // 弱引用只在这里填一次；此后 `submit` / `after` / `every` 都能从 `&self` 升级出 Arc。
        let _ = runtime.me.set(Arc::downgrade(&runtime));
        Ok(runtime)
    }

    /// `&self` → `Arc<Runtime>`（`new` 之后一定成功；测试里手工构造的 Runtime 也走同一路径）。
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

    /// `duration` 之后跑一次（延时删除那类）。
    ///
    /// 排期阶段只在 `sleep`，**不占在飞计数**：shutdown 要能立刻把没到期的任务
    /// 丢掉，而不是等它们睡醒。到期后进闸门才算在飞。
    pub fn after<Fut>(
        &self,
        duration: Duration,
        job: impl FnOnce(Arc<Runtime>) -> Fut + Send + 'static,
    ) where
        Fut: Future<Output = ()> + Send + 'static,
    {
        let Some(runtime) = self.arc() else {
            return;
        };
        let engine = Arc::clone(&self.engine);
        let handle = self.handle.clone();
        handle.spawn(async move {
            tokio::time::sleep(duration).await;
            let Some(ticket) = engine.enter() else {
                return;
            };
            let _ticket = ticket;
            job(runtime).await;
        });
    }

    /// 每隔 `interval` 跑一次（周期扫描那类）。
    ///
    /// 单任务循环 `sleep → job().await`：慢任务不会把自己叠起来（下一轮要等
    /// 本轮干完才开始计时）。循环体退出条件是「闸门关了」。
    pub fn every<Fut>(
        &self,
        interval: Duration,
        job: impl Fn(Arc<Runtime>) -> Fut + Send + Sync + 'static,
    ) where
        Fut: Future<Output = ()> + Send + 'static,
    {
        let Some(runtime) = self.arc() else {
            return;
        };
        let engine = Arc::clone(&self.engine);
        let handle = self.handle.clone();
        handle.spawn(async move {
            loop {
                tokio::time::sleep(interval).await;
                let Some(ticket) = engine.enter() else {
                    return;
                };
                job(Arc::clone(&runtime)).await;
                drop(ticket);
            }
        });
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
    /// 在飞任务数（`submit`/`after`/`every` 各计一个）。
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

/// 从宿主配置里取插件目录（缺省值只在测试替身上出现）。
fn plugins_dir_of(config_json: &str) -> PathBuf {
    serde_json::from_str::<Value>(config_json)
        .ok()
        .and_then(|config| {
            config
                .get("plugins_dir")
                .and_then(Value::as_str)
                .map(PathBuf::from)
        })
        .unwrap_or_else(|| PathBuf::from("plugins"))
}

fn http_agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(HTTP_TIMEOUT))
        // 非 2xx 也要读正文：AI 接口的错误信息（额度、模型名、temperature）都在里面。
        .http_status_as_error(false)
        .build()
        .new_agent()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    #[test]
    fn plugins_dir_parsing() {
        assert_eq!(
            plugins_dir_of(r#"{"plugins_dir":"D:\\demo\\plugins"}"#),
            PathBuf::from(r"D:\demo\plugins")
        );
        assert_eq!(plugins_dir_of("{}"), PathBuf::from("plugins"));
        assert_eq!(plugins_dir_of("not json"), PathBuf::from("plugins"));
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

    /// 关闸之后：新任务进不来，没到期的定时任务被丢弃。
    #[test]
    fn gate_drops_new_work_and_pending_timers() {
        let engine = Engine::wrap(
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_time()
                .build()
                .expect("tokio runtime"),
        );
        let fired = Arc::new(AtomicBool::new(false));
        {
            // 模拟 `after`：睡到很远的将来再进闸——shutdown 时它还在睡。
            let worker_engine = Arc::clone(&engine);
            let fired = Arc::clone(&fired);
            engine_handle_spawn(&engine, async move {
                tokio::time::sleep(Duration::from_secs(60)).await;
                if let Some(ticket) = worker_engine.enter() {
                    fired.store(true, Ordering::SeqCst);
                    drop(ticket);
                }
            });
        }
        engine.shutdown();
        std::thread::sleep(Duration::from_millis(20));
        assert!(!fired.load(Ordering::SeqCst), "没到期的任务不许跑");
        assert!(engine.enter().is_none(), "关闸后不许再进");
    }

    /// 延时任务到点才跑（旧 `scheduler::runs_after_delay` 的 tokio 版）。
    #[test]
    fn delayed_job_runs_after_its_deadline() {
        let engine = Engine::wrap(
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_time()
                .build()
                .expect("tokio runtime"),
        );
        let fired = Arc::new(AtomicBool::new(false));
        let worker_engine = Arc::clone(&engine);
        let set = Arc::clone(&fired);
        engine_handle_spawn(&engine, async move {
            tokio::time::sleep(Duration::from_millis(20)).await;
            if let Some(ticket) = worker_engine.enter() {
                set.store(true, Ordering::SeqCst);
                drop(ticket);
            }
        });
        let deadline = Instant::now() + Duration::from_secs(2);
        while !fired.load(Ordering::SeqCst) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(fired.load(Ordering::SeqCst), "到点的任务应该跑");
        engine.shutdown();
    }

    /// 测试里借个 spawn 入口（生产路径走 `Runtime::handle`）。
    fn engine_handle_spawn(engine: &Arc<Engine>, fut: impl Future<Output = ()> + Send + 'static) {
        let state = lock(&engine.state);
        let rt = state.rt.as_ref().expect("runtime 还活着");
        rt.spawn(fut);
    }
}
