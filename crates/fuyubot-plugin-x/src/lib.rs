//! FuyuBot X / Twitter 文章解析插件（xiaoqvan.x）。
//!
//! 一条 x.com / twitter.com 链接进来，插件做三件事：
//!
//! 1. **取数据**：GET api.fxtwitter.com/status/ID（公开只读接口，不需要 token），
//!    拿回推文 + Article 的完整结构；
//! 2. **拆内容**：Article 的 Draft.js 文档拆成「按文档顺序的块」与「按文档顺序的图片 /
//!    视频」；
//! 3. **发出去**：图片视频按 Telegram 相册规则每组最多 10 个发送（超过就开下一条
//!    相册消息）；一个媒体都没有的纯文本文章，交给 fuyubot.html2img 渲染成一张
//!    X 帖子风格的卡片图。
//!
//! ## 模块
//!
//! | 模块 | 干什么 |
//! | --- | --- |
//! | [commands] | /x 命令、自动识别链接、宿主命令表登记 |
//! | [model] | 解析结果的形状 + FxTwitter JSON 提取 |
//! | [article] | Article 的 blocks / entityMap → 结构化块 + 有序媒体 |
//! | [render] | 块 → Telegram HTML / X 风格卡片 HTML，以及分片 |
//! | [send] | 相册分组（每组 10 个）、单条媒体、文字消息、卡片 |
//! | [service] | 对兄弟插件暴露 `xiaoqvan.x.render`（html / render） |
//! | [runtime] | 插件自有的 tokio 运行时 + 响应路由 + 停机 |
//! | [http] | 解析接口与媒体下载 |
//! | [simplify] | 「人话参数 → TDLib JSON」构造器 + 命令解析（**不依赖 fuyubot-simplify**） |
//! | [translate] | 非中文帖子调谷歌机翻，结果画进卡片 |
//! | [config] | [plugin.xiaoqvan.x] 配置段 |
//! | [log] | 进程级日志槽 |
//!
//! ## 软依赖
//!
//! fuyubot.html2img 是**软依赖**（soft_inject）：没装它插件照常解析、照常发图片和
//! 视频，只是纯文本文章退化成文字消息。装了它、但渲染失败，也一样回退（除非把
//! fallback_text 关掉）。
//!
//! ## 线程模型
//!
//! 宿主的 updateNewMessage 回调里只做「解析参数 + runtime.submit」，网络与 TDLib
//! 往返全在插件自有的 tokio 运行时里 await；stop() 按「退订 → 置取消 → 关闸等在飞
//! 归零 → 关 runtime」收摊（详见 [runtime] 的模块文档）。

mod article;
mod commands;
mod config;
mod http;
mod log;
mod model;
mod render;
mod runtime;
mod send;
mod service;
mod simplify;
mod translate;

use std::sync::Arc;

use fuyubot_abi::{HostApi, Plugin, define_plugin};

use crate::commands::MessageSubscription;
use crate::config::Config;
use crate::runtime::Runtime;

/// 插件状态。
pub struct XPlugin {
    host: HostApi,
    runtime: Option<Arc<Runtime>>,
    /// 外部渲染服务；vtable 必须活到插件卸载。
    service: Option<service::Registration>,
    /// 消息订阅守卫：drop 就退订。
    subscription: Option<MessageSubscription>,
    /// start() 是否真正跑完过：宿主探测插件时也会调 stop()，
    /// 那时不该打「已停止」误导人。
    started: bool,
}

impl Plugin for XPlugin {
    fn init(host: HostApi) -> Result<Self, String> {
        // 日志槽要尽早装：后面几层的日志都靠它。
        log::install(host);
        log::debug("x 插件初始化");
        Ok(Self {
            host,
            runtime: None,
            service: None,
            subscription: None,
            started: false,
        })
    }

    fn start(&mut self, config_json: &str) -> Result<(), String> {
        let config = Config::from_host(&self.host);
        let runtime = Runtime::new(self.host, config, config_json)?;
        self.runtime = Some(Arc::clone(&runtime));

        // 外部渲染服务：其他插件可以通过 xiaoqvan.x.render 复用卡片版式。
        match service::register(Arc::clone(&runtime)) {
            Ok(registration) => self.service = Some(registration),
            Err(error) => log::warn(&format!("外部渲染服务注册失败：{error}")),
        }

        // 一条订阅同时供 /x 与自动识别用：两者只是分流规则不同。
        self.subscription = Some(commands::subscribe(self.host, Arc::clone(&runtime))?);

        // 登记进宿主命令表（失败只记 Warn，不影响 /x 本身）。
        commands::register(self.host);

        let image = if runtime.image_service_available() {
            "可用"
        } else {
            "不可用（纯文本文章发文字）"
        };
        log::info(&format!(
            "已就绪：/{} 与自动识别都挂上了（相册每组 {} 个，图片生成服务 {}）",
            commands::COMMAND,
            runtime.config.album_limit,
            image
        ));
        self.started = true;
        Ok(())
    }

    fn stop(&mut self) {
        // 先断输入（守卫 drop 就退订），再停任务。
        self.subscription = None;
        if let Some(runtime) = self.runtime.take() {
            runtime.shutdown();
        }
        if self.started {
            log::info("已停止");
        }
        self.started = false;
    }

    /// 软依赖：没什么都不激活，只是排在它后面加载、拿不到就降级。
    fn soft_inject() -> Vec<String> {
        vec![config::HTML2IMG_SERVICE.to_owned()]
    }

    fn display_name() -> &'static str {
        "X 解析"
    }

    fn description() -> &'static str {
        "解析 X / Twitter 文章：图片视频按相册发送，纯文本出 X 风格卡片"
    }
}

// 一个 crate 只能展开一次 define_plugin!（七个导出符号名是 ABI 定死的）。
define_plugin!(XPlugin, "xiaoqvan.x");

/// 本地预览用：把一份完整的 FxTwitter 响应渲染成卡片 HTML。
///
/// 插件运行时不走这里（那条路要先判「有没有媒体」），它只是把同一份
/// [render::card_html] 暴露给 examples/preview.rs，方便不连宿主就调版式：
///
/// ```bash
/// cargo run -p fuyubot-plugin-x --example preview -- out.html
/// ```
pub fn preview_card_html(api_json: &str) -> Result<String, String> {
    let json: serde_json::Value =
        serde_json::from_str(api_json).map_err(|error| format!("不是合法 JSON：{error}"))?;
    let post = model::from_api(&json)?;
    Ok(render::card_html(&post, &Config::default()))
}

/// 和上面一样，但可以塞一段机翻文本预览「谷歌翻译」块。
pub fn preview_card_html_with_translation(
    api_json: &str,
    translation: Option<&str>,
) -> Result<String, String> {
    preview_card_html_with_footer(api_json, translation, None, None)
}

/// 和上面一样，再多传一段底部署名（预览用，运行时由宿主身份服务提供）。
pub fn preview_card_html_with_footer(
    api_json: &str,
    translation: Option<&str>,
    footer: Option<&str>,
    footer_avatar: Option<&str>,
) -> Result<String, String> {
    let json: serde_json::Value =
        serde_json::from_str(api_json).map_err(|error| format!("不是合法 JSON：{error}"))?;
    let post = model::from_api(&json)?;
    Ok(render::card_html_with_footer(
        &post,
        &Config::default(),
        translation,
        footer,
        footer_avatar,
        None,
    ))
}

#[cfg(test)]
#[path = "../tests/unit/lib.rs"]
mod tests;
