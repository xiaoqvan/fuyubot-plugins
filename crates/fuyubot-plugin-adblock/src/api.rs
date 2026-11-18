//! TDLib 调用层：请求 → 响应 → 类型化结果。
//!
//! 插件不直接碰 `tdjson`，所有请求经 [`ResponseRouter`]（宿主 `td_invoke_simplified`
//! 的 async 封装）发出，响应按 `request_id` 路由回对应的 future。
//!
//! ## 线程模型
//!
//! 每个方法都是 `async fn`：`call` 里 `tokio::time::timeout` 包住响应 future，
//! **不会占住线程干等**。所有调用都发生在 tokio 工作任务里（事件回调只排队），
//! 宿主的接收循环线程永远不参与等待——这是「回调里不能等响应」约束的 async 形态。
//!
//! ## 方法名以 `td_api.tl` 为准
//!
//! 全部按 TDLib 1.8.69 的真签名写（`banChatMember` / `setChatMemberStatus` /
//! `getChatMember` …）。Bot API 那套（`kickChatMember`、`restrictChatMember`）不存在。
//!
//! ## 读对象先问宿主缓存
//!
//! `getChat` / `getChatMember` / `getSupergroup` 走 **`fuyubot.host.cache`**：
//! 宿主的更新管线（`updateNewChat` / `updateChat*` / `updateChatMember` …）会实时刷新，
//! 所以「有没有缓存」和「是不是最新」是同一件事——插件不再自己攒一份 TTL 缓存，
//! 也就不会出现「刚提权还要等 60 秒」的窗口。未命中且在线才直连 TDLib，成功后回填。

use std::sync::OnceLock;
use std::time::Duration;

use fuyubot_abi::HostApi;
use fuyubot_abi::ResponseRouter;
use serde_json::{Value, json};

use crate::simplify::{
    EditTextParams, IDENTITY_SERVICE, ReplyButton, SendMessageParams, Simplify, Text, Topic,
    send_params,
};

/// 普通请求的等待上限。发消息另算（`send_message` 自带 30 秒预算）。
pub const CALL_TIMEOUT_MS: u64 = 15_000;

/// 宿主预置的**命令回复**服务（方法 `reply`）：由宿主决定发新消息 / 编辑原命令 /
/// 发 ephemeral。名字写错或宿主太旧时调用会失败，[`Td::send`] 会退回普通发送。
pub const COMMAND_SERVICE: &str = "fuyubot.host.command";

/// 宿主内置的 **TDLib 对象缓存**服务（`get` / `put`）。
///
/// 这张缓存由宿主的更新管线维护（`updateNewChat` / `updateChat*` / `updateChatMember`
/// 等实时刷新），所以读它既是「已缓存」也是「跟得上更新」——比插件自己攒一份 TTL
/// 缓存更准，也比每条消息直连 TDLib 便宜。
pub const CACHE_SERVICE: &str = "fuyubot.host.cache";

/// 成员信息的兜底 TTL：权限会变，不能只靠 `updateChatMember`（与 `fuyubot-simplify` 一致）。
///
/// 注意这是**上限**：`updateChatMember` 到达时宿主会覆盖条目并重置计时。
const CHAT_MEMBER_TTL_MS: u64 = 30_000;

// ── 发送者 ────────────────────────────────────────────────────────

/// `messageSenderUser` / `messageSenderChat` 的 Rust 形态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Sender {
    User(i64),
    Chat(i64),
}

impl Sender {
    pub fn from_json(value: &Value) -> Option<Self> {
        match value.get("@type").and_then(Value::as_str) {
            Some("messageSenderUser") => {
                value.get("user_id").and_then(Value::as_i64).map(Self::User)
            }
            Some("messageSenderChat") => {
                value.get("chat_id").and_then(Value::as_i64).map(Self::Chat)
            }
            _ => None,
        }
    }

    pub fn to_json(self) -> Value {
        match self {
            Self::User(user_id) => json!({"@type": "messageSenderUser", "user_id": user_id}),
            Self::Chat(chat_id) => json!({"@type": "messageSenderChat", "chat_id": chat_id}),
        }
    }

    pub fn id(self) -> i64 {
        match self {
            Self::User(id) | Self::Chat(id) => id,
        }
    }

    /// 与 TS 版 `member_id._` 同名的字符串，审计记录里原样保留。
    pub fn type_name(self) -> &'static str {
        match self {
            Self::User(_) => "messageSenderUser",
            Self::Chat(_) => "messageSenderChat",
        }
    }

    pub fn user_id(self) -> Option<i64> {
        match self {
            Self::User(id) => Some(id),
            Self::Chat(_) => None,
        }
    }
}

