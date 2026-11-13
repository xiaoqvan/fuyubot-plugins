//! 命令与自动识别：[`/dy`]/[COMMAND] 显式触发 + 消息里含抖音链接就自动解析。
//!
//! 对应 TS 版 `index.ts` 的 `cmdHandlers` / `updateHandlers` 与
//! `parse/douyin/index.ts`。两个入口共用同一个处理函数。
//!
//! ## 线程模型（这里最容易写出事故）
//!
//! `updateNewMessage` 的回调跑在**宿主的接收循环线程**上。解析要走两次网络往返，
//! 发送还要等 TDLib 应答——在回调里同步做就把整个宿主堵死了。
//!
//! 旧模型是「每条命令 `std::thread::spawn` 一个线程，`stop()` 挨个 join」。
//! 现在改成插件自有的 tokio 运行时（见 [`crate::runtime`]）：
//!
//! - 回调里只做「克隆参数 + [`Runtime::submit`]」，立刻返回；
//! - 网络 / TDLib 往返 / 缓存读写全在 tokio 任务里 `.await`；
//! - `stop()` 的顺序是「退订 → 置取消标志 → 关闸并等在飞归零 → 关 runtime」，
//!   动态库卸载前不会留下跑着本库代码的线程。
//!
//! ## 落点（不引用原消息）
//!
//! 普通会话一律**新发一条消息**（不 `reply_to`、不引用）：旧实现会先 `getChat`
//! 判群聊，群里引用原消息——用户明确要求删掉这套行为，所以 `getChat` 整段也一起删了。
//!
//! **频道帖子例外**：机器人通常没有发言权，只有编辑权，流程与 TS 版
//! `parse/douyin/index.ts` + `utils/videomsg.ts` 对齐——
//! 解析前 `edit_message_text` 把原帖改成「正在解析抖音视频...」，单条媒体
//! `edit_message_media`，图集先 `delete_messages` 删原帖再发相册，失败也编辑原帖写 ❌。
//!
//! ## 自动识别的过滤
//!
//! 群里「发送人是频道身份（`messageSenderChat`）**且**这条是转发消息」时跳过：
//! 那不是在群里正常发链接，而是把频道的转发链带进来，替它自动产出内容没有意义。
//! 频道帖子本身不在此列（TDLib `is_channel_post` 为真，照常解析并走编辑）。
//!
//! ## SQL 缓存（软依赖 `fuyubot.sqlite`）
//!
//! 处理一条链接的顺序：
//!
//! 1. 先从链接认出**作品 id**（本地解析；短链才 HEAD 一次看 302）；
//! 2. `<id>` 命中缓存 → **跳过分享页解析、跳过所有媒体下载**，用缓存的
//!    `inputFileId` 直接重发（[`crate::send::send_cached`]，零网络媒体请求）；
//! 3. 命中但发送失败（文件 id 失效）→ 删缓存 → 回落到完整解析（用户看不到错误）；
//! 4. 未命中 → 完整解析 + 发送，成功后把最终消息里的 `file.id` 写回缓存。
//!
//! 没有 SQL 插件时 `Cache` 永远未命中、写入直接丢弃——插件一切照常，只是没有加速。

use fuyubot_abi::{HostApi, Plugin, SubscriptionHandle};
use serde_json::Value;

use crate::cached::{self, Cached};
use crate::parse;
use crate::runtime::Runtime;
use crate::send::{self, Target};
use crate::simplify::{Command, CommandSubscription, channel_post_of, on_command};

/// 命令名（`/dy`）。
pub const COMMAND: &str = "dy";

/// `/dy` 里这一行说明。
pub const COMMAND_DESCRIPTION: &str = "解析抖音分享链接，回视频或图集";

/// `/dy <汽水音乐链接>` 时回的话。
///
/// 为什么**不能只报「解析失败」**：汽水短链本身是好的，只是没有作品 id，
/// 抖音解析器永远认不出来。用户分不清「链接坏了」和「该找别的插件」，
/// 就会反复重发同一条。所以这里直接说清楚归属，并给出**零成本**的下一步。
const QISHUI_REJECTION: &str =
    "这是汽水音乐的链接，不属于抖音解析——直接把链接发出来就行，音乐插件会处理它";

/// 宿主预置的**命令能力表**：插件经它的 `register` 方法登记命令。
///
/// 从 FuyuBot 0.1.5 起表归宿主（旧的 `fuyubot.help.commands` 只是兼容别名）。
const HOST_COMMANDS: &str = "fuyubot.host.commands";

/// 一个 URL 的 host 部分（`http(s)://<host>/…` → `<host>`，小写、去掉端口）。
///
/// ⚠️ **必须按 host 判，不能子串 contains**：`https://douyin.com.evil.test/x` 这种
/// 「把目标域名当前缀」的链接用 `contains` 会命中，等于替别人家的域名触发解析。
fn host_of(url: &str) -> Option<String> {
    let rest = url.split_once("://").map(|(_, rest)| rest).unwrap_or(url);
    let authority = rest.split(['/', '?', '#']).next()?;
    // 去掉 `user@` 与 `:port`，只留主机
    let host = authority.rsplit('@').next()?;
    let host = host.split(':').next()?;
    (!host.is_empty()).then(|| host.to_ascii_lowercase())
}

/// host 是不是抖音的（`douyin.com` 及其直接子域都算）。
fn is_douyin_host(host: &str) -> bool {
    // `iesdouyin.com` 是**独立**域名（`ies` + `douyin`），不是 `.douyin.com` 的子域，
    // 所以单独列出来——分享页正是走它。
    host == "douyin.com"
        || host.ends_with(".douyin.com")
        || host == "iesdouyin.com"
        || host == "www.iesdouyin.com"
}

/// 插件状态。
pub struct DouyinPlugin {
    host: HostApi,
    runtime: Option<std::sync::Arc<Runtime>>,
    /// 命令订阅守卫：`drop` 就退订。
    subscription: Option<CommandSubscription>,
    /// 扫链接订阅守卫。`drop` 就退订。
    scanning: Option<ScanSubscription>,
}

impl Plugin for DouyinPlugin {
    fn init(host: HostApi) -> Result<Self, String> {
        // 日志槽要尽早装：parse / http 两层的日志都靠它，装晚了前面的日志会丢。
        crate::log::install(host);
        Ok(Self {
            host,
            runtime: None,
            subscription: None,
            scanning: None,
        })
    }

