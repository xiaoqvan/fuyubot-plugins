//! 大文件下载的**回归测试**（旧的 `old_behavior_proof.rs` 换成了这一份）。
//!
//! 背景：旧实现用 `ureq` 的 `read_to_vec()`，而它写死了 10 MiB 上限——任何超过
//! 10 MB 的视频都下不下来，报错还像「HTTP 出问题」。现在下载是
//! `reqwest::Response::chunk()` **流式写盘**：体积无上限、内存恒定。
//!
//! 这一份测试真的起一个本地 HTTP 服务器吐 30 MiB，走**本插件的下载器**
//! （不是某个第三方库的便捷方法），所以它钉的是本插件自己的行为：
//!
//! 1. 30 MiB 的响应能完整落盘（越过旧上限）；
//! 2. 取消标志置位时下载立刻收手、**不留半截文件**（`stop()` 不能等下载跑完）；
//! 3. 同一个服务器被拒（HTTP 404）时不留空壳文件。
//!
//! 用 `#[tokio::test]` 是因为下载器本身是 async 的（插件内的 tokio 运行时之外，
//! 测试用自己的运行时）。

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;

use fuyubot_plugin_douyin::http::{self, Kind};

/// 起一个只服务一次的 HTTP 服务器：按 `size` 吐 `byte`，返回 `(url, join_handle)`。
fn serve_once(
    size: usize,
    byte: u8,
    status_line: &'static str,
) -> (String, std::thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("绑本地端口");
    let addr = listener.local_addr().expect("拿端口号");
    let handle = std::thread::spawn(move || {
        let (stream, _) = listener.accept().expect("接受连接");
        let mut reader = BufReader::new(stream.try_clone().expect("clone"));
        // 读到请求头结束（空行）为止，忽略内容。
        let mut line = String::new();
        while reader.read_line(&mut line).unwrap_or(0) > 0 {
            if line == "\r\n" {
                break;
            }
            line.clear();
        }
        let mut stream = stream;
        let head =
            format!("{status_line}\r\nContent-Type: video/mp4\r\nContent-Length: {size}\r\n\r\n");
        stream.write_all(head.as_bytes()).expect("写响应头");
        // 分块写：一次性写 30 MiB 会撑爆内核缓冲、卡在半路。
        let chunk = vec![byte; 64 * 1024];
        let mut left = size;
        while left > 0 {
            let n = left.min(chunk.len());
            stream.write_all(&chunk[..n]).expect("写正文");
            left -= n;
        }
        stream.flush().ok();
    });
    (format!("http://{addr}/aweme/v1/play/?video_id=big"), handle)
}

/// 每个用例一个干净目录。
fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("douyin-stream-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("建临时目录");
    dir
}

/// **核心回归**：30 MiB 的响应必须完整落盘。
///
/// 30 MiB 是三倍于旧的 10 MiB 硬上限——这条测试若是绿的，说明「大视频下不下来」
/// 的原始成因真的被移除了（流式写盘），而不是被换了个上限数字糊过去。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_30_mib_response_downloads_completely_to_disk() {
    const SIZE: usize = 30 * 1024 * 1024;
    let (url, server) = serve_once(SIZE, b'z', "HTTP/1.1 200 OK");

    let dir = temp_dir("big");
    let agent = http::agent().expect("建 HTTP 客户端");
    let cancel = AtomicBool::new(false);
    let path = http::download_as(&agent, &url, "", Kind::Video, &dir, &cancel)
        .await
        .expect("30 MiB 必须下得下来（这正是以前失败的那种体积）");

    let size = std::fs::metadata(&path).expect("文件应当存在").len();
    assert_eq!(size as usize, SIZE, "落盘大小必须与 Content-Length 一致");

    // 内容也要对：整份读回来比对（30 MiB 在测试里读一遍没问题）。
    let mut file = std::fs::File::open(&path).expect("打开下载结果");
    let mut buffer = Vec::new();
    file.read_to_end(&mut buffer).expect("读回下载结果");
    assert_eq!(buffer.len(), SIZE);
    assert!(
        buffer.iter().all(|byte| *byte == b'z'),
        "下下来的内容必须与服务器给的一致"
    );

    server.join().ok();
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_dir(&dir);
}

/// 取消标志置位后：下载立刻收手，并且**不留下半截文件**。
///
/// `stop()` 不能等一个几百 MB 的下载跑完才返回——它置位这个标志，下载器在
/// 块边界看到就删掉半截文件退出。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_cancelled_download_leaves_no_partial_file() {
    let (url, server) = serve_once(30 * 1024 * 1024, b'q', "HTTP/1.1 200 OK");

    let dir = temp_dir("cancel");
    let agent = http::agent().expect("建 HTTP 客户端");
    // 一开始就取消：下载器必须在写第一个块之前收手。
    let cancel = AtomicBool::new(true);
    let error = http::download_as(&agent, &url, "", Kind::Video, &dir, &cancel)
        .await
        .expect_err("取消后必须立刻放弃");
    assert!(
        error.0.contains("取消"),
        "错误应当是「已取消」：{}",
        error.0
    );

    let leftover: Vec<_> = std::fs::read_dir(&dir)
        .map(|entries| entries.flatten().map(|entry| entry.path()).collect())
        .unwrap_or_default();
    assert!(leftover.is_empty(), "取消后不该留下半截文件：{leftover:?}");

    server.join().ok();
    let _ = std::fs::remove_dir(&dir);
}

/// 被拒（HTTP 404）时直接报错，不建空壳文件。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_rejected_download_creates_nothing() {
    let (url, server) = serve_once(0, b'x', "HTTP/1.1 404 Not Found");

    let dir = temp_dir("rejected");
    let agent = http::agent().expect("建 HTTP 客户端");
    let cancel = AtomicBool::new(false);
    let error = http::download_as(&agent, &url, "", Kind::Image, &dir, &cancel)
        .await
        .expect_err("404 必须报错");
    assert!(error.0.contains("404"), "错误里要带状态码：{}", error.0);

    let leftover: Vec<_> = std::fs::read_dir(&dir)
        .map(|entries| entries.flatten().map(|entry| entry.path()).collect())
        .unwrap_or_default();
    assert!(leftover.is_empty(), "被拒的下载不该留下文件：{leftover:?}");

    server.join().ok();
    let _ = std::fs::remove_dir(&dir);
}
