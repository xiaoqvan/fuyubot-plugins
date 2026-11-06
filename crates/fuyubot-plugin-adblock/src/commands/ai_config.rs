//! `/start aiConfig_<chat_id>` + 私聊里的三步配置流程。
//!
//! 交互与 TS 版一致：
//!
//! ```text
//! 群内 /adconfig 点「配置AI接口」
//!   → t.me/<bot>?start=aiConfig_-100…
//!   → 私聊：API Key → 后端地址（skip = OpenAI 默认）→ 模型名（skip = gpt-4o-mini）
//!   → 自动测试连通性 → 写回该群的 adb_config
//! ```
//!
//! 流程状态只活在内存里（[`crate::cache`]），30 分钟不作答就作废——它是一次性引导，
//! 没有写库的必要，重启丢掉反而干净。

use crate::simplify::Command;
use serde_json::Value;

use crate::ai::{self, mask_secret};
use crate::api::Sender;
use crate::cache::{PendingAiConfig, Step};
use crate::flow;
use crate::log;
use crate::runtime::Runtime;

/// 私聊配置链接里的前缀。
pub const AI_CONFIG_PREFIX: &str = "aiConfig_";
/// 后端地址缺省值。
const DEFAULT_BASE_URL: &str = "https://api.openai.com/v1";
/// 模型缺省值。
const DEFAULT_MODEL: &str = "gpt-4o-mini";

/// 解析 `aiConfig_-100123` → `-100123`。
pub fn parse_payload(payload: &str) -> Option<i64> {
    let raw = payload.strip_prefix(AI_CONFIG_PREFIX)?.trim();
    let chat_id = raw.parse::<i64>().ok()?;
    (chat_id < 0).then_some(chat_id)
}

/// `/start aiConfig_<chat_id>`
pub fn handle_start(rt: &Runtime, command: &Command, sender: Sender, _message: &Value) {
    let payload = command.args.trim();
    let Some(target) = parse_payload(payload) else {
        return;
    };
    let chat_id = command.chat_id;
    let reply_to = Some(command.message_id);
    let Some(user_id) = sender.user_id() else {
        return;
    };

    if !flow::is_group(rt, target) {
        flow::send_tip(
            rt,
            chat_id,
            "❌ 无法识别该群（Bot 可能已不在群内），请在群中使用 /adconfig 重新发起配置。",
            reply_to,
            30,
        );
        return;
    }
    if !rt.td.is_admin(target, sender) {
        flow::send_tip(
            rt,
            chat_id,
            "❌ 只有该群的管理员才能配置 AI 接口。",
            reply_to,
            30,
        );
        return;
    }

    rt.cache.begin_pending(user_id, target);
    let chain = if ai::uses_default_ai(rt, target) {
        "\n\n💡 该群当前使用内置默认接口，配置后将优先使用你的自定义接口。"
    } else {
        "\n\n⚠️ 该群未配置 AI 接口将无法使用 AI 审查，请完成以下配置。"
    };
    flow::send_with(
        rt,
        chat_id,
        &format!(
            "🔑 **开始配置群的 AI 广告审查接口**（OpenAI 兼容）\n\n目标群：`{target}`\n\n**第 1/3 步**：请发送你的 **API Key**（如 `sk-xxxx`）\n\n输入 `/cancel` 可随时取消。{chain}"
        ),
        reply_to,
        Vec::new(),
        0,
    );
}

