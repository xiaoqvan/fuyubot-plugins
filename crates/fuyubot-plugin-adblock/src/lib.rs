//! FuyuBot 广告拦截插件（adblock）。
//!
//! 从 TS 框架的 `plugins/adblock` 重写而来：同一条「检测 → 处置 → 通知」的流水线，
//! 但按 FuyuBot 的插件形态重新组织（cdylib + `HostApiVTable`，无 async runtime）。
//!
//! ## 它做什么
//!
//! | 入口 | 触发 | 干什么 |
//! | --- | --- | --- |
//! | `updateNewMessage` | 群里的每条消息 | 记录消息索引 + 缓存历史 → 命中关键词 / 文件 / 贴纸 / 转发黑名单 → 删除并禁言或封禁 |
//! | | `messageContact` / 引用 / 全局触发词 | 交给 AI 深度审核（OpenAI 兼容接口） |
//! | `updateNewCallbackQuery` | 日志频道里的按钮 | unban / unmute / ban / 贴纸集 / 文件 / 图片 / 复审 |
//! | `updateChatMember` | 新成员入群 | 名称与简介查黑名单 + AI 审核，命中即封禁或「禁言待复审」 |
//! | `updateUser` | 入群 24h 内改资料 | 重新审核资料（带节流） |
//! | `updateDeleteMessages` | 消息被删 | 从索引里同步移除 |
//! | 周期任务 | 每 5 分钟 | 扫「禁言 1 小时未复审」→ 封禁 |
//!
//! ## 依赖：`storage.mongo` 是硬依赖
//!
//! 数据全部走宿主里 `storage.mongo` 提供的进程级共享服务（MongoDB），集合名与字段
//! 沿用 TS 版，所以**同一个库可以直接被新插件接上**。`inject()` 里声明了它：
//! 没有这个服务时本插件不会激活（停在 Pending），而不是静默地「不拦截」。
//!
//! ## 线程模型（先读这一段）
//!
//! 宿主的 `on` 回调跑在**接收循环线程**上。我们只在那里做一件事：把更新塞进工作队列。
//! 所有 TDLib 请求、Mongo 查询、AI 调用都在 4 个工作线程里同步执行；延时任务（提示自删、
//! 周期扫描）交给调度器。`stop()` 的顺序是「退订 → 停调度 → join 工作线程」。
//!
//! ## 与 TS 版的有意差异
//!
//! 1. **密钥不硬编码**：原代码里的 API Key 已提交进仓库，这里只从配置读；
//! 2. **总日志频道可配**（默认不开）：原代码写死某个频道的固定话题；
//! 3. **「Bot 主人」= `owner_ids`**：FuyuBot 没有框架级的 owner 概念；
//! 4. **数据库换成 `storage.mongo`**：不再自带连接池，也不再依赖宿主私有的 db API；
//! 5. **链接预览自己抓**：不拖 `link-preview-js`，只取 `<title>` 与 description。

mod action;
mod ai;
mod api;
mod cache;
mod callbacks;
mod commands;
mod config;
mod detect;
mod flow;
mod log;
mod pool;
mod runtime;
mod scheduler;
/// 本地化的 simplify 层（上游 `fuyubot-simplify` 的裁剪版，不依赖该 crate）。
pub mod simplify;
mod store;
mod subscribe;
mod text;
mod util;

use std::sync::Arc;
use std::time::Duration;

use fuyubot_abi::{HostApi, Plugin, define_plugin};
use serde_json::Value;

/// 复审扫描间隔：与原版 `runHandlers` 的 5 分钟一致。
const SWEEP_INTERVAL: Duration = Duration::from_secs(5 * 60);

/// 插件状态。
pub struct AdblockPlugin {
    host: HostApi,
    runtime: Option<Arc<runtime::Runtime>>,
    /// 所有事件订阅：drop 就退订。
    subscriptions: Vec<subscribe::EventSubscription>,
}

impl Plugin for AdblockPlugin {
    fn init(host: HostApi) -> Result<Self, String> {
        log::info(&host, "adblock 插件初始化");
        Ok(Self {
            host,
            runtime: None,
            subscriptions: Vec::new(),
        })
    }

