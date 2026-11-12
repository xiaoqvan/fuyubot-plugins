//! FuyuBot 音乐解析插件：网易云音乐 + 汽水音乐。
//!
//! 只做三件事，别的都不做：
//!
//! | 能力 | 入口 |
//! | --- | --- |
//! | 搜索 | `/music163 关键词`、`/soda 关键词` |
//! | 解析分享链接 / 歌曲 ID | `/music163 <链接或 ID> [音质]`、`/soda <链接或 ID> [音质]` |
//! | 指定音质取流 → 发音频 | 同上，第二段参数是音质 |
//!
//! 消息里出现两家平台的分享链接时也会自动解析（`/` 开头的不抢）。
//!
//! ## 执行模型：全异步
//!
//! 插件在 `start()` 里建一个**自己的 tokio 多线程 runtime**（4 个 `music-worker`）。
//! 宿主的 `on_command` / `updateNewMessage` 回调里只做「解析参数 + `Runtime::submit`」，
//! 不做任何网络 / 磁盘 / TDLib 请求。所有 HTTP 都是 **reqwest 0.13 async**
//! （搜索 / 详情 / 取流是小 JSON；音频 / 视频下载是流式 `chunk()` + 取消检查），
//! TDLib 响应一律 `ResponseRouter` + `tokio::time::timeout` 等。
//! 汽水的 CENC 解密是 CPU + 磁盘密集的大文件操作，放 `spawn_blocking` 且逐样本
//! 检查取消标志。
//!
//! 停机顺序：**退订 → 置取消标志 → 关闸并等在飞任务归零（限时 30s）→
//! `shutdown_timeout(5s)`**。详见 [`runtime`] 的模块文档（动态库卸载安全）。
//!
//! ## SQL 缓存（软依赖 `fuyubot.sqlite`）
//!
//! 装了 SQLite 插件时，发送成功后把该次音质对应的 TDLib 文件 id / 封面 id 与文本
//! 信息写进 `media_cache`；下次同一个「音乐 id + 音质」命中就直接用 `inputFileId`
//! 发送，**不再请求接口、不再下载**。没装 / 坏掉时一切照常（只是没有加速）。
//!
//! ## 模块
//!
//! | 模块 | 干什么 |
//! | --- | --- |
//! | [`log`] | 进程级日志槽（解析层拿不到 `HostApi`） |
//! | [`runtime`] | tokio 执行引擎 + 响应路由 + HTTP 客户端 + SQL 缓存 |
//! | [`http`] | reqwest async 请求 + 流式下载落盘 |
//! | [`auth`] | 从 `fuyubot.toml` 的 `[plugin.fuyubot.music]` 读 cookie / 设备指纹（旧文件兼容） |
//! | [`cache`] | SQL 文件 id 缓存（软依赖 `fuyubot.sqlite`） |
//! | [`simplify`] | 内联的裁剪版 simplify（参数 → TDLib JSON、命令解析、取已发送文件 id） |
//! | [`netease`] | eapi（AES-128-ECB 签名）+ 搜索 / 详情 / 取流 |
//! | [`soda`] | 汽水：搜索 / 分享链接 / 取流 / 音质 |
//! | [`soda_crypto`] | 汽水 `play_auth`（spade）还原 + MP4/CENC（AES-128-CTR）解密 |
//! | [`soda_sign`] | 汽水 App 端点的逐请求签名（外部签名器接缝） |
//! | [`commands`] | 命令、自动识别、异步处理链、缓存命中 / 回落 |
//! | [`send`] | 发音频 / 视频 / 文本 |
//!
//! ## 装
//!
//! ```text
//! cargo build --release
//! fuyubot plugin install target/release/fuyubot_plugin_music.dll
//! ```
//!
//! cookie 与设备指纹放在 `fuyubot.toml` 的 `[plugin.fuyubot.music]` 配置段
//! （旧版 `plugins/music/*.json` 仍兼容），格式见 `README.md`。
//! 插件是 cdylib，算不出宿主工作目录，所以 `plugins_dir` 由宿主在
//! `plugin_start` 的 config 里给——旧文件从它下面的 `music/` 找。

pub mod auth;
pub mod cache;
pub mod commands;
pub mod http;
pub mod log;
pub mod netease;
pub mod runtime;
pub mod send;
pub mod simplify;
pub mod soda;
pub mod soda_crypto;
pub mod soda_sign;
pub mod soda_video;

use fuyubot_abi::define_plugin;

pub use commands::MusicPlugin;

/// MD5 十六进制（小写）。网易云 eapi 与汽水的 `X-SS-STUB` 都用它。
pub fn md5_hex(text: &str) -> String {
    use md5::{Digest, Md5};
    let mut hasher = Md5::new();
    hasher.update(text.as_bytes());
    let digest = hasher.finalize();
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// 当前 Unix 毫秒时间戳。设备指纹兜底与请求 ID 都靠它。
pub fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

// 一个 crate 只能展开一次 `define_plugin!`（七个导出符号名是 ABI 定死的）。
define_plugin!(MusicPlugin, "fuyubot.music");
