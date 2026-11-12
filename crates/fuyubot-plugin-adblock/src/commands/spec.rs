//! 命令注册表：**一张表**同时是「有哪些命令」「谁能用」「`/help` 怎么描述」的唯一真相。
//!
//! 对应 TS 版 `index.ts` 的 `cmdHandlers` 17 条（名字 / description / scope / permission）。
//!
//! ## 为什么需要这张表
//!
//! TS 的插件框架替插件做 scope 与 permission 校验（`PluginValidator.ts`）。
//! FuyuBot 0.1.5 起宿主命令表也能按 `role` / `scopes` 在**分发前**拦，但
//! 「本群管理员」这类判断要查群成员，仍由插件在自己的入口里做。两层叠加，
//! 所以表里的 scope / permission 仍是唯一真相——散在 17 个 handler 里必然漏
//! （本插件此前就漏了 private scope 与 `admin` 档）。
//!
//! ## 三条约定
//!
//! 1. **表里没有的名字不是命令**。检测流水线照常扫这条消息——TS 里命令分发与
//!    `updateNewMessage` 是两条独立路径，插件不能因为「以 `/` 开头」就吞掉消息
//!    （那是可被利用的绕过口子：`/接单 代收款`）。
//! 2. **拒绝是静默的**。TS 的 `validateCommandAccess` 不通过就 `continue`；
//!    群里可能还装着别的 bot，回一句「你没权限」只会暴露命令存在。
//! 3. **scope 先于 permission**：私聊命令在群里连「仅限主人」都不该说。
//!
//! `/help` 由宿主内置 help 插件统一渲染，本插件在 `start()` 里把 [`COMMANDS`]
//! 登记进宿主命令表 `fuyubot.host.commands`（见 [`super::HOST_COMMANDS`]）。

use std::future::Future;
use std::pin::Pin;

use crate::simplify::Command;
use serde_json::Value;

use crate::api::Sender;
use crate::runtime::Runtime;

/// 命令处理函数返回的 future。表里要存**同一形状**的函数指针，不能存 `async fn`
/// 的 opaque 返回类型，所以统一装箱。
pub type HandlerFuture<'a> = Pin<Box<dyn Future<Output = ()> + Send + 'a>>;

/// 命令处理函数：`async fn` 实现在适配层里 `Box::pin` 成这个形状。
pub type Handler = for<'a> fn(&'a Runtime, &'a Command, Sender, &'a Value) -> HandlerFuture<'a>;

/// 命令允许出现的聊天类型（对应 TS `cmdHandlers[*].scope`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    All,
    Private,
    Group,
}

impl Scope {
    /// 命令表 `scopes` 字段。`All` 就是默认的「私聊 + 群」，不含频道。
    pub fn scopes(self) -> &'static [&'static str] {
        match self {
            Self::All => &["private", "group"],
            Self::Private => &["private"],
            Self::Group => &["group"],
        }
    }
}

/// 谁能执行（对应 TS `cmdHandlers[*].permission`）。
///
/// TS 的 `all` / `admin` / `owner` 三档：
/// - `all`：任何人（群命令通常还会在 handler 里再查一次群管理员）；
/// - `admin`：宿主全局管理员 **或** 主人（`[roles].admins` / `[roles].owners`）；
/// - `owner`：仅主人（`[roles].owners`）。
///
/// 名单归宿主，插件不再自带 `owner_ids` / `admin_ids`。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Permission {
    All,
    Admin,
    Owner,
}

impl Permission {
    /// 命令表 `role` 字段：宿主在分发前按它做角色闸门，`/help` 也按它过滤。
    ///
    /// `Administrator` / `Owner` 是**全局**角色（`[roles]`），与群里 Telegram
    /// 管理员的 `chat_admin` 不是一回事；`All` 命令自己仍会在 handler 里查群管。
    pub fn role(self) -> &'static str {
        match self {
            Self::All => "user",
            Self::Admin => "admin",
            Self::Owner => "owner",
        }
    }
}

/// 一条命令的声明。
pub struct CommandSpec {
    /// 命令名，不带 `/`，小写。
    pub name: &'static str,
    /// `/help` 里的一行说明（沿用 TS `cmdHandlers` 的 description）。
    pub summary: &'static str,
    pub scope: Scope,
    pub permission: Permission,
    pub handler: Handler,
}

impl CommandSpec {
    /// 聊天类型是否允许。频道既不是 private 也不是 group，`Group` 命令在频道里同样不应答。
    pub async fn allows_scope(&self, rt: &Runtime, chat_id: i64) -> bool {
        match self.scope {
            Scope::All => true,
            Scope::Private => crate::flow::is_private(chat_id),
            Scope::Group => crate::flow::is_group(rt, chat_id).await,
        }
    }

    /// 发送者权限是否足够。频道/匿名身份没有 user_id，只可能通过 `All`。
    pub fn allows_permission(&self, rt: &Runtime, sender: Sender) -> bool {
        match self.permission {
            Permission::All => true,
            // 主人 / 管理员是**宿主全局角色**（`[roles]`），插件不再自己存名单。
            Permission::Owner => sender
                .user_id()
                .is_some_and(|id| crate::roles::global_role(&rt.host, id).is_owner()),
            Permission::Admin => sender
                .user_id()
                .is_some_and(|id| crate::roles::global_role(&rt.host, id).is_admin()),
        }
    }
}

