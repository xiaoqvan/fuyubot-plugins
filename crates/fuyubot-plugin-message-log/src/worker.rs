//! 后台工作线程：宿主回调只投递，真正的工作（TDLib 查询 + 拼日志）都在这里。
//!
//! ## 为什么不是直接在回调里打日志
//!
//! 名字要靠 `getChat` / `getUser`，那是异步请求。宿主的事件回调跑在**接收循环线程**
//! 上，在那里等响应就是让收件人自己等自己。所以回调只做一次 `try_send`，工作线程
//! 拿着自己的 current-thread tokio runtime 顺序处理：
//!
//! ```text
//! 宿主接收循环线程                         messagelog-worker 线程
//!   on(updateNewMessage)                     recv()
//!     └─ try_send(update) ────────────────▶   ├─ 名字缓存未命中 → getChat / getUser（await）
//!         队列满就丢（不阻塞接收）           ├─ render → host.log(INFO, 一行)
//!                                             └─ 下一条
//! ```
//!
//! 顺序处理是刻意的：日志读起来要按消息到达顺序，而不是按谁先查到名字。
//!
//! ## 生命周期
//!
//! `stop()` 先退订（不再有新消息进队），再 [`Worker::shutdown`]：置停机标志 →
//! 丢掉发送端 → 工作线程处理完**手上这一条**就退出 → `join`。队列里还没处理的
//! 消息直接丢弃——停机不能被上千条积压消息拖成几十分钟。动态库卸载前不会留下
//! 跑着本库代码的线程。
//!
//! ⚠️ 工作线程**不能**持有发送端（哪怕间接通过 `Arc<Channel>`）：只要它自己还
//! 持有一份 `SyncSender`，`recv()` 就永远等不到 `Disconnected`，停机 `join` 会死锁。
//! 所以计数器与发送端分开：线程只拿 `Arc<Stats>`。

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use fuyubot_abi::{HostApi, ResponseRouter};
use serde_json::Value;

use crate::config::Config;
use crate::log;
use crate::render;
use crate::td::{Names, Td};

/// 队列上限。正常消息处理是「缓存命中 + 一行日志」，瞬间就能排空；
/// 这里只是防止 TDLib 卡住时把内存吃光。
pub const QUEUE_CAPACITY: usize = 1024;

/// 与发送端无关的统计 / 告警 / 停机状态，工作线程可以安全持有。
struct Stats {
    host: HostApi,
    dropped: AtomicU64,
    warned: AtomicBool,
    /// 停机标志：置位后工作线程处理完当前这条就退出，不再消费队列。
    shutdown: AtomicBool,
}

/// 投递端：可以克隆，订阅闭包和插件状态各持有一份。
///
/// `SyncSender` 本身不是 `Sync`（`&Sender` 不能跨线程共享），所以用 `Mutex` 包一层：
/// 宿主的事件闭包要求 `Fn + Send + Sync`，而锁只在一次非阻塞 `try_send` 期间持有。
#[derive(Clone)]
pub struct WorkerHandle {
    channel: Arc<Channel>,
}

struct Channel {
    sender: Mutex<SyncSender<Value>>,
    stats: Arc<Stats>,
}

impl WorkerHandle {
    /// 把一条更新放进队列；满了就丢，绝不在宿主接收线程上等待。
    pub fn submit(&self, update: &Value) {
        let sender = self
            .channel
            .sender
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        match sender.try_send(update.clone()) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => {
                self.channel.stats.dropped.fetch_add(1, Ordering::Relaxed);
                // 只在「从正常变成拥塞」时告警一次；恢复时工作线程会补一条。
                if !self.channel.stats.warned.swap(true, Ordering::Relaxed) {
                    log::warn(
                        &self.channel.stats.host,
                        format!("消息处理队列已满（{QUEUE_CAPACITY} 条），暂时丢弃后续消息"),
                    );
                }
            }
            // 工作线程已经退出（停机中）：静默丢弃。
            Err(TrySendError::Disconnected(_)) => {}
        }
    }
}

/// 工作线程与它的投递端。
pub struct Worker {
    handle: Option<WorkerHandle>,
    thread: Option<JoinHandle<u64>>,
}

impl Worker {
    /// 起线程。`runtime` 在这里建好再移进线程：建失败能直接让 `start()` 报错，
    /// 而不是变成一个「线程起来了但什么都干不了」的半死状态。
    pub fn start(host: HostApi, router: ResponseRouter, config: Config) -> Result<Self, String> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .map_err(|error| format!("建 tokio 运行时失败：{error}"))?;

        let (sender, receiver) = sync_channel(QUEUE_CAPACITY);
        let stats = Arc::new(Stats {
            host,
            dropped: AtomicU64::new(0),
            warned: AtomicBool::new(false),
            shutdown: AtomicBool::new(false),
        });
        let channel = Arc::new(Channel {
            sender: Mutex::new(sender),
            stats: Arc::clone(&stats),
        });
        let handle = WorkerHandle { channel };

