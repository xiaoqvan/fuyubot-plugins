use super::*;

/// 缓存目录 = 插件目录的上一级下的 `cache/`。
/// 插件目录就是项目里的 `plugins/`，所以上一级正是宿主的工作目录——
/// 这与 TS 版 `path.resolve(process.cwd(), "cache")` 是同一处。
#[test]
fn cache_dir_sits_next_to_the_plugins_dir() {
    assert_eq!(
        cache_dir(Path::new("D:/bots/demo/plugins")),
        PathBuf::from("D:/bots/demo/cache")
    );
    // 没有父目录（根）时退回相对路径，不能 panic
    assert_eq!(cache_dir(Path::new("/")), PathBuf::from("cache"));
}

/// 文件名规则照搬 TS 版：取 URL 路径最后一段，非法字符换 `_`。
#[test]
fn cache_name_takes_the_url_basename() {
    assert_eq!(
        cache_name("https://x.com/a/b/v0200fg.mp4?x=1", "mp4"),
        "v0200fg.mp4"
    );
    assert_eq!(
        cache_name("https://x.com/a/b/cover.jpeg", "jpg"),
        "cover.jpeg.jpg"
    );
}

/// ⚠️ 抖音的视频 URL 是 `…/play/?video_id=…`——** `play` 后面还有个斜杠**，
/// 所以 URL 路径的最后一段其实是**空的**（`path.posix.basename("/aweme/v1/play/")`
/// 得到 `""`）。TS 版这时同样会走 `|| file_${Date.now()}` 兜底。
/// 这个行为看着奇怪，但两边一致，别为了「好看」去改实现——改了就跟 TS 版对不上了。
#[test]
fn a_trailing_slash_leaves_no_basename() {
    let name = cache_name(
        "https://aweme.snssdk.com/aweme/v1/play/?video_id=abc",
        "mp4",
    );
    assert!(name.starts_with("file_"), "{name}");
    assert!(name.ends_with(".mp4"), "{name}");
}

/// 抖音的图片 URL 带 `:540:q75` 这种变换后缀——冒号在 Windows 文件名里非法，
/// 不换会写盘失败（TS 版就是为这个才加的替换）。
///
/// 注意 `~` **不在**替换表里：TS 版只换 `<>:"/\\|?*` 和控制字符，`~` 是合法的。
#[test]
fn illegal_windows_characters_are_replaced() {
    assert_eq!(
        cache_name("https://p3.douyinpic.com/img/obj~540:q75.jpeg", "jpg"),
        "obj~540_q75.jpeg.jpg",
        "冒号换掉，波浪号保留（TS 版同）"
    );
    // 反斜杠在 Rust 字符字面量里要写成 '\\'
    for bad in ['<', '>', '"', '/', '\\', '|', '?', '*'] {
        let name = cache_name(&format!("https://x.com/a{bad}b"), "mp4");
        assert!(!name.contains(bad), "{bad} 应被替换：{name}");
    }
}

/// TS 版末尾那条 `replace(/[. ]+$/g, "")`：末尾的点和空格要去掉，
/// 否则 `abc..mp4` 这种名字在 Windows 上会被当成怪扩展名。
#[test]
fn trailing_dots_and_spaces_are_stripped() {
    assert_eq!(cache_name("https://x.com/abc..", "mp4"), "abc.mp4");
    assert_eq!(cache_name("https://x.com/abc  ", "mp4"), "abc.mp4");
}

/// 路径里取不出名字时（比如 URL 以 `/` 结尾）用时间戳兜底，不能叫 `.mp4`。
#[test]
fn an_empty_basename_falls_back_to_a_timestamp() {
    let name = cache_name("https://x.com/", "mp4");
    assert!(name.starts_with("file_"), "{name}");
    assert!(name.ends_with(".mp4"), "{name}");
    assert!(name.len() > "file_.mp4".len(), "时间戳不能是空的：{name}");
}