/// 按名字查表（命令名已由 `command_of` 小写化，这里仍不区分大小写）。
pub fn find(name: &str) -> Option<&'static CommandSpec> {
    COMMANDS
        .iter()
        .find(|spec| spec.name.eq_ignore_ascii_case(name))
}

/// 全部命令。顺序与 TS `index.ts` 的 `cmdHandlers` 一致，`/help` 也按这个顺序。
pub const COMMANDS: &[CommandSpec] = &[
    CommandSpec {
        name: "adconfig",
        summary: "配置广告屏蔽功能(仅本群管理)",
        scope: Scope::Group,
        permission: Permission::All,
        handler: cmd_adconfig,
    },
    CommandSpec {
        name: "spam",
        summary: "AI检测垃圾信息(Bot需群管权限)",
        scope: Scope::Group,
        permission: Permission::All,
        handler: cmd_spam,
    },
    CommandSpec {
        name: "setlogchannel",
        summary: "设置日志频道(仅本群管理)",
        scope: Scope::Group,
        permission: Permission::All,
        handler: cmd_setlogchannel,
    },
    CommandSpec {
        name: "whitelist",
        summary: "管理群组白名单(仅本群管理)（默认添加）",
        scope: Scope::Group,
        permission: Permission::All,
        handler: cmd_whitelist,
    },
    CommandSpec {
        name: "rmwhitelist",
        summary: "从群组白名单移除用户(仅本群管理)",
        scope: Scope::Group,
        permission: Permission::All,
        handler: cmd_rmwhitelist,
    },
    CommandSpec {
        name: "qadid",
        summary: "查询广告ID",
        scope: Scope::Private,
        permission: Permission::Admin,
        handler: cmd_qadid,
    },
    CommandSpec {
        name: "delad",
        summary: "删除黑名单条目(使用 ID)",
        scope: Scope::Private,
        permission: Permission::Admin,
        handler: cmd_delad,
    },
    CommandSpec {
        name: "qban",
        summary: "按 TGID 查询封禁记录",
        scope: Scope::All,
        permission: Permission::All,
        handler: cmd_qban,
    },
    CommandSpec {
        name: "addfwdbl",
        summary: "向转发黑名单添加 chat_id",
        scope: Scope::Private,
        permission: Permission::Admin,
        handler: cmd_addfwdbl,
    },
    CommandSpec {
        name: "rmfwdbl",
        summary: "从转发黑名单移除 chat_id",
        scope: Scope::Private,
        permission: Permission::Admin,
        handler: cmd_rmfwdbl,
    },
    CommandSpec {
        name: "addkw",
        summary: "向 ads/bio 黑名单添加关键词",
        scope: Scope::Private,
        permission: Permission::Admin,
        handler: cmd_addkw,
    },
    CommandSpec {
        name: "editkw",
        summary: "按 _id 修改黑名单关键词",
        scope: Scope::Private,
        permission: Permission::Admin,
        handler: cmd_editkw,
    },
    CommandSpec {
        name: "adkew",
        summary: "管理AI审查触发关键词(仅Bot主人)",
        scope: Scope::Private,
        permission: Permission::Owner,
        handler: cmd_adkew,
    },
    CommandSpec {
        name: "admatch",
        summary: "查询文本命中的广告关键词(仅Bot主人)",
        scope: Scope::Private,
        permission: Permission::Owner,
        handler: cmd_admatch,
    },
    CommandSpec {
        name: "adaiadd",
        summary: "AI判断广告文本，关键词>3则入库并返回ID(仅Bot主人)",
        scope: Scope::Private,
        permission: Permission::Owner,
        handler: cmd_adaiadd,
    },
    CommandSpec {
        name: "adaiset",
        summary: "管理内置AI授权群(仅Bot主人)",
        scope: Scope::Private,
        permission: Permission::Owner,
        handler: cmd_adaiset,
    },
    CommandSpec {
        name: "start",
        summary: "Bot启动指令（配置AI接口）",
        scope: Scope::Private,
        permission: Permission::All,
        handler: cmd_start,
    },
];

// ── 统一签名的适配层：具体 handler 的参数各不相同，注册表只认一种形状 ──────────

fn cmd_adconfig<'a>(rt: &'a Runtime, c: &'a Command, s: Sender, m: &'a Value) -> HandlerFuture<'a> {
    Box::pin(super::config::handle(rt, c, s, m))
}

fn cmd_spam<'a>(rt: &'a Runtime, c: &'a Command, s: Sender, m: &'a Value) -> HandlerFuture<'a> {
    Box::pin(super::spam::handle(rt, c, s, m))
}

fn cmd_setlogchannel<'a>(
    rt: &'a Runtime,
    c: &'a Command,
    s: Sender,
    m: &'a Value,
) -> HandlerFuture<'a> {
    Box::pin(super::log_channel::handle(rt, c, s, m))
}

