//! `/help` 的整条链路：命令解析 → 取命令表 → 分组 → 出图 / 出文字 → 回复命令。
//!
//! 命令按**登记它的插件**分组（`plugin` 由宿主判定，插件伪造不了）：图片里一个
//! 插件一张卡片，组标题优先用宿主报告的展示名；没有归属的收在最后的「其它」里。
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

/// 回退文字版的长度上限：Telegram 单条消息 4096 字符，留一点余量。
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
            .filter_map(|command| {
                let name = command.get("name").and_then(Value::as_str)?;
                let desc = command
                    .get("description")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                Some(json!({ "name": name, "desc": desc }))
            })
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
        let note = json!({
            "name": "…",
            "desc": format!("还有 {} 条命令未显示", total - shown),
        });
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
pub fn text_content(text: &str) -> Value {
    json!({
        "@type": "inputMessageText",
        "text": text,
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
    render_lock: Arc<Mutex<()>>,
    mode: HelpMode,
) {
    let commands = crate::service::collect_commands();
    let names = plugin_display_names(&host);

    // `/help text`：直接出文字，不渲染、不占渲染锁。
    if mode == HelpMode::Text {
        send_text_version(&host, &config, chat_id, message_id, &commands, &names);
        return;
    }

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
                    send_fallback(&host, &config, chat_id, message_id, &commands, &names);
                }
            }
        }
        Err(error) => {
            log::error(&host, format!("/help 出图失败：{error}"));
            send_fallback(&host, &config, chat_id, message_id, &commands, &names);
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
            run_help(host, config, chat_id, message_id, render_lock, mode);
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
    truncate_chars(text, FALLBACK_TEXT_LIMIT)
}