/// 扩展名必须干净地落在末尾——TDLib 靠它判 MIME，
/// 之前 `set_extension("mp4.<时间戳>")` 让发送失败过。
#[test]
fn the_extension_is_clean_and_last() {
    for (url, ext) in [
        ("https://x.com/a/b/play", "mp4"),
        ("https://x.com/a/b/cover", "jpg"),
    ] {
        let name = cache_name(url, ext);
        assert!(
            name.ends_with(&format!(".{ext}")),
            "{name} 应以 .{ext} 结尾"
        );
        // 末尾扩展名之后不能再有别的点
        let after = name.rsplit_once('.').map(|(_, tail)| tail).unwrap_or("");
        assert_eq!(after, ext, "{name} 的扩展名应该就是 {ext}");
    }
}

#[test]
fn kind_maps_to_a_real_extension() {
    assert_eq!(Kind::Video.extension(), "mp4");
    assert_eq!(Kind::Image.extension(), "jpg");
}

/// 撞名重试的序号插在扩展名**之前**：`file_1.mp4` → `file_1-2.mp4`。
/// 顺序反了（变成 `.mp4-2`）TDLib 就认不出这是视频了。
#[test]
fn the_retry_suffix_goes_before_the_extension() {
    assert_eq!(with_suffix("file_1.mp4", "mp4", 2), "file_1-2.mp4");
    assert_eq!(with_suffix("cover.jpg", "jpg", 3), "cover-3.jpg");
    // 没有预期扩展名时至少别把名字搞坏
    assert_eq!(with_suffix("weird", "mp4", 1), "weird-1");
}

// ── 流式落盘的假流（不联网）──────────────────────────────────

/// 一次吐出 `total` 字节（分 `chunk` 大小的块），内容全是 `byte`。
struct Feed {
    left: u64,
    chunk: usize,
    byte: u8,
}

impl ChunkSource for Feed {
    async fn next_chunk(&mut self) -> Result<Option<Vec<u8>>, String> {
        if self.left == 0 {
            return Ok(None);
        }
        let n = (self.left as usize).min(self.chunk);
        self.left -= n as u64;
        Ok(Some(vec![self.byte; n]))
    }
}

/// 一个字都不吐。
struct Empty;

impl ChunkSource for Empty {
    async fn next_chunk(&mut self) -> Result<Option<Vec<u8>>, String> {
        Ok(None)
    }
}

/// 先吐 1 MiB，然后报错——比真的掐网络好造。
struct Broken(u64);

impl ChunkSource for Broken {
    async fn next_chunk(&mut self) -> Result<Option<Vec<u8>>, String> {
        if self.0 == 0 {
            return Err("模拟 CDN 掐连接".to_owned());
        }
        let n = (self.0 as usize).min(64 * 1024);
        self.0 -= n as u64;
        Ok(Some(vec![b'x'; n]))
    }
}

fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("dy-{tag}-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    dir
}

fn leftover(dir: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(dir)
        .map(|entries| entries.flatten().map(|e| e.path()).collect())
        .unwrap_or_default()
}

/// 12 MiB 的流必须完整落盘：**这条是「大视频下不下来」的核心回归测试**
/// （旧实现走 ureq 的 `read_to_vec`，写死 10 MiB 上限）。
#[tokio::test]
async fn a_12_mib_stream_lands_on_disk_completely() {
    const SIZE: u64 = 12 * 1024 * 1024;
    let dir = temp_dir("big");
    let cancel = AtomicBool::new(false);
    let mut source = Feed {
        left: SIZE,
        chunk: 64 * 1024,
        byte: 0,
    };
    let (path, written) =
        write_stream(&dir, "https://x.com/a/big.mp4", "mp4", &mut source, &cancel)
            .await
            .expect("大文件必须写得下来");
    assert_eq!(written, SIZE, "写入字节数要跟源一样多");
    let on_disk = std::fs::metadata(&path).expect("文件应当真的存在").len();
    assert_eq!(on_disk, SIZE, "落盘大小必须完整，不能被截断");
    assert!(path.extension().is_some_and(|ext| ext == "mp4"));

    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_dir(&dir);
}