    fn start(&mut self, config_json: &str) -> Result<(), String> {
        let runtime = runtime::Runtime::new(self.host, config_json)?;

        // 硬依赖：数据全在 `storage.mongo` 里。`inject()` 已经让宿主等过它，
        // 这里再用**不发请求**的 get_service 确认一次——`ping` 放到工作线程，
        // 库暂时没起时不能把宿主启动卡在服务发现上。
        if self.host.get_service(store::SERVICE).is_null() {
            return Err(format!(
                "硬依赖 {} 不可用：请先启用并配置 mongo 插件",
                store::SERVICE
            ));
        }
        log::infof(
            &self.host,
            format_args!(
                "adblock 已启动（库 {}，插件目录 {}）：{}",
                runtime.cfg.db.as_deref().unwrap_or("storage.mongo 默认库"),
                runtime.plugins_dir.display(),
                runtime.cfg.summary()
            ),
        );

        // 命令表登记进宿主的 `/help`。失败只 warn：help 是入口不是业务，
        // 缺了它也没理由把插件拖成 Pending。
        let (registered, help_error) = commands::register_to_help(&self.host);
        match help_error {
            Some(error) => log::warn(
                &self.host,
                format!(
                    "向 /help 登记命令只成功 {registered}/{} 条：{error}",
                    commands::spec::COMMANDS.len()
                ),
            ),
            None => log::info(&self.host, format!("已向 /help 登记 {registered} 条命令")),
        }
        if runtime.cfg.owner_ids.is_empty() && runtime.cfg.admin_ids.is_empty() {
            log::warn(
                &self.host,
                "owner_ids / admin_ids 都没配：管理员命令（/qadid /addkw …）无人可用",
            );
        }

        // 建索引 + 连通性探测都是幂等的慢操作，丢给工作线程。
        {
            let runtime = Arc::clone(&runtime);
            runtime.submit(|rt| {
                match rt.db.ping() {
                    Ok(info) => log::infof(
                        &rt.host,
                        format_args!(
                            "storage.mongo 连通（服务默认库 {}，集合 {} 个；本插件用 {}）",
                            info.get("db").and_then(|db| db.as_str()).unwrap_or("?"),
                            info.get("collections")
                                .and_then(|count| count.as_u64())
                                .unwrap_or(0),
                            rt.cfg.db.as_deref().unwrap_or("同上（未单独指定）")
                        ),
                    ),
                    Err(error) => log::warn(
                        &rt.host,
                        format!("storage.mongo 暂时连不上（{error}）；库起来后会自动重连"),
                    ),
                }
                let (created, failed) = rt.db.ensure_indexes();
                log::infof(
                    &rt.host,
                    format_args!("索引就绪：成功 {created} 条，跳过/冲突 {failed} 条"),
                );
                let counts = rt.db.counts();
                let summary = counts
                    .iter()
                    .map(|(name, count)| format!("{name}={count}"))
                    .collect::<Vec<_>>()
                    .join(" ");
                log::info(&rt.host, format!("数据量：{summary}"));
            });
        }

        // 事件订阅：回调只排队，真正的活在 worker 里。
        self.listen(&runtime, "updateNewMessage", |rt, update| {
            flow::message::handle(rt, update)
        })?;
        self.listen(&runtime, "updateDeleteMessages", |rt, update| {
            flow::records::handle_delete(rt, update)
        })?;
        self.listen(&runtime, "updateNewCallbackQuery", |rt, update| {
            callbacks::handle(rt, update)
        })?;
        self.listen(&runtime, "updateChatMember", |rt, update| {
            flow::new_member::handle_chat_member(rt, update)
        })?;
        self.listen(&runtime, "updateUser", |rt, update| {
            flow::profile::handle_update_user(rt, update)
        })?;

        // 周期扫描 + 启动时立即来一轮（原版 `immediate: true`）。
        let sweeper = Arc::clone(&runtime);
        runtime.every(SWEEP_INTERVAL, |rt| flow::sweeper::sweep_safely(rt));
        sweeper.submit(|rt| flow::sweeper::sweep_safely(&rt));

        self.runtime = Some(runtime);
        Ok(())
    }

    fn stop(&mut self) {
        // 先断输入，再停线程：反过来会留下「线程在跑、事件又进来」的窗口。
        self.subscriptions.clear();
        if let Some(runtime) = self.runtime.take() {
            runtime.shutdown();
            // 宿主探测插件时也会调 `plugin_stop`（那时 start 没跑过），
            // 没真正启动过就别打「已停止」误导人。
            log::info(&self.host, "adblock 已停止");
        }
    }

    /// 硬依赖：`storage.mongo` 不在时不要激活。
    fn inject() -> Vec<String> {
        vec![store::SERVICE.to_owned()]
    }

    /// **软依赖**：宿主内置 help 插件的命令注册服务。
    ///
    /// 声明它只是让宿主先把 help 插件就绪，`start()` 里的
    /// [`commands::register_to_help`] 才不会扑空；没装 help 时本插件照常启动，
    /// 只是 `/help` 里看不到这 17 条命令。
    fn soft_inject() -> Vec<String> {
        vec![commands::HELP_SERVICE.to_owned()]
    }
}

impl AdblockPlugin {
    /// 订阅一个事件；回调里只做「克隆更新 + 入队」。
    fn listen(
        &mut self,
        runtime: &Arc<runtime::Runtime>,
        event: &str,
        handler: impl Fn(&Arc<runtime::Runtime>, &Value) + Send + Sync + 'static,
    ) -> Result<(), String> {
        let owner = Arc::clone(runtime);
        // 闭包会被反复调用（每条更新一次），所以 handler 要共享而不是移动：
        // `Arc<dyn Fn>` 让每次调用各拿一份引用，内层任务拿到的是独立的克隆。
        let handler: Arc<dyn Fn(&Arc<runtime::Runtime>, &Value) + Send + Sync> = Arc::new(handler);
        let subscription = subscribe::subscribe(self.host, event, None, move |update| {
            let update = update.clone();
            let owner = Arc::clone(&owner);
            let handler = Arc::clone(&handler);
            owner.submit(move |rt| handler(&rt, &update));
        })
        .ok_or_else(|| format!("订阅 {event} 失败"))?;
        self.subscriptions.push(subscription);
        Ok(())
    }
}

// 一个 crate 只能展开一次 `define_plugin!`（导出符号名是 ABI 定死的）。
define_plugin!(AdblockPlugin, "fuyubot.adblock");
