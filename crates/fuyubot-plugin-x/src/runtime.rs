//! Runtime：插件 start 之后活到最后的那一份共享状态 + tokio 执行引擎。
//!
//! ## 线程模型
//!
//! 宿主的事件 / 命令回调跑在**接收循环线程**上，那里只做「克隆参数 + submit」，
//! 立刻返回；网络请求、TDLib 往返、出图调用全在插件自有的 tokio 运行时里 await。
//! 在回调里同步发请求会把整个宿主堵死。
//!
//! ## 停机顺序（动态库卸载安全）
//!
//! 1. 先 drop 订阅守卫（不再收新事件）；
//! 2. 置取消标志（在飞下载在块边界看到就删半截文件收手）；
//! 3. 关闸并等在飞任务归零（DRAIN_TIMEOUT）；
//! 4. shutdown_timeout 收掉 runtime。
//!
//! 卸载前不能有任何线程还在跑本动态库里的代码。

use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock, Weak};
use std::time::Duration;

use fuyubot_abi::{HostApi, ResponseRouter};
use serde_json::{Value, json};

use crate::config::{self, Config};
use crate::http;
use crate::simplify::{self, Text};

/// 工作线程数。一条链接最多一次 API 请求 + 若干次发送等待，4 个够用。
const WORKERS: usize = 4;

/// shutdown 等在飞任务归零的上限。
const DRAIN_TIMEOUT: Duration = Duration::from_secs(30);

/// tokio runtime 收摊的上限。
const STOP_TIMEOUT: Duration = Duration::from_secs(5);

/// 一次 op / TDLib 调用的错误。
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
    pub router: ResponseRouter,
    pub http: http::Agent,
    pub config: Config,
    /// 取消标志：stop 置位，下载在块间检查。
    pub cancel: Arc<AtomicBool>,
    /// 直链发不出去时的落盘目录（下载后改发本地文件）。
    pub temp_dir: std::path::PathBuf,
    handle: tokio::runtime::Handle,
    engine: Arc<Engine>,
    me: OnceLock<Weak<Runtime>>,
}

impl Runtime {
    /// 从 plugin_start 的环境信息起一份运行时。
    pub fn new(host: HostApi, config: Config, config_json: &str) -> Result<Arc<Self>, String> {
        let environment: Value = serde_json::from_str(config_json).unwrap_or(Value::Null);
        crate::log::debug(&format!(
            "宿主环境：{}",
            crate::log::shorten(&environment.to_string(), 200)
        ));
        // 响应路由器必须在插件 start 里建：宿主作用域栈顶此刻是本插件。
        let router =
            ResponseRouter::new(host).map_err(|error| format!("建响应路由器失败：{error}"))?;
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(WORKERS)
            .thread_name("x-worker")
            .enable_all()
            .build()
            .map_err(|error| format!("起 tokio 运行时失败：{error}"))?;
        let handle = runtime.handle().clone();
        let engine = Engine::wrap(runtime);
        let http = http::agent().map_err(|error| error.0)?;

        let runtime = Arc::new(Self {
            host,
            router,
            http,
            config,
            cancel: Arc::new(AtomicBool::new(false)),
            temp_dir: temp_dir_from(config_json),
            handle,
            engine,
            me: OnceLock::new(),
        });
        let _ = runtime.me.set(Arc::downgrade(&runtime));
        Ok(runtime)
    }

    fn arc(&self) -> Option<Arc<Runtime>> {
        self.me.get().and_then(Weak::upgrade)
    }

    /// 把一个异步任务丢进执行引擎；闸门已关（停机中）就直接丢弃。
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

    /// 停机：置取消 → 关闸 → 等在飞归零 → 关 runtime。
    pub fn shutdown(&self) {
        self.cancel.store(true, Ordering::SeqCst);
        self.engine.shutdown();
    }

    // ── simplify op ────────────────────────────────────────────────

    /// 下发一条 simplify op 并等它的 simplifyResult（async，不占线程）。
    ///
    /// 成功返回 result 字段；ok:false 变成 CallError（code: message）。
    /// 等待预算比 op 自己的 timeout_ms 宽 5 秒，确保接住的是宿主侧超时。
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

    /// 发一条 HTML 文本消息（不引用任何消息）。
    pub async fn send_html(&self, chat_id: i64, html: &str) -> Result<Value, CallError> {
        let args = json!({
            "chat_id": chat_id,
            "input_message_content": {
                "@type": "inputMessageText",
                "text": simplify::text_json(&Text::html(html)),
            },
        });
        self.invoke_op("send_message", args, self.config.send_timeout_ms)
            .await
    }