/// 按字符数截断（不能按字节切，中文会切坏）。
fn truncate_chars(text: String, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text;
    }
    let mut out: String = text.chars().take(limit).collect();
    out.push_str("\n…（已截断）");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn commands() -> Vec<Value> {
        vec![
            json!({ "name": "start", "description": "开始使用", "plugin": "fuyubot.start" }),
            json!({ "name": "help", "description": "这条帮助", "plugin": "fuyubot.help" }),
        ]
    }

    #[test]
    fn command_is_parsed_from_the_update_envelope() {
        let update = json!({
            "@type": "updateNewMessage",
            "message": {
                "@type": "message",
                "id": 101,
                "chat_id": 777,
                "is_outgoing": false,
                "content": {
                    "@type": "messageText",
                    "text": { "@type": "formattedText", "text": "/Help@SomeBot  参数 ", "entities": [] }
                }
            }
        });
        let command = command_of(&update).expect("应该解析出命令");
        assert!(command.is("help"));
        assert_eq!(command.chat_id, 777);
        assert_eq!(command.message_id, 101);
        assert_eq!(command.args, "参数");
        assert_eq!(command.addressed_to.as_deref(), Some("SomeBot"));
        // 传成 `update["message"]` 就一条命令都收不到。
        assert!(command_of(&update["message"]).is_none());
    }

    #[test]
    fn outgoing_or_plain_messages_are_not_commands() {
        let outgoing = json!({
            "message": {
                "id": 2, "chat_id": 1, "is_outgoing": true,
                "content": { "@type": "messageText", "text": { "text": "/help" } }
            }
        });
        assert!(command_of(&outgoing).is_none());

        let plain = json!({
            "message": {
                "id": 2, "chat_id": 1,
                "content": { "@type": "messageText", "text": { "text": "hello" } }
            }
        });
        assert!(command_of(&plain).is_none());
    }

    #[test]
    fn addressed_commands_only_answer_when_named() {
        let command = Command {
            chat_id: 1,
            message_id: 2,
            name: "help".to_owned(),
            args: String::new(),
            addressed_to: Some("SomeBot".to_owned()),
            text: "/help@SomeBot".to_owned(),
        };
        assert!(command.is_for(&["somebot".to_owned()]));
        assert!(!command.is_for(&["otherbot".to_owned()]));
        // 身份未知时不吞命令。
        assert!(command.is_for(&[]));
    }

    /// 展示名映射：与宿主 `list_plugins` 报告里的 `display_name` 同形。
    fn display_names() -> BTreeMap<String, String> {
        BTreeMap::from([
            ("fuyubot.help".to_owned(), "帮助".to_owned()),
            ("fuyubot.start".to_owned(), "启动问候".to_owned()),
        ])
    }

    #[test]
    fn render_request_groups_by_plugin() {
        let config = Config::default();
        let mut list = commands();
        // 一条没有归属的（宿主自己 / 静态基线）。
        list.push(json!({ "name": "plugins", "description": "看看装了哪些插件" }));
        let (request, stats) = build_render_request(&config, &list, &display_names());
        assert_eq!(request["vue"], true);
        assert_eq!(request["width"], 760);
        assert_eq!(stats.total, 3);
        assert_eq!(stats.plugins, 2, "没有归属的那条不算来源插件");

        let cards = request["data"]["data"].as_array().unwrap();
        // 组按插件名排序（help < start），「其它」永远最后。
        assert_eq!(cards.len(), 3);
        assert_eq!(cards[0]["name"], "帮助（help）");
        assert_eq!(cards[0]["commands"].as_array().unwrap().len(), 1);
        assert_eq!(cards[0]["desc"], "共 1 条命令");
        assert_eq!(cards[1]["name"], "启动问候（start）");
        assert_eq!(cards[2]["name"], OTHER_GROUP);
        assert_eq!(cards[2]["commands"][0]["name"], "plugins");
    }

    /// 组标题没有展示名时退回短名；展示名里已含短名时不重复追加。
    #[test]
    fn group_titles_fall_back_to_the_short_name() {
        let names = BTreeMap::from([("fuyubot.adblock".to_owned(), "adblock".to_owned())]);
        assert_eq!(group_title("fuyubot.adblock", &names), "adblock");
        assert_eq!(group_title("fuyubot.other", &names), "other");

        // 插件包 `plugin.json` 的 name 常见形态：英文名里已经带上了短名。
        let names = BTreeMap::from([
            ("fuyubot.adblock".to_owned(), "Adblock".to_owned()),
            ("fuyubot.help".to_owned(), "Help (Image)".to_owned()),
        ]);
        assert_eq!(group_title("fuyubot.adblock", &names), "Adblock");
        assert_eq!(group_title("fuyubot.help", &names), "Help (Image)");

        // 中文展示名不含短名：补一个括号，点明是哪份插件。
        let names = BTreeMap::from([("fuyubot.adblock".to_owned(), "广告拦截".to_owned())]);
        assert_eq!(
            group_title("fuyubot.adblock", &names),
            "广告拦截（adblock）"
        );
    }

    #[test]
    fn long_tables_are_truncated_with_a_note() {
        let config = Config {
            max_commands: 2,
            ..Config::default()
        };
        let many: Vec<Value> = (0..5)
            .map(|index| {
                json!({
                    "name": format!("cmd{index}"),
                    "description": "x",
                    "plugin": "fuyubot.demo",
                })
            })
            .collect();
        let (request, stats) = build_render_request(&config, &many, &BTreeMap::new());
        assert_eq!(stats.total, 5);
        assert_eq!(stats.shown, 2);
        let cards = request["data"]["data"].as_array().unwrap();
        // 一个来源只画一张卡：2 条真实命令 + 1 条截断提示。
        assert_eq!(cards.len(), 1);
        let items = cards[0]["commands"].as_array().unwrap();
        assert_eq!(items.len(), 3);
        assert_eq!(items[2]["name"], "…");
        assert!(items[2]["desc"].as_str().unwrap().contains("还有 3 条"));
    }

    #[test]
    fn help_mode_accepts_t_and_text() {
        assert_eq!(HelpMode::from_args(""), HelpMode::Image);
        assert_eq!(HelpMode::from_args("  t "), HelpMode::Text);
        assert_eq!(HelpMode::from_args("TEXT"), HelpMode::Text);
        // 别的参数维持老行为：出图。
        assert_eq!(HelpMode::from_args("foo"), HelpMode::Image);
    }

    #[test]
    fn photo_content_matches_tdlib_shape() {
        let content = photo_content("/tmp/help.jpg");
        assert_eq!(content["@type"], "inputMessagePhoto");
        assert_eq!(content["photo"]["@type"], "inputPhoto");
        assert_eq!(content["photo"]["photo"]["@type"], "inputFileLocal");
        assert_eq!(content["photo"]["photo"]["path"], "/tmp/help.jpg");
    }

    /// 回复入参的形状是跨仓契约：`chat_id` / `message_id` 指命令消息，`payload` 装内容。
    #[test]
    fn reply_input_carries_command_ids() {
        let input = reply_input(
            -100,
            9,
            30_000,
            json!({"input_message_content": {"@type": "inputMessageText"}}),
        );
        assert_eq!(input["chat_id"], -100);
        assert_eq!(input["message_id"], 9);
        assert_eq!(input["timeout_ms"], 30_000);
        assert_eq!(
            input["payload"]["input_message_content"]["@type"],
            "inputMessageText"
        );
    }

    #[test]
    fn fallback_text_lists_every_command_by_group() {
        let text = help_text(&commands(), &display_names());
        assert!(text.contains("/start — 开始使用"), "{text}");
        assert!(text.contains("/help — 这条帮助"), "{text}");
        // 与图片同一分组：每个来源一段。
        assert!(text.contains("【帮助（help）】"), "{text}");
        assert!(text.contains("【启动问候（start）】"), "{text}");
    }

    #[test]
    fn fallback_text_is_truncated_by_chars() {
        let long: Vec<Value> = (0..500)
            .map(|index| json!({ "name": format!("cmd{index}"), "description": "很长的说明" }))
            .collect();
        let text = help_text(&long, &BTreeMap::new());
        assert!(text.chars().count() <= FALLBACK_TEXT_LIMIT + 10);
        assert!(
            text.ends_with("（已截断）"),
            "{}",
            &text[text.len().saturating_sub(20)..]
        );
    }
}
