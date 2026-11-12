//! 命令层：查**注册表** → 校验 scope/permission → 交给具体处理器。
//!
//! 命令清单与 TS 版 `index.ts` 的 `cmdHandlers` 一一对应（17 条），见 [`spec`]。
//!
//! | 命令 | 作用域 | 权限 |
//! | --- | --- | --- |
//! | `/adconfig` `/setlogchannel` `/whitelist` `/rmwhitelist` `/spam` | 群 | 所有人（handler 里再查群管理员） |
//! | `/qban` | 任意 | 所有人 |
//! | `/qadid` `/delad` `/addkw` `/editkw` `/addfwdbl` `/rmfwdbl` | 私聊 | 宿主 `[roles].admins` 或主人 |
//! | `/adkew` `/admatch` `/adaiadd` `/adaiset` | 私聊 | 宿主 `[roles].owners`（主人） |
//! | `/start aiConfig_<chat_id>` | 私聊 | 所有人（handler 里查目标群管理员） |
//!
//! ## 权限
//!
//! ABI v6 起宿主有了命令能力表：`role` / `scopes` 在分发前就能拦。本插件仍保留
//! [`spec`] 里的 scope / permission 检查——群管、主人这些判断要查本群成员，
//! handler 自己最清楚；两层是**叠加**的，不改 TS 版的语义。
//!
//! 拒绝是**静默**的：TS 的 `validateCommandAccess` 不通过就 `continue`，
//! 群里可能还装着别的 bot，回一句「你没权限」只会暴露命令存在。
//!
//! ## `/help`
//!
//! 命令表归宿主（[`HOST_COMMANDS`]）；[`register_commands`] 在 `start()` 里登记
//! [`spec::COMMANDS`] 并带上 `role` / `scopes`。宿主服务启动时就在，登记失败不是
//! 启动失败：`/help` 里少本插件几行而已。

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

use fuyubot_abi::HostApi;
use serde_json::Value;

use crate::api::Sender;
use crate::log;
use crate::runtime::Runtime;

/// 宿主预置的命令能力表（方法 `register`）。
///
/// 跨仓契约：名字写错不会报错，只会让登记静默失败、`/help` 里看不到本插件的命令。
pub use crate::roles::HOST_COMMANDS;

/// 命令入口：查表 → scope → permission → handler。
///
/// 返回值表示「这条消息是不是本插件的命令」（`true` 也包括被权限拒绝的情况）。
/// **调用方不要据此跳过检测**：TS 里命令分发与消息检测是两条独立路径。
pub async fn dispatch(rt: &Runtime, update: &Value, sender: Sender) -> bool {
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
    if !spec.allows_scope(rt, command.chat_id).await {
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
    (spec.handler)(rt, &command, sender, message).await;
    true
}

/// 把命令参数按空白切成词（与 TS 版 `text.split(/\s+/)` 同义，但**不含命令本身**）。
pub fn words(command: &crate::simplify::Command) -> Vec<String> {
    command.args.split_whitespace().map(str::to_owned).collect()
}

/// 命令表的登记入参：`{"name","description","role","scopes"}`。
///
/// `role` 由 [`spec::Permission`] 决定，`scopes` 由 [`spec::Scope`] 决定；`hidden`
/// 不写（默认 `false`），本插件的命令都要在 `/help` 里露面。
fn command_payload(spec: &spec::CommandSpec) -> String {
    serde_json::json!({
        "name": spec.name,
        "description": spec.summary,
        "role": spec.permission.role(),
        "scopes": spec.scope.scopes(),
    })
    .to_string()
}

/// 宿主成功时回 `{"ok":true,…}`，入参不合法回 `{"error":"…"}`。
///
/// ABI v6 起 `call_service` 的 `Err` 是**真失败**（服务不存在 / 没这个方法 / 入参非法，
/// 原因由宿主经 `last_error` 带回）；`Ok` 只表示调用回来了，所以还要看回包——
/// `{"error":…}` 表示宿主拒了这条命令（比如同名冲突），不能算登记成功。
fn registration_accepted(reply: &str) -> bool {
    serde_json::from_str::<Value>(reply)
        .ok()
        .and_then(|value| value.get("ok").and_then(Value::as_bool))
        == Some(true)
}

/// 把 [`spec::COMMANDS`] 全部登记进宿主命令表，返回 `(成功条数, 第一条错误)`。
///
/// **登记失败不是启动失败**：命令表只是 `/help` 与闸门的入口，缺了它命令本身
/// 仍然能用，不该让整个插件停在 Pending——所以调用方只 warn，不 `?`。
pub fn register_commands(host: &HostApi) -> (usize, Option<String>) {
    let mut registered = 0;
    let mut first_error = None;
    for spec in spec::COMMANDS {
        let outcome = match host.call_service(HOST_COMMANDS, "register", &command_payload(spec)) {
            Ok(reply) if registration_accepted(&reply) => Ok(()),
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

    /// 登记到宿主命令表时，scope / permission 必须翻译成 `scopes` / `role`——
    /// 少了它们，宿主闸门与 `/help` 都只能按最宽松的默认值处理。
    #[test]
    fn registration_payload_carries_role_and_scopes() {
        let payload = |name: &str| -> Value {
            serde_json::from_str(&command_payload(spec::find(name).expect("命令存在")))
                .expect("登记入参是 JSON")
        };

        let addkw = payload("addkw");
        assert_eq!(addkw["name"], "addkw");
        assert_eq!(addkw["role"], "admin");
        assert_eq!(addkw["scopes"], serde_json::json!(["private"]));

        let adkew = payload("adkew");
        assert_eq!(adkew["role"], "owner");

        let adconfig = payload("adconfig");
        assert_eq!(adconfig["role"], "user");
        assert_eq!(adconfig["scopes"], serde_json::json!(["group"]));

        let qban = payload("qban");
        assert_eq!(qban["scopes"], serde_json::json!(["private", "group"]));
    }
}
