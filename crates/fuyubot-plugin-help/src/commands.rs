//! `/help` 的整条链路：命令解析 → 取宿主命令表 → 权限过滤 → 分组 →
//! 出图 / 出文字 → 回复命令。
//!
//! 命令表归宿主（`fuyubot.host.commands`），本插件只读；`role` / `scopes` /
//! `hidden` 决定谁能看见哪条命令。命令按**登记它的插件**分组（`plugin` 由宿主判定，
//! 插件伪造不了）：图片里一个插件一张卡片，组标题优先用宿主报告的展示名；
//! 没有归属的收在最后的「其它」里。
//! `/help t` / `/help text` 走同一份分组数据的**文字版**，不渲染。
//!
//! 回复走**宿主的命令回复服务**（`fuyubot.host.command`，方法 `reply`）：普通的发
//! 新消息 + 回原命令；user 账户自己触发命令时编辑原命令消息；群里的 ephemeral 命令
//! 改走 `sendEphemeralMessage`（只对执行者可见）。插件不用自己判断账号类型，
//! 也不用拼 `sendMessage`。
//!
//! 这里刻意不依赖 `fuyubot-simplify`（它是 `publish = false`）：命令解析与本地图片的
//! 参数构造都只保留本插件用到的这一小块。
//!
//! ## 线程模型
//!
//! 宿主的 `on` 回调跑在接收循环线程上，那里只做「解析命令 + 起一个线程」。
//! 真正慢的两件事——同步渲染（html2img 的服务方法）与等发送结果（宿主的回复服务是
//! 同步阻塞的）——都在 `fuyubot-help` 工作线程里做。渲染用一把锁串行化，避免多人
//! 同时 `/help` 时把 CPU 打满；发送不占这把锁。

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::{CStr, c_char, c_void};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use fuyubot_abi::{HostApi, SubscriptionHandle};
use serde_json::{Value, json};

use crate::config::{self, Config};
use crate::log;

/// 宿主身份服务的名字（登录后由 `getMe` 填好）。
pub const IDENTITY_SERVICE: &str = "fuyubot.host.identity";

/// 宿主预置的**命令回复**服务（方法 `reply`）。
///
/// 跨仓契约：与 `fuyubot.host.command` 同名同形，宿主按 `chat_id` / `message_id`
/// 找到命令上下文，决定发新消息、编辑原命令还是发 ephemeral。
pub const COMMAND_SERVICE: &str = "fuyubot.host.command";

/// 宿主预置的**命令能力表**（方法 `register` / `list` / `role`）。
///
/// 从 FuyuBot 0.1.5 起命令表归宿主：插件在 `start()` 里登记，宿主按 owner 管
/// 生命周期，`/help` 只读。旧的 `fuyubot.help.commands` 只是宿主提供的兼容别名，
/// 本插件不再自己维护注册表。
pub const HOST_COMMANDS: &str = "fuyubot.host.commands";

/// 宿主的 TDLib 对象缓存（方法 `get`）。会话类型 / 成员身份优先读它，
/// 未命中再问一次 TDLib——与内置 `/help` 的两级判断一致。
const HOST_CACHE: &str = "fuyubot.host.cache";

/// 权限判断时等 TDLib 的上限。跑在 `/help` 工作线程上，阻塞不影响接收循环。
const LOOKUP_TIMEOUT_MS: u64 = 1_500;

/// 回退文字版的长度上限：Telegram 单条消息 4096 个 **UTF-16 码元**，留一点余量。
const FALLBACK_TEXT_LIMIT: usize = 3_800;

/// 同时在跑的 `/help` 上限。渲染很吃 CPU，超出的请求直接丢弃（只记日志），
/// 避免一次刷屏把工作线程数拉爆。
pub const MAX_PENDING: usize = 8;

// ── 命令解析 ──────────────────────────────────────────────────────

/// 一条 `updateNewMessage` 里的命令：`/help`、`/help@SomeBot 参数`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Command {
    pub chat_id: i64,
    pub message_id: i64,
    /// 命令名：去掉 `/` 与 `@BotName` 后缀，已转小写。
    pub name: String,
    /// `/cmd` 后面那一段（去首尾空白，可能为空）。
    pub args: String,
    /// `/help@SomeBot` 里点名的那个 bot；裸命令是 `None`。
    pub addressed_to: Option<String>,
    /// 发送者 user id（频道帖子的发送者是频道身份，这里就是 `None`）。
    pub sender_user_id: Option<i64>,
    /// 整条消息文本。
    pub text: String,
}

impl Command {
    /// 是这条命令吗（不分大小写）。
    pub fn is(&self, name: &str) -> bool {
        self.name.eq_ignore_ascii_case(name)
    }

    /// 该不该由「用户名是这些」的账号响应：裸命令谁都可以，点名命令只有被点到的那个。
    ///
    /// `usernames` 为空表示**还不知道自己是谁**（没登录 / 没接 TDLib）——这时一律算
    /// 「该响应」：宁可多回一句，也别在身份没就绪时把命令吞掉。
    pub fn is_for(&self, usernames: &[String]) -> bool {
        match &self.addressed_to {
            None => true,
            Some(_) if usernames.is_empty() => true,
            Some(target) => usernames
                .iter()
                .any(|name| name.eq_ignore_ascii_case(target)),
        }
    }
}

