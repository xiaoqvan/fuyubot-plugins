//! 汽水 App 端点的应用级签名（`x-helios` / `x-medusa`）。
//!
//! ## 为什么需要外部签名器
//!
//! 整曲 / 会员音质走 `POST /luna/pc/track_v2`，服务端只认官方客户端原生安全组件
//! （`mssdk`）产出的签名头，而且**逐请求**计算：换了 body 或 track_id，同一对头就
//! 被判空响应。纯 Rust 算不出来，所以这里留一个 HTTP 签名器接缝：
//!
//! ```text
//! 插件 ──POST /sign {"url","method","body","ts_ms","headers"}──▶ 签名器（Windows/macOS）
//!      ◀── {"headers":{"x-helios":"…","x-medusa":"…"},"msToken":"…","a_bogus":"…"}
//! ```
//!
//! 回包也兼容 Meting-API 那种扁平写法：`{"ok":true,"X-Helios":"…","X-Medusa":"…"}`。
//!
//! 不配签名器时退回 `soda.json` 里静态抓包的那一对头——能用的场景有限（签名通常
//! 与 body 绑定），失败时会给出可读原因。

use std::collections::BTreeMap;

use serde::Deserialize;

use crate::auth::SodaAuth;
use crate::http::{self, Agent};
use crate::log;

/// 签名器返回的签名。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Signature {
    pub headers: Vec<(String, String)>,
    pub ms_token: String,
    pub a_bogus: String,
}

/// 一次签名请求的输入。
#[derive(Debug, serde::Serialize)]
pub struct SignRequest<'a> {
    pub url: &'a str,
    pub method: &'a str,
    pub body: &'a str,
    pub ts_ms: u64,
    pub headers: BTreeMap<String, String>,
}

/// 签名器回包的宽松形状（结构体 / 扁平两种都认）。
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct Reply {
    ok: Option<bool>,
    error: Option<String>,
    code: Option<String>,
    #[serde(alias = "msToken")]
    ms_token: Option<String>,
    #[serde(alias = "aBogus", alias = "a_bogus")]
    a_bogus: Option<String>,
    headers: BTreeMap<String, String>,
}

/// 签好的一次请求（URL 可能被追加了 `msToken` / `a_bogus`）。
#[derive(Debug, Clone)]
pub struct Signed {
    pub url: String,
    pub headers: Vec<(String, String)>,
}

/// 为一次请求取签名：有签名器走签名器，否则用静态抓包头。
pub async fn signed_request(
    agent: &Agent,
    auth: &SodaAuth,
    url: &str,
    body: &str,
    headers: Vec<(String, String)>,
) -> Result<Signed, String> {
    let mut headers = headers;

    if auth.has_signer() {
        match request_signature(agent, auth, url, body, &headers).await {
            Ok(signature) => {
                log::debug(&format!(
                    "汽水签名器返回 {} 个请求头{}",
                    signature.headers.len(),
                    if signature.ms_token.is_empty() && signature.a_bogus.is_empty() {
                        String::new()
                    } else {
                        "（含查询参数签名）".to_owned()
                    }
                ));
                let signed_url = signed_url(url, &signature);
                merge(&mut headers, &signature.headers);
                return Ok(Signed {
                    url: signed_url,
                    headers,
                });
            }
            Err(error) => log::warn(&format!("汽水签名器调用失败，退回静态头：{error}")),
        }
    }

    merge(
        &mut headers,
        &[
            ("x-helios", auth.x_helios.trim()),
            ("x-medusa", auth.x_medusa.trim()),
        ]
        .into_iter()
        .filter(|(_, value)| !value.is_empty())
        .map(|(name, value)| (name.to_owned(), value.to_owned()))
        .collect::<Vec<_>>(),
    );
    Ok(Signed {
        url: url.to_owned(),
        headers,
    })
}

/// 调一次签名器并解析回包。
pub async fn request_signature(
    agent: &Agent,
    auth: &SodaAuth,
    url: &str,
    body: &str,
    headers: &[(String, String)],
) -> Result<Signature, String> {
    let request = SignRequest {
        url,
        method: "POST",
        body,
        ts_ms: crate::now_millis(),
        headers: headers.iter().cloned().collect(),
    };
    let payload =
        serde_json::to_vec(&request).map_err(|error| format!("签名请求编码失败：{error}"))?;

    // `post_json` 自己会补 Content-Type，这里别再写一遍（否则发出两个同名头）。
    let mut request_headers = vec![("Accept", "application/json")];
    let bearer;
    if !auth.signer_token.trim().is_empty() {
        bearer = format!("Bearer {}", auth.signer_token.trim());
        request_headers.push(("Authorization", bearer.as_str()));
    }
    let reply = http::post_json(agent, auth.signer_url.trim(), &payload, &request_headers).await?;
    if !(200..300).contains(&reply.status) {
        return Err(format!("签名器 HTTP {}：{}", reply.status, reply.text()));
    }
    parse_response(&reply.body)
}