/// 空的响应体不能留下一个 0 字节的 `.mp4`：那会让 TDLib 报「文件损坏」，
/// 比直接说「下载下来是空文件」难查得多。
#[tokio::test]
async fn an_empty_stream_leaves_no_file_behind() {
    let dir = temp_dir("empty");
    let cancel = AtomicBool::new(false);
    let error = write_stream(
        &dir,
        "https://x.com/a/empty.mp4",
        "mp4",
        &mut Empty,
        &cancel,
    )
    .await
    .expect_err("空正文必须报错");
    assert!(error.0.contains("空文件"), "{}", error.0);
    assert!(leftover(&dir).is_empty(), "空下载不该留下文件");
    let _ = std::fs::remove_dir(&dir);
}

/// 读到一半断了（模拟 CDN 掐连接）：半截文件必须删掉，不能留在缓存里。
///
/// 留着的后果是下一次发送拿到一个「长度不对的 mp4」，
/// 报错变成 TDLib 的「文件损坏」，与本来的网络问题对不上。
#[tokio::test]
async fn a_broken_stream_deletes_the_partial_file() {
    let dir = temp_dir("broken");
    let cancel = AtomicBool::new(false);
    let error = write_stream(
        &dir,
        "https://x.com/a/broken.mp4",
        "mp4",
        &mut Broken(1024 * 1024),
        &cancel,
    )
    .await
    .expect_err("中断的流必须报错");
    assert!(error.0.contains("下载中断"), "{}", error.0);
    assert!(leftover(&dir).is_empty(), "半截文件必须被删掉");
    let _ = std::fs::remove_dir(&dir);
}

/// 取消标志置位后，下载必须在**块边界**收手并删掉半截文件——
/// `stop()` 不能等一个几百 MB 的下载跑完才返回。
#[tokio::test]
async fn a_cancelled_download_stops_between_chunks() {
    let dir = temp_dir("cancel");
    let cancel = AtomicBool::new(true);
    let error = write_stream(
        &dir,
        "https://x.com/a/cancel.mp4",
        "mp4",
        &mut Feed {
            left: 30 * 1024 * 1024,
            chunk: 64 * 1024,
            byte: b'z',
        },
        &cancel,
    )
    .await
    .expect_err("取消后必须立刻放弃");
    assert!(error.0.contains("取消"), "{}", error.0);
    assert!(leftover(&dir).is_empty(), "取消的半截文件必须删掉");
    let _ = std::fs::remove_dir(&dir);
}

/// 撞名时换名字重试，而不是覆盖别人正在读的文件。
#[tokio::test]
async fn a_second_download_does_not_overwrite_the_first() {
    let dir = temp_dir("collide");
    let cancel = AtomicBool::new(false);
    let mut first_src = Feed {
        left: 3,
        chunk: 8,
        byte: b'a',
    };
    let (first, _) = write_stream(
        &dir,
        "https://x.com/a/v0200.mp4",
        "mp4",
        &mut first_src,
        &cancel,
    )
    .await
    .expect("第一次应当写成功");
    let mut second_src = Feed {
        left: 2,
        chunk: 8,
        byte: b'b',
    };
    // 形状完全相同的 URL：文件名会撞（`cache_name` 带毫秒时间戳，这里刻意
    // 用同一秒内的两次调用来逼近撞名场景；真撞了就必须换名字重试）。
    let (second, _) = write_stream(
        &dir,
        "https://x.com/a/v0200.mp4",
        "mp4",
        &mut second_src,
        &cancel,
    )
    .await
    .expect("撞名后应当换名重试成功");
    assert_ne!(first, second, "不能写回同一个文件");
    assert_eq!(std::fs::read(&first).unwrap(), b"aaa");
    assert_eq!(std::fs::read(&second).unwrap(), b"bb");

    let _ = std::fs::remove_file(&first);
    let _ = std::fs::remove_file(&second);
    let _ = std::fs::remove_dir(&dir);
}