/// 从 `updateNewMessage` 的负载里认出一条命令；不是命令就 `None`。
///
/// 字段路径照真 `td_api.tl`：`updateNewMessage.message` →
/// `content:messageText` → `text:formattedText` → `text:string`。
pub fn command_of(update: &Value) -> Option<Command> {
    let message = update.get("message")?;
    if message
        .get("is_outgoing")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return None;
    }
    let content = message.get("content")?;
    if content.get("@type").and_then(Value::as_str) != Some("messageText") {
        return None;
    }
    let text = content.get("text")?.get("text")?.as_str()?;
    let trimmed = text.trim_start();
    let token = trimmed.split_whitespace().next()?;
    let rest = token.strip_prefix('/')?;
    let (name, addressed_to) = match rest.split_once('@') {
        Some((name, target)) => (name, Some(target)),
        None => (rest, None),
    };
    if name.is_empty() {
        return None;
    }
    Some(Command {
        chat_id: message.get("chat_id")?.as_i64()?,
        message_id: message.get("id")?.as_i64()?,
        name: name.to_ascii_lowercase(),
        args: trimmed[token.len()..].trim().to_owned(),
        addressed_to: addressed_to
            .filter(|target| !target.is_empty())
            .map(str::to_owned),
        sender_user_id: message
            .pointer("/sender_id/user_id")
            .and_then(Value::as_i64),
        text: text.to_owned(),
    })
}

/// 问宿主「我是谁」：返回自身全部活跃用户名；拿不到就返回空表。
///
/// 走的是身份 **service**，不碰 TDLib，所以在事件回调里调不会卡住接收循环。
pub fn my_usernames(host: &HostApi) -> Vec<String> {
    let Ok(reply) = host.call_service(IDENTITY_SERVICE, "usernames", "{}") else {
        return Vec::new();
    };
    serde_json::from_str(&reply).unwrap_or_default()
}

// ── 命令订阅 ──────────────────────────────────────────────────────

struct CommandHandler {
    host: HostApi,
    handler: Box<dyn Fn(&Command) + Send + Sync>,
}

impl CommandHandler {
    /// 这条命令该不该交给本插件：点名别人的不交（裸命令、点名自己的都交）。
    fn accepts(&self, command: &Command) -> bool {
        if command.addressed_to.is_none() {
            return true;
        }
        // 每次现问「我是谁」：用户名会变，缓存住就会一直按旧名字判断。
        command.is_for(&my_usernames(&self.host))
    }
}

/// 一个活着的「命令订阅」：**drop 就是退订**。
pub struct EventSubscription {
    host: HostApi,
    handle: SubscriptionHandle,
    /// 只负责**持有**回调；退订之前它的地址必须稳定。
    _handler: Box<CommandHandler>,
}

impl Drop for EventSubscription {
    fn drop(&mut self) {
        if self.handle.is_valid() {
            self.host.off(self.handle);
        }
    }
}

/// 订阅 `updateNewMessage`，把**该由本插件响应的**命令交给 `handler`。
///
/// 不打扰它的：非命令、自己发的、以及点名给别人的。插件在自己的 `start` 里调一次，
/// 把返回的守卫存进插件状态。
pub fn subscribe_help(
    host: HostApi,
    handler: impl Fn(&Command) + Send + Sync + 'static,
) -> Option<EventSubscription> {
    let mut owned: Box<CommandHandler> = Box::new(CommandHandler {
        host,
        handler: Box::new(handler),
    });
    // SAFETY: 指针指向堆上的 `CommandHandler`；`EventSubscription` 持有这个 Box，
    // 保证退订之前地址一直有效。
    let user_data = (&raw mut *owned).cast::<c_void>();
    let handle = host.on("updateNewMessage", None, trampoline, user_data);
    handle.is_valid().then(|| EventSubscription {
        host,
        handle,
        _handler: owned,
    })
}

/// 命令订阅的 C 回调：解出命令 → 过滤点名 → 交给闭包。
extern "C-unwind" fn trampoline(user_data: *mut c_void, payload: *const c_char) -> i32 {
    if user_data.is_null() || payload.is_null() {
        return -1;
    }
    // SAFETY: `user_data` 是 `subscribe_help` 里那个 Box，订阅活多久它就活多久。
    let state = unsafe { &*(user_data as *const CommandHandler) };
    // SAFETY: 宿主保证 `payload` 是 NUL 结尾的临时字符串。
    let text = unsafe { CStr::from_ptr(payload) }.to_string_lossy();
    let Ok(update) = serde_json::from_str::<Value>(&text) else {
        return 0;
    };
    if let Some(command) = command_of(&update)
        && state.accepts(&command)
    {
        (state.handler)(&command);
    }
    0
}

// ── 分组（按登记它的插件） ─────────────────────────────────────────

/// 未知来源（宿主自己 / 静态基线）那一组的标题。
pub const OTHER_GROUP: &str = "其它";

/// 一组命令：一个提供方插件的全部命令。
#[derive(Debug, Clone)]
pub struct CommandGroup {
    pub title: String,
    pub commands: Vec<Value>,
}

/// 问宿主「插件名 → 展示名」。读不到（老宿主 / 没装插件目录）就空表，组标题退回短名。
///
/// 只读宿主报告，**不 dlopen**：名字只用来排版，不值得为它加载任何东西。
pub fn plugin_display_names(host: &HostApi) -> BTreeMap<String, String> {
    let mut names = BTreeMap::new();
    let Ok(report) = serde_json::from_str::<Value>(&host.list_plugins()) else {
        return names;
    };
    let Some(plugins) = report.get("plugins").and_then(Value::as_array) else {
        return names;
    };
    for plugin in plugins {
        let Some(name) = plugin.get("name").and_then(Value::as_str) else {
            continue;
        };
        let display = plugin
            .get("display_name")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim();
        if !display.is_empty() {
            names.insert(name.to_owned(), display.to_owned());
        }
    }
    names
}

