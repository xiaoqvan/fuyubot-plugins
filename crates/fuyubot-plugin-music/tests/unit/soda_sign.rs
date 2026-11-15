use super::*;
use std::io::{Read, Write};
use std::net::TcpListener;

#[test]
fn structured_reply_is_parsed() {
    let signature = parse_response(
        br#"{"ok":true,"headers":{"x-helios":"h","x-medusa":"m"},"msToken":"t","aBogus":"a"}"#,
    )
    .unwrap();
    assert_eq!(signature.headers.len(), 2);
    assert_eq!(signature.ms_token, "t");
    assert_eq!(signature.a_bogus, "a");
}

#[test]
fn flat_meting_style_reply_is_parsed() {
    let signature = parse_response(br#"{"ok":true,"X-Helios":"h","X-Medusa":"m"}"#).unwrap();
    assert!(signature.headers.iter().any(|(name, _)| name == "x-helios"));
    assert!(signature.headers.iter().any(|(name, _)| name == "x-medusa"));
}

#[test]
fn signer_errors_are_reported() {
    let error =
        parse_response(br#"{"ok":false,"error":"nope","code":"unauthorized"}"#).unwrap_err();
    assert!(error.contains("signer_token"), "{error}");
    assert!(parse_response(br#"{"ok":true}"#).is_err());
    assert!(parse_response(b"not json").is_err());
}

#[test]
fn signed_url_replaces_existing_params_and_keeps_the_rest() {
    let signature = Signature {
        ms_token: "new token".to_owned(),
        a_bogus: "bogus".to_owned(),
        ..Signature::default()
    };
    let url = "https://api.qishui.com/luna/pc/track_v2?aid=386088&msToken=old&a_bogus=x";
    let signed = signed_url(url, &signature);
    assert!(signed.contains("aid=386088"), "{signed}");
    assert_eq!(signed.matches("msToken=").count(), 1, "{signed}");
    assert!(signed.contains("msToken=new%20token"), "{signed}");
    assert!(signed.contains("a_bogus=bogus"), "{signed}");
    assert!(!signed.contains("old"), "{signed}");
}

#[test]
fn no_signature_leaves_the_url_untouched() {
    let url = "https://x/y?aid=1";
    assert_eq!(signed_url(url, &Signature::default()), url);
}

#[test]
fn merge_overwrites_headers_case_insensitively() {
    let mut headers = vec![("X-Helios".to_owned(), "old".to_owned())];
    merge(&mut headers, &[("x-helios".to_owned(), "new".to_owned())]);
    assert_eq!(headers.len(), 1);
    assert_eq!(headers[0].1, "new");
}

#[tokio::test]
async fn static_headers_are_used_without_a_signer() {
    let agent = http::agent();
    let auth = SodaAuth {
        x_helios: "h".to_owned(),
        x_medusa: "m".to_owned(),
        ..SodaAuth::default()
    };
    let signed = signed_request(&agent, &auth, "https://x/y?aid=1", "{}", Vec::new())
        .await
        .unwrap();
    assert_eq!(signed.url, "https://x/y?aid=1");
    assert!(
        signed
            .headers
            .iter()
            .any(|(name, value)| name == "x-helios" && value == "h")
    );
    assert!(
        signed
            .headers
            .iter()
            .any(|(name, value)| name == "x-medusa" && value == "m")
    );
}

/// 起一个只回一次的本地签名器，把收到的请求原样交回测试线程。
fn serve_once(
    reply: &'static str,
) -> (
    String,
    std::sync::mpsc::Receiver<String>,
    std::thread::JoinHandle<()>,
) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let address = listener.local_addr().expect("addr");
    let (sender, receiver) = std::sync::mpsc::channel();
    let handle = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept");
        let mut data = Vec::new();
        let mut buffer = [0u8; 4096];
        loop {
            let read = stream.read(&mut buffer).expect("read");
            if read == 0 {
                break;
            }
            data.extend_from_slice(&buffer[..read]);
            let Some(head_end) = data.windows(4).position(|window| window == b"\r\n\r\n") else {
                continue;
            };
            let head = String::from_utf8_lossy(&data[..head_end]).to_string();
            let length = head
                .lines()
                .find_map(|line| {
                    let lower = line.to_ascii_lowercase();
                    lower
                        .strip_prefix("content-length:")
                        .map(|value| value.trim().parse::<usize>().unwrap_or(0))
                })
                .unwrap_or(0);
            if data.len() >= head_end + 4 + length {
                break;
            }
        }
        sender
            .send(String::from_utf8_lossy(&data).to_string())
            .expect("send");
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            reply.len(),
            reply
        );
        let _ = stream.write_all(response.as_bytes());
        let _ = stream.flush();
    });
    (format!("http://{address}/sign"), receiver, handle)
}

#[tokio::test]
async fn signer_wire_contract_is_what_the_service_expects() {
    let (url, receiver, handle) =
        serve_once(r#"{"ok":true,"headers":{"x-helios":"h","x-medusa":"m"},"msToken":"tok"}"#);
    let auth = SodaAuth {
        signer_url: url,
        signer_token: "secret".to_owned(),
        ..SodaAuth::default()
    };
    let signature = request_signature(
        &http::agent(),
        &auth,
        "https://api.qishui.com/luna/pc/track_v2?aid=386088",
        r#"{"track_id":"1"}"#,
        &[("X-SS-STUB".to_owned(), "ABCDEF".to_owned())],
    )
    .await
    .expect("签名器应当返回签名");
    handle.join().expect("server");

    assert_eq!(signature.ms_token, "tok");
    assert_eq!(signature.headers.len(), 2);

    let request = receiver.recv().expect("请求");
    assert!(request.contains("POST /sign HTTP/1.1"), "{request}");
    assert!(
        request
            .to_ascii_lowercase()
            .contains("authorization: bearer secret"),
        "{request}"
    );
    // 请求体是签名器契约里的五个字段，而且带着要签的原始请求头。
    assert!(
        request.contains(r#""url":"https://api.qishui.com/luna/pc/track_v2?aid=386088""#),
        "{request}"
    );
    assert!(request.contains(r#""method":"POST""#), "{request}");
    assert!(
        request.contains(r#""body":"{\"track_id\":\"1\"}""#),
        "{request}"
    );
    assert!(request.contains(r#""ts_ms":"#), "{request}");
    assert!(request.contains(r#""X-SS-STUB":"ABCDEF""#), "{request}");
}
