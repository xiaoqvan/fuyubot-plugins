//! 延时任务调度器。
//!
//! 用途只有两类，但都很关键：
//!
//! 1. **延时删除**：处置提示默认 30 秒后自删（`spam_tips_meg_delay`）、禁言超时提示
//!    10 秒后删——TS 版用 `setTimeout`，这里必须自己排期；
//! 2. **周期任务**：每 5 分钟扫一遍「禁言 1 小时未复审」的记录。
//!
//! 实现是「一个线程 + 二叉堆 + `recv_timeout`」：到期就交给工作线程池执行，
//! 不在这里干重活（`deleteMessages` 也要等响应，排在这里会把后面的任务全拖住）。

use std::collections::BinaryHeap;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::pool::{Job, Pool};

enum Message {
    Job(Instant, Job),
    Stop,
}

struct Entry {
    when: Instant,
    sequence: u64,
    job: Job,
}

impl PartialEq for Entry {
    fn eq(&self, other: &Self) -> bool {
        self.when == other.when && self.sequence == other.sequence
    }
}

impl Eq for Entry {}

impl PartialOrd for Entry {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Entry {
    // 二叉堆是大顶堆：`when` 越早越「大」，这样 `pop()` 出来的总是最早到期的。
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        other
            .when
            .cmp(&self.when)
            .then_with(|| other.sequence.cmp(&self.sequence))
    }
}

/// 调度器句柄（`Clone` 共享同一个线程）。
#[derive(Clone)]
pub struct Scheduler {
    sender: Arc<Mutex<Option<Sender<Message>>>>,
    handle: Arc<Mutex<Option<JoinHandle<()>>>>,
}

impl Scheduler {
    pub fn new(name: &str, pool: Pool) -> Self {
        let (sender, receiver) = mpsc::channel::<Message>();
        let handle = match std::thread::Builder::new()
            .name(name.to_owned())
            .spawn(move || scheduler_loop(receiver, pool))
        {
            Ok(handle) => handle,
            Err(error) => {
                eprintln!("[adblock] 起调度线程 {name} 失败：{error}");
                return Self {
                    sender: Arc::new(Mutex::new(None)),
                    handle: Arc::new(Mutex::new(None)),
                };
            }
        };
        Self {
            sender: Arc::new(Mutex::new(Some(sender))),
            handle: Arc::new(Mutex::new(Some(handle))),
        }
    }

    /// `duration` 之后执行 `job`。
    pub fn after(&self, duration: Duration, job: Job) {
        self.at(Instant::now() + duration, job);
    }

    /// 绝对时间点执行。
    pub fn at(&self, when: Instant, job: Job) {
        let Ok(guard) = self.sender.lock() else {
            return;
        };
        if let Some(sender) = guard.as_ref() {
            let _ = sender.send(Message::Job(when, job));
        }
    }

    /// 停掉线程并丢掉所有还没到期的任务。
    ///
    /// 丢掉是**有意的**：那些任务大多是「删一条提示」，插件都卸载了没必要再发请求；
    /// 而且它们捕获着 `Arc<Runtime>`，不清掉会让运行时回收不了。
    pub fn shutdown(&self) {
        if let Ok(mut guard) = self.sender.lock()
            && let Some(sender) = guard.take()
        {
            let _ = sender.send(Message::Stop);
        }
        let handle = match self.handle.lock() {
            Ok(mut handle) => handle.take(),
            Err(poisoned) => poisoned.into_inner().take(),
        };
        if let Some(handle) = handle {
            let _ = handle.join();
        }
    }
}

fn scheduler_loop(receiver: Receiver<Message>, pool: Pool) {
    let mut heap: BinaryHeap<Entry> = BinaryHeap::new();
    let mut sequence = 0_u64;
    loop {
        let wait = match heap.peek() {
            Some(entry) => entry.when.saturating_duration_since(Instant::now()),
            None => Duration::from_secs(60),
        };
        match receiver.recv_timeout(wait) {
            Ok(Message::Job(when, job)) => {
                sequence = sequence.wrapping_add(1);
                heap.push(Entry {
                    when,
                    sequence,
                    job,
                });
            }
            Ok(Message::Stop) => {
                // 丢掉所有待执行任务：它们持有运行时的 Arc，不清会拖住回收。
                heap.clear();
                return;
            }
            Err(RecvTimeoutError::Timeout) => {
                let now = Instant::now();
                while heap.peek().is_some_and(|entry| entry.when <= now) {
                    if let Some(entry) = heap.pop() {
                        pool.submit(entry.job);
                    }
                }
            }
            Err(RecvTimeoutError::Disconnected) => {
                heap.clear();
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    #[test]
    fn runs_after_delay() {
        let pool = Pool::new("sched-test", 1);
        let scheduler = Scheduler::new("sched", pool.clone());
        let flag = Arc::new(AtomicBool::new(false));
        let set = Arc::clone(&flag);
        scheduler.after(
            Duration::from_millis(30),
            Box::new(move || {
                set.store(true, Ordering::SeqCst);
            }),
        );
        let deadline = Instant::now() + Duration::from_secs(2);
        while !flag.load(Ordering::SeqCst) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(flag.load(Ordering::SeqCst));
        scheduler.shutdown();
        pool.shutdown();
    }

    #[test]
    fn shutdown_drops_pending_jobs() {
        let pool = Pool::new("sched-test", 1);
        let scheduler = Scheduler::new("sched", pool.clone());
        let flag = Arc::new(AtomicBool::new(false));
        let set = Arc::clone(&flag);
        scheduler.after(
            Duration::from_secs(60),
            Box::new(move || {
                set.store(true, Ordering::SeqCst);
            }),
        );
        scheduler.shutdown();
        std::thread::sleep(Duration::from_millis(20));
        assert!(!flag.load(Ordering::SeqCst));
        pool.shutdown();
    }
}
