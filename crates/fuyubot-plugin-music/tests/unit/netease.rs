use aes::cipher::BlockDecrypt;

fn aes_ecb_decrypt(cipher_text: &[u8]) -> Vec<u8> {
    let cipher = Aes128::new(GenericArray::from_slice(EAPI_KEY));
    let mut data = cipher_text.to_vec();
    for chunk in data.chunks_mut(16) {
        cipher.decrypt_block(GenericArray::from_mut_slice(chunk));
    }
    let pad = *data.last().unwrap_or(&0) as usize;
    if (1..=16).contains(&pad) && data.len() >= pad {
        data.truncate(data.len() - pad);
    }
    data
}

fn hex_decode(text: &str) -> Option<Vec<u8>> {
    let bytes = text.as_bytes();
    if !bytes.len().is_multiple_of(2) {
        return None;
    }
    let mut out = Vec::with_capacity(bytes.len() / 2);
    for pair in bytes.chunks(2) {
        let high = (pair[0] as char).to_digit(16)?;
        let low = (pair[1] as char).to_digit(16)?;
        out.push((high * 16 + low) as u8);
    }
    Some(out)
}

use super::*;

#[test]
fn md5_matches_a_known_vector() {
    assert_eq!(crate::md5_hex("abc"), "900150983cd24fb0d6963f7d28e17f72");
}

#[test]
fn eapi_params_round_trip_contains_path_json_and_digest() {
    let path = "/api/song/enhance/player/url/v1";
    let json = r#"{"ids":[1],"level":"exhigh"}"#;
    let params = encrypt_params(path, json);
    assert!(
        params
            .chars()
            .all(|ch| ch.is_ascii_hexdigit() && !ch.is_ascii_uppercase()),
        "params 必须是小写十六进制"
    );
    let plain = String::from_utf8(aes_ecb_decrypt(&hex_decode(&params).unwrap())).unwrap();
    let parts: Vec<&str> = plain.split(EAPI_SEPARATOR).collect();
    assert_eq!(parts.len(), 3);
    assert_eq!(parts[0], path);
    assert_eq!(parts[1], json);
    assert_eq!(parts[2], digest(path, json));
}

/// 与 OpenSSL 独立算出来的向量对齐：证明 AES-128-ECB + PKCS#7 + MD5 +
/// 小写 hex 这一整套和服务端认的完全一致，而不是「自己加自己解都对」。
#[test]
fn eapi_output_matches_an_independent_openssl_vector() {
    let params = encrypt_params(
        "/api/song/enhance/player/url/v1",
        r#"{"ids":[1],"level":"exhigh"}"#,
    );
    assert_eq!(
        params,
        "fa90b329e9614f79e79598f37dc2edb487f00d1bc4c9b24cd57e6c318b9073569338432cd7d98d1a3626e997a2c5312171f9fa98ff5799fa3c4c6026223e0fa27c8e643e149384e4c350b4297c7189ef99d82df6f21020b859f72380357dfa34422775dd462499320db0bec309e203b493580b8c9132e49722a0fc34cd2fe64c"
    );
}

#[test]
fn api_path_drops_the_eapi_prefix() {
    assert_eq!(
        api_path_of("/eapi/song/enhance/player/url/v1"),
        "/api/song/enhance/player/url/v1"
    );
    assert_eq!(api_path_of("/api/search"), "/api/search");
}

#[test]
fn every_quality_has_a_working_alias() {
    assert_eq!(parse_quality("exhigh"), Some("exhigh"));
    assert_eq!(parse_quality("SQ"), Some("lossless"));
    assert_eq!(parse_quality("set"), Some("lossless"));
    assert_eq!(parse_quality("无损"), Some("lossless"));
    assert_eq!(parse_quality("jymaster"), Some("jymaster"));
    assert_eq!(parse_quality("nope"), None);
    // 默认值必须真的可用
    assert_eq!(parse_quality(DEFAULT_QUALITY), Some(DEFAULT_QUALITY));
}