    fn start(&mut self, config_json: &str) -> Result<(), String> {
        let runtime = Runtime::new(self.host, config_json)?;
        self.runtime = Some(std::sync::Arc::clone(&runtime));

        // ① 缓存初始化（异步）：探测 `fuyubot.sqlite` + 幂等建表。
        //    **失败绝不影响插件启动**——缓存只是加速。第一条命令若比这更快到，
        //    `cache_get` 里还会补一次 init（幂等、带锁）。
        runtime.submit(|rt| async move {
            let _ = rt.init_cache().await;
        });

        // ② `/dy <链接>`
        let command_runtime = std::sync::Arc::clone(&runtime);
        let subscription = on_command(self.host, move |command: &Command| {
            // ⚠️ `on_command` 对**所有**斜杠命令都会回调：只认 `/dy`，别的插件
            // （`/music163`、`/start`…）的命令不能被我们抢来发「用法」。
            if !command.is(COMMAND) {
                return;
            }
            let command = command.clone();
            let runtime = std::sync::Arc::clone(&command_runtime);
            // 宿主回调线程只排队，重活在这个任务里。
            runtime.submit(|rt| async move { handle_command(&rt, command).await });
        });
        let subscription = subscription.ok_or_else(|| "订阅 updateNewMessage 失败".to_owned())?;
        self.subscription = Some(subscription);

        // ③ 消息里含抖音链接就自动解析。
        //
        // ⚠️ **必须另开一条订阅**：`on_command` 走 `command_of`，而它要求文本以 `/`
        // 开头（`strip_prefix('/')?`），纯链接消息根本进不来回调——
        // 「自动识别」这条路 `on_command` 装不下。
        self.scanning = Some(scan_douyin_links(
            self.host,
            std::sync::Arc::clone(&runtime),
        ));

        // ④ 把 `/dy` 登记进宿主命令表——**不硬编码在 help 插件里**，走
        //    `fuyubot.host.commands` 的 `register` 方法。失败不影响 /dy 本身
        //    （只是 /help 里少一条说明、闸门也不认识它）。
        register_help(self.host);

        crate::log::info("已就绪：/dy 与自动识别都已挂上");
        Ok(())
    }

    /// 声明对 help 命令服务 / SQL 缓存的**软依赖**。
    ///
    /// 软依赖（`soft_inject`）而不是硬依赖（`inject`）：**没有 SQL 插件时 `/dy`
    /// 也必须能用**，只是没有缓存加速。命令表服务由宿主预置，不需要在这里声明。
    fn soft_inject() -> Vec<String> {
        vec![crate::cache::SERVICE.to_owned()]
    }

    fn stop(&mut self) {
        // 先断输入（守卫 drop 就退订），再停任务：反过来会留下「任务在跑、事件又进来」的窗口。
        self.subscription = None;
        self.scanning = None;
        if let Some(runtime) = self.runtime.take() {
            // 置取消标志 → 关闸 → 等在飞归零 → 关 runtime（顺序见 runtime 模块文档）。
            runtime.shutdown();
            crate::log::info("已停止");
        }
    }
}

/// 处理一条 `/dy` 命令（已在 tokio 任务里）。
async fn handle_command(rt: &Runtime, command: Command) {
    // ⚠️ 不走 `first_url`：文本里同时有汽水与抖音链接时，要解析的是抖音那条。
    // 汽水音乐归 `fuyubot.music`，抢过来只会 HEAD 一趟然后发一条 ❌。
    //
    // 频道里 `/dy` 也走编辑：命令消息本身就是那条频道帖，不再新发（与 TS 版一致）。
    let target = Target::of(&command);
    match pick_for_command(&format!("{} {}", command.args, command.text)) {
        Selection::Douyin(link) => {
            reply_with_video(rt, &target, &link, &command.text).await;
        }
        Selection::Qishui(link) => {
            // 记一条日志：用户报「/dy 发了但没反应」时，这里能直接看出是被让给了音乐插件。
            crate::log::info(&format!(
                "让出汽水音乐链接（由 fuyubot.music 处理）：{}",
                crate::log::redact_url(&link)
            ));
            // 汽水音乐的短链**没有作品 id**，抖音解析器认不出来。
            // 直接说清楚归谁管，比让它跑一遍再报「认不出作品」有用。
            reply_text(rt, &target, QISHUI_REJECTION).await;
        }
        Selection::None => {
            reply_text(rt, &target, "用法：`/dy 抖音链接`").await;
        }
    }
}

/// 回一句话：频道里编辑原帖，其它会话新发一条（不引用原消息）。
///
/// 频道里先查文本编辑权限；没有权限就**不编辑**（也不新发，避免往频道里灌消息）。
async fn reply_text(rt: &Runtime, target: &Target, text: &str) {
    if target.is_edit() {
        match send::channel_rights(rt, target).await {
            Ok(rights) if rights.can_edit_text => {
                if let Err(error) = send::replace_text(rt, target, text).await {
                    crate::log::warn(&format!("频道里编辑回复失败：{error}"));
                }
            }
            Ok(_) => crate::log::info("频道帖没有编辑文本的权限，跳过回复"),
            Err(error) => crate::log::warn(&format!("查频道帖权限失败，跳过回复：{error}")),
        }
    } else {
        rt.say(target.chat_id, text).await;
    }
}

/// 解析一条链接并把结果发回去；失败给出**人能看懂**的原因。
///
/// 把错误贴回聊天是刻意的：用户看不到宿主日志。
///
/// 频道里与 TS 版对齐：先查编辑权限，有权限才先把原帖编辑成「正在解析抖音视频...」，
/// 解析完再把它编辑成媒体；失败时也是**编辑原帖**写 ❌，不再往频道里另发错误消息。
///
/// **没有编辑媒体权限就整段跳过**：不解析、不编辑、也不新发——频道不是群，不能把
/// 「没权限」变成一次次失败的编辑尝试。
async fn reply_with_video(rt: &Runtime, target: &Target, link: &str, source_text: &str) {
    let mut can_edit_text = false;
    if target.is_edit() {
        match send::channel_rights(rt, target).await {
            Ok(rights) if rights.can_edit_media => {
                can_edit_text = rights.can_edit_text;
                if can_edit_text {
                    send::announce_parsing(rt, target, source_text).await;
                }
            }
            Ok(_) => {
                crate::log::info("频道帖没有编辑媒体的权限，跳过解析");
                return;
            }
            Err(error) => {
                crate::log::warn(&format!("查频道帖编辑权限失败，跳过解析：{error}"));
                return;
            }
        }
    }
    if let Err(error) = try_reply(rt, target, link).await {
        if target.is_edit() {
            if can_edit_text {
                send::announce_error(rt, target, &error).await;
            } else {
                crate::log::warn(&format!("频道帖没有编辑文本的权限，错误只写日志：{error}"));
            }
        } else {
            // 错误提示也不引用原消息（用户要求去掉引用行为）。
            rt.say(target.chat_id, &format!("❌ {error}")).await;
        }
    }
}