// ── 宿主命令表 + 权限过滤（FuyuBot 0.1.5） ────────────────────────
//
// 命令表的主人从 `/help` 插件换成了宿主：插件把命令登记进
// `fuyubot.host.commands`，条目带 `role` / `scopes` / `hidden`；`/help` 只读
// `list`，再按「当前用户的全局角色 × 当前会话类型 × 本会话管理权」过滤。
//
// 过滤必须在回复线程里做：群管理员要问 `getChatMember`，不能在接收循环上等
// TDLib。会话类型与成员身份先读宿主缓存（`fuyubot.host.cache`），未命中再发一次
// TDLib 请求，未命中且离线就按保守值处理。

/// 会话类型（命令表 `scopes` 里的三种）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ChatKind {
    Private,
    Group,
    Channel,
}

/// 命令要求的最低角色（命令表 `role` 字段）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Role {
    User,
    ChatAdmin,
    Admin,
    Owner,
}

impl Role {
    fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "" | "user" => Some(Self::User),
            "chat_admin" | "chat-admin" | "chatadmin" => Some(Self::ChatAdmin),
            "admin" => Some(Self::Admin),
            "owner" => Some(Self::Owner),
            _ => None,
        }
    }

    /// 条目描边颜色（图片版）：主人紫、管理员绿、群管黄、普通用户白。
    ///
    /// 都是**淡色**（往白里调过），1px 描边才不至于压过卡片本身；饱和度高一级时
    /// 在浅色卡片上会像一圈粗线。
    fn color(self) -> &'static str {
        match self {
            Self::Owner => "#c9a2fb",
            Self::Admin => "#7fd8a0",
            Self::ChatAdmin => "#f2d268",
            Self::User => "#ffffff",
        }
    }

    /// 条目阴影（图片版）：**中性灰**的一点点投影，颜色与描边无关——同色阴影会把
    /// 1px 描边糊成 2px 的粗边。
    ///
    /// 纯白描边在浅色卡片上看不出来，所以普通用户可以再垫一圈很淡的灰外圈勾轮廓；
    /// 那是中性灰，不是「白描边的阴影」。
    fn shadow(self) -> &'static str {
        match self {
            Self::User => "0 0 0 1px rgba(203,213,225,.55), 0 1px 2px rgba(15,23,42,.04)",
            _ => "0 1px 2px rgba(15,23,42,.04)",
        }
    }

    /// 这个人能不能看到 / 使用要求该角色的命令。
    fn allows(self, global: GlobalRole, chat_admin: bool) -> bool {
        match self {
            Self::User => true,
            Self::ChatAdmin => chat_admin,
            Self::Admin => matches!(global, GlobalRole::Admin | GlobalRole::Owner),
            Self::Owner => global == GlobalRole::Owner,
        }
    }
}

/// 配置算出的全局角色（宿主 `role` 方法的 `global` 字段）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GlobalRole {
    User,
    Admin,
    Owner,
}

impl GlobalRole {
    fn parse(raw: &str) -> Self {
        match raw.trim().to_ascii_lowercase().as_str() {
            "owner" => Self::Owner,
            "admin" => Self::Admin,
            _ => Self::User,
        }
    }
}

/// `/help` 实际要显示的命令：宿主表 → 角色 / 会话 / `hidden` 过滤。
/// **文字版**吃这份结果（保持原有语义，不跟随图片的私聊全量视图）。
fn visible_commands(host: &HostApi, chat_id: i64, sender_user_id: Option<i64>) -> Vec<Value> {
    filtered_commands(host, chat_id, sender_user_id, false)
}

/// 图片版要显示的命令：在 [`visible_commands`] 的基础上，**私聊里的主人 / 管理员
/// 能看到整张表**（主人看全部，管理员看除主人命令以外的全部）。
///
/// 这是图片版专属的展示口径：只影响 `/help` 画出来的卡片，不改变命令本身的
/// 执行权限，也不影响 `/help text`。`hidden` 仍然排除——它表达的是「别列出来」。
fn visible_image_commands(host: &HostApi, chat_id: i64, sender_user_id: Option<i64>) -> Vec<Value> {
    filtered_commands(host, chat_id, sender_user_id, true)
}

fn filtered_commands(
    host: &HostApi,
    chat_id: i64,
    sender_user_id: Option<i64>,
    image_view: bool,
) -> Vec<Value> {
    let kind = chat_kind(host, chat_id);
    let global = global_role(host, sender_user_id);
    let chat_admin = is_chat_admin(host, kind, chat_id, sender_user_id);
    registered_commands(host)
        .into_iter()
        .filter(|entry| {
            if image_view {
                entry_visible_to_image(entry, global, kind, chat_admin)
            } else {
                entry_visible(entry, global, kind, chat_admin)
            }
        })
        .collect()
}

/// 从宿主读整张命令表。读不到 / 形状不对就空表——`/help` 回一句「没有可用命令」，
/// 而不是把插件搞崩。
fn registered_commands(host: &HostApi) -> Vec<Value> {
    let Ok(raw) = host.call_service(HOST_COMMANDS, "list", "{}") else {
        return Vec::new();
    };
    serde_json::from_str(&raw).unwrap_or_default()
}

