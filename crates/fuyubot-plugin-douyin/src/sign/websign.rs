//! SecSDK web 签名（`uifid` + `x-secsdk-web-signature`）：抖音网页端
//! 「签名保护区」的门禁，`/aweme/v1/web/aweme/detail/` 这类接口查的就是它。
//!
//! 算法（2026-09 逆向并双重验证：与真实浏览器产出的签名逐字节一致；原生计算的
//! 参数请求 detail 接口 6/6 返回完整数据）：
//!
//! ```text
//! query = URLSearchParams(除签名外的全部参数).toString()
//! sig   = md5("{uifid}_{timestamp}_{SALT}_{query}").hexdigest()
//! ```
//!
//! 纯函数：无 nonce、无会话状态。同一秒内两次调用产出相同的签名。
//!
//! 参数与请求头的**顺序是平台的**：站点发送
//! `...&a_bogus=&verifyFp=&fp=&uifid=&timestamp=&x-secsdk-web-signature=`，
//! 签名覆盖它之前的全部内容再序列化（见 [`sign`]）。
//!
//! 来源：Evil0ctal/Douyin_TikTok_Download_API（Apache-2.0）`websign.py`。

use md5::{Digest, Md5};

use super::encode::quote_keep_star;

/// 「VM 表第 39 个字符串」，按项目定值；换表即换值。
pub const SALT: &str = "A96D855A08C0A9707F8BEF0D9A527E4E";

/// 签名者查找访客 id 的 cookie 名（按序取第一个非空）。
pub const UIFID_COOKIE_NAMES: &[&str] = &[
    "uifid",
    "uifid_temp",
    "uifidtemp",
    "UIFID",
    "UIFID_TEMP",
    "UIFIDTEMP",
];

/// 就是 `verifyFp` / `fp` 的那个 cookie。
pub const VERIFY_FP_COOKIE: &str = "s_v_web_id";

pub const SIGNATURE_PARAM: &str = "x-secsdk-web-signature";
pub const UIFID_PARAM: &str = "uifid";
pub const TIMESTAMP_PARAM: &str = "timestamp";
pub const VERIFY_FP_PARAMS: &[&str] = &["verifyFp", "fp"];

/// 与 query 一起发的请求头。平台可以不校验，但它自己的页面总发——像它才是目的。
pub const EXPIRE_HEADER: &str = "x-secsdk-web-expire";

/// 一份签名结果：要发的 query（签名已附加）+ 三个请求头。
#[derive(Debug, Clone)]
pub struct SignedQuery {
    pub query: String,
    pub signature: String,
    /// `(头名, 头值)`：`uifid` / `x-secsdk-web-signature` / `x-secsdk-web-expire`。
    pub headers: Vec<(String, String)>,
}

/// 从 cookie 表里挑访客 id。
pub fn pick_uifid(cookies: &std::collections::BTreeMap<String, String>) -> Option<String> {
    UIFID_COOKIE_NAMES.iter().find_map(|name| {
        cookies
            .get(*name)
            .filter(|value| !value.is_empty())
            .cloned()
    })
}

/// `URLSearchParams.toString()` 的序列化：每对 `k=v` 以 `&` 相连。
///
/// 编码必须与被发送的字节一致——**同一条串既被哈希又被发送**。
pub fn encode_pairs(pairs: &[(String, String)]) -> String {
    pairs
        .iter()
        .map(|(key, value)| format!("{}={}", quote_keep_star(key), quote_keep_star(value)))
        .collect::<Vec<_>>()
        .join("&")
}

