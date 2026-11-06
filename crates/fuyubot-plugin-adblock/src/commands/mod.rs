//! 命令层：查**注册表** → 校验 scope/permission → 交给具体处理器。
//!
//! 命令清单与 TS 版 `index.ts` 的 `cmdHandlers` 一一对应（17 条），见 [`spec`]。
//!
//! | 命令 | 作用域 | 权限 |
//! | --- | --- | --- |
//! | `/adconfig` `/setlogchannel` `/whitelist` `/rmwhitelist` `/spam` | 群 | 所有人（handler 里再查群管理员） |
//! | `/qban` | 任意 | 所有人 |
//! | `/qadid` `/delad` `/addkw` `/editkw` `/addfwdbl` `/rmfwdbl` | 私聊 | Bot 主人 或 `admin_ids` |
//! | `/adkew` `/admatch` `/adaiadd` `/adaiset` | 私聊 | Bot 主人 |
//! | `/start aiConfig_<chat_id>` | 私聊 | 所有人（handler 里查目标群管理员） |
//!
//! ## 为什么权限在这里，而不是在封装里
//!
//! [`command_of`] 只解析 `/cmd@bot args`，「点名给别人的」靠 [`Command::is_for`] 过滤，
//! `Command` 里没有 sender；宿主也没有命令权限层。TS 的框架有（`PluginValidator`），
//! 所以这一层由本插件用 [`spec`] 补上——**一张表 + 一个入口**，而不是散在 17 个
//! handler 里（散着必然漏：`private` 作用域与 `admin` 档此前就漏了）。
//!
//! 拒绝是**静默**的：TS 的 `validateCommandAccess` 不通过就 `continue`，
//! 群里可能还装着别的 bot，回一句「你没权限」只会暴露命令存在。
//!
//! ## `/help`
//!
//! 宿主内置 help 插件通过服务 [`HELP_SERVICE`] 收集命令；[`register_to_help`] 在
//! `start()` 里登记 [`spec::COMMANDS`]。它是**软依赖**（`soft_inject`）：help 缺席时
//! 插件照常启动，只是 `/help` 里少本插件这几行。

pub mod ai_config;
pub mod ai_keywords;
pub mod ai_manager;
pub mod config;
pub mod forward;
pub mod keywords;
pub mod log_channel;
pub mod lookup;
pub mod owner;
pub mod spam;
pub mod spec;
pub mod whitelist;

use std::sync::Arc;

use fuyubot_abi::HostApi;
use serde_json::Value;

use crate::api::Sender;
use crate::log;
use crate::runtime::Runtime;

/// 宿主内置 `/help` 插件的命令注册服务（进程级共享，方法名 `register`）。
///
/// 跨仓契约：名字写错不会报错，只会让登记静默失败、`/help` 里看不到本插件的命令。
pub const HELP_SERVICE: &str = "fuyubot.help.commands";

/// 命令入口：查表 → scope → permission → handler。
///
/// 返回值表示「这条消息是不是本插件的命令」（`true` 也包括被权限拒绝的情况）。
/// **调用方不要据此跳过检测**：TS 里命令分发与消息检测是两条独立路径。
pub fn dispatch(rt: &Arc<Runtime>, update: &Value, sender: Sender) -> bool {
    let Some(command) = crate::simplify::command_of(update) else {
        return false;
    };
    // 表里没有的名字**不是命令**：交给检测流水线当普通文本扫。
    let Some(spec) = spec::find(&command.name) else {
        return false;
    };
    let Some(message) = update.get("message") else {
        return false;
    };
    // `@BotName` 的命令只有被点名的那个响应；裸命令都响应。
    let usernames = crate::simplify::my_usernames(&rt.host);
    if !command.is_for(&usernames) {
        return false;
    }
    // scope 先于 permission：私聊命令在群里连「仅限主人」都不该说。
    if !spec.allows_scope(rt, command.chat_id) {
        log::debug(
            &rt.host,
            format!(
                "忽略 /{}：scope {:?} 不允许 chat {}",
                spec.name, spec.scope, command.chat_id
            ),
        );
        return true;
    }
    if !spec.allows_permission(rt, sender) {
        log::debug(
            &rt.host,
            format!(
                "忽略 /{}：sender {} 权限不足（需要 {:?}）",
                spec.name,
                sender.id(),
                spec.permission
            ),
        );
        return true;
    }
    (spec.handler)(rt, &command, sender, message);
    true
}