/// 一条命令对当前用户 / 会话是否可见。
///
/// 缺字段按宿主默认值处理：`role = user`、`scopes = private + group`、
/// `hidden = false`（老宿主 / 兼容别名登记的命令没有这几个字段）。
fn entry_visible(entry: &Value, global: GlobalRole, kind: ChatKind, chat_admin: bool) -> bool {
    if entry
        .get("hidden")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return false;
    }
    let role = entry_role(entry);
    if !role.allows(global, chat_admin) {
        return false;
    }
    match entry.get("scopes").and_then(Value::as_array) {
        Some(scopes) => scopes
            .iter()
            .filter_map(Value::as_str)
            .any(|scope| scope_allows(scope, kind)),
        None => kind != ChatKind::Channel,
    }
}

/// 图片版的可见性：**私聊里的主人 / 管理员看整张表**，其余情况与 [`entry_visible`] 相同。
///
/// - 主人（`owner`）：私聊里所有命令都画出来（含 `admin` / `owner` 命令、含只允许
///   频道 / 群执行的命令）——主人本来就什么都能用，`/help` 给一张全表更有用。
/// - 管理员（`admin`）：私聊里能看到除 `owner` 以外的全部命令。
/// - 其他人 / 群 / 频道：`scopes` 照旧，只在当前会话类型下画能用的命令。
///
/// `hidden` 一律排除：它表达的是「不要列出来」，与角色无关。
fn entry_visible_to_image(
    entry: &Value,
    global: GlobalRole,
    kind: ChatKind,
    chat_admin: bool,
) -> bool {
    let hidden = entry
        .get("hidden")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if hidden {
        return false;
    }
    let private_all_access = kind == ChatKind::Private
        && match global {
            GlobalRole::Owner => true,
            GlobalRole::Admin => entry_role(entry) != Role::Owner,
            GlobalRole::User => false,
        };
    if private_all_access {
        return true;
    }
    entry_visible(entry, global, kind, chat_admin)
}

/// 条目要求的角色；缺字段 / 认不出按宿主默认 `user`。
fn entry_role(entry: &Value) -> Role {
    entry
        .get("role")
        .and_then(Value::as_str)
        .and_then(Role::parse)
        .unwrap_or(Role::User)
}

fn scope_allows(raw: &str, kind: ChatKind) -> bool {
    match raw.trim().to_ascii_lowercase().as_str() {
        "private" | "pm" | "dm" => kind == ChatKind::Private,
        "group" | "supergroup" => kind == ChatKind::Group,
        "channel" => kind == ChatKind::Channel,
        _ => false,
    }
}

/// 条目声明了哪些会话类型：按**宿主给的原始 `scopes`** 解析（不按当前会话裁剪），
/// 图片右侧的图标就是这三个开关。缺字段按宿主默认值 `private + group`。
fn entry_scopes(entry: &Value) -> (bool, bool, bool) {
    let Some(scopes) = entry.get("scopes").and_then(Value::as_array) else {
        return (true, true, false);
    };
    let mut private = false;
    let mut group = false;
    let mut channel = false;
    for raw in scopes.iter().filter_map(Value::as_str) {
        match raw.trim().to_ascii_lowercase().as_str() {
            "private" | "pm" | "dm" => private = true,
            "group" | "supergroup" => group = true,
            "channel" => channel = true,
            _ => {}
        }
    }
    (private, group, channel)
}

/// 问宿主「这个人的全局角色」：纯内存，不碰 TDLib。
fn global_role(host: &HostApi, user_id: Option<i64>) -> GlobalRole {
    let Some(user_id) = user_id else {
        return GlobalRole::User;
    };
    let input = json!({ "user_id": user_id });
    let Ok(raw) = host.call_service(HOST_COMMANDS, "role", &input.to_string()) else {
        return GlobalRole::User;
    };
    let value: Value = serde_json::from_str(&raw).unwrap_or(Value::Null);
    GlobalRole::parse(value.get("global").and_then(Value::as_str).unwrap_or(""))
}

/// 会话类型：先看缓存 / `getChat`，拿不到再退回 id 符号。
fn chat_kind(host: &HostApi, chat_id: i64) -> ChatKind {
    if chat_id > 0 {
        return ChatKind::Private;
    }
    if let Some(kind) = cached_object(host, "chat", chat_id, None)
        .as_ref()
        .and_then(kind_of_chat)
    {
        return kind;
    }
    if let Some(kind) = td_call(host, json!({ "@type": "getChat", "chat_id": chat_id }))
        .as_ref()
        .and_then(kind_of_chat)
    {
        return kind;
    }
    // 负 id 是群 / 频道；分不清时按群处理（默认 scopes 就含 group）。
    ChatKind::Group
}

/// `getChat` 的 `type`：`chatTypePrivate` / `chatTypeBasicGroup` /
/// `chatTypeSupergroup`（再看 `is_channel`）。
fn kind_of_chat(chat: &Value) -> Option<ChatKind> {
    match chat.pointer("/type/@type")?.as_str()? {
        "chatTypePrivate" => Some(ChatKind::Private),
        "chatTypeBasicGroup" => Some(ChatKind::Group),
        "chatTypeSupergroup" => {
            let is_channel = chat
                .pointer("/type/is_channel")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            Some(if is_channel {
                ChatKind::Channel
            } else {
                ChatKind::Group
            })
        }
        _ => None,
    }
}

