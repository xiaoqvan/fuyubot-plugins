//! 抖音签名栈：`/aweme/v1/web/aweme/detail/` 这类**签名保护区**端点的门禁。
//!
//! 三条腿，全部是纯函数（可离线对拍）：
//!
//! | 模块 | 干什么 |
//! | --- | --- |
//! | [`encode`] | Python `urllib.parse` 三种百分号编码语义 |
//! | [`sm3`] | SM3（GB/T 32905-2016），a_bogus 的哈希 |
//! | [`websign`] | SecSDK web 签名：`md5("{uifid}_{timestamp}_{SALT}_{query}")` |
//!
//! 组合顺序（平台的，不是我们的）：业务参数 → `urlencode` → **a_bogus** →
//! `verifyFp`/`fp` → `uifid` → `timestamp` → **x-secsdk-web-signature**。
//!
//! 移植自 Evil0ctal/Douyin_TikTok_Download_API（Apache-2.0）的
//! `dtk/signing/native/`，行为与 Python 版逐字节一致——对拍向量在 crate 根的
//! `sign-vectors.json`，测试见 [`vectors`]。

pub mod abogus;
pub mod detail;
pub mod encode;
pub mod sm3;
pub mod websign;

#[cfg(test)]
mod vectors;
