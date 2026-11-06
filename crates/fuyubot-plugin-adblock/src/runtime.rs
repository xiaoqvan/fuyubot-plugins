//! `Runtime`：插件 `start` 之后活到最后的那一份共享状态。
//!
//! 它把这几样东西钉在一起——宿主句柄、TDLib 门面、Mongo 数据层、插件配置、
//! HTTP 客户端、线程池/调度器——所有回调拿到的都是它的 `Arc`，谁都不用再层层传参。
//!
//! ## 生命周期
//!
//! ```text
//! plugin_start → Runtime::new ─┬─ Pool（4 个工作线程）
//!                              ├─ Scheduler（延时删除 / 周期扫描）
//!                              ├─ Db（走 storage.mongo 服务，硬依赖）
//!                              └─ HTTP agent（AI 接口复用连接）
//! plugin_stop  → Runtime::shutdown ─ 先退订（插件做的事）→ 停调度 → join 工作线程
//! ```
//!
//! ⚠️ **顺序是有意的**：工作线程在跑动态库里的代码，必须 join 完才能让宿主卸载；
//! 调度器先停，是因为它排着的任务持有 `Arc<Runtime>`，不清会拖住回收。

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use fuyubot_abi::HostApi;
use serde_json::Value;

use crate::api::Td;
use crate::cache::Cache;
use crate::config::PluginConfig;
use crate::pool::{Job, Pool};
use crate::scheduler::Scheduler;
use crate::store::Db;

/// 工作线程数。AI 审核一次要几秒，太少了会把后面排队的消息压住；
/// 太多了又会同时打爆接口限流——4 是「几个群同时出广告」时的经验值。
const WORKERS: usize = 4;
/// AI 接口的整体超时。
const HTTP_TIMEOUT: Duration = Duration::from_secs(90);

/// 插件运行期共享状态。
pub struct Runtime {
    pub host: HostApi,
    pub td: Td,
    pub db: Db,
    pub cfg: PluginConfig,
    pub http: ureq::Agent,
    /// 宿主的插件目录（排障时打日志用）。
    pub plugins_dir: PathBuf,
    pub pool: Pool,
    pub scheduler: Scheduler,
    pub cache: Cache,
}

impl Runtime {
    /// 从 `plugin_start` 的 `config_json` 起一份运行时。
    ///
    /// `config_json` 由宿主给，形状是 `{"plugins_dir": "...", "scripts_dir": "...", "spawn": true}`。
    /// 插件是动态库，`current_exe()` 拿到的是宿主 exe，**算不出自己旁边有什么**，只能靠它。
    pub fn new(host: HostApi, config_json: &str) -> Result<Arc<Self>, String> {
        let plugins_dir = plugins_dir_of(config_json);
        let cfg = PluginConfig::load(&host);
        let db = Db::new(host, cfg.db.clone());
        let pool = Pool::new("adblock-worker", WORKERS);
        let scheduler = Scheduler::new("adblock-sched", pool.clone());

        Ok(Arc::new(Self {
            host,
            td: Td::new(host),
            db,
            cfg,
            http: http_agent(),
            plugins_dir,
            pool,
            scheduler,
            cache: Cache::new(),
        }))
    }

    /// 把一个任务丢进工作线程池。
    pub fn submit(self: &Arc<Self>, job: impl FnOnce(Arc<Runtime>) + Send + 'static) {
        let runtime = Arc::clone(self);
        self.pool.submit(Box::new(move || job(runtime)));
    }

    /// `duration` 之后跑一次（延时删除那类）。由调度器在工作线程上执行。
    #[allow(dead_code)]
    pub fn after(
        self: &Arc<Self>,
        duration: Duration,
        job: impl FnOnce(Arc<Runtime>) + Send + 'static,
    ) {
        let runtime = Arc::clone(self);
        self.scheduler
            .after(duration, Box::new(move || job(runtime)));
    }

    /// 每隔 `interval` 跑一次（周期扫描那类）。
    ///
    /// 任务干完活之后重新排下一次：慢任务不会把自己叠起来。
    pub fn every(
        self: &Arc<Self>,
        interval: Duration,
        job: impl Fn(&Runtime) + Send + Sync + 'static,
    ) {
        let job: Arc<dyn Fn(&Runtime) + Send + Sync> = Arc::new(job);
        arm(self, interval, job);
    }

    /// 停掉所有后台线程。
    pub fn shutdown(&self) {
        self.scheduler.shutdown();
        self.pool.shutdown();
    }
}

fn arm(runtime: &Arc<Runtime>, interval: Duration, job: Arc<dyn Fn(&Runtime) + Send + Sync>) {
    let owner = Arc::clone(runtime);
    runtime.scheduler.after(
        interval,
        Box::new(move || {
            job(&owner);
            arm(&owner, interval, job);
        }) as Job,
    );
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

    #[test]
    fn plugins_dir_parsing() {
        assert_eq!(
            plugins_dir_of(r#"{"plugins_dir":"D:\\demo\\plugins"}"#),
            PathBuf::from(r"D:\demo\plugins")
        );
        assert_eq!(plugins_dir_of("{}"), PathBuf::from("plugins"));
        assert_eq!(plugins_dir_of("not json"), PathBuf::from("plugins"));
    }
}