/// [`reply_with_video`] 的主体：缓存命中 → 直接重发；否则完整解析 + 发送 + 写缓存。
async fn try_reply(rt: &Runtime, target: &Target, link: &str) -> Result<(), String> {
    if rt.cookie.is_empty() {
        return Err(
            "插件没读到 cookie：在 fuyubot.toml 的 [plugin.fuyubot.douyin] 段配              cookies = \"…\"（旧 cookies.json 仍兼容）再重启"
                .to_owned(),
        );
    }
    crate::log::debug(&format!("收到链接 {}", crate::log::redact_url(link)));

    // ① 先认作品 id（本地解析；短链才 HEAD 一次看 302）。认不出来就直接走完整解析，
    //    让 parse 层报出更有用的原因（汽水 / 坏链 / 风控页）。
    let detail_id = match parse::detail_id_of(&rt.http, link).await {
        Ok(id) => id,
        Err(error) => {
            crate::log::debug(&format!("认不出作品 id（{error}），走完整解析"));
            return reply_full(rt, target, link, None).await;
        }
    };
    let key = crate::cache::key(&["douyin", &detail_id]);

    // ② 缓存命中：跳过分享页解析与所有媒体下载。
    if let Some(doc) = rt.cache_get(&key).await {
        match Cached::parse(&doc) {
            Some(cached) => {
                crate::log::debug(&format!(
                    "缓存命中 {key}（跳过解析与下载，直接按文件 id 发送）"
                ));
                match send::send_cached(rt, &cached, target).await {
                    Ok(_) => return Ok(()),
                    Err(error) => {
                        // 文件 id 失效（TDLib 侧文件被清掉 / 换了账号）：删缓存、回落完整解析。
                        // 用户看不到这条错误——他只会收到正常解析出来的那条。
                        crate::log::warn(&format!(
                            "缓存命中的文件 id 发送失败（{error}），删缓存后回落完整解析"
                        ));
                        rt.cache_remove(&key).await;
                    }
                }
            }
            None => {
                crate::log::warn(&format!("缓存文档形状不对（{key}），删掉后走完整解析"));
                rt.cache_remove(&key).await;
            }
        }
    }

    // ③ 未命中 / 命中但失效：完整解析 + 发送，成功后写缓存。
    reply_full(rt, target, link, Some(&key)).await
}

/// 完整解析 → 发送 → （可选）写缓存。返回最终消息给调用方用于写缓存。
async fn reply_full(
    rt: &Runtime,
    target: &Target,
    link: &str,
    cache_key: Option<&str>,
) -> Result<(), String> {
    let info = parse::get_douyin(&rt.http, link, &rt.cookie)
        .await
        .map_err(|error| error.to_string())?;
    let caption = send::build_caption(&info);
    let messages = send::send_video(rt, &info, target)
        .await
        .map_err(|error| error.to_string())?;

    if let Some(key) = cache_key
        && let Some(doc) = cached::from_sent(&info, &Value::Array(messages), &caption)
    {
        // 写缓存失败不能影响发送结果：cache_put 内部只记日志。
        rt.cache_put(key, &doc).await;
    }
    Ok(())
}

/// `/dy` 在宿主命令表里的登记入参。
///
/// `scopes` 写 `private + group + channel`：频道里命令消息本身就是那条帖子，
/// 解析结果用 `editMessageMedia` 编辑回去（与 TS 版一致）。
fn registration_payload() -> serde_json::Value {
    serde_json::json!({
        "name": COMMAND,
        "description": COMMAND_DESCRIPTION,
        "role": "user",
        "scopes": ["private", "group", "channel"],
    })
}

/// 把 `/dy` 登记进宿主命令表。
///
/// 走宿主预置的 `fuyubot.host.commands`（`call_service`），入参
/// `{name, description, role, scopes}`，回 `{"ok":true,…}`。`/dy` 是普通用户命令、
/// 私聊和群都能用；频道里没有解析场景，不声明 `channel`。
///
/// 失败只记 Warn、不返回 Err：`/dy` 本身能正常工作，只是 `/help` 里少一行说明，
/// 不该因为这个不让插件激活。
fn register_help(host: HostApi) {
    let input = registration_payload().to_string();

    match host.call_service(HOST_COMMANDS, "register", &input) {
        Ok(reply) => {
            // 记住这次答复：`{"ok":true,…}` 与 `{"error":"…"}` 都是 200 式返回，
            // 服务方不会用错误通道。
            let ok = serde_json::from_str::<serde_json::Value>(&reply)
                .ok()
                .and_then(|value| value.get("ok").and_then(|v| v.as_bool()))
                .unwrap_or(false);
            if ok {
                crate::log::info(&format!("/{COMMAND} 已登记进宿主命令表"));
            } else {
                crate::log::warn(&format!("/{COMMAND} 登记进宿主命令表被拒：{reply}"));
            }
        }
        Err(error) => {
            crate::log::warn(&format!(
                "/{COMMAND} 没能登记进宿主命令表（{HOST_COMMANDS}）：{error}"
            ));
        }
    }
}

/// 一条活着的「扫链接」订阅：**drop 就退订**。
///
/// 与 `CommandSubscription` 同构（那份是 simplify 层给的，但它是按命令解包的，
/// 这里要看的是任意文本）。
pub struct ScanSubscription {
    host: HostApi,
    handle: SubscriptionHandle,
    _handler: Box<Box<MessageFn>>,
}

impl Drop for ScanSubscription {
    fn drop(&mut self) {
        if self.handle.is_valid() {
            self.host.off(self.handle);
        }
    }
}

/// 扫链接回调的签名：`&str` 是整条消息文本。
type MessageFn = dyn Fn(&Message) + Send + Sync;

