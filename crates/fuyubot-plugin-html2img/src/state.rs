//! 插件运行期共享状态：配置、临时目录、文件名计数。
//!
//! 服务方法拿到的都是同一份 `Arc<Shared>`（通过 vtable 的 `context` 传进来），
//! 所以这里只放**线程安全**的东西（`AtomicU64` / 不可变配置）。

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

use fuyubot_abi::HostApi;

use crate::config::Config;
use crate::log;

/// 渲染产物的保留时长。文件返回给调用方使用，过期后由插件清理。
const TEMP_TTL: Duration = Duration::from_secs(24 * 60 * 60);

/// 进程里所有服务调用共享的状态。
pub struct Shared {
    pub host: HostApi,
    pub config: Config,
    /// 渲染产物落盘的目录（`<系统临时目录>/fuyubot-html2img`）。
    pub temp_dir: PathBuf,
    /// 文件名去重（同一毫秒里两次渲染也不会撞名）。
    counter: AtomicU64,
}

impl Shared {
    /// 建共享状态：创建临时目录，并清理上一次进程留下的过期文件。
    ///
    /// `plugins_dir` 是宿主在 `plugin_start` 的 config 里给的插件目录，只在
    /// 「系统临时目录不可写」时用来兜底（见 [`resolve_temp_dir`]）。
    pub fn new(
        host: HostApi,
        config: Config,
        plugins_dir: Option<&Path>,
    ) -> Result<Arc<Self>, String> {
        let (temp_dir, fallback) = resolve_temp_dir(&config, plugins_dir)?;
        if fallback {
            log::warn(
                &host,
                format!(
                    "系统临时目录不可写，改用插件数据目录 {}（可在配置里用 temp_dir 指定）",
                    temp_dir.display()
                ),
            );
        }

        let shared = Arc::new(Self {
            host,
            config,
            temp_dir,
            counter: AtomicU64::new(0),
        });
        shared.cleanup_expired();
        Ok(shared)
    }

    /// 给这次渲染分配一个临时文件路径（扩展名跟着输出格式走）。
    pub fn next_path(&self, extension: &str) -> PathBuf {
        let sequence = self.counter.fetch_add(1, Ordering::Relaxed);
        self.temp_dir.join(format!(
            "html2img_{}_{sequence}.{extension}",
            std::process::id()
        ))
    }

    /// 把渲染产物写到临时目录。
    pub fn write_bytes(&self, path: &Path, bytes: &[u8]) -> Result<(), String> {
        fs::write(path, bytes).map_err(|error| format!("写 {} 失败：{error}", path.display()))
    }

    /// 清理超过 [`TEMP_TTL`] 的旧产物（只动本插件的 `.png`）。
    pub fn cleanup_expired(&self) {
        let Ok(entries) = fs::read_dir(&self.temp_dir) else {
            return;
        };
        let now = SystemTime::now();
        let mut removed = 0usize;
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|value| value.to_str()) != Some("png") {
                continue;
            }
            let expired = entry
                .metadata()
                .and_then(|metadata| metadata.modified())
                .ok()
                .and_then(|modified| now.duration_since(modified).ok())
                .is_some_and(|age| age > TEMP_TTL);
            if expired && fs::remove_file(&path).is_ok() {
                removed += 1;
            }
        }
        if removed > 0 {
            log::info(
                &self.host,
                format!("清理了 {removed} 个超过 24 小时的临时图片"),
            );
        }
    }
}

/// 选一个可写的落盘目录。
///
/// 优先级：配置里的 `temp_dir` → 系统临时目录 → 插件数据目录
/// （`<plugins_dir>/../data/plugins/fuyubot.html2img/tmp`）。
/// 第二个返回值表示用的是不是兜底目录，调用方据此决定要不要打 warn。
fn resolve_temp_dir(
    config: &Config,
    plugins_dir: Option<&Path>,
) -> Result<(PathBuf, bool), String> {
    if let Some(dir) = &config.temp_dir {
        fs::create_dir_all(dir)
            .map_err(|error| format!("创建 temp_dir {} 失败：{error}", dir.display()))?;
        return Ok((dir.clone(), false));
    }

    let default = std::env::temp_dir().join("fuyubot-html2img");
    match fs::create_dir_all(&default) {
        Ok(()) => Ok((default, false)),
        Err(default_error) => {
            if let Some(plugins_dir) = plugins_dir {
                let dir = plugins_dir
                    .join("..")
                    .join("data")
                    .join("plugins")
                    .join("fuyubot.html2img")
                    .join("tmp");
                if fs::create_dir_all(&dir).is_ok() {
                    // 日志里别出现 `../..`；规范化失败就保留原路径。
                    let dir = fs::canonicalize(&dir).unwrap_or(dir);
                    return Ok((strip_verbatim(dir), true));
                }
            }
            Err(format!(
                "创建临时目录 {} 失败：{default_error}；插件数据目录也不可写，请在配置里设置 temp_dir",
                default.display()
            ))
        }
    }
}

/// Windows 的 `canonicalize` 会带 `\\\\?\\` 前缀。留着它日志难看，返回给调用方的
/// 路径也应该是普通 UTF-8 路径。
fn strip_verbatim(path: PathBuf) -> PathBuf {
    #[cfg(windows)]
    {
        let text = path.to_string_lossy();
        if let Some(rest) = text.strip_prefix(r"\\\\?\\UNC\\") {
            return PathBuf::from(format!(r"\\\\{rest}"));
        }
        if let Some(rest) = text.strip_prefix(r"\\\\?\\") {
            return PathBuf::from(rest);
        }
    }
    path
}
