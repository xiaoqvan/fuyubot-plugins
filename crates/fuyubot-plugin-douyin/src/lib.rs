//! FuyuBot 抖音解析插件。
//!
//! 从 TS 版 `plugins/video_parse` 的抖音分支迁过来的模块：
//!
//! | 模块 | 对应 TS | 干什么 |
//! | --- | --- | --- |
//! | [`runtime`] | —（旧版是 `std::thread`） | 插件自有的 tokio 多线程运行时 + 共享状态（HTTP / 路由器 / 缓存 / 取消标志） |
//! | [`http`] | `axios` 调用 | 短链取 302 / 分享页取正文 / 流式下载（`reqwest` async） |
//! | [`model`] | `types/douyin.d.ts` | 解析结果的形状 |
//! | [`parse`] | `parse/douyin/getvideo.ts` | cookie、SSX 提取、`bit_rate` 挑清晰度 |
//! | [`sign`] | —（新增） | 抖音签名栈（SecSDK web 签名 / SM3 / a_bogus）：API 解析路线的门禁 |
//! | [`send`] | `utils/videomsg.ts` | 标题构造、发视频 / 发图集 / 缓存命中重发 |
//! | [`cached`] | —（新增） | 缓存文档：发送结果 → 文件 id 文档 → 重建 `inputFileId` 媒体 |
//! | [`cache`] | —（新增） | 软依赖 `fuyubot.sqlite` 的同步 SQL 缓存（全部调用包 `spawn_blocking`） |
//! | [`simplify`] | —（内联） | 「人话参数 → TDLib JSON」构造器 + 命令解析 + `sent_media`（**不依赖 `fuyubot-simplify`**） |
//! | [`commands`] | `index.ts` + `parse/douyin/index.ts` | `/dy` 命令与消息里的自动识别 |
//!
//! **快手 / YouTube / 内联查询没有跟着迁**——本工单只覆盖抖音那一条分支。
//!
//! ## 执行模型
//!
//! 宿主回调里只做「克隆参数 + `runtime.submit`」，网络与 TDLib 往返都在插件自己的
//! tokio 运行时里 `.await`；`stop()` 按「退订 → 置取消 → 关闸等在飞归零 → 关 runtime」
//! 收摊（详见 [`runtime`] 的模块文档）。
//!
//! ## SQL 缓存（软依赖）
//!
//! 装了 `fuyubot.sqlite` 时，发送成功后按作品 id 缓存 TDLib 文件 id 与文案；
//! 下次同一个作品直接 `inputFileId` 重发，**不解析、不下载**。没有 SQL 插件时
//! 一切照常，只是没有加速（见 [`cache`] 与 [`commands`] 的模块文档）。
//!
//! ## 装与用
//!
//! ```text
//! cargo build --release
//! fuyubot plugin install target/release/fuyubot_plugin_douyin.dll
//! ```
//!
//! ## cookie 配置
//!
//! 配在全局配置文件里本插件自己的段（**配置优先**）：
//!
//! ```toml
//! [plugin.fuyubot.douyin]
//! cookies = "ttwid=…; msToken=…; odin_tt=…"
//! ```
//!
//! 没配才回退旧文件 `plugins/douyin/cookies.json`（与 TS 版同格式，仍兼容）。
//! 读取顺序与错误提示见 [`parse::read_cookies`]；插件目录由宿主在 `plugin_start`
//! 的 `plugins_dir` 里给——插件自己是 cdylib，算不出这个路径。

pub mod cache;
pub mod cached;
pub mod commands;
pub mod http;
pub mod log;
pub mod model;
pub mod parse;
pub mod runtime;
pub mod send;
pub mod sign;
pub mod simplify;

use fuyubot_abi::define_plugin;

pub use commands::DouyinPlugin;

// 一个 crate 只能展开一次 `define_plugin!`（七个导出符号名是 ABI 定死的）。
define_plugin!(DouyinPlugin, "fuyubot.douyin");