/// 一条待判断的消息（只带用得上的那几个字段）。
#[derive(Debug, Clone)]
struct Message {
    chat_id: i64,
    message_id: i64,
    text: String,
    is_outgoing: bool,
    /// 发送者是频道（`messageSenderChat`）——群里匿名频道马甲也满足这一点。
    sender_is_chat: bool,
    /// 这条消息是转发的（`forward_info` 存在，来源可能被隐藏）。
    is_forwarded: bool,
    /// 真正的频道帖：TDLib `is_channel_post` 为真（见 [`channel_post_of`]）。
    is_channel_post: bool,
}

/// 自动识别要不要处理这条消息；返回「发到哪儿 + 链接」。
///
/// 过滤顺序：
/// 1. 自己发的不解析（否则机器人自己贴的链接会被再解析一遍，无限套娃）；
///    频道帖例外：在自己的频道里发链接就是要插件把原帖编辑成视频；
/// 2. `/…` 开头的是别的插件的命令，不抢；
/// 3. **群聊里由频道身份发出、而且是转发的消息**：发送人是 `messageSenderChat`
///    且 `forward_info` 存在。这类消息不是群友自己写的，解析它会替别人的转发链
///    自动产出内容。频道帖子是例外——频道本身要解析，只是走编辑。
fn auto_target(message: &Message) -> Option<(Target, String)> {
    // 自己发的不解析（否则机器人自己贴的链接会被再解析一遍，无限套娃）；
    // 频道帖例外——在自己的频道里发链接，本来就是要让插件把这条帖子编辑成视频。
    // 编辑不会产生新的 `updateNewMessage`，不存在套娃。
    if message.is_outgoing && !message.is_channel_post {
        return None;
    }
    if message.text.trim_start().starts_with('/') {
        return None;
    }
    if !message.is_channel_post && message.sender_is_chat && message.is_forwarded {
        return None;
    }
    let link = first_douyin_url(&message.text)?;
    // 频道帖子：机器人通常没有发言权，只有编辑自己那条的权限——TS 版就是这么
    // 绕过去的（`isChannel` 为真的分支走 `editMessageMedia`）。普通消息只带
    // chat_id：不引用原消息（用户要求），也不需要 getChat 判群聊。
    let target = if message.is_channel_post {
        Target::edit(message.chat_id, message.message_id)
    } else {
        Target::send(message.chat_id)
    };
    Some((target, link))
}

/// 订阅 `updateNewMessage`，把「含抖音链接的**非命令**消息」交给处理函数。
fn scan_douyin_links(host: HostApi, runtime: std::sync::Arc<Runtime>) -> ScanSubscription {
    let mut handler: Box<Box<MessageFn>> = Box::new(Box::new(move |message: &Message| {
        let Some((target, link)) = auto_target(message) else {
            return;
        };
        // 频道里占位提示要带原文；普通会话用不上，但一起带过去省得再判一次。
        let source_text = message.text.clone();
        // 回调线程只排队：真正的解析/下载/发送在这个任务里。
        let runtime = std::sync::Arc::clone(&runtime);
        runtime.submit(move |rt| async move {
            reply_with_video(&rt, &target, &link, &source_text).await
        });
    }));
    // 与 `on_command` 同一套 user_data 约定：递进去的是**内层 Box 的地址**
    // （只解一层：`Box<dyn Fn>` 本身是个瘦指针，外层 Box 活着它就稳定）。
    // 解两层（`**handler`）拿到的是闭包本体，回调里再按 `*const Box<dyn Fn>` 转回去
    // 就读了错的地方——这处符号解引用次数差一，症状就是回调从不触发。
    let user_data = (&raw mut *handler).cast::<std::ffi::c_void>();
    let handle = host.on("updateNewMessage", None, scan_trampoline, user_data);
    ScanSubscription {
        host,
        handle,
        _handler: handler,
    }
}

/// 扫链接的 C 回调：把负载解成 [`Message`]，再交给闭包。
extern "C-unwind" fn scan_trampoline(
    user_data: *mut std::ffi::c_void,
    payload: *const std::ffi::c_char,
) -> i32 {
    if user_data.is_null() || payload.is_null() {
        return -1;
    }
    // SAFETY: `user_data` 是 `scan_douyin_links` 里那个 Box，订阅活多久它就活多久。
    let handler = unsafe { &*(user_data as *const Box<MessageFn>) };
    // SAFETY: 宿主保证 `payload` 是 NUL 结尾的临时字符串。
    let text = unsafe { std::ffi::CStr::from_ptr(payload) }.to_string_lossy();
    let Ok(update) = serde_json::from_str::<serde_json::Value>(&text) else {
        return 0;
    };
    if let Some(message) = message_of(&update) {
        handler(&message);
    }
    0
}

/// 从 `updateNewMessage` 的负载里取出一条消息；不是文本消息就 `None`。
///
/// 字段路径照真 `td_api.tl`：`updateNewMessage.message` →
/// `content:messageText` → `text:formattedText` → `text:string`。
fn message_of(update: &serde_json::Value) -> Option<Message> {
    let value = update.get("message")?;
    let content = value.get("content")?;
    if content.get("@type").and_then(|kind| kind.as_str()) != Some("messageText") {
        return None;
    }
    let text = content.get("text")?.get("text")?.as_str()?;
    let chat_id = value.get("chat_id")?.as_i64()?;
    let message_id = value.get("id")?.as_i64()?;
    let is_outgoing = value
        .get("is_outgoing")
        .and_then(|flag| flag.as_bool())
        .unwrap_or(false);
    let sender_is_chat = value
        .get("sender_id")
        .and_then(|sender| sender.get("@type"))
        .and_then(|kind| kind.as_str())
        == Some("messageSenderChat");
    // 转发消息：`forward_info` 存在即可（来源被隐藏时 `origin` 可能为 null，
    // 但「转发过」这件事仍然成立，正是要过滤的那一类）。
    let is_forwarded = value
        .get("forward_info")
        .is_some_and(|info| !info.is_null());
    Some(Message {
        chat_id,
        message_id,
        text: text.to_owned(),
        is_outgoing,
        sender_is_chat,
        is_forwarded,
        // 频道帖用 TDLib 自己的 `is_channel_post` 判：群里匿名频道马甲虽然发送者
        // 也是 `messageSenderChat`，但这个标志为 false，不能去编辑别人的消息。
        is_channel_post: channel_post_of(value),
    })
}

/// 文本里的第一个 http(s) 链接（`/dy` 的**兜底**用，见 [`pick_for_command`]）。
fn first_url(text: &str) -> Option<String> {
    extract_url(text, |_| true)
}

