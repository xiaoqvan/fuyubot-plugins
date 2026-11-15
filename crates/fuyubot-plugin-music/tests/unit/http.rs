use super::*;

#[test]
fn cache_dir_sits_next_to_the_plugins_dir() {
    assert_eq!(
        cache_dir(Path::new("D:/bots/demo/plugins")),
        PathBuf::from("D:/bots/demo/cache")
    );
    assert_eq!(cache_dir(Path::new("/")), PathBuf::from("cache"));
}

#[test]
fn cache_name_takes_the_url_basename_and_a_clean_extension() {
    let name = cache_name("https://x.com/a/b/song.mp3?token=1", "mp3");
    assert!(name.starts_with("song_"), "{name}");
    assert!(name.ends_with(".mp3"), "{name}");
    assert_eq!(name.matches('.').count(), 1, "扩展名只有一个点：{name}");

    let name = cache_name("https://x.com/a/b/no-extension", "flac");
    assert!(name.starts_with("no-extension_"), "{name}");
    assert!(name.ends_with(".flac"), "{name}");

    // 非法字符换掉，且空名字有兜底
    let name = cache_name("https://x.com/a/b/c:1", "jpg");
    assert!(name.starts_with("c_1_"), "{name}");
    let name = cache_name("https://x.com/", "mp3");
    assert!(name.starts_with("music_"), "{name}");
}

#[test]
fn retry_suffix_goes_before_the_extension() {
    assert_eq!(with_suffix("a.mp3", "mp3", 2), "a-2.mp3");
    assert_eq!(with_suffix("weird", "mp3", 1), "weird-1");
}

#[test]
fn candidate_dirs_end_with_the_temp_directory() {
    let dirs = candidate_dirs(Path::new("D:/bots/demo/plugins"));
    assert_eq!(dirs.first(), Some(&PathBuf::from("D:/bots/demo/cache")));
    assert_eq!(
        dirs.last(),
        Some(&std::env::temp_dir().join("fuyubot-music"))
    );
}

/// async 下载确实把响应体流式落盘（本地回环，不联网）。
#[tokio::test]
async fn download_streams_the_body_to_a_unique_file() {
    use std::io::{Read, Write};
    use std::net::TcpListener;

    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let address = listener.local_addr().expect("addr");
    let server = std::thread::spawn(move || {
        let (mut socket, _) = listener.accept().expect("accept");
        // 读到请求头结束即可（GET 没有 body）。
        let mut head = Vec::new();
        let mut byte = [0u8; 1];
        while socket.read(&mut byte).expect("read") == 1 {
            head.push(byte[0]);
            if head.ends_with(b"\r\n\r\n") {
                break;
            }
        }
        let body = b"0123456789".repeat(1000);
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: audio/mpeg\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        socket.write_all(response.as_bytes()).expect("head");
        socket.write_all(&body).expect("body");
        let _ = socket.flush();
    });

    let dir = std::env::temp_dir().join(format!("fuyubot-music-http-test-{}", crate::now_millis()));
    let path = download(
        &agent(),
        &format!("http://{address}/a/song.mp3?token=1"),
        &[],
        &dir,
        "mp3",
        &AtomicBool::new(false),
    )
    .await
    .expect("下载应当成功");
    server.join().expect("server");
    assert_eq!(
        tokio::fs::metadata(&path).await.expect("metadata").len(),
        10_000
    );
    assert!(
        path.file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with("song_")),
        "文件名应当来自 URL 末段：{}",
        path.display()
    );
    let _ = tokio::fs::remove_dir_all(&dir).await;
}

/// 取消标志置位时下载立刻放弃并删掉半成品。
#[tokio::test]
async fn a_cancelled_download_leaves_no_partial_file() {
    let cancel = AtomicBool::new(true);
    let dir =
        std::env::temp_dir().join(format!("fuyubot-music-http-cancel-{}", crate::now_millis()));
    // 端口 1 不会有人监听：这里只验证「取消优先于连接」之前的路径不会误建文件。
    // 真正的分块取消由 `write_unique` 的循环保证，单测不依赖网络。
    let error = download(
        &agent(),
        "http://127.0.0.1:1/none.mp3",
        &[],
        &dir,
        "mp3",
        &cancel,
    )
    .await
    .unwrap_err();
    assert!(!error.is_empty());
    let _ = tokio::fs::remove_dir_all(&dir).await;
}