/// 私聊消息是否被配置流程消费。
///
/// 只有「私聊 + 文本 + 该用户有进行中的配置」才会返回 `true`，其余情况原样放行。
pub fn consume_private(rt: &Runtime, message: &Value) -> bool {
    let Some(chat_id) = message.get("chat_id").and_then(Value::as_i64) else {
        return false;
    };
    if chat_id <= 0 {
        return false;
    }
    if message
        .get("content")
        .and_then(|content| content.get("@type"))
        .and_then(Value::as_str)
        != Some("messageText")
    {
        return false;
    }
    let Some(user_id) = message
        .get("sender_id")
        .and_then(|sender| sender.get("user_id"))
        .and_then(Value::as_i64)
    else {
        return false;
    };
    let Some(pending) = rt.cache.pending_snapshot(user_id) else {
        return false;
    };
    let text = message
        .get("content")
        .and_then(|content| content.get("text"))
        .and_then(|text| text.get("text"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_owned();
    let reply_to = message.get("id").and_then(Value::as_i64);

    if text == "/cancel" || text == "取消" {
        rt.cache.clear_pending(user_id);
        flow::send_tip(rt, chat_id, "❌ 已取消 AI 接口配置。", reply_to, 0);
        return true;
    }
    // 配置期间不处理别的命令。
    if text.starts_with('/') {
        return false;
    }

    match pending.step {
        Step::ApiKey => {
            if text.chars().count() < 8 {
                flow::send_tip(
                    rt,
                    chat_id,
                    "❌ API Key 格式过短，请重新发送有效的 API Key。",
                    reply_to,
                    0,
                );
                return true;
            }
            let mut pending = pending;
            pending.api_key = text.clone();
            pending.step = Step::BaseUrl;
            pending.created_at = crate::util::now_ms();
            rt.cache.store_pending(user_id, Some(pending));
            flow::send_tip(
                rt,
                chat_id,
                &format!(
                    "✅ API Key 已记录：`{}`\n\n**第 2/3 步**：请发送 **后端地址**（如 `https://api.openai.com/v1`），输入 `skip` 使用默认地址 `{DEFAULT_BASE_URL}`。",
                    mask_secret(&text)
                ),
                reply_to,
                0,
            );
            true
        }
        Step::BaseUrl => {
            let base_url = if text == "skip" {
                DEFAULT_BASE_URL.to_owned()
            } else {
                match normalize_url(&text) {
                    Some(url) => url,
                    None => {
                        flow::send_tip(
                            rt,
                            chat_id,
                            "❌ 后端地址格式不正确，请发送完整 URL（如 https://api.openai.com/v1）。",
                            reply_to,
                            0,
                        );
                        return true;
                    }
                }
            };
            let mut pending = pending;
            pending.base_url = base_url.clone();
            pending.step = Step::Model;
            pending.created_at = crate::util::now_ms();
            rt.cache.store_pending(user_id, Some(pending));
            flow::send_tip(
                rt,
                chat_id,
                &format!(
                    "✅ 后端地址已记录：`{base_url}`\n\n**第 3/3 步**：请发送 **模型名称**（如 `gpt-4o-mini`），输入 `skip` 使用默认模型 `{DEFAULT_MODEL}`。"
                ),
                reply_to,
                0,
            );
            true
        }
        Step::Model => {
            let model = if text == "skip" {
                DEFAULT_MODEL.to_owned()
            } else {
                text.clone()
            };
            let target = pending.chat_id;
            let api_key = pending.api_key.clone();
            let base_url = pending.base_url.clone();
            rt.db.update_config(target, |config| {
                config.ai_api_key = Some(api_key.clone());
                config.ai_base_url = Some(base_url.clone());
                config.ai_model = Some(model.clone());
            });
            flow::send_tip(
                rt,
                chat_id,
                &format!(
                    "✅ 模型已记录：`{model}`\n\n**🔧 测试连接**：正在使用此配置测试 AI 接口连通性…\n\n请稍候，测试完成后将显示最终结果。"
                ),
                reply_to,
                0,
            );

            // 当场测一次（这是工作线程，不需要再开线程）
            let test = ai::test_connection(rt, &api_key, &base_url, &model);
            rt.cache.clear_pending(user_id);
            match test {
                Ok(()) => {
                    log::info(
                        &rt.host,
                        format!(
                            "群 {target} 由用户 {user_id} 配置了自定义 AI 接口（模型: {model}，后端: {base_url}），测试通过"
                        ),
                    );
                    flow::send_tip(
                        rt,
                        chat_id,
                        &format!(
                            "🎉 **AI 接口配置完成！**\n\n目标群：`{target}`\n模型：`{model}`\n后端：`{base_url}`\nAPI Key：`{}`\n\n✅ 测试通过：AI 接口连通正常。\n\nℹ️ 该群 AI 深度审核尚未开启，请回到群内使用 /adconfig 打开 `ai_review_enabled` 开关。",
                            mask_secret(&api_key)
                        ),
                        reply_to,
                        0,
                    );
                }
                Err(error) => {
                    log::warn(
                        &rt.host,
                        format!(
                            "群 {target} AI 接口测试失败（模型: {model}，后端: {base_url}）：{error}"
                        ),
                    );
                    flow::send_tip(
                        rt,
                        chat_id,
                        &format!(
                            "❌ **AI 接口配置已保存，但测试失败！**\n\n目标群：`{target}`\n模型：`{model}`\n后端：`{base_url}`\nAPI Key：`{}`\n\n⚠️ 测试错误：`{}`\n\n请检查 API Key、后端地址和模型名称是否正确，然后使用 /adconfig 重新配置。",
                            mask_secret(&api_key),
                            crate::util::truncate(&error, 300)
                        ),
                        reply_to,
                        0,
                    );
                }
            }
            true
        }
    }
}

/// 只认 http(s) + 有主机名 + 去掉尾部斜杠。
fn normalize_url(text: &str) -> Option<String> {
    let text = text.trim();
    let rest = text
        .strip_prefix("https://")
        .or_else(|| text.strip_prefix("http://"))?;
    let host = rest.split('/').next()?;
    if host.is_empty() || !host.contains('.') || host.contains(' ') {
        return None;
    }
    let mut url = text.to_owned();
    while url.ends_with('/') {
        url.pop();
    }
    Some(url)
}

/// 让 `PendingAiConfig` 的构造保持集中（测试/未来扩展用）。
#[allow(dead_code)]
fn pending_for(chat_id: i64) -> PendingAiConfig {
    PendingAiConfig {
        chat_id,
        step: Step::ApiKey,
        api_key: String::new(),
        base_url: String::new(),
        created_at: crate::util::now_ms(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_payload() {
        assert_eq!(
            parse_payload("aiConfig_-1001234567890"),
            Some(-1_001_234_567_890)
        );
        assert_eq!(parse_payload("aiConfig_123"), None);
        assert_eq!(parse_payload("something"), None);
    }

    #[test]
    fn normalizes_urls() {
        assert_eq!(
            normalize_url("https://api.openai.com/v1/"),
            Some("https://api.openai.com/v1".to_owned())
        );
        assert_eq!(normalize_url("ftp://x"), None);
        assert_eq!(normalize_url("https://no-dot"), None);
    }
}