    /// 发一条纯文本（报错用）。失败只记日志：这是已经出错之后的通知。
    pub async fn say(&self, chat_id: i64, text: &str) {
        let args = json!({
            "chat_id": chat_id,
            "input_message_content": {
                "@type": "inputMessageText",
                "text": simplify::text_json(&Text::plain(text)),
            },
        });
        if let Err(error) = self
            .invoke_op("send_message", args, self.config.send_timeout_ms)
            .await
        {
            crate::log::error(&format!("回复失败：{error}"));
        }
    }

    // ── 图片生成服务（软依赖）─────────────────────────────────────

    /// fuyubot.html2img 现在在不在。
    ///
    /// 只是读服务表，不发请求。
    pub fn image_service_available(&self) -> bool {
        !self.host.get_service(config::HTML2IMG_SERVICE).is_null()
    }

    /// 把一份 HTML 渲染成图片，返回本地路径。
    ///
    /// 服务方法是**同步阻塞**的（渲染在调用方线程跑完），所以丢 spawn_blocking，
    /// 不能占着 tokio 的工作线程。
    pub async fn render_card(&self, html: String) -> Result<String, String> {
        let host = self.host;
        let width = self.config.card_width;
        let scale = self.config.card_scale;
        let format = self.config.card_format.clone();
        let dark = self.config.card_dark;
        tokio::task::spawn_blocking(move || {
            let input = json!({
                "html": html,
                "width": width,
                "scale": scale,
                "format": format,
                "auto_height": true,
                "dark": dark,
            })
            .to_string();
            let reply = host
                .call_service(config::HTML2IMG_SERVICE, "render", &input)
                .map_err(|error| format!("调用 {} 失败：{error}", config::HTML2IMG_SERVICE))?;
            let value: Value = serde_json::from_str(&reply)
                .map_err(|error| format!("出图结果不是 JSON：{error}"))?;
            if value.get("ok").and_then(Value::as_bool) == Some(true) {
                value
                    .get("path")
                    .and_then(Value::as_str)
                    .filter(|path| !path.is_empty())
                    .map(str::to_owned)
                    .ok_or_else(|| "出图结果里没有 path".to_owned())
            } else {
                Err(value
                    .get("error")
                    .and_then(Value::as_str)
                    .unwrap_or("出图失败")
                    .to_owned())
            }
        })
        .await
        .map_err(|error| format!("出图任务被打断：{error}"))?
    }

    /// 同步出图，给外部 service 用。换一个新线程 block_on，避免调用方本身在 tokio 上下文里。
    pub fn render_card_blocking(self: &Arc<Self>, html: String) -> Result<String, String> {
        let runtime = Arc::clone(self);
        std::thread::spawn(move || runtime.handle.block_on(runtime.render_card(html)))
            .join()
            .map_err(|_| "渲染线程崩溃".to_owned())?
    }
}

/// 直链发不出去时把媒体落到哪儿。
///
/// 优先用宿主给的 plugins_dir 下的 cache/x（重启也还在，便于排查）；拿不到就退回
/// 系统临时目录。文件用完即删，不占盘。
fn temp_dir_from(config_json: &str) -> std::path::PathBuf {
    serde_json::from_str::<Value>(config_json)
        .ok()
        .and_then(|value| {
            value
                .get("plugins_dir")
                .and_then(Value::as_str)
                .map(std::path::PathBuf::from)
        })
        .map(|dir| dir.join("cache").join("x"))
        .unwrap_or_else(|| std::env::temp_dir().join("fuyubot-plugin-x"))
}

/// 等一个 ResponseFuture，超时统一成 CallError。
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

// ── 执行引擎（闸门 + 在飞计数）────────────────────────────────────

struct Engine {
    state: Mutex<State>,
    idle: Condvar,
}

struct State {
    rt: Option<tokio::runtime::Runtime>,
    open: bool,
    inflight: usize,
}

/// 在飞计数的 RAII：drop（含任务被取消）时减一。
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

fn lock(state: &Mutex<State>) -> MutexGuard<'_, State> {
    state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
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

    /// 进闸门；闸门已关返回 None（调用方丢掉 future 就是取消）。
    fn enter(self: &Arc<Self>) -> Option<Ticket> {
        let mut state = lock(&self.state);
        if !state.open {
            return None;
        }
        state.inflight += 1;
        Some(Ticket(Arc::clone(self)))
    }

    /// 关闸 + 等在飞归零 + 收掉 runtime。
    fn shutdown(&self) {
        let rt = {
            let mut state = lock(&self.state);
            state.open = false;
            let step = Duration::from_millis(50);
            let mut waited = Duration::ZERO;
            while state.inflight > 0 && waited < DRAIN_TIMEOUT {
                let (guard, _timeout) = self
                    .idle
                    .wait_timeout(state, step)
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                state = guard;
                waited += step;
            }
            state.rt.take()
        };
        if let Some(rt) = rt {
            rt.shutdown_timeout(STOP_TIMEOUT);
        }
    }
}
