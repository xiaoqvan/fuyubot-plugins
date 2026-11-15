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
