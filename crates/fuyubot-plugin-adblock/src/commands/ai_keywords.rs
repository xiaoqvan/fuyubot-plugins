//! `/adkew`：维护全局 AI 审查触发关键词（仅 Bot 主人）。
//!
//! 「触发词」不是「广告词」：它只决定**要不要送 AI 看**，不直接封人。
//! 所以这里对「包含关系」处理得比较细：
//! - 新词被已有词包含 → 拦下（已有词覆盖更广，再添加没意义）；
//! - 新词包含已有词 → 允许，但提示「匹配范围会变宽」。

use fuyubot_simplify::Command;

use crate::api::Sender;
use crate::flow;
use crate::log;
use crate::runtime::Runtime;
use crate::util::escape_inline_code;

const USAGE: &str = "用法:\n  /adkew add <关键词>\n  /adkew rm <关键词>\n  /adkew list\n\n示例:\n  /adkew add 兼职\n  /adkew rm 兼职\n  /adkew list";

pub fn handle(rt: &Runtime, command: &Command, sender: Sender) {
    let chat_id = command.chat_id;
    let reply_to = Some(command.message_id);
    if !flow::ensure_owner(rt, chat_id, sender, reply_to) {
        return;
    }
    let words = super::words(command);
    let Some(sub) = words.first().map(String::as_str) else {
        flow::send_tip(rt, chat_id, USAGE, reply_to, 30);
        return;
    };
    match sub {
        "add" if words.len() >= 2 => {
            let keyword = words[1..].join(" ").trim().to_owned();
            if keyword.is_empty() {
                flow::send_tip(rt, chat_id, "❌ 关键词不能为空。", reply_to, 10);
                return;
            }
            add(rt, chat_id, reply_to, &keyword);
        }
        "rm" if words.len() >= 2 => {
            let keyword = words[1..].join(" ").trim().to_owned();
            if keyword.is_empty() {
                flow::send_tip(rt, chat_id, "❌ 关键词不能为空。", reply_to, 10);
                return;
            }
            remove(rt, chat_id, reply_to, &keyword);
        }
        "list" => list(rt, chat_id, reply_to),
        _ => {
            flow::send_tip(
                rt,
                chat_id,
                "❌ 未知子命令。可用: add, rm, list",
                reply_to,
                10,
            );
        }
    }
}

fn normalize(keyword: &str) -> String {
    keyword.trim().to_lowercase()
}

/// 添加前的包含关系检查。
enum Conflict {
    /// 拦下（返回给用户的原因）。
    Block(String),
    /// 允许，但要提示。
    Allow(String),
}

fn find_conflict(keyword: &str, current: &[String]) -> Option<Conflict> {
    let target = normalize(keyword);
    if target.is_empty() {
        return None;
    }
    for existing in current {
        let normalized = normalize(existing);
        if normalized.is_empty() {
            continue;
        }
        if normalized == target {
            return Some(Conflict::Block(format!(
                "⚠️ 全局关键词 `{}` 已存在。",
                escape_inline_code(existing)
            )));
        }
        if normalized.contains(&target) {
            return Some(Conflict::Block(format!(
                "⚠️ 已有更宽的关键词 `{}`，已覆盖 `{}`，无需重复添加。",
                escape_inline_code(existing),
                escape_inline_code(keyword)
            )));
        }
        if target.contains(&normalized) {
            return Some(Conflict::Allow(format!(
                "ℹ️ 已有更具体的关键词 `{}`，添加 `{}` 后匹配范围会变宽。",
                escape_inline_code(existing),
                escape_inline_code(keyword)
            )));
        }
    }
    None
}

fn add(rt: &Runtime, chat_id: i64, reply_to: Option<i64>, keyword: &str) {
    let current = rt.db.global_ai_keywords();
    let mut notice = String::new();
    if let Some(conflict) = find_conflict(keyword, &current) {
        match conflict {
            Conflict::Block(message) => {
                flow::send_tip(rt, chat_id, &message, reply_to, 15);
                return;
            }
            Conflict::Allow(message) => notice = format!("\n{message}"),
        }
    }
    let mut keywords = current;
    keywords.push(keyword.to_owned());
    rt.db.set_global_ai_keywords(keywords);
    flow::send_tip(
        rt,
        chat_id,
        &format!(
            "✅ 已添加全局 AI 审查触发关键词：`{}`{notice}",
            escape_inline_code(keyword)
        ),
        reply_to,
        20,
    );
    log::info(&rt.host, format!("[AIKeywords] 添加触发关键词: {keyword}"));
}

fn remove(rt: &Runtime, chat_id: i64, reply_to: Option<i64>, keyword: &str) {
    let current = rt.db.global_ai_keywords();
    let target = normalize(keyword);
    let mut keywords = current.clone();
    keywords.retain(|existing| normalize(existing) != target);
    if keywords.len() == current.len() {
        flow::send_tip(
            rt,
            chat_id,
            &format!("❌ 未找到关键词 `{}`。", escape_inline_code(keyword)),
            reply_to,
            10,
        );
        return;
    }
    rt.db.set_global_ai_keywords(keywords);
    flow::send_tip(
        rt,
        chat_id,
        &format!(
            "✅ 已移除全局 AI 审查触发关键词：`{}`",
            escape_inline_code(keyword)
        ),
        reply_to,
        20,
    );
    log::info(&rt.host, format!("[AIKeywords] 移除触发关键词: {keyword}"));
}

fn list(rt: &Runtime, chat_id: i64, reply_to: Option<i64>) {
    let keywords = rt.db.global_ai_keywords();
    if keywords.is_empty() {
        flow::send_tip(rt, chat_id, "📭 暂无全局 AI 审查触发关键词。", reply_to, 15);
        return;
    }
    let body = keywords
        .iter()
        .enumerate()
        .map(|(index, keyword)| format!("{}. `{}`", index + 1, escape_inline_code(keyword)))
        .collect::<Vec<_>>()
        .join("\n");
    flow::send_tip(
        rt,
        chat_id,
        &format!(
            "🔑 **全局 AI 审查触发关键词**（共 {} 个）\n\n{body}\n\n这些关键词对所有开启了 AI 审查的群组生效。",
            keywords.len()
        ),
        reply_to,
        60,
    );
}