fn cmd_whitelist<'a>(
    rt: &'a Runtime,
    c: &'a Command,
    s: Sender,
    m: &'a Value,
) -> HandlerFuture<'a> {
    Box::pin(super::whitelist::handle(rt, c, s, m, "add"))
}

fn cmd_rmwhitelist<'a>(
    rt: &'a Runtime,
    c: &'a Command,
    s: Sender,
    m: &'a Value,
) -> HandlerFuture<'a> {
    Box::pin(super::whitelist::handle(rt, c, s, m, "rm"))
}

fn cmd_qadid<'a>(rt: &'a Runtime, c: &'a Command, s: Sender, _m: &'a Value) -> HandlerFuture<'a> {
    Box::pin(super::lookup::query_ad_id(rt, c, s))
}

fn cmd_delad<'a>(rt: &'a Runtime, c: &'a Command, s: Sender, _m: &'a Value) -> HandlerFuture<'a> {
    Box::pin(super::lookup::delete_ad(rt, c, s))
}

fn cmd_qban<'a>(rt: &'a Runtime, c: &'a Command, s: Sender, _m: &'a Value) -> HandlerFuture<'a> {
    Box::pin(super::lookup::query_ban(rt, c, s))
}

fn cmd_addfwdbl<'a>(
    rt: &'a Runtime,
    c: &'a Command,
    s: Sender,
    _m: &'a Value,
) -> HandlerFuture<'a> {
    Box::pin(super::forward::add(rt, c, s))
}

fn cmd_rmfwdbl<'a>(rt: &'a Runtime, c: &'a Command, s: Sender, _m: &'a Value) -> HandlerFuture<'a> {
    Box::pin(super::forward::remove(rt, c, s))
}

fn cmd_addkw<'a>(rt: &'a Runtime, c: &'a Command, s: Sender, _m: &'a Value) -> HandlerFuture<'a> {
    Box::pin(super::keywords::add(rt, c, s))
}

fn cmd_editkw<'a>(rt: &'a Runtime, c: &'a Command, s: Sender, _m: &'a Value) -> HandlerFuture<'a> {
    Box::pin(super::keywords::edit(rt, c, s))
}

fn cmd_adkew<'a>(rt: &'a Runtime, c: &'a Command, s: Sender, _m: &'a Value) -> HandlerFuture<'a> {
    Box::pin(super::ai_keywords::handle(rt, c, s))
}

fn cmd_admatch<'a>(rt: &'a Runtime, c: &'a Command, s: Sender, _m: &'a Value) -> HandlerFuture<'a> {
    Box::pin(super::owner::ad_match(rt, c, s))
}

fn cmd_adaiadd<'a>(rt: &'a Runtime, c: &'a Command, s: Sender, _m: &'a Value) -> HandlerFuture<'a> {
    Box::pin(super::owner::ad_ai_add(rt, c, s))
}

fn cmd_adaiset<'a>(rt: &'a Runtime, c: &'a Command, s: Sender, _m: &'a Value) -> HandlerFuture<'a> {
    Box::pin(super::ai_manager::handle(rt, c, s))
}

fn cmd_start<'a>(rt: &'a Runtime, c: &'a Command, s: Sender, m: &'a Value) -> HandlerFuture<'a> {
    Box::pin(super::ai_config::handle_start(rt, c, s, m))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 17 条，与 TS `cmdHandlers` 一一对应——少一条就是少一个功能。
    #[test]
    fn registry_covers_every_ts_command() {
        let names: Vec<&str> = COMMANDS.iter().map(|spec| spec.name).collect();
        assert_eq!(names.len(), 17, "TS 版是 17 条命令");
        for expected in [
            "adconfig",
            "spam",
            "setlogchannel",
            "whitelist",
            "rmwhitelist",
            "qadid",
            "delad",
            "qban",
            "addfwdbl",
            "rmfwdbl",
            "addkw",
            "editkw",
            "adkew",
            "admatch",
            "adaiadd",
            "adaiset",
            "start",
        ] {
            assert!(names.contains(&expected), "缺命令 /{expected}");
        }
    }

    /// 表里没有的名字不是命令——检测流水线要继续扫它。
    #[test]
    fn unknown_names_are_not_commands() {
        assert!(find("接单").is_none());
        assert!(find("").is_none());
        // 大小写不敏感（`command_of` 已小写化，这里再兜一层）。
        assert!(find("AdConfig").is_some());
    }

    /// scope/permission 与 TS 的 `cmdHandlers` 一致：11 条私聊、6 条 admin、4 条 owner。
    #[test]
    fn scopes_and_permissions_match_the_ts_table() {
        let private = COMMANDS
            .iter()
            .filter(|spec| spec.scope == Scope::Private)
            .count();
        let admin = COMMANDS
            .iter()
            .filter(|spec| spec.permission == Permission::Admin)
            .count();
        let owner = COMMANDS
            .iter()
            .filter(|spec| spec.permission == Permission::Owner)
            .count();
        assert_eq!(private, 11);
        assert_eq!(admin, 6);
        assert_eq!(owner, 4);
    }
}