/// 群管理权。私聊恒 `false`；频道帖子的发送者本来就是频道身份，恒 `true`。
///
/// 群聊先读宿主缓存（`chat_member` 带 30 秒 TTL），未命中再问 TDLib；
/// 查不到一律 `false`——不确定时说「没有」比说「有」安全。
fn is_chat_admin(
    host: &HostApi,
    kind: ChatKind,
    chat_id: i64,
    sender_user_id: Option<i64>,
) -> bool {
    match kind {
        ChatKind::Private => false,
        ChatKind::Channel => true,
        ChatKind::Group => {
            let Some(user_id) = sender_user_id else {
                return false;
            };
            let member = cached_object(host, "chat_member", chat_id, Some(user_id)).or_else(|| {
                td_call(
                    host,
                    json!({
                        "@type": "getChatMember",
                        "chat_id": chat_id,
                        "member_id": { "@type": "messageSenderUser", "user_id": user_id },
                    }),
                )
            });
            member.as_ref().is_some_and(member_is_admin)
        }
    }
}

fn member_is_admin(member: &Value) -> bool {
    matches!(
        member.pointer("/status/@type").and_then(Value::as_str),
        Some("chatMemberStatusCreator" | "chatMemberStatusAdministrator")
    )
}

/// 读宿主对象缓存：命中返回原始 TDLib 对象，未命中 / 离线返回 `None`。
fn cached_object(host: &HostApi, kind: &str, id: i64, user_id: Option<i64>) -> Option<Value> {
    let mut input = json!({ "kind": kind, "id": id });
    if let Some(user_id) = user_id {
        input["user_id"] = json!(user_id);
    }
    let raw = host
        .call_service(HOST_CACHE, "get", &input.to_string())
        .ok()?;
    let value: Value = serde_json::from_str(&raw).ok()?;
    value.get("value").filter(|value| !value.is_null()).cloned()
}

/// 同步问一次 TDLib（`simplify_invoke` 的 `invoke` op，返回值直接拿）。
///
/// 只在 `/help` 工作线程上调用；超时 [`LOOKUP_TIMEOUT_MS`]，失败返回 `None`。
fn td_call(host: &HostApi, request: Value) -> Option<Value> {
    let op = json!({
        "op": "invoke",
        "args": { "request": request, "timeout_ms": LOOKUP_TIMEOUT_MS },
    });
    let raw = host.simplify_invoke_result(&op.to_string());
    if raw.is_empty() {
        return None;
    }
    let value: Value = serde_json::from_str(&raw).ok()?;
    (value.get("ok").and_then(Value::as_bool) == Some(true))
        .then(|| value.get("result").cloned())
        .flatten()
}

/// 把 `/help` 登记进宿主命令表。失败只记日志：`/help` 本身仍然能用，
/// 只是它不出现在自己的列表里、也不受命令闸门保护。
pub fn register_help(host: &HostApi) {
    let input = json!({
        "name": "help",
        "description": "这条帮助（/help text 出文字版）",
        "role": "user",
        "scopes": ["private", "group", "channel"],
    });
    match host.call_service(HOST_COMMANDS, "register", &input.to_string()) {
        Ok(reply) => {
            let ok = serde_json::from_str::<Value>(&reply)
                .ok()
                .and_then(|value| value.get("ok").and_then(Value::as_bool))
                .unwrap_or(false);
            if !ok {
                crate::log::warn(host, format!("help 登记进宿主命令表被拒：{reply}"));
            }
        }
        Err(error) => crate::log::warn(host, format!("help 登记进宿主命令表失败：{error}")),
    }
}

/// 按**登记它的插件**分组：组按插件名排序，「其它」永远在最后。
///
/// `plugin` 字段是宿主判定的（`register` 时回问 `current_plugin`），插件伪造不了；
/// 没有归属的命令（宿主自己 / 静态基线）收进「其它」——不知道就别编。
pub fn group_commands(commands: &[Value], names: &BTreeMap<String, String>) -> Vec<CommandGroup> {
    let mut groups: BTreeMap<&str, Vec<Value>> = BTreeMap::new();
    let mut unknown = Vec::new();
    for command in commands {
        match command.get("plugin").and_then(Value::as_str) {
            Some(plugin) => groups.entry(plugin).or_default().push(command.clone()),
            None => unknown.push(command.clone()),
        }
    }

    let mut out: Vec<CommandGroup> = groups
        .into_iter()
        .map(|(plugin, commands)| CommandGroup {
            title: group_title(plugin, names),
            commands,
        })
        .collect();
    if !unknown.is_empty() {
        out.push(CommandGroup {
            title: OTHER_GROUP.to_owned(),
            commands: unknown,
        });
    }
    out
}

/// 组标题：展示名 + 短名（`广告拦截（adblock）`）；没有展示名就只写短名。
///
/// 展示名里已经含短名时（`Adblock` / `Help (Image)`）不再重复追加——
/// 插件包的 `plugin.json` 名字经常就长这样。
fn group_title(plugin: &str, names: &BTreeMap<String, String>) -> String {
    let short = plugin.rsplit('.').next().unwrap_or(plugin);
    match names.get(plugin) {
        Some(display)
            if !display.eq_ignore_ascii_case(short)
                && !display
                    .to_ascii_lowercase()
                    .contains(&short.to_ascii_lowercase()) =>
        {
            format!("{display}（{short}）")
        }
        Some(display) => display.clone(),
        None => short.to_owned(),
    }
}

// ── 出图 + 发送 ───────────────────────────────────────────────────

/// 一张图的统计：命令总数、来源插件数、实际画出来的条数。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HelpStats {
    pub total: usize,
    pub plugins: usize,
    pub shown: usize,
}