#[test]
fn song_ids_are_recognised_in_every_shape() {
    assert_eq!(song_id_of("186016"), Some(186016));
    assert_eq!(
        song_id_of("https://music.163.com/song?id=186016"),
        Some(186016)
    );
    assert_eq!(
        song_id_of("https://y.music.163.com/m/song/186016/?uct=1"),
        Some(186016)
    );
    assert_eq!(
        song_id_of("https://music.163.com/#/song?id=186016"),
        Some(186016)
    );
    assert_eq!(song_id_of("周杰伦 晴天"), None);
    assert_eq!(song_id_of("https://music.163.com/album?id=1"), None);
}

#[test]
fn only_netease_hosts_are_picked_up() {
    assert!(first_link("听 https://music.163.com/song?id=1 吧").is_some());
    assert!(first_link("https://163cn.tv/abc").is_some());
    assert!(first_link("https://music.163.com.evil.test/song?id=1").is_none());
    assert!(first_link("https://example.com/music.163.com").is_none());
    assert!(first_link("没有链接").is_none());
}

#[test]
fn lookalike_domains_are_rejected() {
    assert!(!is_netease_host(
        "https://music.163.com.evil.test/song?id=1"
    ));
    assert!(!is_netease_host("https://notmusic.163.com/song?id=1"));
    assert!(is_netease_host("https://y.music.163.com/m/song?id=1"));
    assert!(is_netease_host("https://163cn.tv/x"));
}

#[test]
fn trailing_punctuation_is_stripped_from_links() {
    assert_eq!(
        first_link("看看这个：https://music.163.com/song?id=1。").as_deref(),
        Some("https://music.163.com/song?id=1")
    );
}

#[test]
fn play_entry_tolerates_null_fields_when_the_song_is_unavailable() {
    // 匿名请求 VIP 曲目时网易云真的会这样回：url/level/type/size/br 全 null。
    let response: PlayResponse = serde_json::from_str(
        r#"{"data":[{"url":null,"level":null,"type":null,"size":null,"br":null,"code":404}]}"#,
    )
    .expect("null 字段不能把解析打挂");
    let entry = response.data.into_iter().next().unwrap();
    assert!(entry.url.is_none());
    assert_eq!(entry.code, 404);
    assert!(entry.level.is_empty());
    assert!(entry.format.is_empty());
}

#[test]
fn play_entry_marks_trial_streams() {
    let value: PlayResponse = serde_json::from_str(
            r#"{"data":[{"url":"http://x/1.mp3","level":"standard","type":"mp3","size":10,"br":128000,"code":200,"freeTrialInfo":{"start":0,"end":30}}]}"#,
        )
        .unwrap();
    let entry = value.data.into_iter().next().unwrap();
    assert!(entry.free_trial_info.is_some());
    assert_eq!(entry.format, "mp3");
}

#[test]
fn covers_are_requested_as_small_thumbnails() {
    assert_eq!(
        thumbnail_cover("http://p1.music.126.net/a.jpg"),
        "http://p1.music.126.net/a.jpg?param=320y320"
    );
    assert_eq!(
        thumbnail_cover("http://p1.music.126.net/a.jpg?x=1"),
        "http://p1.music.126.net/a.jpg?x=1&param=320y320"
    );
    assert_eq!(
        thumbnail_cover("http://p1.music.126.net/a.jpg?param=300y300"),
        "http://p1.music.126.net/a.jpg?param=300y300",
        "已经有 param 的不重复加"
    );
    assert_eq!(thumbnail_cover(""), "");
}

#[test]
fn search_response_is_read_from_the_official_shape() {
    let value: SearchResponse = serde_json::from_str(
        r#"{"code":200,"data":{"resources":[{"baseInfo":{"simpleSongData":{
                "id":186016,"name":"晴天","dt":269000,
                "ar":[{"name":"周杰伦"}],"al":{"name":"叶惠美","picUrl":"http://p/1.jpg"}
            }}}]}}"#,
    )
    .unwrap();
    let track = value
        .data
        .resources
        .into_iter()
        .next()
        .unwrap()
        .base_info
        .simple_song_data
        .into_track();
    assert_eq!(track.id, 186016);
    assert_eq!(track.artist_text(), "周杰伦");
    assert_eq!(track.album, "叶惠美");
    assert_eq!(track.duration_secs(), 269);
}