/// 计算签名，返回「签名后的 query + 签名 + 请求头」。
///
/// `pairs` 是签名前 query 的全部内容（业务参数 + a_bogus 层追加的），顺序敏感。
/// 已有的 `uifid` 对保持原位不重复追加（重复会得到平台上不出现的前像）；
/// `timestamp` 在这里补上，因为签名覆盖它。
pub fn sign(pairs: &[(String, String)], uifid: &str, timestamp: i64) -> SignedQuery {
    let stamp = timestamp.to_string();
    let mut covered = pairs.to_vec();
    if !covered.iter().any(|(key, _)| key == UIFID_PARAM) {
        covered.push((UIFID_PARAM.to_owned(), uifid.to_owned()));
    }
    covered.push((TIMESTAMP_PARAM.to_owned(), stamp.clone()));
    let query = encode_pairs(&covered);
    let signature = md5_hex(&format!("{uifid}_{stamp}_{SALT}_{query}"));
    let headers = vec![
        (UIFID_PARAM.to_owned(), uifid.to_owned()),
        (SIGNATURE_PARAM.to_owned(), signature.clone()),
        (EXPIRE_HEADER.to_owned(), stamp),
    ];
    SignedQuery {
        query: format!("{query}&{SIGNATURE_PARAM}={signature}"),
        signature,
        headers,
    }
}

/// 把已编码的 query 拆回对（[`super::encode::unquote`]），以便重新序列化后签名。
pub fn pairs_from_query(query: &str) -> Vec<(String, String)> {
    query
        .split('&')
        .filter(|part| !part.is_empty())
        .map(|part| {
            let (key, value) = super::encode::split_pair(part);
            (super::encode::unquote(key), super::encode::unquote(value))
        })
        .collect()
}

/// 追加 `verifyFp` / `fp` 与签名层，返回最终 query（平台顺序）。
///
/// 没有 `UIFID_TEMP` 的 jar 就原样返回：未保护端点照样能用，保护端点会由平台
/// 自己报 `uifid` 缺失——那比任何编造都有信息量。
pub fn add_web_signature(
    query: &str,
    uifid: Option<&str>,
    verify_fp: Option<&str>,
    timestamp: i64,
) -> (String, Vec<(String, String)>) {
    let Some(uifid) = uifid.filter(|value| !value.is_empty()) else {
        return (query.to_owned(), Vec::new());
    };
    let mut pairs = pairs_from_query(query);
    if let Some(verify_fp) = verify_fp.filter(|value| !value.is_empty()) {
        for name in VERIFY_FP_PARAMS {
            pairs.push(((*name).to_owned(), verify_fp.to_owned()));
        }
    }
    let signed = sign(&pairs, uifid, timestamp);
    (signed.query, signed.headers)
}

fn md5_hex(text: &str) -> String {
    let mut hasher = Md5::new();
    hasher.update(text.as_bytes());
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// `urlencode` 重导出：a_bogus 的输入编码与本层共用一个入口。
pub use super::encode::urlencode as form_encode;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn md5_matches_the_reference() {
        // Python: hashlib.md5(b"abc").hexdigest()
        assert_eq!(md5_hex("abc"), "900150983cd24fb0d6963f7d28e17f72");
    }

    #[test]
    fn encode_pairs_is_urlsearchparams() {
        let pairs = vec![
            ("k".to_owned(), "has space".to_owned()),
            ("star*".to_owned(), "a+b".to_owned()),
        ];
        assert_eq!(encode_pairs(&pairs), "k=has%20space&star*=a%2Bb");
    }

    #[test]
    fn sign_appends_in_platform_order() {
        let pairs = vec![("aweme_id".to_owned(), "1".to_owned())];
        let signed = sign(&pairs, "U", 1762600000);
        assert_eq!(
            signed.query,
            format!(
                "aweme_id=1&uifid=U&timestamp=1762600000&x-secsdk-web-signature={}",
                signed.signature
            )
        );
        assert_eq!(signed.signature.len(), 32);
        assert_eq!(signed.headers.len(), 3);
    }

    #[test]
    fn existing_uifid_keeps_its_position() {
        let pairs = vec![
            ("uifid".to_owned(), "KEEP".to_owned()),
            ("a".to_owned(), "1".to_owned()),
        ];
        let signed = sign(&pairs, "IGNORED", 1762600000);
        assert!(signed.query.starts_with("uifid=KEEP&a=1&timestamp="));
        assert!(!signed.query.contains("IGNORED"));
    }
}