/// 一条消息的发送者；解析不出来（比如 `messageSenderHidden`）就是 `None`。
pub fn message_sender(message: &Value) -> Option<Sender> {
    message.get("sender_id").and_then(Sender::from_json)
}

// ── 错误 ──────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct ApiError {
    pub method: String,
    pub code: i64,
    pub message: String,
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} 失败（{}）：{}", self.method, self.code, self.message)
    }
}

impl std::error::Error for ApiError {}

impl ApiError {
    fn timeout(method: &str) -> Self {
        Self {
            method: method.to_owned(),
            code: 0,
            message: "等待 TDLib 响应超时".to_owned(),
        }
    }

    fn from_host(method: &str, error: impl std::fmt::Display) -> Self {
        Self {
            method: method.to_owned(),
            code: 0,
            message: error.to_string(),
        }
    }

    fn from_response(method: &str, response: &Value) -> Self {
        Self {
            method: method.to_owned(),
            code: response.get("code").and_then(Value::as_i64).unwrap_or(0),
            message: response
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("未知错误")
                .to_owned(),
        }
    }

    /// 权限类错误（没进群 / 不是管理员 / 消息太旧）不值得刷 Error 日志。
    #[allow(dead_code)]
    pub fn is_expected(&self) -> bool {
        matches!(self.code, 400 | 403)
    }
}

// ── 门面 ──────────────────────────────────────────────────────────

/// TDLib 门面。`HostApi` 是 `Copy` 的；`ResponseRouter` 只能 `Clone`（内部共享）。
#[derive(Debug, Clone)]
pub struct Td {
    host: HostApi,
    router: ResponseRouter,
    /// 自己的 user id：身份服务拿到一次就记住（id 不会变）。
    me_id: OnceLock<i64>,
}

impl Td {
    /// `router` 必须在插件 `start` 里建（宿主作用域栈顶是本插件），
    /// 由 [`crate::runtime::Runtime::new`] 创建后传进来。
    pub fn new(host: HostApi, router: ResponseRouter) -> Self {
        Self {
            host,
            router,
            me_id: OnceLock::new(),
        }
    }

    /// 发一条请求并等它的响应；`@type == "error"` 变成 [`ApiError`]。
    ///
    /// 响应 future 由 [`ResponseRouter`] 按 `request_id` 路由；超时用
    /// [`tokio::time::timeout`] 包一层（drop future 即取消等待）。
    pub async fn call(&self, method: &str, params: Value) -> Result<Value, ApiError> {
        let (_id, response) = self
            .router
            .invoke_simplified(method, &params.to_string())
            .map_err(|error| ApiError::from_host(method, error))?;
        match tokio::time::timeout(Duration::from_millis(CALL_TIMEOUT_MS), response).await {
            Err(_elapsed) => Err(ApiError::timeout(method)),
            Ok(Err(error)) => Err(ApiError::from_host(method, error)),
            Ok(Ok(response)) => {
                if response.get("@type").and_then(Value::as_str) == Some("error") {
                    Err(ApiError::from_response(method, &response))
                } else {
                    Ok(response)
                }
            }
        }
    }

    pub async fn call_ok(&self, method: &str, params: Value) -> Result<(), ApiError> {
        self.call(method, params).await.map(|_| ())
    }

    // ── 查询（宿主缓存优先） ─────────────────────────────────────

    /// `getChat`：**宿主缓存优先**（`updateNewChat` / `updateChat*` 实时刷新）。
    ///
    /// 未命中且 TDLib 在线才真的发请求，成功后回填缓存；离线时直接 `None`，
    /// 不再为一个已知问不到的 `getChat` 等满 15 秒。
    pub async fn chat(&self, chat_id: i64) -> Option<Value> {
        self.cached(
            "chat",
            chat_id,
            None,
            0,
            "getChat",
            json!({ "chat_id": chat_id }),
        )
        .await
    }

    /// `getUser`：**宿主缓存优先**（`updateUser` / `updateUserStatus` 实时刷新）。
    pub async fn user(&self, user_id: i64) -> Option<Value> {
        self.cached(
            "user",
            user_id,
            None,
            0,
            "getUser",
            json!({ "user_id": user_id }),
        )
        .await
    }

    pub async fn user_full(&self, user_id: i64) -> Result<Value, ApiError> {
        self.call("getUserFullInfo", json!({ "user_id": user_id }))
            .await
    }