/// 模板里一个条目的默认值（截断提示这类「不是命令」的条目用它）。
fn placeholder_item(name: &str, desc: &str) -> Value {
    json!({
        "name": name,
        "desc": desc,
        "color": Role::User.color(),
        "shadow": Role::User.shadow(),
        "scopes": { "private": false, "group": false, "channel": false },
    })
}

/// 命令条目在图片里的样子：`/名字` + 说明 + 角色描边色 + 右侧会话图标。
fn render_item(command: &Value) -> Option<Value> {
    let name = command.get("name").and_then(Value::as_str)?;
    let desc = command
        .get("description")
        .and_then(Value::as_str)
        .unwrap_or("");
    let role = entry_role(command);
    let (private, group, channel) = entry_scopes(command);
    Some(json!({
        "name": name,
        "desc": desc,
        "color": role.color(),
        "shadow": role.shadow(),
        "scopes": { "private": private, "group": group, "channel": channel },
    }))
}

/// 底部的图例：**一排**，四项角色描边色 + 三项会话图标。
///
/// 图例是**静态**的（不随这张图里的命令变化），保证同一张图在不同会话里
/// 读法一致。模板只按 `kind` 决定画色块还是画图标。
fn legend() -> Value {
    json!({
        "items": [
            { "key": "owner", "kind": "role", "label": "主人", "color": Role::Owner.color() },
            { "key": "admin", "kind": "role", "label": "管理员", "color": Role::Admin.color() },
            { "key": "chat_admin", "kind": "role", "label": "群管理", "color": Role::ChatAdmin.color() },
            { "key": "user", "kind": "role", "label": "用户", "color": Role::User.color() },
            { "key": "scope-private", "kind": "scope", "label": "私聊" },
            { "key": "scope-group", "kind": "scope", "label": "群组" },
            { "key": "scope-channel", "kind": "scope", "label": "频道" },
        ],
    })
}

/// 拼出交给 `fuyubot.html2img` 的 `render` 入参。
///
/// **一个提供方插件一张卡片**：组标题优先用宿主报告的展示名，命令在组内保持
/// 「`/start` `/help` `/plugins` 优先、其余按名字」的顺序；没有归属的收在最后的
/// 「其它」里。`{count}`（本组条数）/ `{plugins}`（来源插件数）/ `{version}`
/// 会在文案里展开。
pub fn build_render_request(
    config: &Config,
    commands: &[Value],
    names: &BTreeMap<String, String>,
) -> (Value, HelpStats) {
    let groups = group_commands(commands, names);
    let total = commands.len();
    let plugins = commands
        .iter()
        .filter_map(|command| command.get("plugin").and_then(Value::as_str))
        .collect::<BTreeSet<_>>()
        .len();

    let mut cards: Vec<Value> = Vec::new();
    let mut shown = 0usize;
    for group in &groups {
        if shown >= config.max_commands {
            break;
        }
        let take = group.commands.len().min(config.max_commands - shown);
        let items: Vec<Value> = group.commands[..take]
            .iter()
            .filter_map(render_item)
            .collect();
        if items.is_empty() {
            continue;
        }
        shown += items.len();
        cards.push(json!({
            "name": group.title,
            "desc": config.expand(&config.table_desc, items.len(), plugins),
            "commands": items,
        }));
    }
    // 截断提示挂在最后一张卡上：不额外占一张空卡片。
    if total > shown {
        let note = placeholder_item("…", &format!("还有 {} 条命令未显示", total - shown));
        match cards.last_mut().and_then(|card| card.get_mut("commands")) {
            Some(Value::Array(list)) => list.push(note),
            _ => cards.push(json!({
                "name": OTHER_GROUP,
                "desc": "",
                "commands": [note],
            })),
        }
    }

    let data = json!({
        "title": config.expand(&config.title, total, plugins),
        "description": config.expand(&config.description, total, plugins),
        "tips": config.expand(&config.tips, total, plugins),
        "version": config::version_label(),
        "imgSrc": config.image,
        "data": cards,
        "legend": legend(),
    });

    let request = json!({
        "html": crate::template::TEMPLATE,
        "vue": true,
        "data": data,
        "width": config.width,
        "height": config.height,
        "scale": config.scale,
        "format": config.format,
        "auto_height": true,
        "dark": false,
        "transparent": false,
    });

    (
        request,
        HelpStats {
            total,
            plugins,
            shown,
        },
    )
}

/// 调 html2img 的 `render`，返回生成的本地图片路径。
pub fn render_help_image(
    host: &HostApi,
    config: &Config,
    commands: &[Value],
    names: &BTreeMap<String, String>,
) -> Result<(String, HelpStats), String> {
    let (request, stats) = build_render_request(config, commands, names);
    let reply = host
        .call_service(config::HTML2IMG_SERVICE, "render", &request.to_string())
        .map_err(|error| format!("调用 {} 失败：{error}", config::HTML2IMG_SERVICE))?;
    let value: Value = serde_json::from_str(&reply)
        .map_err(|error| format!("html2img 返回的不是 JSON：{error}"))?;
    if value.get("ok").and_then(Value::as_bool) != Some(true) {
        let message = value
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("未知错误");
        return Err(format!("html2img 渲染失败：{message}"));
    }
    let path = value
        .get("path")
        .and_then(Value::as_str)
        .filter(|path| !path.is_empty())
        .ok_or_else(|| "html2img 没有返回图片路径".to_owned())?;
    Ok((path.to_owned(), stats))
}

