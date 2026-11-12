//! 与 Python 参考实现的对拍向量（crate 根 `sign-vectors.json`，由
//! `D:/GitHub/_ref/gen_vectors.py` 生成）。
//!
//! 向量覆盖编码边角（空格 / `+` / `*` / `~` / 中文 / emoji / 空串）、固定时间戳、
//! 固定随机种子的 a_bogus，以及一条端到端 flow。**本文件只许失败于语义变化**——
//! 改动编码或签名逻辑时先想清楚为什么字节会变。

use serde_json::Value;

use super::encode::urlencode;
use super::sm3::sm3_hexdigest;
use super::websign::{encode_pairs, sign};

fn vectors() -> Value {
    serde_json::from_str(include_str!("../../sign-vectors.json")).expect("向量文件是合法 JSON")
}

fn pairs_of(value: &Value) -> Vec<(String, String)> {
    value
        .as_array()
        .expect("pairs 是数组")
        .iter()
        .map(|pair| {
            let items = pair.as_array().expect("pair 是二元组");
            (
                items[0].as_str().expect("key 是字符串").to_owned(),
                items[1].as_str().expect("value 是字符串").to_owned(),
            )
        })
        .collect()
}

#[test]
fn encode_pairs_matches_python_quote() {
    let data = vectors();
    for case in data["encode_pairs"].as_array().expect("encode_pairs 数组") {
        let pairs = pairs_of(&case["pairs"]);
        assert_eq!(
            encode_pairs(&pairs),
            case["want"].as_str().unwrap(),
            "encode_pairs[{}]",
            case["label"].as_str().unwrap()
        );
    }
}

#[test]
fn urlencode_matches_python_urlencode() {
    let data = vectors();
    for case in data["urlencode"].as_array().expect("urlencode 数组") {
        let pairs = pairs_of(&case["pairs"]);
        assert_eq!(
            urlencode(&pairs),
            case["want"].as_str().unwrap(),
            "urlencode[{}]",
            case["label"].as_str().unwrap()
        );
    }
}

#[test]
fn websign_matches_python() {
    let data = vectors();
    for case in data["websign"].as_array().expect("websign 数组") {
        let pairs = pairs_of(&case["pairs"]);
        let signed = sign(
            &pairs,
            case["uifid"].as_str().unwrap(),
            case["timestamp"].as_i64().unwrap(),
        );
        let label = case["label"].as_str().unwrap();
        assert_eq!(
            signed.query,
            case["want_query"].as_str().unwrap(),
            "query[{label}]"
        );
        assert_eq!(
            signed.signature,
            case["want_signature"].as_str().unwrap(),
            "signature[{label}]"
        );
        let want_headers = case["want_headers"].as_object().expect("headers 是对象");
        assert_eq!(signed.headers.len(), want_headers.len(), "headers[{label}]");
        for (name, value) in &signed.headers {
            assert_eq!(
                value,
                want_headers.get(name).and_then(Value::as_str).unwrap_or(""),
                "header {name}[{label}]"
            );
        }
    }
}

#[test]
fn sm3_matches_python_and_the_standard() {
    let data = vectors();
    for case in data["sm3"].as_array().expect("sm3 数组") {
        let hex = case["msg_hex"].as_str().unwrap();
        let message: Vec<u8> = (0..hex.len())
            .step_by(2)
            .map(|index| u8::from_str_radix(&hex[index..index + 2], 16).unwrap())
            .collect();
        assert_eq!(sm3_hexdigest(&message), case["want"].as_str().unwrap());
    }
}
