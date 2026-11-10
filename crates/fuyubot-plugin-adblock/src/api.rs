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

use std::time::Duration;

use fuyubot_abi::HostApi;
use fuyubot_abi::ResponseRouter;
use serde_json::{Value, json};

use crate::simplify::{
    EditTextParams, ReplyButton, SendMessageParams, Simplify, Text, Topic, send_params,
};

/// 普通请求的等待上限。发消息另算（`send_message` 自带 30 秒预算）。
pub const CALL_TIMEOUT_MS: u64 = 15_000;

/// 宿主预置的**命令回复**服务（方法 `reply`）：由宿主决定发新消息 / 编辑原命令 /
/// 发 ephemeral。名字写错或宿主太旧时调用会失败，[`Td::send`] 会退回普通发送。
pub const COMMAND_SERVICE: &str = "fuyubot.host.command";

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
}

impl Td {
    /// `router` 必须在插件 `start` 里建（宿主作用域栈顶是本插件），
    /// 由 [`crate::runtime::Runtime::new`] 创建后传进来。
    pub fn new(host: HostApi, router: ResponseRouter) -> Self {
        Self { host, router }
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

    // ── 查询 ──────────────────────────────────────────────────────

    pub async fn chat(&self, chat_id: i64) -> Result<Value, ApiError> {
        self.call("getChat", json!({ "chat_id": chat_id })).await
    }

    pub async fn user(&self, user_id: i64) -> Result<Value, ApiError> {
        self.call("getUser", json!({ "user_id": user_id })).await
    }

    pub async fn user_full(&self, user_id: i64) -> Result<Value, ApiError> {
        self.call("getUserFullInfo", json!({ "user_id": user_id }))
            .await
    }

    /// 取不到（用户不在群里之类）就当 `None`——调用方九成只想「能用就用」。
    pub async fn chat_member(&self, chat_id: i64, sender: Sender) -> Option<Value> {
        self.call(
            "getChatMember",
            json!({ "chat_id": chat_id, "member_id": sender.to_json() }),
        )
        .await
        .ok()
    }

    pub async fn supergroup(&self, supergroup_id: i64) -> Result<Value, ApiError> {
        self.call("getSupergroup", json!({ "supergroup_id": supergroup_id }))
            .await
    }

    pub async fn supergroup_full(&self, supergroup_id: i64) -> Result<Value, ApiError> {
        self.call(
            "getSupergroupFullInfo",
            json!({ "supergroup_id": supergroup_id }),
        )
        .await
    }

    pub async fn basic_group(&self, basic_group_id: i64) -> Result<Value, ApiError> {
        self.call("getBasicGroup", json!({ "basic_group_id": basic_group_id }))
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

    // ── 群 / 成员判断 ─────────────────────────────────────────────

    /// 是「群」吗（超级群的非频道形态，或基本群）。
    pub async fn chat_is_group(&self, chat_id: i64) -> bool {
        let Ok(chat) = self.chat(chat_id).await else {
            return false;
        };
        match chat
            .get("type")
            .and_then(|kind| kind.get("@type"))
            .and_then(Value::as_str)
        {
            Some("chatTypeBasicGroup") => true,
            Some("chatTypeSupergroup") => chat
                .get("type")
                .and_then(|kind| kind.get("is_channel"))
                .and_then(Value::as_bool)
                .is_some_and(|is_channel| !is_channel),
            _ => false,
        }
    }

    pub async fn chat_title(&self, chat_id: i64) -> String {
        self.chat(chat_id)
            .await
            .ok()
            .and_then(|chat| chat.get("title").and_then(Value::as_str).map(str::to_owned))
            .unwrap_or_else(|| chat_id.to_string())
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
    pub async fn am_i_admin(&self, chat_id: i64, rights: &[&str]) -> bool {
        let Ok(chat) = self.chat(chat_id).await else {
            return false;
        };
        let status = match chat
            .get("type")
            .and_then(|kind| kind.get("@type"))
            .and_then(Value::as_str)
        {
            Some("chatTypeBasicGroup") => {
                let id = chat
                    .get("type")
                    .and_then(|kind| kind.get("basic_group_id"))
                    .and_then(Value::as_i64);
                match id {
                    Some(id) => self
                        .basic_group(id)
                        .await
                        .ok()
                        .and_then(|group| group.get("status").cloned()),
                    None => None,
                }
            }
            Some("chatTypeSupergroup") => {
                let id = chat
                    .get("type")
                    .and_then(|kind| kind.get("supergroup_id"))
                    .and_then(Value::as_i64);
                match id {
                    Some(id) => self
                        .supergroup(id)
                        .await
                        .ok()
                        .and_then(|group| group.get("status").cloned()),
                    None => None,
                }
            }
            _ => None,
        };
        status
            .as_ref()
            .is_some_and(|status| status_has_rights(status, rights))
    }

    /// 某个发送者是不是该群的管理员（含「群自身」与关联频道这两条特例）。
    pub async fn is_admin(&self, chat_id: i64, sender: Sender) -> bool {
        if sender == Sender::Chat(chat_id) {
            return true;
        }
        if let Ok(chat) = self.chat(chat_id).await
            && let Some(supergroup_id) = chat
                .get("type")
                .and_then(|kind| kind.get("supergroup_id"))
                .and_then(Value::as_i64)
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
        self.call_ok(
            "setChatMemberStatus",
            json!({
                "chat_id": chat_id,
                "member_id": sender.to_json(),
                "status": {
                    "@type": "chatMemberStatusRestricted",
                    "is_member": true,
                    "restricted_until_date": 0,
                    "permissions": {
                        "@type": "chatPermissions",
                        "can_send_basic_messages": false,
                        "can_send_audios": false,
                        "can_send_documents": false,
                        "can_send_photos": false,
                        "can_send_videos": false,
                        "can_send_video_notes": false,
                        "can_send_voice_notes": false,
                        "can_send_polls": false,
                        "can_send_other_messages": false,
                        "can_add_link_previews": false,
                        "can_react_to_messages": false,
                        "can_edit_tag": false,
                        "can_change_info": false,
                        "can_invite_users": false,
                        "can_pin_messages": false,
                        "can_create_topics": false,
                    },
                },
            }),
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
                let Ok(chat) = self.chat(chat_id).await else {
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
                    && let Ok(supergroup) = self.supergroup(supergroup_id).await
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

/// 用户显示名：`first last`，两边都空时返回 `None`。
///
/// TS 版直接 `first_name + last_name`（没有空格，缺失时还会拼出 `undefinedundefined`）；
/// 这里统一成「有空格、缺一个就只显示另一个、全缺才回退 ID」。
fn display_name(first: &str, last: &str) -> Option<String> {
    let name = format!("{} {}", first.trim(), last.trim());
    let name = name.trim();
    (!name.is_empty()).then(|| name.to_owned())
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
mod tests {
    use super::*;

    #[test]
    fn sender_round_trip() {
        let user = Sender::User(5);
        assert_eq!(Sender::from_json(&user.to_json()), Some(user));
        let chat = Sender::Chat(-100);
        assert_eq!(Sender::from_json(&chat.to_json()), Some(chat));
        assert_eq!(
            Sender::from_json(&json!({"@type": "messageSenderHidden"})),
            None
        );
    }

    #[test]
    fn rights_are_checked() {
        let admin = json!({
            "@type": "chatMemberStatusAdministrator",
            "rights": {"can_delete_messages": true, "can_restrict_members": false}
        });
        assert!(status_has_rights(&admin, &["can_delete_messages"]));
        assert!(!status_has_rights(&admin, &["can_restrict_members"]));
        assert!(status_has_rights(&admin, &[]));
        let creator = json!({"@type": "chatMemberStatusCreator"});
        assert!(status_has_rights(&creator, &["can_restrict_members"]));
        let member = json!({"@type": "chatMemberStatusMember"});
        assert!(!status_has_rights(&member, &[]));
    }

    #[test]
    fn display_names_are_trimmed_and_fall_back() {
        assert_eq!(display_name("Ada", "Lovelace"), Some("Ada Lovelace".into()));
        assert_eq!(display_name("Ada", ""), Some("Ada".into()));
        assert_eq!(display_name("", "Lovelace"), Some("Lovelace".into()));
        assert_eq!(display_name("  ", "\t"), None);
    }

    #[test]
    fn callback_data_shapes() {
        assert_eq!(unban_callback(-100, Sender::User(5)), "unban?cid=-100&u=5");
        assert_eq!(
            unban_callback(-100, Sender::Chat(-200)),
            "unban?cid=-100&c=-200"
        );
        assert_eq!(member_param(Sender::User(5)), "u=5");
        assert_eq!(member_param(Sender::Chat(-200)), "c=-200");
    }
}