    /// `getChatMember`：**宿主缓存优先**（30 秒兜底 TTL，`updateChatMember` 实时刷新）。
    ///
    /// 取不到（用户不在群里之类）就当 `None`——调用方九成只想「能用就用」。
    /// 频道身份没有 user_id，缓存键对不上，只能直连。
    pub async fn chat_member(&self, chat_id: i64, sender: Sender) -> Option<Value> {
        let params = json!({ "chat_id": chat_id, "member_id": sender.to_json() });
        match sender {
            Sender::User(user_id) => {
                self.cached(
                    "chat_member",
                    chat_id,
                    Some(user_id),
                    CHAT_MEMBER_TTL_MS,
                    "getChatMember",
                    params,
                )
                .await
            }
            Sender::Chat(_) => self.call("getChatMember", params).await.ok(),
        }
    }

    /// `getSupergroup`：**宿主缓存优先**（`updateSupergroup` 实时刷新）。
    pub async fn supergroup(&self, supergroup_id: i64) -> Option<Value> {
        self.cached(
            "supergroup",
            supergroup_id,
            None,
            0,
            "getSupergroup",
            json!({ "supergroup_id": supergroup_id }),
        )
        .await
    }

    pub async fn supergroup_full(&self, supergroup_id: i64) -> Result<Value, ApiError> {
        self.call(
            "getSupergroupFullInfo",
            json!({ "supergroup_id": supergroup_id }),
        )
        .await
    }

    pub async fn sticker_set(&self, set_id: i64) -> Option<Value> {
        self.call("getStickerSet", json!({ "set_id": set_id }))
            .await
            .ok()
    }

    pub async fn message(&self, chat_id: i64, message_id: i64) -> Option<Value> {
        self.call(
            "getMessage",
            json!({ "chat_id": chat_id, "message_id": message_id }),
        )
        .await
        .ok()
    }

    pub async fn search_public_chat(&self, username: &str) -> Result<Value, ApiError> {
        self.call("searchPublicChat", json!({ "username": username }))
            .await
    }

    pub async fn message_link(&self, chat_id: i64, message_id: i64) -> Option<String> {
        self.call(
            "getMessageLink",
            json!({
                "chat_id": chat_id,
                "message_id": message_id,
                "media_timestamp": 0,
                "checklist_task_id": 0,
                "poll_option_id": "",
                "for_album": false,
                "in_message_thread": false,
            }),
        )
        .await
        .ok()
        .and_then(|link| link.get("link").and_then(Value::as_str).map(str::to_owned))
    }

    /// 下载一个远程文件，返回本地路径（AI 多模态审核要用）。
    ///
    /// 两步：`getRemoteFile` 拿到 `file.id`，再 `downloadFile(synchronous = true)`。
    /// TDLib 的 `file.id` 才是下载入口（`remote.unique_id` 只是标识）。
    pub async fn download_remote(&self, remote_id: &str, file_type: &str) -> Option<String> {
        let file = self
            .call(
                "getRemoteFile",
                json!({ "remote_file_id": remote_id, "file_type": json!({"@type": file_type}) }),
            )
            .await
            .ok()?;
        let file_id = file.get("id").and_then(Value::as_i64)?;
        let downloaded = self
            .call(
                "downloadFile",
                json!({
                    "file_id": file_id,
                    "priority": 1,
                    "offset": 0,
                    "limit": 0,
                    "synchronous": true,
                }),
            )
            .await
            .ok()?;
        downloaded
            .get("local")
            .and_then(|local| local.get("path"))
            .and_then(Value::as_str)
            .filter(|path| !path.is_empty())
            .map(str::to_owned)
    }

    /// downloadFile：按 file.id 直接下载。
    ///
    /// 贴纸 / 自定义 emoji 的对象里已经有 file.id（sticker.sticker / thumbnail.file），
    /// 不用像照片那样先 getRemoteFile 换一次 remote id。
    pub async fn download_file(&self, file_id: i64) -> Option<String> {
        let downloaded = self
            .call(
                "downloadFile",
                json!({
                    "file_id": file_id,
                    "priority": 1,
                    "offset": 0,
                    "limit": 0,
                    "synchronous": true,
                }),
            )
            .await
            .ok()?;
        downloaded
            .get("local")
            .and_then(|local| local.get("path"))
            .and_then(Value::as_str)
            .filter(|path| !path.is_empty())
            .map(str::to_owned)
    }

    // ── 群 / 成员判断 ─────────────────────────────────────────────

    /// 是「群」吗（超级群的非频道形态，或基本群）。
    pub async fn chat_is_group(&self, chat_id: i64) -> bool {
        let Some(chat) = self.chat(chat_id).await else {
            return false;
        };
        match chat.pointer("/type/@type").and_then(Value::as_str) {
            Some("chatTypeBasicGroup") => true,
            Some("chatTypeSupergroup") => !chat
                .pointer("/type/is_channel")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            _ => false,
        }
    }

