use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;

use serde_json::Value;

use super::*;
use crate::sign::encode::{quote_strict, urlencode};
use crate::sign::websign::{VERIFY_FP_PARAMS, pairs_from_query, sign};

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

fn stream_of(value: &Value) -> Vec<f64> {
    value
        .as_array()
        .expect("rng_stream 是数组")
        .iter()
        .map(|item| item.as_f64().expect("rng_stream 是浮点"))
        .collect()
}

/// 重放录好的 `rng_stream`（Python 侧 `random.Random(seed)` 逐次
/// `random()` 的返回值）。返回的队列用来断言流被**恰好**用尽。
fn replay_rng(stream: &[f64]) -> (Rc<RefCell<VecDeque<f64>>>, impl FnMut() -> f64) {
    let queue = Rc::new(RefCell::new(
        stream.iter().copied().collect::<VecDeque<_>>(),
    ));
    let inner = Rc::clone(&queue);
    let rng = move || {
        inner
            .borrow_mut()
            .pop_front()
            .expect("rng 流耗尽：移植代码比 Python 多抽了随机数")
    };
    (queue, rng)
}

/// 四条 a_bogus 向量逐字节对拍 Python。
#[test]
fn abogus_vectors_match_python() {
    let data = vectors();
    for case in data["abogus"].as_array().expect("abogus 数组") {
        let label = case["label"].as_str().unwrap();
        let stream = stream_of(&case["rng_stream"]);
        let (queue, mut rng) = replay_rng(&stream);
        let mut signer = ABogus::new(
            case["user_agent"].as_str().unwrap(),
            case["browser_info"].as_str().unwrap(),
            &mut rng,
        );
        let got = signer.get_value(
            case["query"].as_str().unwrap(),
            case["body"].as_str().unwrap(),
            case["content_type"].as_str().unwrap(),
            case["now_ms"].as_u64().unwrap(),
        );
        assert_eq!(got, case["want"].as_str().unwrap(), "abogus[{label}]");
        assert!(queue.borrow().is_empty(), "abogus[{label}] rng 流未用尽");
    }
}

/// 端到端 flow：urlencode → a_bogus（值 quote(safe='') 回填）→ 拆对
/// 重序列化 + `verifyFp`/`fp` → websign.sign，四个 want 全对拍。
#[test]
fn flow_vector_matches_python() {
    let data = vectors();
    let case = &data["flow"].as_array().expect("flow 数组")[0];
    let params = pairs_of(&case["params"]);
    let query = urlencode(&params);
    let stream = stream_of(&case["rng_stream"]);
    let (queue, mut rng) = replay_rng(&stream);
    let mut signer = ABogus::new(
        case["user_agent"].as_str().unwrap(),
        DEFAULT_BROWSER_INFO,
        &mut rng,
    );
    let bogus = signer.get_value(&query, "", "", case["now_ms"].as_u64().unwrap());
    assert_eq!(
        bogus,
        case["want_a_bogus"].as_str().unwrap(),
        "flow a_bogus"
    );
    assert!(queue.borrow().is_empty(), "flow rng 流未用尽");

    let query2 = format!("{query}&a_bogus={}", quote_strict(&bogus));
    let mut pairs = pairs_from_query(&query2);
    let verify_fp = case["verify_fp"].as_str().unwrap();
    for name in VERIFY_FP_PARAMS {
        pairs.push(((*name).to_owned(), verify_fp.to_owned()));
    }
    let signed = sign(
        &pairs,
        case["uifid"].as_str().unwrap(),
        case["timestamp"].as_i64().unwrap(),
    );
    assert_eq!(
        signed.query,
        case["want_query"].as_str().unwrap(),
        "flow query"
    );
    assert_eq!(
        signed.signature,
        case["want_signature"].as_str().unwrap(),
        "flow signature"
    );
    let want_headers = case["want_headers"].as_object().expect("headers 是对象");
    assert_eq!(signed.headers.len(), want_headers.len(), "flow headers");
    for (name, value) in &signed.headers {
        assert_eq!(
            value,
            want_headers.get(name).and_then(Value::as_str).unwrap_or(""),
            "flow header {name}"
        );
    }
}

