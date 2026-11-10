//! FuyuBot `/help` 图片版插件（`fuyubot.help`）。
//!
//! 它**替代框架内置 help 插件**：id 相同（`fuyubot.help`），并且继续提供同一个
//! 进程级共享服务 `fuyubot.help.commands`（`register` / `list`），所以 `/start`、
//! `/plugins`、adblock 等插件不用改一行代码就能把命令登记进来。
//!
//! ## 与内置 help 的差异
//!
//! | | 内置 help | 本插件 |
//! | --- | --- | --- |
//! | `/help` 回复 | Markdown 文本 | **图片**（命令表渲染） |
//! | 命令注册服务 | `fuyubot.help.commands` | 同名同方法，兼容 |
//! | 命令分组 | 按插件标归属 | **一张平表**（不分组） |
//! | 依赖 | 无 | **硬依赖** `fuyubot.html2img` |
//!
//! ## 硬依赖
//!
//! [`Plugin::inject`] 声明了 `fuyubot.html2img`（由 `fuyubot.html2img` 插件提供的
//! 进程级共享服务）。图片生成服务不在时本插件停在 Pending，不会静默退化成文字版。
//!
//! ## 安装
//!
//! 两者 id 相同，**不能同时装**。安装本插件前先把内置的平铺 DLL 移走 / 禁用：
//!
//! ```text
//! plugins/
//! ├── fuyubot_plugin_help.dll          ← 内置 help，必须先移走或改成 .disable
//! ├── fuyubot.help/                    ← 本插件（目录包）
//! │   ├── plugin.json
//! │   └── bin/fuyubot_plugin_help.dll
//! └── fuyubot.html2img/                ← 硬依赖
//!     ├── plugin.json
//!     └── bin/fuyubot_plugin_html2img.dll
//! ```
//!
//! ## 线程模型
//!
//! 宿主的 `updateNewMessage` 回调只解析命令，然后起一个 `fuyubot-help` 工作线程；
//! 同步渲染与等发送结果都在那个线程里。渲染用一把锁串行化，`stop()` 会 join 掉
//! 仍在跑的线程——动态库卸载之后不能让它们继续跑本插件的代码。

mod commands;
mod config;
mod log;
mod service;
mod template;

use std::sync::atomic::AtomicUsize;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use fuyubot_abi::{HostApi, Plugin, define_plugin};

use crate::config::Config;

/// 插件状态。
pub struct HelpPlugin {
    host: HostApi,
    /// `/help` 命令订阅：drop 就退订。
    subscription: Option<commands::EventSubscription>,
    /// `fuyubot.help.commands` 的服务表；`stop()` 放下即释放。
    service: Option<Box<service::HelpService>>,
    /// 启动时读到的配置快照。
    config: Config,
    /// 渲染是同步且吃 CPU 的：同一时刻只让一张图在渲染。
    render_lock: Arc<Mutex<()>>,
    /// 还在跑的 `/help` 工作线程：`stop()` 里 join 掉。
    workers: Arc<Mutex<Vec<JoinHandle<()>>>>,
    /// 正在跑 + 排队的 `/help` 数（上限见 [`commands::MAX_PENDING`]）。
    pending: Arc<AtomicUsize>,
    /// `start()` 是否真正跑完过：宿主探测插件时也会调 `plugin_stop`，
    /// 那时不该打「已停止」误导人。
    started: bool,
}

impl Plugin for HelpPlugin {
    fn init(host: HostApi) -> Result<Self, String> {
        log::info(&host, "help 插件初始化（图片版）");
        Ok(Self {
            host,
            subscription: None,
            service: None,
            config: Config::default(),
            render_lock: Arc::new(Mutex::new(())),
            workers: Arc::new(Mutex::new(Vec::new())),
            pending: Arc::new(AtomicUsize::new(0)),
            started: false,
        })
    }