/// 命令回复服务的入参：`chat_id` / `message_id` 指向**命令消息**，`payload` 是
/// `sendMessage` 的参数（`input_message_content` + 可选的 `reply_markup` / `topic_id`）。
pub fn reply_input(chat_id: i64, message_id: i64, timeout_ms: u64, payload: Value) -> Value {
    json!({
        "chat_id": chat_id,
        "message_id": message_id,
        "timeout_ms": timeout_ms,
        "payload": payload,
    })
}

/// 一张本地图片的 `input_message_content`（形状照 `td_api.tl` 的 `inputMessagePhoto`）。
pub fn photo_content(path: &str) -> Value {
    json!({
        "@type": "inputMessagePhoto",
        "photo": {
            "@type": "inputPhoto",
            "photo": { "@type": "inputFileLocal", "path": path },
            "width": 0,
            "height": 0,
        },
        "has_spoiler": false,
    })
}

/// 一条纯文本的 `input_message_content`（关掉链接预览）。
///
/// `inputMessageText.text` 在 TDLib 里是 **`formattedText` 对象**，不是字符串。
/// 直接塞字符串会被 TDLib 以 400 拒绝：
/// `Failed to parse JSON object as TDLib request: Expected Object, but receive String`。
pub fn text_content(text: &str) -> Value {
    json!({
        "@type": "inputMessageText",
        "text": {
            "@type": "formattedText",
            "text": text,
            "entities": [],
        },
        "link_preview_options": {
            "@type": "linkPreviewOptions",
            "is_disabled": true,
        },
    })
}

/// 回复一张本地图片；等到宿主的最终结果。
pub fn send_photo(
    host: &HostApi,
    config: &Config,
    chat_id: i64,
    message_id: i64,
    path: &str,
) -> Result<Value, String> {
    reply(host, config, chat_id, message_id, photo_content(path))
}

/// 回复一条纯文本；等到宿主的最终结果。
pub fn send_text(
    host: &HostApi,
    config: &Config,
    chat_id: i64,
    message_id: i64,
    text: &str,
) -> Result<Value, String> {
    reply(host, config, chat_id, message_id, text_content(text))
}

/// 调宿主回复一条命令：发新消息 / 编辑原命令 / ephemeral 全由宿主决定。
///
/// **同步阻塞**：宿主当场等 TDLib 的最终消息（最多 `config.send_timeout_ms`），
/// 所以只在 `/help` 自己的工作线程里调，别放在宿主的更新回调里。
fn reply(
    host: &HostApi,
    config: &Config,
    chat_id: i64,
    message_id: i64,
    content: Value,
) -> Result<Value, String> {
    let payload = json!({ "input_message_content": content });
    let input = reply_input(chat_id, message_id, config.send_timeout_ms, payload);
    let raw = host
        .call_service(COMMAND_SERVICE, "reply", &input.to_string())
        .map_err(|error| format!("命令回复服务不可用：{error}"))?;
    let value: Value =
        serde_json::from_str(&raw).map_err(|error| format!("命令回复结果不是 JSON：{error}"))?;
    // TDLib 的失败是以 `{"@type":"error",…}` 回来的，不是错误码。
    if value.get("@type").and_then(Value::as_str) == Some("error") {
        let code = value.get("code").and_then(Value::as_i64).unwrap_or(0);
        let message = value.get("message").and_then(Value::as_str).unwrap_or("");
        return Err(format!("回复失败（{code}）：{message}"));
    }
    Ok(value)
}

// ── 一次 /help 的完整流程 ──────────────────────────────────────────

/// `/help` 要出什么：图片（默认）还是文字。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HelpMode {
    Image,
    Text,
}

impl HelpMode {
    /// 从命令参数里认形态：`/help t` / `/help text` 要文字版，其余（含空）出图。
    ///
    /// 大小写不敏感；多给了别的词就当没给（老行为：出图）。
    pub fn from_args(args: &str) -> Self {
        match args.trim().to_ascii_lowercase().as_str() {
            "t" | "text" => Self::Text,
            _ => Self::Image,
        }
    }
}

/// 取命令表 → 出图 / 出文字 → 回复；图片失败时按配置补发文字版。
///
/// 跑在工作线程上，可以安全地阻塞等 TDLib 结果。
pub fn run_help(
    host: HostApi,
    config: Config,
    chat_id: i64,
    message_id: i64,
    sender_user_id: Option<i64>,
    render_lock: Arc<Mutex<()>>,
    mode: HelpMode,
) {
    let names = plugin_display_names(&host);

    // `/help text`：直接出文字，不渲染、不占渲染锁。**文字版保持原有过滤语义**
    // （按 role / scopes 裁剪），不跟随图片版「私聊里主人 / 管理员看全表」的口径。
    if mode == HelpMode::Text {
        let commands = visible_commands(&host, chat_id, sender_user_id);
        send_text_version(&host, &config, chat_id, message_id, &commands, &names);
        return;
    }

    // 图片版：私聊里的主人 / 管理员能看整张表（主人全部、管理员除 owner 外全部）。
    let commands = visible_image_commands(&host, chat_id, sender_user_id);
    // 图片链路失败时补发的文字版仍用**严格口径**：文字版不跟随图片版的放宽。
    let strict = visible_commands(&host, chat_id, sender_user_id);

    // 渲染是同步且吃 CPU 的：串行化，但别把发送也锁在里面。
    let rendered = {
        let _guard = render_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        render_help_image(&host, &config, &commands, &names)
    };

    match rendered {
        Ok((path, stats)) => {
            log::info(
                &host,
                format!(
                    "/help：{} 条命令（{} 个来源）已出图（{}），准备发送",
                    stats.total, stats.plugins, path
                ),
            );
            match send_photo(&host, &config, chat_id, message_id, &path) {
                Ok(_) => log::debug(&host, format!("/help 图片已回复到 chat {chat_id}")),
                Err(error) => {
                    log::error(&host, format!("/help 图片发送失败：{error}"));
                    send_fallback(&host, &config, chat_id, message_id, &strict, &names);
                }
            }
        }
        Err(error) => {
            log::error(&host, format!("/help 出图失败：{error}"));
            send_fallback(&host, &config, chat_id, message_id, &strict, &names);
        }
    }
}

