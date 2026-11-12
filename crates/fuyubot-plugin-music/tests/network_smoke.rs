//! 联网冒烟测试：验证两家接口的形状与本仓库的解析仍然对得上。
//!
//! 默认不跑（`#[ignore]`）——CI 里没有网络，也不该依赖第三方服务的可用性。
//!
//! ```text
//! cargo test --test network_smoke -- --ignored --nocapture
//! ```
//!
//! 这些用例只用**匿名**请求：不读本机 cookie，不打印任何 URL 的查询串。
//! 需要登录态的路径（网易云 VIP 音质、汽水 App 端点）不在这里覆盖。
//!
//! 全部是 `#[tokio::test]`：HTTP 层已经改成 reqwest async，`download` 也是流式的。

use std::sync::atomic::AtomicBool;

use fuyubot_plugin_music::auth::{NETEASE_USER_AGENT, NeteaseAuth, SodaAuth};
use fuyubot_plugin_music::{http, netease, soda, soda_video};

#[tokio::test]
#[ignore = "需要联网"]
async fn netease_search_returns_tracks_anonymously() {
    let agent = http::agent();
    let auth = NeteaseAuth::default();
    let tracks = netease::search(&agent, &auth, "晴天", 5)
        .await
        .expect("匿名搜索应当有结果");
    assert!(!tracks.is_empty());
    for track in &tracks {
        println!("{} - {} (id={})", track.name, track.artist_text(), track.id);
        assert!(track.id > 0);
        assert!(!track.name.is_empty());
    }
}

#[tokio::test]
#[ignore = "需要联网"]
async fn netease_detail_and_play_url_are_reachable() {
    let agent = http::agent();
    let auth = NeteaseAuth::default();
    let tracks = netease::search(&agent, &auth, "晴天", 5)
        .await
        .expect("搜索");
    let id = tracks[0].id;

    let track = netease::detail(&agent, &auth, id).await.expect("详情");
    println!("{} - {}", track.name, track.artist_text());
    assert_eq!(track.id, id);

    match netease::play_url(&agent, &auth, id, "standard").await {
        Ok(stream) => {
            println!(
                "stream: level={} format={} size={} trial={}",
                stream.level, stream.format, stream.size, stream.trial
            );
            assert!(!stream.url.is_empty());
        }
        // 匿名拿不到 VIP 曲目是预期行为，不算失败。
        Err(error) => println!("匿名取流失败（可能是 VIP 曲目）：{error}"),
    }
}

#[tokio::test]
#[ignore = "需要联网"]
async fn soda_search_returns_tracks_anonymously() {
    let agent = http::agent();
    let auth = SodaAuth::default();
    let tracks = soda::search(&agent, &auth, "晴天", 5)
        .await
        .expect("匿名搜索应当有结果");
    assert!(!tracks.is_empty());
    for track in &tracks {
        println!(
            "{} - {} (id={}, {}s)",
            track.name,
            track.artist_text(),
            track.id,
            track.duration
        );
        assert!(!track.id.is_empty());
        assert!(track.duration > 0, "时长应当已经归一成秒");
    }
}

#[tokio::test]
#[ignore = "需要联网"]
async fn soda_share_link_resolves_to_a_track_id() {
    let agent = http::agent();
    let auth = SodaAuth::default();
    let link = "https://www.qishui.com/track/7304719759323564095";
    let id = soda::resolve_id(&agent, &auth, link)
        .await
        .expect("track 链接应当能解析");
    assert_eq!(id, "7304719759323564095");
}

#[tokio::test]
#[ignore = "需要联网"]
async fn soda_detail_resolves_a_playable_stream() {
    let agent = http::agent();
    let auth = SodaAuth::default();
    // 搜索结果里的免费曲目。
    let resolved = soda::resolve(&agent, &auth, "7676337516358617115", "best")
        .await
        .expect("免费曲目应当能取到流");
    println!(
        "{} - {} / {} 秒 / quality={} / encrypted={} / preview={}",
        resolved.track.name,
        resolved.track.artist_text(),
        resolved.track.duration,
        resolved.stream.quality,
        resolved.stream.is_encrypted(),
        resolved.preview
    );
    assert!(!resolved.stream.url.is_empty());
}

#[tokio::test]
#[ignore = "需要联网"]
async fn both_audio_urls_actually_download_bytes() {
    let agent = http::agent();
    let dir = std::env::temp_dir().join("fuyubot-music-smoke");
    let _ = std::fs::remove_dir_all(&dir);

    // 汽水：免费曲目的明文流。
    let soda_auth = SodaAuth::default();
    let soda = soda::resolve(&agent, &soda_auth, "7676337516358617115", "best")
        .await
        .expect("汽水取流");
    let path = http::download(
        &agent,
        &soda.stream.url,
        &[("User-Agent", soda::CHROME_UA)],
        &dir,
        soda.extension(),
        &AtomicBool::new(false),
    )
    .await
    .expect("汽水音频应当能下载");
    let len = std::fs::metadata(&path).unwrap().len();
    println!("汽水下载 {len} 字节 -> {}", path.display());
    assert!(len > 100_000, "音频不该这么小：{len}");

    // 网易云：匿名 standard 流。
    let netease_auth = NeteaseAuth::default();
    let tracks = netease::search(&agent, &netease_auth, "晴天", 3)
        .await
        .expect("网易云搜索");
    let stream = netease::play_url(&agent, &netease_auth, tracks[0].id, "standard")
        .await
        .expect("网易云取流");
    let path = http::download(
        &agent,
        &stream.url,
        &[
            ("User-Agent", NETEASE_USER_AGENT),
            ("Referer", "https://music.163.com/"),
        ],
        &dir,
        "mp3",
        &AtomicBool::new(false),
    )
    .await
    .expect("网易云音频应当能下载");
    let len = std::fs::metadata(&path).unwrap().len();
    println!("网易云下载 {len} 字节 -> {}", path.display());
    assert!(len > 100_000, "音频不该这么小：{len}");

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
#[ignore = "需要联网"]
async fn soda_ugc_video_share_resolves() {
    let agent = http::agent();
    let auth = SodaAuth::default();
    // 《塔不灭 …》那条 UGC 视频分享。
    let post = soda_video::resolve(&agent, &auth, "7625615883351772468")
        .await
        .expect("视频分享页");
    println!(
        "{} / By {} / {}s / {}x{} / cover={} / preview={}",
        post.title,
        post.author,
        post.duration,
        post.width,
        post.height,
        !post.cover.is_empty(),
        post.preview
    );
    assert!(!post.url.is_empty());
    assert!(!post.title.is_empty());
    assert!(post.duration > 0);
}

#[tokio::test]
#[ignore = "需要联网"]
async fn soda_short_link_kind_is_detected() {
    let agent = http::agent();
    let auth = SodaAuth::default();
    let track = soda::classify_share(&agent, &auth, "https://qishui.douyin.com/s/iXvvQyuC/")
        .await
        .expect("单曲短链");
    println!("track share = {track:?}");
    assert!(matches!(track, soda::Share::Track(_)));

    let video = soda::classify_share(&agent, &auth, "https://qishui.douyin.com/s/iXvvC5YV/")
        .await
        .expect("视频短链");
    println!("video share = {video:?}");
    assert!(matches!(video, soda::Share::Video(_)));
}