/// 向量里的每个签名都必须通过结构校验，且解码回来时钟 / `page_id` /
/// `aid` / 几何串与生成输入一致（`ink` 恰好慢一毫秒）。
#[test]
fn vector_signatures_pass_structural_checks() {
    let data = vectors();
    for case in data["abogus"].as_array().expect("abogus 数组") {
        let label = case["label"].as_str().unwrap();
        let want = case["want"].as_str().unwrap();
        assert_eq!(structure_error(want), None, "structure_error[{label}]");
        let decoded = decode(want).expect("want 可解码");
        assert_eq!(
            decoded.now_ms,
            case["now_ms"].as_u64().unwrap(),
            "now_ms[{label}]"
        );
        assert_eq!(
            decoded.ink_ms,
            case["now_ms"].as_u64().unwrap() - 1,
            "ink_ms[{label}]"
        );
        assert_eq!(decoded.page_id, PAGE_ID, "page_id[{label}]");
        assert_eq!(decoded.aid, AID, "aid[{label}]");
        assert_eq!(
            decoded.browser_info,
            case["browser_info"].as_str().unwrap(),
            "browser_info[{label}]"
        );
    }
}

/// 噪声层可逆：`unmask_pair(mask_pair(p)) == p`、`collapse(expand(b)) == b`。
#[test]
fn noise_layers_round_trip() {
    let mut rng = || 0.5f64;
    let pair = mask_pair(HEADER_MAGIC, &mut rng, None, None);
    assert_eq!(unmask_pair(&pair), HEADER_MAGIC);
    let pair = mask_pair(
        (SDK_VERSION[2], SDK_VERSION[3]),
        &mut rng,
        Some(0x12),
        Some(0x34),
    );
    assert_eq!(unmask_pair(&pair), (SDK_VERSION[2], SDK_VERSION[3]));

    // 97 字节 = 32 组 + 1 字节尾巴，走 SDK 收尾分支。
    let body: Vec<u8> = (0u8..=96).collect();
    let frame = expand_noise(&body, &mut rng);
    assert_eq!(frame.len(), 129);
    assert_eq!(collapse_noise(&frame), body);
}

/// `multipart/form-data`（大小写不敏感）时 body 哈希前被置空——与
/// 「空体签名」逐字节相同。
#[test]
fn multipart_body_is_blanked_before_hashing() {
    let data = vectors();
    let case = &data["abogus"].as_array().expect("abogus 数组")[0];
    let query = case["query"].as_str().unwrap();
    let now_ms = case["now_ms"].as_u64().unwrap();
    let ua = case["user_agent"].as_str().unwrap();
    let info = case["browser_info"].as_str().unwrap();
    let stream = stream_of(&case["rng_stream"]);

    let (queue_a, mut rng_a) = replay_rng(&stream);
    let mut with_body = ABogus::new(ua, info, &mut rng_a);
    let signed_form = with_body.get_value(
        query,
        "ignored=body",
        "Multipart/Form-Data; boundary=x",
        now_ms,
    );
    assert!(queue_a.borrow().is_empty(), "multipart rng 流未用尽");

    let (queue_b, mut rng_b) = replay_rng(&stream);
    let mut no_body = ABogus::new(ua, info, &mut rng_b);
    let signed_empty = no_body.get_value(query, "", "", now_ms);
    assert!(queue_b.borrow().is_empty(), "empty rng 流未用尽");

    assert_eq!(signed_form, signed_empty);
    assert_eq!(signed_form, case["want"].as_str().unwrap());
}

/// 时钟早于双周期纪元直接拒绝（Python `ValueError` 语义）。
#[test]
#[should_panic(expected = "clock is before the a_bogus epoch")]
fn rejects_a_clock_before_the_epoch() {
    let mut rng = || 0.5f64;
    let mut signer = ABogus::new("ua", DEFAULT_BROWSER_INFO, &mut rng);
    let _ = signer.get_value("", "", "", FORTNIGHT_EPOCH_MS - 1);
}
