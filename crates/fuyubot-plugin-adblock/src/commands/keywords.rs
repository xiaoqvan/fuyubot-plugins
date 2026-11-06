//! `/addkw`、`/editkw`：维护 `ads_blacklist` / `bio_blacklist` 的关键词组。
//!
//! 关键词组是 **AND** 语义（一组里每个词都出现才算命中），这是原版的设计：
//! 「兼职」「日赚」分开都能命中，合成一组就只会打掉同时说这两件事的广告。

use fuyubot_simplify::Command;

use crate::api::Sender;
use crate::flow;
use crate::log;
use crate::runtime::Runtime;

const ADD_USAGE: &str = "用法: /addkw <ads|bio> [r18] <kw1> <kw2> ...\n例:\n/addkw ads 赌博 博彩\n/addkw ads r18 成人 色情\n/addkw bio 推广 广告";
const EDIT_USAGE: &str =
    "用法: /editkw <_id> <kw1> <kw2> ...\n例: /editkw 60c72b2f9b1d8e1a2c3d4e5f 赌博 博彩";

/// `/addkw`
pub fn add(rt: &Runtime, command: &Command, sender: Sender) {
    let chat_id = command.chat_id;
    let reply_to = Some(command.message_id);
    if !flow::ensure_bot_admin(rt, chat_id, sender, reply_to) {
        return;
    }
    let words = super::words(command);
    // `words` **不含命令本身**，所以「集合 + 至少 1 个关键词」是 2 个词；
    // TS 那边判的是含命令的 `parts.length < 3`，等价于这里的 `< 2`。
    if words.len() < 2 {
        flow::send_tip(rt, chat_id, ADD_USAGE, reply_to, 20);
        return;
    }
    let collection = words[0].to_lowercase();
    if collection != "ads" && collection != "bio" {
        flow::send_tip(
            rt,
            chat_id,
            &format!("❌ 集合类型无效: {}，请使用 ads 或 bio。", words[0]),
            reply_to,
            10,
        );
        return;
    }
    let (is_r18, start) = if words[1].eq_ignore_ascii_case("r18") {
        (true, 2)
    } else {
        (false, 1)
    };
    let keywords: Vec<String> = words[start..]
        .iter()
        .map(|word| word.trim().to_owned())
        .filter(|word| !word.is_empty())
        .collect();
    if keywords.is_empty() {
        flow::send_tip(rt, chat_id, "❌ 请至少提供一个关键词。", reply_to, 10);
        return;
    }

    let result = if collection == "ads" {
        rt.db.add_ad_keywords(&keywords, is_r18)
    } else {
        rt.db.add_bio(&keywords)
    };
    match result {
        Ok(id) => {
            flow::send_tip(
                rt,
                chat_id,
                &format!("✅ 已向 {collection}_blacklist 添加关键词组，记录 ID: {id}"),
                reply_to,
                20,
            );
            log::info(
                &rt.host,
                format!(
                    "[addkw] {collection} 新增 {id}（{} 个关键词）",
                    keywords.len()
                ),
            );
        }
        Err(error) => {
            flow::send_tip(rt, chat_id, &format!("❌ 添加失败: {error}"), reply_to, 20);
        }
    }
}

/// `/editkw`
pub fn edit(rt: &Runtime, command: &Command, sender: Sender) {
    let chat_id = command.chat_id;
    let reply_to = Some(command.message_id);
    if !flow::ensure_bot_admin(rt, chat_id, sender, reply_to) {
        return;
    }
    let words = super::words(command);
    // `words` **不含命令本身**，所以「集合 + 至少 1 个关键词」是 2 个词；
    // TS 那边判的是含命令的 `parts.length < 3`，等价于这里的 `< 2`。
    if words.len() < 2 {
        flow::send_tip(rt, chat_id, EDIT_USAGE, reply_to, 20);
        return;
    }
    let id = &words[0];
    if !crate::util::is_object_id(id) {
        flow::send_tip(
            rt,
            chat_id,
            &format!("❌ 无效的 _id: {id}，需为 mongodb 中的 24 位十六进制字符串。"),
            reply_to,
            10,
        );
        return;
    }
    let keywords: Vec<String> = words[1..]
        .iter()
        .map(|word| word.trim().to_owned())
        .filter(|word| !word.is_empty())
        .collect();
    if keywords.is_empty() {
        flow::send_tip(rt, chat_id, "❌ 请至少提供一个关键词。", reply_to, 10);
        return;
    }
    match rt.db.update_keywords(id, &keywords) {
        Ok(Some(collection)) => {
            flow::send_tip(
                rt,
                chat_id,
                &format!("✅ 已更新集合 {} 中 _id={id} 的关键词。", collection.name()),
                reply_to,
                20,
            );
        }
        Ok(None) => {
            flow::send_tip(
                rt,
                chat_id,
                &format!("❌ 未找到对应记录: {id}"),
                reply_to,
                20,
            );
        }
        Err(error) => {
            flow::send_tip(rt, chat_id, &format!("❌ 更新失败: {error}"), reply_to, 20);
        }
    }
}
