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
#[path = "../tests/unit/soda_sign.rs"]
mod tests;