    pub async fn chat_title(&self, chat_id: i64) -> String {
        self.chat(chat_id)
            .await
            .and_then(|chat| chat.get("title").and_then(Value::as_str).map(str::to_owned))
            .unwrap_or_else(|| chat_id.to_string())
    }

    /// 群绑定的频道 id（`supergroupFullInfo.linked_chat_id`）。
    ///
    /// 没绑定频道（或不是超级群）时是 `None`；TDLib 用 0 表示「没绑定」。
    pub async fn linked_channel_id(&self, chat_id: i64) -> Option<i64> {
        let chat = self.chat(chat_id).await?;
        let supergroup_id = chat
            .pointer("/type/supergroup_id")
            .and_then(Value::as_i64)?;
        let full = self.supergroup_full(supergroup_id).await.ok()?;
        full.get("linked_chat_id")
            .and_then(Value::as_i64)
            .filter(|id| *id != 0)
    }

    /// 聊天成员的 `status.@type`（`chatMemberStatusMember` / `…Administrator` / …）。
    pub async fn member_status(&self, chat_id: i64, sender: Sender) -> Option<String> {
        self.chat_member(chat_id, sender).await.and_then(|member| {
            member
                .get("status")
                .and_then(|status| status.get("@type"))
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
    }

    #[allow(dead_code)]
    pub async fn is_member(&self, chat_id: i64, sender: Sender) -> bool {
        matches!(
            self.member_status(chat_id, sender).await.as_deref(),
            Some("chatMemberStatusMember")
                | Some("chatMemberStatusAdministrator")
                | Some("chatMemberStatusCreator")
                | Some("chatMemberStatusRestricted")
        )
    }

    /// 自己（Bot）在这个群里有没有这些管理员权限。
    ///
    /// 两次读取都走宿主预置的东西，插件不再自己缓存 60 秒：
    /// 1. `fuyubot.host.identity` 的 `me` —— 登录时宿主就填好了，**不碰 TDLib**；
    /// 2. `getChatMember(chat, 我)` —— 宿主缓存优先，`updateChatMember` 实时刷新，
    ///    30 秒只是「更新丢了」时的兜底。
    ///
    /// 提升/降权后宿主会收到 `updateChatMember` 并刷新缓存，所以这里拿到的基本是实时值。
    pub async fn am_i_admin(&self, chat_id: i64, rights: &[&str]) -> bool {
        let Some(user_id) = self.my_id().await else {
            return false;
        };
        let Some(member) = self.chat_member(chat_id, Sender::User(user_id)).await else {
            return false;
        };
        member
            .get("status")
            .is_some_and(|status| status_has_rights(status, rights))
    }

    /// 某个发送者是不是该群的管理员（含「群自身」与关联频道这两条特例）。
    pub async fn is_admin(&self, chat_id: i64, sender: Sender) -> bool {
        if sender == Sender::Chat(chat_id) {
            return true;
        }
        if let Some(chat) = self.chat(chat_id).await
            && let Some(supergroup_id) = chat.pointer("/type/supergroup_id").and_then(Value::as_i64)
            && let Ok(full) = self.supergroup_full(supergroup_id).await
            && let Some(linked) = full.get("linked_chat_id").and_then(Value::as_i64)
            && sender == Sender::Chat(linked)
        {
            return true;
        }
        let Some(status) = self
            .chat_member(chat_id, sender)
            .await
            .and_then(|member| member.get("status").cloned())
        else {
            return false;
        };
        status_has_rights(&status, &[])
    }

    // ── 处置 ──────────────────────────────────────────────────────

    /// 永久封禁（`chatMemberStatusBanned`，`banned_until_date = 0`）。
    pub async fn ban(&self, chat_id: i64, sender: Sender) -> Result<(), ApiError> {
        self.call_ok(
            "setChatMemberStatus",
            json!({
                "chat_id": chat_id,
                "member_id": sender.to_json(),
                "status": {
                    "@type": "chatMemberStatusBanned",
                    "banned_until_date": 0,
                },
            }),
        )
        .await
    }

    /// 永久禁言（`chatMemberStatusRestricted`，所有发言权限关掉）。
    ///
    /// 字段名照 `td_api.tl` 的 `chatPermissions`（1.8.69 的 `can_send_basic_messages`
    /// 而不是老的 `can_send_messages`）。
    pub async fn restrict(&self, chat_id: i64, sender: Sender) -> Result<(), ApiError> {
        self.set_restricted(chat_id, sender, false).await
    }

    /// 只封禁「回应」权限（`can_react_to_messages = false`），发言权限全部照常。
    ///
    /// 自动点赞机器人的默认处置：点表情的机器人不该被禁言，但也不该继续刷回应。
    pub async fn restrict_reactions(&self, chat_id: i64, sender: Sender) -> Result<(), ApiError> {
        self.set_restricted(chat_id, sender, true).await
    }

    /// `restrict` / `restrict_reactions` 的共同实现：`can_send` 决定发言权限开不开。
    async fn set_restricted(
        &self,
        chat_id: i64,
        sender: Sender,
        can_send: bool,
    ) -> Result<(), ApiError> {
        self.call_ok(
            "setChatMemberStatus",
            json!({
                "chat_id": chat_id,
                "member_id": sender.to_json(),
                "status": {
                    "@type": "chatMemberStatusRestricted",
                    "is_member": true,
                    "restricted_until_date": 0,
                    "permissions": chat_permissions(can_send),
                },
            }),
        )
        .await
    }

    /// 删除某个发送者在群里最近的**全部回应**。
    ///
    /// TDLib 1.8.69+ 的 `deleteAllRecentMessageReactionsFromSender`：只支持基础群 /
    /// 超级群，调用者需要「删除消息」管理员权限（`can_delete_messages`）。失败说明
    /// 权限不够或对象不是群——调用方按「尽力而为」处理，因为处置本身已经生效。
    pub async fn delete_reactions_from_sender(
        &self,
        chat_id: i64,
        sender: Sender,
    ) -> Result<(), ApiError> {
        self.call_ok(
            "deleteAllRecentMessageReactionsFromSender",
            json!({
                "chat_id": chat_id,
                "sender_id": sender.to_json(),
            }),
        )
        .await
    }

    /// 取消置顶（`unpinChatMessage`）：基础群 / 超级群需要「置顶消息」权限。
    pub async fn unpin(&self, chat_id: i64, message_id: i64) -> Result<(), ApiError> {
        self.call_ok(
            "unpinChatMessage",
            json!({ "chat_id": chat_id, "message_id": message_id }),
        )
        .await
    }

    /// 解除封禁 / 解除限制：设回普通成员。
    pub async fn unban(&self, chat_id: i64, sender: Sender) -> Result<(), ApiError> {
        self.call_ok(
            "setChatMemberStatus",
            json!({
                "chat_id": chat_id,
                "member_id": sender.to_json(),
                "status": {
                    "@type": "chatMemberStatusMember",
                    "member_until_date": 0,
                },
            }),
        )
        .await
    }

    /// 删消息（`revoke = true`：对所有人撤回）。
    pub async fn delete_messages(&self, chat_id: i64, message_ids: &[i64]) -> Result<(), ApiError> {
        if message_ids.is_empty() {
            return Ok(());
        }
        Simplify::new(self.host)
            .delete_message(chat_id, message_ids.iter().copied(), true, 15_000)
            .await
            .map_err(|error| ApiError {
                method: "deleteMessages".to_owned(),
                code: 0,
                message: error.to_string(),
            })
    }

    /// 给消息加一个 emoji 回应（AI 审查时的 👀）。
    ///
    /// Bot 账号必须用 `setMessageReactions`：`addMessageReaction` 只对 user 账号开放。
    pub async fn react(&self, chat_id: i64, message_id: i64, emoji: &str) -> Result<(), ApiError> {
        self.call_ok(
            "setMessageReactions",
            json!({
                "chat_id": chat_id,
                "message_id": message_id,
                "reaction_types": [{"@type": "reactionTypeEmoji", "emoji": emoji}],
                "is_big": false,
            }),
        )
        .await
    }

    // ── 消息收发 ──────────────────────────────────────────────────

    /// 走宿主的命令回复服务回一条消息。`None` = 没发出去（服务不存在 / 入参被拒 /
    /// TDLib 报错 / 返回的不是消息）。
    ///
    /// 宿主知道 `message_id` 是不是一条命令：是就按账号类型决定**发新消息**还是
    /// **编辑原命令消息**；群里的 ephemeral 命令还会改走 `sendEphemeralMessage`。
    /// 不是命令时与 `sendMessage + reply_to` 等价。
    ///
    /// `call_service` 是**同步阻塞**的（最长等到 `params.timeout_ms`），所以丢进
    /// `spawn_blocking`：不能用一次发送占住 tokio worker。
    async fn reply_command(
        &self,
        chat_id: i64,
        message_id: i64,
        params: &SendMessageParams,
    ) -> Option<Value> {
        let simplify = Simplify::new(self.host);
        let payload = send_params(chat_id, params, &|text| simplify.formatted(text));
        let input = json!({
            "chat_id": chat_id,
            "message_id": message_id,
            "timeout_ms": params.timeout_ms,
            "payload": payload,
        })
        .to_string();
        let host = self.host;
        // join 失败（任务被取消）与服务调用失败（宿主没有这个服务）都返回 None，
        // 调用方据此退回普通发送——回复宁可变成普通消息，也不能丢。
        let reply = tokio::task::spawn_blocking(move || {
            host.call_service(COMMAND_SERVICE, "reply", &input)
        })
        .await
        .ok()?
        .ok()?;
        let value: Value = serde_json::from_str(&reply).ok()?;
        if value.get("@type").and_then(Value::as_str) == Some("error") {
            let code = value.get("code").and_then(Value::as_i64).unwrap_or(0);
            let message = value.get("message").and_then(Value::as_str).unwrap_or("");
            crate::log::warn(
                &self.host,
                format!("命令回复失败（chat {chat_id} / message {message_id}）：{code} {message}"),
            );
            return None;
        }
        Some(value)
    }

    /// 发一条消息，等到「真的发出去了」再返回最终消息。
    ///
    /// `reply_to` 有值时先走**宿主的命令回复服务**（见 [`Self::reply_command`]）：
    /// 宿主要么发新消息、要么编辑原命令消息，插件不用自己判断账号类型与 ephemeral。
    /// 服务不可用（宿主太旧 / 名字不对）时退回 simplify 的普通发送。
    ///
    /// `None` 表示失败（网络 / 权限 / 超时），调用方一般只需要记日志。
    pub async fn send(&self, chat_id: i64, params: &SendMessageParams) -> Option<Value> {
        if let Some(message_id) = params.reply_to_message_id
            && let Some(message) = self.reply_command(chat_id, message_id, params).await
        {
            return Some(message);
        }
        Simplify::new(self.host)
            .send_message(chat_id, params)
            .await
            .ok()
    }

    /// 发纯文本（不带格式化）。
    pub async fn send_plain(&self, chat_id: i64, text: impl Into<String>) -> Option<Value> {
        self.send(chat_id, &SendMessageParams::text(text.into()))
            .await
    }

    /// 发格式化文本（`text::markdown` 的产物），可带回复 / 话题 / 键盘。
    #[allow(clippy::too_many_arguments)]
    pub async fn send_text(
        &self,
        chat_id: i64,
        text: Text,
        reply_to: Option<i64>,
        buttons: Vec<Vec<ReplyButton>>,
        topic: Option<Topic>,
        link_preview: Option<bool>,
    ) -> Option<Value> {
        self.send(
            chat_id,
            &SendMessageParams {
                text: Some(text),
                reply_to_message_id: reply_to,
                reply_markup: buttons,
                topic_id: topic,
                link_preview,
                ..SendMessageParams::default()
            },
        )
        .await
    }

    /// 改一条消息的文本（可同时换键盘）。消息被删 / 超过 48 小时会失败。
    pub async fn edit_text(
        &self,
        chat_id: i64,
        message_id: i64,
        text: Text,
        buttons: Vec<Vec<ReplyButton>>,
        link_preview: Option<bool>,
    ) -> Option<Value> {
        Simplify::new(self.host)
            .edit_message_text(
                chat_id,
                message_id,
                &EditTextParams {
                    text: Some(text),
                    reply_markup: buttons,
                    link_preview,
                    ..EditTextParams::default()
                },
            )
            .await
            .ok()
    }

    /// 回一条 callback query（带 `cache_time`，用于「5 分钟内别重复点」）。
    pub async fn answer_callback(&self, query_id: i64, text: &str, alert: bool, cache_time: u32) {
        let _ = self
            .call_ok(
                "answerCallbackQuery",
                json!({
                    "callback_query_id": query_id,
                    "text": text,
                    "show_alert": alert,
                    "url": "",
                    "cache_time": cache_time,
                }),
            )
            .await;
    }

    // ── 展示 ──────────────────────────────────────────────────────

    /// 发送者 → Markdown 链接（对应 TS 版 `chatoruserMdown`）。
    ///
    /// `name = true` 时链接文字用**名字**（用户在群里被处置时更有用），
    /// 否则用 ID。
    ///
    /// 与 TS 版有两个刻意的不同（都是原版日志里「名字显示成 id」的根因）：
    /// - 频道/超级群在 `name = true` 时用 **title**，而不是把 `chat_id` 当链接文字；
    /// - 没有公开用户名时不再生成 `[title](id:…)` 这种 TDLib 不认的链接，
    ///   直接给纯 title。调用方本来就会在末尾追加 `(id)`，不需要伪链接。
    pub async fn sender_mention(&self, sender: Sender, name: bool) -> String {
        match sender {
            Sender::User(user_id) => {
                if !name {
                    return format!("[{user_id}](tg://user?id={user_id})");
                }
                let (first, last) = self
                    .user(user_id)
                    .await
                    .map(|user| {
                        (
                            user.get("first_name")
                                .and_then(Value::as_str)
                                .unwrap_or("")
                                .to_owned(),
                            user.get("last_name")
                                .and_then(Value::as_str)
                                .unwrap_or("")
                                .to_owned(),
                        )
                    })
                    .unwrap_or_default();
                // 用户没有名字时退回 ID：`[](tg://user?id=…)` 在日志里是一片空白。
                let label = display_name(&first, &last).unwrap_or_else(|| user_id.to_string());
                format!("[{label}](tg://user?id={user_id})")
            }
            Sender::Chat(chat_id) => {
                if !name {
                    return chat_id.to_string();
                }
                let Some(chat) = self.chat(chat_id).await else {
                    return chat_id.to_string();
                };
                let title = chat.get("title").and_then(Value::as_str).unwrap_or("");
                let label = if title.trim().is_empty() {
                    chat_id.to_string()
                } else {
                    title.trim().to_owned()
                };
                let mut username = None;
                if let Some(supergroup_id) = chat
                    .get("type")
                    .and_then(|kind| kind.get("supergroup_id"))
                    .and_then(Value::as_i64)
                    && let Some(supergroup) = self.supergroup(supergroup_id).await
                {
                    username = supergroup
                        .get("usernames")
                        .and_then(|names| names.get("active_usernames"))
                        .and_then(Value::as_array)
                        .and_then(|names| names.first())
                        .and_then(Value::as_str)
                        .map(str::to_owned);
                }
                match username {
                    Some(username) => format!("[{label}](tg://resolve?domain={username})"),
                    None => label,
                }
            }
        }
    }
}

// ── 宿主缓存 ──────────────────────────────────────────────────────

/// 宿主缓存查询的三种结果（与 `fuyubot-simplify` 的判定一致）。
enum CacheAnswer {
    /// 命中，直接用。
    Hit(Value),
    /// 未命中，但 TDLib 在线，可以去问。
    Miss,
    /// 未命中且离线：再问只会白等超时。
    Offline,
}

impl Td {
    /// `getChat` / `getChatMember` … 的统一入口：**缓存优先**。
    ///
    /// 未命中且在线才发一次 TDLib 请求，成功后回填宿主缓存——下一次（包括别的插件）
    /// 就都能命中。宿主太旧、没有缓存服务时按未命中处理，行为退回直连。
    async fn cached(
        &self,
        kind: &str,
        id: i64,
        user_id: Option<i64>,
        ttl_ms: u64,
        method: &str,
        params: Value,
    ) -> Option<Value> {
        match self.cache_lookup(kind, id, user_id, ttl_ms) {
            CacheAnswer::Hit(value) => return Some(value),
            CacheAnswer::Offline => return None,
            CacheAnswer::Miss => {}
        }
        let value = self.call(method, params).await.ok()?;
        self.cache_store(kind, id, user_id, &value);
        Some(value)
    }

    fn cache_lookup(&self, kind: &str, id: i64, user_id: Option<i64>, ttl_ms: u64) -> CacheAnswer {
        let input = json!({
            "kind": kind,
            "id": id,
            "user_id": user_id,
            "ttl_ms": ttl_ms,
        });
        let Ok(raw) = self
            .host
            .call_service(CACHE_SERVICE, "get", &input.to_string())
        else {
            // 老宿主没有缓存服务：按未命中处理，走原来的直连。
            return CacheAnswer::Miss;
        };
        let Ok(reply) = serde_json::from_str::<Value>(&raw) else {
            return CacheAnswer::Miss;
        };
        if let Some(value) = reply.get("value").filter(|value| !value.is_null()) {
            return CacheAnswer::Hit(value.clone());
        }
        if reply.get("online").and_then(Value::as_bool) == Some(false) {
            CacheAnswer::Offline
        } else {
            CacheAnswer::Miss
        }
    }

    fn cache_store(&self, kind: &str, id: i64, user_id: Option<i64>, value: &Value) {
        let input = json!({
            "kind": kind,
            "id": id,
            "user_id": user_id,
            "value": value,
        });
        // 写失败不影响调用方：它已经拿到 TDLib 的结果。
        let _ = self
            .host
            .call_service(CACHE_SERVICE, "put", &input.to_string());
    }

    /// 自己的 user id：宿主身份服务的 `me`（登录时填好，不碰 TDLib），
    /// 老宿主退回 `getMe`。id 不会变，拿到一次就记住。
    pub async fn my_id(&self) -> Option<i64> {
        if let Some(id) = self.me_id.get() {
            return Some(*id);
        }
        let id = match self.identity_me() {
            Some(id) => id,
            None => self
                .call("getMe", json!({}))
                .await
                .ok()?
                .get("id")
                .and_then(Value::as_i64)?,
        };
        let _ = self.me_id.set(id);
        Some(id)
    }

    fn identity_me(&self) -> Option<i64> {
        let raw = self.host.call_service(IDENTITY_SERVICE, "me", "{}").ok()?;
        let me: Value = serde_json::from_str(&raw).ok()?;
        me.get("id").and_then(Value::as_i64)
    }
}

/// 用户显示名：`first last`，两边都空时返回 `None`。
/// TS 版直接 `first_name + last_name`（没有空格，缺失时还会拼出 `undefinedundefined`）；
/// 这里统一成「有空格、缺一个就只显示另一个、全缺才回退 ID」。
fn display_name(first: &str, last: &str) -> Option<String> {
    let name = format!("{} {}", first.trim(), last.trim());
    let name = name.trim();
    (!name.is_empty()).then(|| name.to_owned())
}

/// `chatPermissions`：`can_send = false` 时关掉全部发言权限（禁言）；
/// `can_send = true` 时保留发言，只把 `can_react_to_messages` 关掉（只封禁回应）。
///
/// 字段名照 `td_api.tl` 的 `chatPermissions`（1.8.69 的 `can_send_basic_messages`
/// 而不是老的 `can_send_messages`）。
fn chat_permissions(can_send: bool) -> Value {
    json!({
        "@type": "chatPermissions",
        "can_send_basic_messages": can_send,
        "can_send_audios": can_send,
        "can_send_documents": can_send,
        "can_send_photos": can_send,
        "can_send_videos": can_send,
        "can_send_video_notes": can_send,
        "can_send_voice_notes": can_send,
        "can_send_polls": can_send,
        "can_send_other_messages": can_send,
        "can_add_link_previews": can_send,
        "can_react_to_messages": false,
        "can_edit_tag": can_send,
        "can_change_info": can_send,
        "can_invite_users": can_send,
        "can_pin_messages": can_send,
        "can_create_topics": can_send,
    })
}

/// 管理员状态是否覆盖这些权限（空列表 = 只要是管理员就行）。
///
/// 字段名照 `td_api.tl` 的 `chatAdministratorRights`：
/// `can_delete_messages` / `can_restrict_members` / `can_invite_users` …
pub fn status_has_rights(status: &Value, rights: &[&str]) -> bool {
    match status.get("@type").and_then(Value::as_str) {
        Some("chatMemberStatusCreator") => true,
        Some("chatMemberStatusAdministrator") => {
            if rights.is_empty() {
                return true;
            }
            let Some(granted) = status.get("rights") else {
                return false;
            };
            rights
                .iter()
                .all(|right| granted.get(right).and_then(Value::as_bool).unwrap_or(false))
        }
        _ => false,
    }
}

// ── 键盘 ──────────────────────────────────────────────────────────

/// 一行里放一个回调按钮（数据会被 `crate::simplify` 自动 base64 编码）。
pub fn callback_row(text: impl Into<String>, data: impl Into<String>) -> Vec<ReplyButton> {
    vec![ReplyButton::callback(text, data)]
}

/// 打开链接的按钮（`/adconfig` 里跳到 Bot 私聊的那颗）。
pub fn url_row(text: impl Into<String>, url: impl Into<String>) -> Vec<ReplyButton> {
    vec![ReplyButton::url(text, url)]
}

/// `unban` 按钮的回调数据（与 TS 版 `buildUnbanCallbackData` 同形）。
pub fn unban_callback(chat_id: i64, sender: Sender) -> String {
    match sender {
        Sender::User(user_id) => format!("unban?cid={chat_id}&u={user_id}"),
        Sender::Chat(chat_id_target) => format!("unban?cid={chat_id}&c={chat_id_target}"),
    }
}

/// `u=…` / `c=…` 这一段，供 ban / unmute / SpamX 的回调数据复用。
pub fn member_param(sender: Sender) -> String {
    match sender {
        Sender::User(user_id) => format!("u={user_id}"),
        Sender::Chat(chat_id) => format!("c={chat_id}"),
    }
}

#[cfg(test)]
#[path = "../tests/unit/api.rs"]
mod tests;