/// 一条链接该由谁处理。
///
/// 为什么需要这个枚举而不是一个 `bool`：三种结局的**反应不一样**——
/// 抖音要解析、汽水要**解释一句为什么不做**、别的域名是「压根不是我们的」。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Route {
    /// 抖音作品：本插件处理。
    Douyin,
    /// 汽水音乐（`qishui.douyin.com` / `qishui.com` / `music.douyin.com/qishui/…`）：
    /// 归 `fuyubot.music`。
    Qishui,
    /// 其它域名：不是抖音的东西。
    Foreign,
}

/// 这条 URL 归谁。
///
/// ⚠️ 汽水音乐虽然也在 `*.douyin.com` 下，但它的分享链接**没有作品 id**：
/// 短链 `qishui.douyin.com/s/…` 302 到 `music.douyin.com/qishui/share/{track,ugc_video}?…`，
/// 路径里既没有 `video` / `note` / `slides` 段，也没有纯数字 id。
/// 抖音插件抢过来只会 HEAD 一趟、认不出作品、重试两次、然后发一条 ❌——
/// 而音乐插件那边其实已经把歌发出去了。所以这里必须让出去。
fn route_of(url: &str) -> Route {
    if is_qishui_url(url) {
        return Route::Qishui;
    }
    match host_of(url) {
        Some(host) if is_douyin_host(&host) => Route::Douyin,
        _ => Route::Foreign,
    }
}

/// 这条 URL 是不是汽水音乐的。
///
/// 两处入口（命令层 / 解析层）共用这一条判据，免得两边规则漂移：
/// 命令层少判一种就漏给抖音，解析层少判一种就在别处炸。
pub fn is_qishui_url(url: &str) -> bool {
    match host_of(url) {
        Some(host) => is_qishui_host(&host) || (host == "music.douyin.com" && is_qishui_path(url)),
        None => false,
    }
}

/// 汽水音乐自己的域名（`qishui.com` 与 `qishui.douyin.com` 及其子域）。
fn is_qishui_host(host: &str) -> bool {
    let bare = |base: &str| host == base || host.ends_with(&format!(".{base}"));
    bare("qishui.com") || bare("qishui.douyin.com")
}

/// `music.douyin.com` 上是不是汽水音乐的路径。
///
/// 只认 `/qishui/`：同一台主机上还有别的分享页，别把那些也一起让出去。
fn is_qishui_path(url: &str) -> bool {
    url.to_ascii_lowercase().contains("/qishui/")
}

/// 这条 URL 是不是该由抖音插件处理。
fn is_douyin_link(url: &str) -> bool {
    route_of(url) == Route::Douyin
}

/// 文本里的第一个抖音链接（自动识别用）。
///
/// 汽水音乐的链接会被 [`route_of`] 判成 [`Route::Qishui`]，这里自然跳过。
fn first_douyin_url(text: &str) -> Option<String> {
    extract_url(text, is_douyin_link)
}

/// `/dy` 的参数里挑哪条链接。
///
/// 优先级：
/// 1. **抖音链接优先**——文本里同时有汽水和抖音时（转发的配文常常两个都带），
///    要解析的是抖音那条，不能因为汽水链接排在前面就把它当成拒绝理由；
/// 2. 没有抖音链接但有汽水的 → 返回 [`Selection::Qishui`]，由调用方解释清楚；
/// 3. 都没有 → 退回「第一个链接」，保持 `/dy <任意链接>` 的老行为不变
///    （别的域名走到解析层会自己报错，不必在这里另加一套拒绝）。
fn pick_for_command(text: &str) -> Selection {
    if let Some(link) = first_douyin_url(text) {
        return Selection::Douyin(link);
    }
    if let Some(link) = extract_url(text, |url| route_of(url) == Route::Qishui) {
        return Selection::Qishui(link);
    }
    match first_url(text) {
        Some(link) => Selection::Douyin(link),
        None => Selection::None,
    }
}

/// [`pick_for_command`] 的结果。
///
/// `Debug` 是给测试的 `panic!("… {other:?}")` 用的：断言失败时要能直接看出
/// 实际拿到的是哪种结局，而不是一句「不相等」。
#[derive(Debug)]
enum Selection {
    /// 交给本插件解析（抖音链接，或旧行为下的任意链接）。
    Douyin(String),
    /// 是汽水音乐链接，本插件不该碰。
    Qishui(String),
    /// 文本里没有链接。
    None,
}

