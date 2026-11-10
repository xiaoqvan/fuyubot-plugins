//! GKY 封禁记录的转发：群里某个第三方机器人播报封禁时，顺手转到本群日志频道。
//!
//! 对应 TS 版 `update/gkyBanForward.ts`。逻辑很窄（只认一个 Bot + 一种文本格式），
//! 但格式是**自然语言**，所以这里不用正则，而是按标记切成三段：`TGID:` 的数字、
//! 「已經封左 / Banned」、箭头后的原因。
//!
//! ⚠️ 这一段是原样保留的「别人家的业务」：封禁由 @GodKnowYouBot 做，本插件只转发记录，
//! 不参与判定，也不对 TGID 做任何处置。

use serde_json::Value;

use crate::runtime::Runtime;
use crate::text;
use crate::util::fmt_local_plain;

/// 播报封禁的机器人。
const GKY_BOT_USER_ID: i64 = 8_048_217_739;
/// 箭头字符（U+1F449，4 字节）。
const ARROW: char = '👉';

/// 从消息里提取 `(tgid, reason)`；不是 GKY 封禁格式就 `None`。
pub fn parse_ban_record(body: &str) -> Option<(i64, String)> {
    let start = body.find("TGID:")? + "TGID:".len();
    let rest = body[start..].trim_start();
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    if digits.is_empty() {
        return None;
    }
    let tgid = digits.parse::<i64>().ok()?;
    let after_digits = &rest[digits.len()..];
    let arrow = after_digits.find(ARROW)?;
    let prefix = &after_digits[..arrow];
    if !prefix.contains("已經封左") && !prefix.contains("Banned") {
        return None;
    }
    let reason = after_digits[arrow + ARROW.len_utf8()..]
        .trim()
        .lines()
        .next()
        .unwrap_or("")
        .trim()
        .to_owned();
    if reason.is_empty() {
        return None;
    }
    Some((tgid, reason))
}

/// 处理一条消息：是 GKY 的封禁播报就转到群日志频道。
pub async fn forward_ban_record(rt: &Runtime, message: &Value) {
    let Some(chat_id) = message.get("chat_id").and_then(Value::as_i64) else {
        return;
    };
    let Some(sender) = message
        .get("sender_id")
        .and_then(|sender| sender.get("user_id"))
        .and_then(Value::as_i64)
    else {
        return;
    };
    if sender != GKY_BOT_USER_ID {
        return;
    }
    let Some(content) = message.get("content") else {
        return;
    };
    if content.get("@type").and_then(Value::as_str) != Some("messageText") {
        return;
    }
    let body = content
        .get("text")
        .and_then(|text| text.get("text"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let Some((tgid, reason)) = parse_ban_record(body) else {
        return;
    };

    let config = rt.db.config_or_default(chat_id).await;
    let Some(channel) = config.log_channel else {
        return;
    };
    let title = rt.td.chat_title(chat_id).await;
    let time = message
        .get("date")
        .and_then(Value::as_i64)
        .map(|seconds| fmt_local_plain(seconds * 1000))
        .unwrap_or_default();
    let user_mention = rt
        .td
        .sender_mention(crate::api::Sender::User(tgid), true)
        .await;
    let body = format!(
        "#GKYBAN\nTGID: {user_mention}\nChat ID: {chat_id}({title})\nReason: [{reason}](https://gkybot.gmeow.cc/banlist?tgid={tgid})\ntime: {time}\n\n该用户为 @GodKnowYouBot 封禁，与本bot无关,本BOT只是转发封禁记录到日志频道,请勿在群内讨论封禁事宜,如有疑问请联系群主或管理员"
    );
    let _ = rt
        .td
        .send_text(
            channel,
            text::markdown(&body),
            None,
            Vec::new(),
            None,
            Some(false),
        )
        .await;
    crate::log::info(
        &rt.host,
        format!(
            "[GKYBanForward] 已转发封禁记录到日志频道 {channel}: TGID={tgid}, Group={title}({chat_id})"
        ),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_cantonese_format() {
        let body = "TGID: 8775864277\n已經封左 👉 販賣廣告\n如被誤封就搵 機械人";
        assert_eq!(
            parse_ban_record(body),
            Some((8_775_864_277, "販賣廣告".to_owned()))
        );
    }

    #[test]
    fn parses_english_format() {
        let body = "TGID: 42 Banned 👉 spam links\nmore";
        assert_eq!(parse_ban_record(body), Some((42, "spam links".to_owned())));
    }

    #[test]
    fn rejects_other_messages() {
        assert_eq!(parse_ban_record("普通聊天 TGID: 42"), None);
        assert_eq!(parse_ban_record("TGID: abc 已經封左 👉 x"), None);
        assert_eq!(parse_ban_record("TGID: 42 其它 👉 x"), None);
    }
}
