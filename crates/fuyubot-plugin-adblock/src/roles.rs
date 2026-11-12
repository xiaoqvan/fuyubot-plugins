//! 宿主全局角色（`fuyubot.toml` 的 `[roles]`）的只读查询。
//!
//! FuyuBot 0.1.5 起「主人 / 管理员」是**宿主概念**，不再由每个插件各配一份
//! `owner_ids` / `admin_ids`：状态由宿主按 `[roles].owners` / `[roles].admins`
//! 算好，插件经 `fuyubot.host.commands` 的 `role` 方法读。
//!
//! - `user`：普通用户
//! - `admin`：全局管理员（`[roles].admins`）
//! - `owner`：主人（`[roles].owners`；user 账户在 owners 为空时默认自己是主人）
//!
//! 群里的 Telegram 管理员不是这里的 `admin`：那是**会话角色**，由
//! [`crate::api::Td::is_admin`] 查。

use fuyubot_abi::HostApi;
use serde_json::{Value, json};

/// 宿主预置的命令能力表（方法 `register` / `list` / `role`）。
pub const HOST_COMMANDS: &str = "fuyubot.host.commands";

/// 宿主给某个用户算出的全局角色。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum GlobalRole {
    #[default]
    User,
    Admin,
    Owner,
}

impl GlobalRole {
    pub fn parse(raw: &str) -> Self {
        match raw.trim().to_ascii_lowercase().as_str() {
            "owner" => Self::Owner,
            "admin" => Self::Admin,
            _ => Self::User,
        }
    }

    /// 主人 **或** 管理员（TS `permission:"admin"` 那一档）。
    pub fn is_admin(self) -> bool {
        matches!(self, Self::Admin | Self::Owner)
    }

    /// 仅主人。
    pub fn is_owner(self) -> bool {
        self == Self::Owner
    }
}

/// 问宿主「这个人的全局角色」。宿主没接 / 老宿主时一律按普通用户处理。
pub fn global_role(host: &HostApi, user_id: i64) -> GlobalRole {
    let input = json!({ "user_id": user_id }).to_string();
    let Ok(raw) = host.call_service(HOST_COMMANDS, "role", &input) else {
        return GlobalRole::User;
    };
    let value: Value = serde_json::from_str(&raw).unwrap_or(Value::Null);
    GlobalRole::parse(value.get("global").and_then(Value::as_str).unwrap_or(""))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roles_parse_and_compare() {
        assert_eq!(GlobalRole::parse("owner"), GlobalRole::Owner);
        assert_eq!(GlobalRole::parse(" ADMIN "), GlobalRole::Admin);
        assert_eq!(GlobalRole::parse("user"), GlobalRole::User);
        assert_eq!(GlobalRole::parse("nope"), GlobalRole::User);

        assert!(GlobalRole::Owner.is_admin());
        assert!(GlobalRole::Admin.is_admin());
        assert!(!GlobalRole::User.is_admin());

        assert!(GlobalRole::Owner.is_owner());
        assert!(!GlobalRole::Admin.is_owner());
        assert!(!GlobalRole::User.is_owner());
    }
}