/// 起一个工作线程跑 [`run_help`]；已结束的句柄顺手清掉。
///
/// `pending` 是**正在跑 + 排队**的请求数，超过 [`MAX_PENDING`] 时直接丢弃并记一条
/// 日志：`/help` 是入口命令，刷屏时宁可少回一条，也不要把线程数拉爆。
#[allow(clippy::too_many_arguments)]
pub fn spawn_help(
    host: HostApi,
    config: Config,
    chat_id: i64,
    message_id: i64,
    sender_user_id: Option<i64>,
    render_lock: Arc<Mutex<()>>,
    workers: Arc<Mutex<Vec<JoinHandle<()>>>>,
    pending: Arc<AtomicUsize>,
    mode: HelpMode,
) {
    let queued = pending.fetch_add(1, Ordering::SeqCst) + 1;
    if queued > MAX_PENDING {
        pending.fetch_sub(1, Ordering::SeqCst);
        log::warn(
            &host,
            format!("/help 排队已满（{MAX_PENDING}），忽略 chat {chat_id} 的这次请求"),
        );
        return;
    }
    let guard = PendingGuard(Arc::clone(&pending));

    let mut handles = workers
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    handles.retain(|handle| !handle.is_finished());

    let spawned = std::thread::Builder::new()
        .name("fuyubot-help".to_owned())
        .spawn(move || {
            let _guard = guard;
            run_help(
                host,
                config,
                chat_id,
                message_id,
                sender_user_id,
                render_lock,
                mode,
            );
        });
    match spawned {
        Ok(handle) => handles.push(handle),
        Err(error) => log::error(&host, format!("起 /help 工作线程失败：{error}")),
    }
}

/// `spawn_help` 的计数器守卫：线程结束（含 panic / 起线程失败）时把 pending 减回去。
struct PendingGuard(Arc<AtomicUsize>);

impl Drop for PendingGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

/// `/help text`：直接回一份文字版命令表（与图片同一分组，不渲染）。
fn send_text_version(
    host: &HostApi,
    config: &Config,
    chat_id: i64,
    message_id: i64,
    commands: &[Value],
    names: &BTreeMap<String, String>,
) {
    let text = help_text(commands, names);
    match send_text(host, config, chat_id, message_id, &text) {
        Ok(_) => log::debug(host, format!("/help 文字版已回复到 chat {chat_id}")),
        Err(error) => log::error(host, format!("/help 文字版发送失败：{error}")),
    }
}

/// 补发文字版：图片链路出问题时 `/help` 不能什么都不回。
fn send_fallback(
    host: &HostApi,
    config: &Config,
    chat_id: i64,
    message_id: i64,
    commands: &[Value],
    names: &BTreeMap<String, String>,
) {
    if !config.fallback_text {
        return;
    }
    let text = help_text(commands, names);
    match send_text(host, config, chat_id, message_id, &text) {
        Ok(_) => log::warn(host, "/help 已改发文字版"),
        Err(error) => log::error(host, format!("/help 文字版也发送失败：{error}")),
    }
}

/// 纯文本命令表（只用于图片链路失败后的回退）。
///
/// 与图片同一个分组：一个插件一段，没有归属的收在最后的「其它」里。
pub fn help_text(commands: &[Value], names: &BTreeMap<String, String>) -> String {
    let mut text = format!("可用命令（共 {} 条）\n\n", commands.len());
    for group in group_commands(commands, names) {
        text.push_str(&format!("【{}】\n", group.title));
        for command in &group.commands {
            let name = command.get("name").and_then(Value::as_str).unwrap_or("?");
            let description = command
                .get("description")
                .and_then(Value::as_str)
                .unwrap_or("");
            if description.is_empty() {
                text.push_str(&format!("/{name}\n"));
            } else {
                text.push_str(&format!("/{name} — {description}\n"));
            }
        }
        text.push('\n');
    }
    truncate_utf16(text, FALLBACK_TEXT_LIMIT)
}

/// 按 UTF-16 码元截断：Telegram 的 4096 上限按 UTF-16 计（emoji 占 2 个），
/// 按 `chars()` 算会被 emoji 多的说明撑爆。仍按字符边界切，不会切坏中文。
fn truncate_utf16(text: String, limit: usize) -> String {
    if text.encode_utf16().count() <= limit {
        return text;
    }
    let mut units = 0usize;
    let mut out = String::new();
    for character in text.chars() {
        let width = character.len_utf16();
        if units + width > limit {
            break;
        }
        out.push(character);
        units += width;
    }
    out.push_str("\n…（已截断）");
    out
}

#[cfg(test)]
#[path = "../tests/unit/commands.rs"]
mod tests;