        let thread = std::thread::Builder::new()
            .name("messagelog-worker".to_owned())
            .spawn(move || run(host, router, config, runtime, receiver, stats))
            .map_err(|error| format!("起消息日志工作线程失败：{error}"))?;

        Ok(Self {
            handle: Some(handle),
            thread: Some(thread),
        })
    }

    /// 给订阅闭包用的投递端。
    pub fn handle(&self) -> WorkerHandle {
        self.handle
            .as_ref()
            .expect("Worker 在 shutdown 之后不会再被使用")
            .clone()
    }

    /// 停机：断开队列 → 等工作线程处理完当前这条 → join。
    ///
    /// 返回工作线程一共记录了多少条（线程 panic 时按 0 算，不影响卸载）。
    pub fn shutdown(mut self) -> u64 {
        self.join()
    }

    fn join(&mut self) -> u64 {
        // 先置停机标志：队列里还没处理的消息不再逐条跑（每条最多两个 3 秒超时，
        // 上千条积压会把停机拖成几十分钟）。已经开工的那条会跑完。
        if let Some(handle) = self.handle.as_ref() {
            handle.channel.stats.shutdown.store(true, Ordering::Relaxed);
        }
        // 再放掉本结构里的投递端；订阅闭包那份在 `stop()` 里已经先 drop 了。
        self.handle = None;
        self.thread
            .take()
            .and_then(|thread| thread.join().ok())
            .unwrap_or(0)
    }
}

impl Drop for Worker {
    /// 兜底：即使 `start()` 在订阅失败后提前返回、或插件状态被 panic 展开，
    /// 也要把线程 join 掉，不能让它在动态库卸载后继续跑。
    fn drop(&mut self) {
        let _ = self.join();
    }
}

fn run(
    host: HostApi,
    router: ResponseRouter,
    config: Config,
    runtime: tokio::runtime::Runtime,
    receiver: Receiver<Value>,
    stats: Arc<Stats>,
) -> u64 {
    let td = Td::new(router);
    let mut names = Names::new();
    let mut recorded = 0u64;

    while let Ok(update) = receiver.recv() {
        // 停机中：手上这条是刚收到的，但队列里的不再处理。
        if stats.shutdown.load(Ordering::Relaxed) {
            break;
        }

        // 拥塞恢复：告警过才补一条，顺便报一共丢了多少。
        if stats.warned.swap(false, Ordering::Relaxed) {
            let dropped = stats.dropped.swap(0, Ordering::Relaxed);
            log::info(
                &host,
                format!("消息队列已恢复；此前因积压丢弃 {dropped} 条"),
            );
        }

        runtime.block_on(handle_update(&host, &td, &mut names, &config, &update));
        recorded += 1;
    }

    recorded
}

async fn handle_update(
    host: &HostApi,
    td: &Td,
    names: &mut Names,
    config: &Config,
    update: &Value,
) {
    let Some(message) = update.get("message") else {
        return;
    };
    if config.ignore_outgoing
        && message
            .get("is_outgoing")
            .and_then(Value::as_bool)
            .unwrap_or(false)
    {
        return;
    }
    let Some(chat_id) = message.get("chat_id").and_then(Value::as_i64) else {
        return;
    };
    let sender = message.get("sender_id");

    // 发送者名字：用户查 getUser，频道 / 匿名管理员查 getChat。
    let (user_name, sender_chat_title) = match sender.and_then(|sender| {
        sender
            .get("@type")
            .and_then(Value::as_str)
            .map(|kind| (kind, sender))
    }) {
        Some(("messageSenderUser", sender)) => {
            let user_id = sender.get("user_id").and_then(Value::as_i64);
            match user_id {
                Some(user_id) => (names.user_name(td, user_id).await, None),
                None => (None, None),
            }
        }
        Some(("messageSenderChat", sender)) => {
            let sender_chat_id = sender.get("chat_id").and_then(Value::as_i64);
            match sender_chat_id {
                Some(sender_chat_id) => (None, names.chat_title(td, sender_chat_id).await),
                None => (None, None),
            }
        }
        _ => (None, None),
    };

    // 群 / 频道的标题；私聊（chat_id > 0）固定 Private，不查。
    let chat_title = if chat_id < 0 {
        names.chat_title(td, chat_id).await
    } else {
        None
    };

    let chat = render::chat_label(chat_id, chat_title.as_deref());
    let sender = render::sender_label(sender, user_name.as_deref(), sender_chat_title.as_deref());
    let line = render::render(message, &chat, &sender, config.max_text_chars);

    log::info(host, line);
}