    fn start(&mut self, config_json: &str) -> Result<(), String> {
        // `config_json` 是宿主给的环境信息（plugins_dir / spawn …），不是本插件配置；
        // 自己的配置在 `[plugin.fuyubot.help]`，由 `Config::from_host` 读。
        let _ = config_json;
        let host = self.host;
        let config = Config::from_host(&host);

        // 硬依赖的第二次确认：`inject()` 已经让宿主等过它，这里再用**不发请求**的
        // get_service 确认一次，避免服务在激活与 start 之间消失时静默降级。
        if host.get_service(config::HTML2IMG_SERVICE).is_null() {
            return Err(format!(
                "硬依赖 {} 不可用：请先启用并配置 fuyubot.html2img 插件",
                config::HTML2IMG_SERVICE
            ));
        }

        // 1) 把命令注册表暴露成进程级共享服务——与内置 help 的服务名、方法完全一致。
        let service = service::HelpService::new(host);
        let handle = host.provide_service_shared(service::SERVICE_NAME, service.vtable());
        if handle.is_valid() {
            self.service = Some(service);
        } else {
            log::error(
                &host,
                format!(
                    "注册共享服务 {} 失败：其它插件将无法向 /help 登记命令",
                    service::SERVICE_NAME
                ),
            );
        }

        // 2) 登记 /help 自己（其它命令由各插件经上面的服务登记）。
        service::register_command(
            "help",
            "这条帮助（/help text 出文字版）",
            Some(config::PLUGIN_NAME),
        );

        // 3) 订阅 /help；回调里只解析命令 + 起线程。
        let config_for_handler = config.clone();
        let render_lock = Arc::clone(&self.render_lock);
        let workers = Arc::clone(&self.workers);
        let pending = Arc::clone(&self.pending);
        let subscription = commands::subscribe_help(host, move |command| {
            if !command.is("help") {
                return;
            }
            // `/help t` / `/help text` 出文字版，其余（含裸 `/help`）出图。
            commands::spawn_help(
                host,
                config_for_handler.clone(),
                command.chat_id,
                command.message_id,
                Arc::clone(&render_lock),
                Arc::clone(&workers),
                Arc::clone(&pending),
                commands::HelpMode::from_args(&command.args),
            );
        });
        let subscription = subscription.ok_or_else(|| "订阅 updateNewMessage 失败".to_owned())?;
        self.subscription = Some(subscription);
        self.config = config;

        log::info(
            &host,
            format!(
                "help 已就绪（图片版）：{}×{} @{} → {}，依赖 {}，注册服务 {}",
                self.config.width,
                self.config.height,
                self.config.scale,
                self.config.format,
                config::HTML2IMG_SERVICE,
                service::SERVICE_NAME
            ),
        );
        if self.config.image.is_empty() {
            log::info(&host, "help 头图已关闭（image = \"\"）");
        }
        self.started = true;
        Ok(())
    }

    fn stop(&mut self) {
        // 先断输入，再等等在飞的工作线程：反过来会留下「线程在跑、新命令又进来」的窗口。
        let started = std::mem::replace(&mut self.started, false);
        self.subscription = None;
        self.service = None;

        let mut workers = self
            .workers
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if started && !workers.is_empty() {
            log::info(
                &self.host,
                format!("help 正在等待 {} 个出图任务结束", workers.len()),
            );
        }
        for handle in workers.drain(..) {
            let _ = handle.join();
        }
        drop(workers);

        if started {
            log::info(&self.host, "help 已停止");
        }
    }

    /// 硬依赖：没有 `fuyubot.html2img` 服务时不激活（停在 Pending）。
    fn inject() -> Vec<String> {
        vec![config::HTML2IMG_SERVICE.to_owned()]
    }

    fn display_name() -> &'static str {
        "帮助"
    }

    fn description() -> &'static str {
        "把 /help 渲染成图片（/help text 出文字版），按插件分组"
    }
}

// 一个 crate 只能展开一次 `define_plugin!`（七个导出符号名是 ABI 定死的）。
// id 与框架内置 help 相同：这是「替代品」而不是「另一个 help」。
define_plugin!(HelpPlugin, "fuyubot.help");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn html2img_is_a_hard_dependency() {
        assert_eq!(HelpPlugin::inject(), vec!["fuyubot.html2img"]);
    }

    #[test]
    fn service_name_matches_the_builtin_contract() {
        // 跨仓契约：名字写错不会报错，只会让所有插件静默登记失败。
        assert_eq!(service::SERVICE_NAME, "fuyubot.help.commands");
    }
}