/// 把命令参数按空白切成词（与 TS 版 `text.split(/\s+/)` 同义，但**不含命令本身**）。
pub fn words(command: &crate::simplify::Command) -> Vec<String> {
    command.args.split_whitespace().map(str::to_owned).collect()
}

/// help 插件的登记入参：`{"name":"…","description":"…"}`。
fn help_payload(spec: &spec::CommandSpec) -> String {
    serde_json::json!({ "name": spec.name, "description": spec.summary }).to_string()
}

/// help 插件成功时回 `{"ok":true}`，入参不合法回 `{"error":"…"}`。
///
/// `call_service` 在两种情况下都返回 `Ok`（它只表示「调用回来了」），
/// 所以要自己看回包，否则一条被拒的命令会被算成登记成功。
fn help_accepted(reply: &str) -> bool {
    serde_json::from_str::<Value>(reply)
        .ok()
        .and_then(|value| value.get("ok").and_then(Value::as_bool))
        == Some(true)
}

/// 把 [`spec::COMMANDS`] 全部登记进宿主 `/help`，返回 `(成功条数, 第一条错误)`。
///
/// **登记失败不是启动失败**：help 只是入口，缺了它 `/help` 少几行，
/// 不该让整个插件停在 Pending——所以调用方只 warn，不 `?`。
pub fn register_to_help(host: &HostApi) -> (usize, Option<String>) {
    if host.get_service(HELP_SERVICE).is_null() {
        return (
            0,
            Some(format!("宿主没有 {HELP_SERVICE}（没装 help 插件）")),
        );
    }
    let mut registered = 0;
    let mut first_error = None;
    for spec in spec::COMMANDS {
        let outcome = match host.call_service(HELP_SERVICE, "register", &help_payload(spec)) {
            Ok(reply) if help_accepted(&reply) => Ok(()),
            Ok(reply) => Err(format!("/{} 被拒：{reply}", spec.name)),
            Err(error) => Err(format!("/{} 登记失败：{error}", spec.name)),
        };
        match outcome {
            Ok(()) => registered += 1,
            Err(error) => {
                if first_error.is_none() {
                    first_error = Some(error);
                }
            }
        }
    }
    (registered, first_error)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 传参层次是这条链上最容易错的地方：`command_of` 吃的是整条 update。
    #[test]
    fn command_is_parsed_from_the_update_envelope() {
        let update = serde_json::json!({
            "@type": "updateNewMessage",
            "message": {
                "@type": "message",
                "id": 101,
                "chat_id": 777,
                "sender_id": { "@type": "messageSenderUser", "user_id": 777 },
                "is_outgoing": false,
                "content": {
                    "@type": "messageText",
                    "text": { "@type": "formattedText", "text": "/addkw ads 甲 乙 丙", "entities": [] }
                }
            }
        });
        let command = crate::simplify::command_of(&update).expect("应该解析出命令");
        assert_eq!(command.name, "addkw");
        assert_eq!(command.chat_id, 777);
        assert_eq!(command.args, "ads 甲 乙 丙");
        // 传成 `update["message"]` 就一条命令都收不到——把这条钉死。
        assert!(crate::simplify::command_of(&update["message"]).is_none());
    }

    #[test]
    fn command_words_split_on_whitespace() {
        let update = serde_json::json!({
            "message": {
                "id": 1,
                "chat_id": 2,
                "content": {
                    "@type": "messageText",
                    "text": { "@type": "formattedText", "text": "/adconfig  limit   ban" }
                }
            }
        });
        let command = crate::simplify::command_of(&update).expect("命令");
        assert_eq!(words(&command), vec!["limit", "ban"]);
    }

    /// 注册表是「什么算命令」的唯一真相：不是命令的文本必须返回 `None`，
    /// 这样消息流水线才会继续按普通消息检测。
    #[test]
    fn only_registered_names_are_commands() {
        assert!(spec::find("adconfig").is_some());
        assert!(spec::find("接单").is_none());
    }
}