/// 抽出第一个链接；`accept` 决定要不要它。
///
/// 按空白切词而不是跑正则：分享链接常常被中文标点和括号包住
/// （`看看这个：https://v.douyin.com/xx/。`），只削尾部的标点就够了。
fn extract_url(text: &str, accept: impl Fn(&str) -> bool) -> Option<String> {
    const TRAILING: &[char] = &[
        ')', '，', ',', '。', '.', '！', '!', '？', '?', ':', '：', ';', '；', '"', '"', '」',
        '』', '】', '、',
    ];
    for candidate in text.split_whitespace() {
        // URL 不一定在词首：`看看这个：https://v.douyin.com/xx/。` 里它被中文标点
        // 顶在中间，所以先从第一个 `http` 切进去，再削掉尾部标点。
        let Some(start) = candidate.find("http") else {
            continue;
        };
        let trimmed = candidate[start..].trim_end_matches(TRAILING);
        if accept(trimmed) {
            return Some(trimmed.to_owned());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_urls_are_picked_up_from_the_arguments() {
        assert_eq!(
            first_url("/dy https://v.douyin.com/abc/").as_deref(),
            Some("https://v.douyin.com/abc/")
        );
        // 命令名本身不是链接，不能把它当候选
        assert_eq!(first_url("no url here"), None);
    }

    /// 分享链接常被中文标点包住：`看看这个：https://v.douyin.com/xx/。`
    #[test]
    fn trailing_punctuation_is_stripped() {
        assert_eq!(
            first_douyin_url("看看这个：https://v.douyin.com/xx/。").as_deref(),
            Some("https://v.douyin.com/xx/")
        );
        assert_eq!(
            first_douyin_url("(https://www.douyin.com/video/123)").as_deref(),
            Some("https://www.douyin.com/video/123")
        );
    }

    #[test]
    fn only_douyin_hosts_trigger_automatic_parsing() {
        assert!(first_douyin_url("https://v.douyin.com/a/").is_some());
        assert!(first_douyin_url("https://www.douyin.com/video/1").is_some());
        assert!(first_douyin_url("https://www.iesdouyin.com/share/video/1/").is_some());
        // 别的站不能误触发
        assert!(first_douyin_url("https://www.youtube.com/watch?v=1").is_none());
        assert!(first_douyin_url("https://v.kuaishou.com/a/").is_none());
        assert!(first_douyin_url("没有链接").is_none());
    }

    /// `douyin.com` 当成**子串**去 contains 会误伤：下面的链接 host 是
    /// `douyin.com.evil.test` 和 `example.com`，都不是抖音。
    #[test]
    fn lookalike_domains_do_not_match() {
        assert!(
            first_douyin_url("https://douyin.com.evil.test/x").is_none(),
            "别家域名不能替它触发解析"
        );
        assert!(
            first_douyin_url("https://example.com/douyin").is_none(),
            "路径里有 douyin 也不算"
        );
        assert!(first_douyin_url("https://notdouyin.com/x").is_none());
        assert!(first_douyin_url("https://bilibili.com/video/1").is_none());
    }

    #[test]
    fn host_extraction_handles_scheme_port_and_case() {
        assert_eq!(
            host_of("https://v.douyin.com/abc/").as_deref(),
            Some("v.douyin.com")
        );
        assert_eq!(
            host_of("HTTP://WWW.DOUYIN.COM/x").as_deref(),
            Some("www.douyin.com")
        );
        assert_eq!(
            host_of("https://douyin.com:443/x").as_deref(),
            Some("douyin.com")
        );
        assert_eq!(host_of("https://douyin.com").as_deref(), Some("douyin.com"));
        assert_eq!(
            host_of("https://douyin.com?a=1").as_deref(),
            Some("douyin.com")
        );
        assert_eq!(
            host_of("https://u:p@douyin.com/x").as_deref(),
            Some("douyin.com")
        );
        assert!(is_douyin_host("douyin.com"));
        assert!(is_douyin_host("v.douyin.com"));
        assert!(is_douyin_host("iesdouyin.com"));
        assert!(!is_douyin_host("evil-douyin.com"));
        assert!(!is_douyin_host("douyin.com.evil.test"));

        // 汽水音乐的链接虽然也在 *.douyin.com 下，但归 fuyubot.music 管。
        assert!(!is_douyin_link("https://qishui.douyin.com/s/iXvvC5YV/"));
        assert!(!is_douyin_link(
            "https://music.douyin.com/qishui/share/ugc_video?ugc_video_id=1"
        ));
        assert!(!is_douyin_link(
            "https://music.douyin.com/qishui/share/track?track_id=1"
        ));
        // 同一台主机上的非汽水路径仍然归抖音。
        assert!(is_douyin_link("https://music.douyin.com/other/video/123"));
        assert!(is_douyin_link("https://v.douyin.com/abc/"));

        // 自动识别：混合文本里只挑抖音链接，不能把汽水链接抢过来。
        assert!(first_douyin_url("看看 https://qishui.douyin.com/s/iXvvC5YV/").is_none());
        assert_eq!(
            first_douyin_url(
                "两个链接 https://v.douyin.com/abc/ 和 https://qishui.douyin.com/s/x/"
            )
            .as_deref(),
            Some("https://v.douyin.com/abc/")
        );
    }

    /// 汽水音乐的三种链接都要认出来（短链、落地页视频、落地页曲目）。
    ///
    /// ⚠️ `qishui.com` 是**独立域名**，不是 `douyin.com` 的子域——
    /// 只按 `*.douyin.com` 判会漏掉它，用户看到的症状就是「汽水链接还是被抖音插件抢走」。
    #[test]
    fn qishui_links_are_routed_away_from_douyin() {
        for url in [
            "https://qishui.douyin.com/s/iXvvC5YV/",
            "https://qishui.douyin.com/s/iXvvC5YV",
            "https://www.qishui.com/track/7304719759323564095",
            "https://qishui.com/track/1",
            "https://music.douyin.com/qishui/share/track?track_id=1",
            "https://music.douyin.com/qishui/share/ugc_video?ugc_video_id=1",
            // 大小写不敏感：用户从别处粘过来的链接可能被转成大写
            "https://MUSIC.douyin.com/QISHUI/share/track?track_id=1",
        ] {
            assert_eq!(route_of(url), Route::Qishui, "{url} 应归音乐插件");
        }
    }

    /// 判据只该命中汽水自己，不能把抖音或同主机的其它路径一起让出去。
    #[test]
    fn qishui_matching_does_not_swallow_other_douyin_links() {
        // `music.douyin.com` 上非 `/qishui/` 的路径仍归抖音
        assert_eq!(
            route_of("https://music.douyin.com/other/video/123"),
            Route::Douyin
        );
        // 普通的抖音作品
        assert_eq!(route_of("https://v.douyin.com/abc/"), Route::Douyin);
        assert_eq!(
            route_of("https://www.douyin.com/video/7401234567890123456"),
            Route::Douyin
        );
        // 带 qishui 字样的**别的域名**不算（防止 `qishui.com.evil.test` 那种）
        assert_eq!(
            route_of("https://qishui.com.evil.test/x"),
            Route::Foreign,
            "把目标域名当前缀不能命中"
        );
        assert_eq!(route_of("https://notqishui.com/x"), Route::Foreign);
        assert_eq!(route_of("https://bilibili.com/video/1"), Route::Foreign);
    }

    /// `/dy` 里挑链接的三条规则。
    #[test]
    fn the_command_picks_the_douyin_link_first() {
        // ① 只有抖音 → 解析
        match pick_for_command("https://v.douyin.com/abc/") {
            Selection::Douyin(link) => assert_eq!(link, "https://v.douyin.com/abc/"),
            other => panic!("应当交给抖音解析，实际 {other:?}"),
        }

        // ② 只有汽水 → 让出去（并且真的认出来是哪条，日志里要用）
        match pick_for_command("听听这个 https://qishui.douyin.com/s/iXvvC5YV/") {
            Selection::Qishui(link) => assert_eq!(link, "https://qishui.douyin.com/s/iXvvC5YV/"),
            other => panic!("应当让给音乐插件，实际 {other:?}"),
        }

        // ③ 两个都在、汽水排在**前面** → 仍然解析抖音那条。
        // 这是最容易写错的一条：先按顺序取第一个链接就会把抖音漏掉，
        // 用户看到的是「我明明带了抖音链接，它却说这是汽水的」。
        match pick_for_command(
            "https://qishui.douyin.com/s/x/ 和 https://www.douyin.com/video/7401234567890123456",
        ) {
            Selection::Douyin(link) => {
                assert_eq!(link, "https://www.douyin.com/video/7401234567890123456")
            }
            other => panic!("抖音链接优先，实际 {other:?}"),
        }

        // ④ 没有链接
        assert!(matches!(pick_for_command("帮我解析一下"), Selection::None));

        // ⑤ 别的域名：保持老行为（交给解析层自己报错），不在这里新加拒绝
        match pick_for_command("https://example.com/x") {
            Selection::Douyin(link) => assert_eq!(link, "https://example.com/x"),
            other => panic!("非抖音域名应维持旧行为，实际 {other:?}"),
        }
    }

    /// `/dy <汽水链接>` 的文案必须把「该找谁」说清楚。
    ///
    /// 只说「解析失败」会让用户反复重发——链接本身是好的，只是不归这个插件管。
    #[test]
    fn the_qishui_rejection_message_says_where_to_go() {
        let text = QISHUI_REJECTION;
        assert!(text.contains("汽水"), "{text}");
        assert!(text.contains("音乐"), "要指出归音乐插件管：{text}");
        assert!(
            text.contains("直接") || text.contains("发出来"),
            "要告诉用户该怎么办：{text}"
        );
        // 别把命令名写成 `/soda` 这种用户用不上的形式——自动识别就够了。
        assert!(!text.contains("/soda"), "不该让用户去敲命令：{text}");
    }

    /// 插件目录来自 `plugin_start` 的 config；缺 / 坏 JSON 时退回 `plugins/`。
    /// （实现在 runtime 里——`start` 要用它读 cookie、定下载目录。）
    #[test]
    fn plugins_dir_comes_from_the_config_or_defaults() {
        use std::path::PathBuf;
        let config = r#"{"plugins_dir":"D:/bots/demo/plugins"}"#;
        assert_eq!(
            crate::runtime::plugins_dir_from(config),
            PathBuf::from("D:/bots/demo/plugins")
        );
        // 配置缺 / 不是 JSON → 退回 plugins/
        assert_eq!(
            crate::runtime::plugins_dir_from(""),
            PathBuf::from("plugins")
        );
        assert_eq!(
            crate::runtime::plugins_dir_from("not json"),
            PathBuf::from("plugins")
        );
        assert_eq!(
            crate::runtime::plugins_dir_from("{}"),
            PathBuf::from("plugins")
        );
    }

    #[test]
    fn the_command_name_is_stable() {
        // 改了它，用户的 `/dy` 就失灵了——钉住。
        assert_eq!(COMMAND, "dy");
    }

    /// 《更新 → Message》这一步单独钉住：字段路径错一个字，自动识别就整个哑掉。
    #[test]
    fn update_new_message_is_read_into_a_message() {
        // chat_id 是 int53：不能写成裸字面量（`json!` 会把它推成 i32 然后溢出），
        // 这样写才能保证它是 i64。
        let chat_id: i64 = -1001234567890;
        let update = serde_json::json!({
            "@type": "updateNewMessage",
            "message": {
                "@type": "message",
                "id": 22,
                "chat_id": chat_id,
                "is_outgoing": false,
                "sender_id": { "@type": "messageSenderUser", "user_id": 42 },
                "content": {
                    "@type": "messageText",
                    "text": { "@type": "formattedText", "text": "https://v.douyin.com/x/", "entities": [] },
                },
            },
        });
        let message = message_of(&update).expect("文本消息应当解得出");
        assert_eq!(message.chat_id, chat_id);
        assert_eq!(message.message_id, 22);
        assert_eq!(message.text, "https://v.douyin.com/x/");
        assert!(!message.is_outgoing);
        assert!(!message.is_channel_post);
    }

    /// 频道判定用 TDLib 的 `is_channel_post`；群里「匿名频道马甲」的发送者也是
    /// `messageSenderChat`，但 `is_channel_post` 为 false，不能走编辑。
    #[test]
    fn channel_posts_are_detected_by_the_tdlib_flag() {
        let update = |chat_id: i64, is_channel_post: bool, sender: serde_json::Value| {
            serde_json::json!({
                "@type": "updateNewMessage",
                "message": {
                    "@type": "message",
                    "id": 5,
                    "chat_id": chat_id,
                    "is_outgoing": false,
                    "is_channel_post": is_channel_post,
                    "sender_id": sender,
                    "content": {
                        "@type": "messageText",
                        "text": { "@type": "formattedText", "text": "https://v.douyin.com/x/", "entities": [] },
                    },
                },
            })
        };
        let post = message_of(&update(
            -100777,
            true,
            serde_json::json!({ "@type": "messageSenderChat", "chat_id": -100777 }),
        ))
        .unwrap();
        assert!(post.is_channel_post, "频道帖要认出来");
        assert!(post.sender_is_chat);
        assert!(!post.is_forwarded);

        // 群里匿名频道马甲：发送者是关联频道 -100999，当前会话是 -100888。
        let anonymous = message_of(&update(
            -100888,
            false,
            serde_json::json!({ "@type": "messageSenderChat", "chat_id": -100999 }),
        ))
        .unwrap();
        assert!(anonymous.sender_is_chat);
        assert!(
            !anonymous.is_channel_post,
            "群里的匿名频道马甲不是频道帖，不能去编辑别人的消息"
        );

        let user = message_of(&update(
            -100777,
            false,
            serde_json::json!({ "@type": "messageSenderUser", "user_id": 1 }),
        ))
        .unwrap();
        assert!(!user.sender_is_chat);
        assert!(!user.is_channel_post, "普通群成员不是频道帖");
    }

    /// 自动识别的过滤：群里「发送人是频道 + 转发」的消息直接跳过；普通转发照常。
    #[test]
    fn channel_identity_forwards_are_skipped_in_groups() {
        let message = |chat_id: i64,
                       is_channel_post: bool,
                       sender: serde_json::Value,
                       forward_info: Option<serde_json::Value>| {
            let mut update = serde_json::json!({
                "@type": "updateNewMessage",
                "message": {
                    "@type": "message",
                    "id": 7,
                    "chat_id": chat_id,
                    "is_outgoing": false,
                    "is_channel_post": is_channel_post,
                    "sender_id": sender,
                    "content": {
                        "@type": "messageText",
                        "text": { "@type": "formattedText", "text": "https://v.douyin.com/x/", "entities": [] },
                    },
                },
            });
            if let Some(forward) = forward_info {
                update["message"]["forward_info"] = forward;
            }
            message_of(&update).unwrap()
        };

        let channel_persona = serde_json::json!({
            "@type": "messageSenderChat",
            "chat_id": -100999_i64
        });
        let forwarded = serde_json::json!({
            "origin": { "@type": "messageOriginChannel", "chat_id": -100999_i64, "message_id": 1 }
        });
        let user = serde_json::json!({ "@type": "messageSenderUser", "user_id": 42 });

        // 群（不是频道帖）+ 频道身份 + 转发 → 跳过
        assert!(
            auto_target(&message(
                -100888,
                false,
                channel_persona.clone(),
                Some(forwarded.clone())
            ))
            .is_none(),
            "群里频道身份发出的转发消息不该自动解析"
        );
        // 频道身份但**不是转发** → 照常解析（只是普通新消息）
        assert!(
            auto_target(&message(-100888, false, channel_persona.clone(), None)).is_some(),
            "群里频道身份的普通发言仍然解析"
        );
        // 普通用户转发 → 照常解析
        assert!(
            auto_target(&message(
                -100888,
                false,
                user.clone(),
                Some(forwarded.clone())
            ))
            .is_some(),
            "普通群成员转发的抖音链接仍然解析"
        );
        // 真正的频道帖即使是转发也照常解析，并走编辑
        let channel = serde_json::json!({
            "@type": "messageSenderChat",
            "chat_id": -100777_i64
        });
        let (target, _) = auto_target(&message(-100777, true, channel, Some(forwarded))).unwrap();
        assert!(target.is_edit(), "频道帖走编辑而不是新发");
    }

    /// 自己发的频道帖仍要解析（自家频道里发链接 → 编辑成视频）；自己发的群消息不解析。
    #[test]
    fn outgoing_channel_posts_are_still_recognized() {
        let update = |chat_id: i64, is_channel_post: bool, sender: serde_json::Value| {
            serde_json::json!({
                "@type": "updateNewMessage",
                "message": {
                    "@type": "message",
                    "id": 8,
                    "chat_id": chat_id,
                    "is_outgoing": true,
                    "is_channel_post": is_channel_post,
                    "sender_id": sender,
                    "content": {
                        "@type": "messageText",
                        "text": { "@type": "formattedText", "text": "https://v.douyin.com/x/", "entities": [] },
                    },
                },
            })
        };

        let channel = message_of(&update(
            -100777,
            true,
            serde_json::json!({ "@type": "messageSenderChat", "chat_id": -100777 }),
        ))
        .unwrap();
        let (target, _) = auto_target(&channel).expect("自己发的频道帖也要解析");
        assert!(target.is_edit(), "并且走编辑");

        let group = message_of(&update(
            -100888,
            false,
            serde_json::json!({ "@type": "messageSenderUser", "user_id": 42 }),
        ))
        .unwrap();
        assert!(
            auto_target(&group).is_none(),
            "自己发的群消息仍然跳过，避免套娃"
        );
    }

    #[test]
    fn non_text_messages_are_ignored() {
        let update = serde_json::json!({
            "@type": "updateNewMessage",
            "message": { "@type": "message", "id": 1, "chat_id": 1,
                "content": { "@type": "messagePhoto", "photo": {} } },
        });
        assert!(message_of(&update).is_none());
        assert!(message_of(&serde_json::json!({})).is_none());
    }

    /// `user_data` 的取址层数曾经写错过：写成 `&raw mut **handler`（解两层，指向闭包
    /// 本体），回调里再按 `*const Box<dyn Fn>` 转回去就读错了地方——症状是
    /// **自动识别那条订阅从来不触发**，而且不报错、不崩。
    ///
    /// 所以这里钉的不是地址数值，而是**这条指针路径真的能调到闭包**：
    /// 按 `scan_douyin_links` 的方式取址、按 `scan_trampoline` 的方式转回来并调用，
    /// 副作用必须看得见。（比地址不可靠——零大小闭包的地址可能是对齐占位值。）
    #[test]
    fn user_data_round_trip_reaches_the_closure() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        static CALLS: AtomicUsize = AtomicUsize::new(0);
        let mut owned: Box<Box<MessageFn>> = Box::new(Box::new(|_message: &Message| {
            CALLS.fetch_add(1, Ordering::SeqCst);
        }));

        // ① 取址（与 `scan_douyin_links` 里一致：只解一层）
        let user_data = (&raw mut *owned).cast::<std::ffi::c_void>();
        // ② 转回来并调用（与 `scan_trampoline` 里一致）
        let back = user_data as *const Box<MessageFn>;
        let handler = unsafe { &*back };
        handler(&Message {
            chat_id: 1,
            message_id: 1,
            text: "x".to_owned(),
            is_outgoing: false,
            sender_is_chat: false,
            is_forwarded: false,
            is_channel_post: false,
        });

        assert_eq!(CALLS.load(Ordering::SeqCst), 1, "回调必须真的调到闭包");
    }

    /// 登记入参形状必须和宿主 `register` 对得上：`name` 非空，`role` / `scopes`
    /// 决定闸门与 `/help` 的可见性。
    #[test]
    fn registration_payload_has_the_expected_shape() {
        let input = registration_payload();
        assert_eq!(input["name"], "dy");
        assert!(
            input["description"].as_str().is_some_and(|d| !d.is_empty()),
            "说明不能为空，否则 /help 里会显示一条没有解释的命令"
        );
        assert_eq!(input["role"], "user");
        assert_eq!(
            input["scopes"],
            serde_json::json!(["private", "group", "channel"]),
            "频道里 /dy 要能用，且按编辑模式处理"
        );
    }

    /// 服务名是跨仓的**字符串约定**，改了它就等于换了协议，所以钉住。
    #[test]
    fn the_host_command_table_name_is_pinned() {
        assert_eq!(HOST_COMMANDS, "fuyubot.host.commands");
    }

    /// 软依赖而不是硬依赖：没有 SQL 插件时 `/dy` 照样要能用。
    #[test]
    fn sql_is_a_soft_dependency_not_a_hard_one() {
        assert_eq!(
            <DouyinPlugin as Plugin>::soft_inject(),
            vec![crate::cache::SERVICE.to_owned()],
            "只有 SQL 服务需要等；命令表由宿主预置"
        );
        assert!(
            <DouyinPlugin as Plugin>::inject().is_empty(),
            "不能是硬依赖：缺 SQL 时插件也要激活"
        );
        assert_eq!(crate::cache::SERVICE, "fuyubot.sqlite");
    }
}
