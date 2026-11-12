//! FuyuBot 消息日志插件（`fuyubot.message-log`）。
//!
//! 把 `updateNewMessage` 拼成**一行**，经 `host.log(Info, …)` 打出去。宿主负责加上
//! 时间、来源与级别，所以控制台 / TUI 日志页 / `logs/fuyubot.log` 里看到的是：
//!
//! ```text
//! 2026-11-11 13:45:00 [plugins - fuyubot.message-log] INFO  [开发群(id:-1001234567890)][张三(id:123456)]: 你好
//! ```
//!
//! 这不只是「换个输出通道」：TS 版自带 pino + 按日轮转的 `logs/message.*.log`，
//! 而 FuyuBot 的插件日志本来就会同时进控制台、日志页和 `logs/fuyubot.log`，
//! 级别还受 `[plugins].log_level` 统一控制。再写一份文件只会得到两套不一致的日志。
//!
//! ## 做什么 / 不做什么
//!
//! - 只订阅 `updateNewMessage`，不注册命令、不发消息、不动消息；
//! - 文本 / 富消息 / 图片 / 视频 / 文件 / 音频 / 语音 / 动态表情 / 贴纸 / 联系人 /
//!   位置 / 地点 / 投票 / 骰子 / 游戏 / 故事 / 通话都有可读的正文；
//! - 回复、引用、转发、话题、频道身份都按 TS 版的口径拼成方括号标签；
//! - 正文里的换行转成字面量 `\n`，控制字符转义——一条消息永远只占一行，
//!   也不会把 ANSI 转义序列带进终端或日志文件。
//!
//! ## 线程模型
//!
//! 宿主回调只 `try_send` 一条更新（非阻塞），工作线程用 current-thread tokio
//! runtime 顺序处理：名字缓存未命中时 `getChat` / `getUser`，然后打一行日志。
//! `stop()` 先退订，再处理完手上这一条并 `join`（队列里的积压直接丢弃）。细节见 [`worker`]。
//!
//! ## 配置
//!
//! ```toml
//! [plugin.fuyubot.message-log]
//! enabled = true
//! ignore_outgoing = false
//! max_text_chars = 4096
//! ```

mod config;
mod log;
mod render;
mod subscribe;
mod td;
mod worker;

use fuyubot_abi::{HostApi, Plugin, ResponseRouter, define_plugin};

/// 插件状态。
pub struct MessageLogPlugin {
    host: HostApi,
    /// 事件订阅：drop 就退订。**必须声明在 `worker` 前面**——Rust 按声明顺序
    /// drop 字段，订阅闭包先放手，工作线程才能看到所有发送端都断开。
    subscription: Option<subscribe::EventSubscription>,
    worker: Option<worker::Worker>,
    /// `start()` 是否真正跑完过：宿主探测插件时也会调 `plugin_stop`，
    /// 那时不该打「已停止」误导人。
    started: bool,
}

impl Plugin for MessageLogPlugin {
    fn init(host: HostApi) -> Result<Self, String> {
        log::info(&host, "消息日志插件初始化");
        Ok(Self {
            host,
            subscription: None,
            worker: None,
            started: false,
        })
    }

    fn start(&mut self, config_json: &str) -> Result<(), String> {
        // `config_json` 是宿主给的环境信息（plugins_dir / spawn …），不是本插件配置；
        // 自己的配置在 `[plugin.fuyubot.message-log]`，由 `Config::from_host` 读。
        let _ = config_json;
        let config = config::Config::from_host(&self.host);

        if !config.enabled {
            log::info(
                &self.host,
                "消息日志已关闭（enabled = false），不订阅 updateNewMessage",
            );
            self.started = true;
            return Ok(());
        }

        // 响应路由器必须在 `start()` 里建：宿主作用域栈顶此刻是本插件，
        // 之后在工作线程上发的请求才能归属到本插件名下。
        let router =
            ResponseRouter::new(self.host).map_err(|error| format!("建响应路由器失败：{error}"))?;
        let worker = worker::Worker::start(self.host, router, config.clone())?;

        // 回调只做一次非阻塞投递；真正的活在工作线程里。
        let handle = worker.handle();
        let subscription =
            match subscribe::subscribe(self.host, "updateNewMessage", None, move |update| {
                handle.submit(update)
            }) {
                Some(subscription) => subscription,
                None => {
                    // 没订上就别把线程留到 `start()` 失败之后：先 join 再返回。
                    worker.shutdown();
                    return Err("订阅 updateNewMessage 失败".to_owned());
                }
            };

        self.worker = Some(worker);
        self.subscription = Some(subscription);
        self.started = true;
        log::info(&self.host, format!("消息日志已就绪：{}", config.summary()));
        Ok(())
    }

    fn stop(&mut self) {
        // 先断输入，再等线程：反过来会留下「线程还在跑、新消息又进来」的窗口。
        let started = std::mem::replace(&mut self.started, false);
        self.subscription = None;
        let recorded = self
            .worker
            .take()
            .map(|worker| worker.shutdown())
            .unwrap_or(0);
        if started {
            log::info(
                &self.host,
                format!("消息日志已停止（共记录 {recorded} 条）"),
            );
        }
    }

    fn display_name() -> &'static str {
        "消息日志"
    }

    fn description() -> &'static str {
        "把收到的新消息按框架日志格式打印到控制台和 logs/fuyubot.log"
    }
}

// 一个 crate 只能展开一次 `define_plugin!`（七个导出符号名是 ABI 定死的）。
define_plugin!(MessageLogPlugin, "fuyubot.message-log");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plugin_id_matches_the_config_section() {
        // 跨仓契约：`define_plugin!` 的名字、`[plugin.<名>]` 段、日志来源词缀是同一个。
        assert_eq!(config::PLUGIN_NAME, "fuyubot.message-log");
    }

    #[test]
    fn plugin_is_loaded_without_hard_or_soft_dependencies() {
        assert!(MessageLogPlugin::inject().is_empty());
        assert!(MessageLogPlugin::soft_inject().is_empty());
    }
}