/// 解析签名器回包。
pub fn parse_response(raw: &[u8]) -> Result<Signature, String> {
    let value: serde_json::Value =
        serde_json::from_slice(raw).map_err(|error| format!("签名器回包不是 JSON：{error}"))?;
    let parsed: Reply = serde_json::from_value(value.clone()).unwrap_or_default();

    if parsed.ok == Some(false) {
        let message = parsed.error.unwrap_or_else(|| "签名器返回失败".to_owned());
        let unauthorized = parsed
            .code
            .as_deref()
            .is_some_and(|code| code.eq_ignore_ascii_case("unauthorized"))
            || message.to_ascii_lowercase().contains("unauthorized")
            || message.contains("401");
        return Err(if unauthorized {
            format!("{message}（签名器要求鉴权：在 soda.json 里填 signer_token）")
        } else {
            message
        });
    }

    let mut signature = Signature {
        headers: parsed
            .headers
            .into_iter()
            .map(|(name, value)| (name, value.trim().to_owned()))
            .filter(|(_, value)| !value.is_empty())
            .collect(),
        ms_token: parsed.ms_token.unwrap_or_default().trim().to_owned(),
        a_bogus: parsed.a_bogus.unwrap_or_default().trim().to_owned(),
    };

    // 兼容扁平写法：{"ok":true,"X-Helios":"…","X-Medusa":"…"}。
    for (alias, canonical) in [
        ("X-Helios", "x-helios"),
        ("X-Medusa", "x-medusa"),
        ("x-helios", "x-helios"),
        ("x-medusa", "x-medusa"),
    ] {
        if let Some(text) = value.get(alias).and_then(|item| item.as_str())
            && !text.trim().is_empty()
        {
            merge(
                &mut signature.headers,
                &[(canonical.to_owned(), text.trim().to_owned())],
            );
        }
    }

    if signature.headers.is_empty() && signature.ms_token.is_empty() && signature.a_bogus.is_empty()
    {
        return Err("签名器回包里没有任何签名".to_owned());
    }
    Ok(signature)
}

/// 把 `ms_token` / `a_bogus` 追加到查询串（已有的同名参数会被替换）。
pub fn signed_url(url: &str, signature: &Signature) -> String {
    if signature.ms_token.is_empty() && signature.a_bogus.is_empty() {
        return url.to_owned();
    }
    let (base, query) = match url.split_once('?') {
        Some((base, query)) => (base, query),
        None => (url, ""),
    };
    let mut pairs: Vec<(String, String)> = query
        .split('&')
        .filter(|pair| !pair.is_empty())
        .filter_map(|pair| {
            let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
            let key = crate::soda::percent_decode(key);
            if key == "msToken" || key == "a_bogus" {
                None
            } else {
                Some((key, crate::soda::percent_decode(value)))
            }
        })
        .collect();
    if !signature.ms_token.is_empty() {
        pairs.push(("msToken".to_owned(), signature.ms_token.clone()));
    }
    if !signature.a_bogus.is_empty() {
        pairs.push(("a_bogus".to_owned(), signature.a_bogus.clone()));
    }
    let query = pairs
        .iter()
        .map(|(key, value)| {
            format!(
                "{}={}",
                crate::soda::percent_encode(key),
                crate::soda::percent_encode(value)
            )
        })
        .collect::<Vec<_>>()
        .join("&");
    format!("{base}?{query}")
}

/// 同名头覆盖，不同名追加。
fn merge(headers: &mut Vec<(String, String)>, extra: &[(String, String)]) {
    for (name, value) in extra {
        if let Some(existing) = headers
            .iter_mut()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
        {
            existing.1 = value.clone();
        } else {
            headers.push((name.clone(), value.clone()));
        }
    }
}

#[cfg(test)]
mod tests {
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
                let Some(head_end) = data.windows(4).position(|window| window == b"\r\n\r\n")
                else {
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
}
