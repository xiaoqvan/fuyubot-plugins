//! 固定大小的工作线程池。
//!
//! 宿主的事件回调跑在**接收循环线程**上，里面既不能等 TDLib 响应，也不能做网络请求。
//! 所以插件把每条更新包成一个任务丢进这里，真正的活由 N 个常驻线程干。
//!
//! 为什么不是「每条更新 spawn 一个线程」：广告消息会成串来，一个刷屏就能把宿主进程
//! 的线程数顶到几百；而 `stop()` 要 join 的句柄一多，停机也会跟着变慢。
//!
//! ⚠️ **卸载前必须 `shutdown()`**：动态库卸载之后线程还在跑库里的代码就是未定义行为。

use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::JoinHandle;

/// 一个任务：拥有自己的数据，不需要借用。
pub type Job = Box<dyn FnOnce() + Send + 'static>;

struct Inner {
    sender: Mutex<Option<Sender<Job>>>,
    handles: Mutex<Vec<JoinHandle<()>>>,
}

use std::sync::Mutex;

/// 可克隆的任务入口。
#[derive(Clone)]
pub struct Pool {
    inner: std::sync::Arc<Inner>,
}

impl Pool {
    /// 起 `size` 个常驻线程（至少 1）。
    pub fn new(name: &str, size: usize) -> Self {
        let size = size.max(1);
        let (sender, receiver) = mpsc::channel::<Job>();
        let inner = std::sync::Arc::new(Inner {
            sender: Mutex::new(Some(sender)),
            handles: Mutex::new(Vec::with_capacity(size)),
        });
        let receiver = std::sync::Arc::new(Mutex::new(receiver));
        for index in 0..size {
            let receiver = std::sync::Arc::clone(&receiver);
            let thread_name = format!("{name}-{index}");
            let handle = match std::thread::Builder::new()
                .name(thread_name.clone())
                .spawn(move || worker_loop(&receiver))
            {
                Ok(handle) => handle,
                Err(error) => {
                    eprintln!("[adblock] 起工作线程 {thread_name} 失败：{error}");
                    continue;
                }
            };
            if let Ok(mut handles) = inner.handles.lock() {
                handles.push(handle);
            }
        }
        Self { inner }
    }

    /// 丢一个任务进去。池子已经关了（`shutdown` 之后）就当场丢掉。
    pub fn submit(&self, job: Job) {
        let Ok(guard) = self.inner.sender.lock() else {
            return;
        };
        if let Some(sender) = guard.as_ref() {
            // 发送失败 = 所有 worker 都没了；任务直接丢弃，不让 panic 传播。
            let _ = sender.send(job);
        }
    }

    /// 关掉入口、等所有线程把队列里的任务做完再退出。
    pub fn shutdown(&self) {
        if let Ok(mut guard) = self.inner.sender.lock() {
            guard.take();
        }
        let handles = match self.inner.handles.lock() {
            Ok(mut handles) => std::mem::take(&mut *handles),
            Err(poisoned) => std::mem::take(&mut *poisoned.into_inner()),
        };
        for handle in handles {
            let _ = handle.join();
        }
    }
}

fn worker_loop(receiver: &std::sync::Arc<Mutex<Receiver<Job>>>) {
    loop {
        // 锁只护着 `recv` 这一下：任务本身在锁外跑，否则整个池子会退化成串行。
        let job = {
            let Ok(receiver) = receiver.lock() else {
                return;
            };
            receiver.recv()
        };
        match job {
            Ok(job) => job(),
            Err(_) => return,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    #[test]
    fn runs_jobs() {
        let pool = Pool::new("test", 2);
        let counter = std::sync::Arc::new(AtomicUsize::new(0));
        for _ in 0..10 {
            let counter = std::sync::Arc::clone(&counter);
            pool.submit(Box::new(move || {
                counter.fetch_add(1, Ordering::SeqCst);
            }));
        }
        pool.shutdown();
        assert_eq!(counter.load(Ordering::SeqCst), 10);
    }

    #[test]
    fn submit_after_shutdown_is_ignored() {
        let pool = Pool::new("test", 1);
        pool.shutdown();
        pool.submit(Box::new(|| {}));
        std::thread::sleep(Duration::from_millis(10));
    }
}
