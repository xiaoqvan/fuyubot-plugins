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
