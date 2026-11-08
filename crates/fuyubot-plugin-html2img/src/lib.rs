//! FuyuBot HTML 转图片插件（`fuyubot.html2img`）：**只提供服务**，不注册命令。
//!
//! 渲染器是 [hyper-render](https://github.com/thomasmost/hyper-render) 的本地
//! vendored fork——纯 Rust 的 Blitz + Stylo + Taffy + Vello CPU，**不需要 Chromium、
//! 不需要外部进程、不需要 Node**。插件本身也是纯 Rust cdylib，跟其它插件同一套
//! 构建 / 打包流程。
//!
//! ## 它做什么
//!
//! 插件不订阅聊天更新、不注册工具，唯一对外的东西是进程级共享服务
//! `fuyubot.html2img` 的一个方法 `render`：入参是一段 HTML（或 Vue 模板 + 数据）与
//! 尺寸选项，出参是渲染好的 **PNG / JPEG** 的**本地文件路径**，默认输出 JPEG。
//! 别的插件在自己的代码里这样调：
//!
//! ```ignore
//! let reply = host.call_service(
//!     "fuyubot.html2img",
//!     "render",
//!     r#"{"html":"<h1>你好</h1>","width":640,"scale":2.0}"#,
//! )?;
//! let image: serde_json::Value = serde_json::from_str(&reply)?;
//! let path = image["path"].as_str().unwrap_or_default();
//! ```
//!
//! 渲染器不负责发送：图片怎么用（发聊天、当附件、二次处理）由调用方决定。
//!
//! ## 服务契约
//!
//! | 项 | 内容 |
//! | --- | --- |
//! | 服务名 | `fuyubot.html2img`（`provide_service_shared`，跨插件可见） |
//! | 方法 | `render` |
//! | 入参 | `{"html": "...", "vue"?: bool, "data"?: object, "width"?: u32, "height"?: u32, "scale"?: f32, "dark"?: bool, "transparent"?: bool, "auto_height"?: bool, "format"?: "png"\|"jpg"}`，`html` 必填 |
//! | 成功 | `{"ok": true, "path": "…jpg", "format": "jpg", "width": 960, "height": 496, "bytes": 12345}` |
//! | 失败 | `{"ok": false, "error": "……"}` |
//!
//! `vue = true` 时 `html` 按 Vue 模板编译、用 `data` 渲染；`format` 不给就用配置里的
//! 默认值（默认 `jpg`）。成功返回的是本地路径，文件保留 24 小时（过期文件由插件启动时清理）。
//! 细节见 [`service`]。
//!
//! ## 线程模型
//!
//! 服务方法是**同步**的：渲染在调用方线程上直接跑完。渲染没有超时中断，所以单次
//! 请求有硬上限：HTML 字节数、宽高、scale、总像素预算，超过就返回错误而不渲染。
//!
//! ## 配置
//!
//! 配置段 `[plugin.fuyubot.html2img]`，字段见 [`config`]；服务入参里没给的尺寸
//! 选项都从这里取默认值。

mod config;
mod log;
mod render;
mod service;
mod state;

use std::ffi::c_void;
use std::sync::Arc;

use fuyubot_abi::{HostApi, Plugin, define_plugin};
use serde_json::Value;

use crate::config::Config;
use crate::state::Shared;

/// 插件状态。
///
/// 共享状态本身由 vtable 的 `context` 持有（见 `start_inner`），这里只留宿主的
/// 句柄用来打日志。
pub struct Html2ImgPlugin {
    host: HostApi,
}

impl Html2ImgPlugin {
    fn start_inner(&mut self, config_json: &str) -> Result<(), String> {
        let config = Config::from_host(&self.host);
        if !config.within_budget(config.width, config.height, config.scale) {
            log::warn(
                &self.host,
                format!(
                    "默认尺寸 {}×{} @{} 超过像素上限 {}，单次请求会被拒绝；请调小 width/height/scale",
                    config.width, config.height, config.scale, config.max_pixels
                ),
            );
        }

        // 宿主给的运行环境信息里有 `plugins_dir`，系统临时目录不可写时拿它兜底。
        let environment: Value = serde_json::from_str(config_json).unwrap_or(Value::Null);
        let plugins_dir = environment
            .get("plugins_dir")
            .and_then(Value::as_str)
            .map(std::path::PathBuf::from);

        let shared = Shared::new(self.host, config, plugins_dir.as_deref())?;

        // 服务表里放的是 `Arc<Shared>` 的裸指针：服务方法同步、可能从任意宿主线程
        // 进来，共享状态必须活到进程结束，所以 `Arc::into_raw` 之后不再收回。
        let context = Arc::into_raw(Arc::clone(&shared)) as *mut c_void;
        if let Err(error) = service::build(context).register_shared(&self.host) {
            // SAFETY: 指针刚由 `Arc::into_raw` 产生，还没交给宿主。
            drop(unsafe { Arc::from_raw(context as *const Shared) });
            return Err(error);
        }

        log::info(
            &self.host,
            format!(
                "html2img 服务已就绪：{}（默认 {}×{} @{}，auto_height={}，dark={}，临时目录 {}）",
                service::SERVICE_NAME,
                shared.config.width,
                shared.config.height,
                shared.config.scale,
                shared.config.auto_height,
                shared.config.dark,
                shared.temp_dir.display()
            ),
        );
        Ok(())
    }
}

impl Plugin for Html2ImgPlugin {
    fn init(host: HostApi) -> Result<Self, String> {
        log::info(&host, "html2img 插件初始化");
        Ok(Self { host })
    }

    fn start(&mut self, config_json: &str) -> Result<(), String> {
        // `config_json` 是宿主给的环境信息（plugins_dir / spawn …），不是本插件配置；
        // 自己的配置在 `[plugin.fuyubot.html2img]`，由 `Config::from_host` 读。
        match self.start_inner(config_json) {
            Ok(()) => Ok(()),
            Err(error) => {
                log::error(&self.host, format!("html2img 启动失败：{error}"));
                Err(error)
            }
        }
    }

    fn stop(&mut self) {
        // 服务注册由宿主在 `plugin_stop` 之后回滚；泄漏出去的共享状态不回收，
        // 避免这期间迟到的调用摸到已释放的内存。
        log::info(&self.host, "html2img 已停止");
    }
}

// 一个 crate 只能展开一次 `define_plugin!`（七个导出符号名是 ABI 定死的）。
define_plugin!(Html2ImgPlugin, "fuyubot.html2img");
