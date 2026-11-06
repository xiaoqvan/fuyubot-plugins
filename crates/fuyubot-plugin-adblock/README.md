# fuyubot-plugin-adblock

FuyuBot 的广告拦截插件：把 TS 框架里的 `plugins/adblock` 重写成 **Rust cdylib 插件**。

一条消息进来，按「关键词 / 文件 / 贴纸 / 转发来源 / 用户名 / 资料」逐项匹配；命中就
撤回消息、禁言或封禁，并把处置记录写进群绑定的日志频道。没命中的可疑消息（引用、
联系人、带全局触发词、入群 24h 内改过资料的人）再送 **AI 深度审核**，判定为广告后
自动把关键词 / 全文 / 名称 / 简介 / 转发来源 / 贴纸集写回黑名单——下一次同样的广告
就不用再过 AI 了。

## 功能

| 入口 | 说明 |
| --- | --- |
| 消息检测 | 文本（含富消息）、图片、视频、联系人、贴纸；广告关键词组按 **AND** 匹配 |
| 处置 | 撤回触发消息 + 该用户 48 小时内的历史消息；按群配置 **禁言**（默认）或 **封禁** |
| 通知 | 群内提示（可配置自删延时）、群绑定的日志频道（带 `unban` / `unmute` / `Ban` 按钮）、可选的全局审计频道 |
| 转发黑名单 | 来自黑名单频道/超级群的消息一律处置；AI 判定后自动把来源加进去 |
| AI 审核 | OpenAI 兼容接口；群可自定义接口，也可由 Bot 主人给「内置接口」授权 |
| 入群检查 | 新成员按名称 / 简介 / 主页挂载频道查黑名单与 AI；高危直接封，存疑先禁言并给「重新审查」按钮 |
| 24h 监视 | 入群后改资料会触发复检；复审通过后不再因弱信号被禁言 |
| 超时兜底 | 禁言 1 小时内没通过复审 → 自动封禁（每 5 分钟扫一次） |
| 人工举报 | `/spam` 可对消息、用户、`@用户名` 做一次 AI 判定，并把结果沉淀成规则 |

## 依赖

- **`storage.mongo`（硬依赖）**：数据全部通过宿主里的 MongoDB 插件服务读写。
  插件在 `inject()` 里声明了它——mongo 插件不在时本插件**不会激活**（停在 Pending），
  而不是静默地「不拦截」。集合名与字段沿用 TS 版，**同一个库可以直接接上**。
- FuyuBot 运行时仓库（`fuyubot-abi` / `fuyubot-simplify` 只发/只存在于该仓库，见下）。

> mongo 插件就在本仓库的 [`crates/fuyubot-plugin-mongo`](../fuyubot-plugin-mongo/)；
> 本插件只认服务名 `storage.mongo` 与它的 JSON 方法约定，两个插件各自独立发布。

## 构建

```bash
# 两个仓库并排（根 Cargo.toml 用的是 ../FuyuBot 相对路径）
git clone <Fuyu-Bot>            FuyuBot
git clone <本仓库>              fuyubot-plugins

cd fuyubot-plugins
cargo build --release           # 产物：target/release/fuyubot_plugin_adblock.dll
```

换目录布局就改根 `Cargo.toml` 的 `[workspace.dependencies]` 里两行 path（也可以用绝对路径）。

### Windows 上的两个坑

1. **cargo 在受限沙箱里起不了 rustc**：这是运行环境的限制，不是仓库问题，在普通终端里正常。
2. **MSVC 工具链选错会报 `cl : Command line error D8050`**：机器上装了多套 MSVC 时，
   `vcvars64.bat` 选中的版本和 cc-rs 按版本号挑的最高版本可能不是同一套。
   `ring`（rustls 的依赖）需要编 C 代码，版本对不上就会失败。**仓库根上备好了
   [`build.cmd`](../../build.cmd)，一键把环境对齐**；手工做的话配方是：

   ```bat
   call "…\VC\Auxiliary\Build\vcvars64.bat"
   set CC=…\VC\Tools\MSVC\<同版本>\bin\HostX64\x64\cl.exe
   set CRATE_CC_NO_DEFAULTS=1
   set CFLAGS_x86_64_pc_windows_msvc=/MD
   cargo build --release
   ```

   ⚠️ `CRATE_CC_NO_DEFAULTS=1` 会把 cc-rs 的 `/MD` 一起剥掉（裸 cl 默认 `/MT`），
   所以必须手动补 `/MD`，否则链接时一片 `LNK4098: defaultlib 'LIBCMT' conflicts`。bat
   call "…\VC\Auxiliary\Build\vcvars64.bat"
   set CC=…\VC\Tools\MSVC\<同版本>\bin\HostX64\x64\cl.exe
   set CRATE_CC_NO_DEFAULTS=1
   cargo build --release
   ```

不想折腾 C 工具链的话，可以把 `ureq` 的 feature 换成 `native-tls-no-default`
（Windows 走 schannel，不需要 C 编译器；本机缓存里缺 `der` 时需要联网拉一次）。

## 安装

```bash
fuyubot plugin install target/release/fuyubot_plugin_adblock.dll
# 或者直接把动态库拷进项目的 plugins/
```

前提：同一个项目里已经装了 mongo 插件（它注册 `storage.mongo`），并且
`fuyubot.toml` 的 `[plugin.fuyubot.mongo]` 指向你的 MongoDB。

## 配置

全部在 `fuyubot.toml` 的 `[plugin.fuyubot.adblock]` 下，**所有项都可选**：

```toml
[plugin.fuyubot.adblock]
# 能执行「仅 Bot 主人」命令的 TGID（/adkew /admatch /adaiadd /adaiset …）
owner_ids = [123456789]
# 「Bot 管理员」：可执行 TS 里 permission=admin 的那 6 条
# （/qadid /delad /addkw /editkw /addfwdbl /rmfwdbl）；主人天然也算管理员
admin_ids = [987654321]
# 覆盖 mongo 插件的默认库（不写就用 mongo 插件的 db）
db = "fuyubot"
# 全局封禁审计频道（可选；原 TS 版写死了一个频道，这里默认关闭）
log_channel = -1001234567890
log_topic = 1622

# 内置默认 AI 接口（可选；没有它就只在「群自定义了接口」的群里启用 AI）
[plugin.fuyubot.adblock.ai]
api_key = "sk-…"
base_url = "https://api.openai.com/v1"
model = "gpt-4o-mini"

# AI 审查总日志（可选）
[plugin.fuyubot.adblock.ai_log]
channel = -1001234567890
topic = 11740

# /spam 判定后写库的通知（可选）
[plugin.fuyubot.adblock.ai_db_notify]
channel = -1001234567890
topic = 3034
```

> ⚠️ 密钥不要提交进版本库。TS 版源码里硬编码过 API Key，这一版**一个都没搬**。

## 离线自检（不需要 Telegram 账号）

FuyuBot 的 `run --replay` 不接 TDLib，可以直接验证「插件加载 → 硬依赖 → 建索引 → 命令写库」：

```bash
# plugins/ 里放 mongo 与 adblock 两个 dll，fuyubot.toml 里给 mongo 的连接串
fuyubot run --config fuyubot.toml --replay replay.json --replay-wait 2000
```

一次实测（本机 MongoDB，`owner_ids = [777]`）：

```text
adblock 已启动（库 adblock_smoke，…）：主人 1 人；…
storage.mongo 连通（服务默认库 xqanime，集合 8 个；本插件用 adblock_smoke）
索引就绪：成功 13 条，跳过/冲突 0 条
[addkw] ads 新增 6ae6…（3 个关键词）
```

库里能看到集合与索引真的建在该库上（`message_records.createdAt` TTL 172800s、
`new_member_watch_records.expires_at` TTL 0、`adb_config.chat_id` 唯一…），
`/addkw`、`/adkew`、`/adaiset` 回放后各自落一条记录。

>TDLib 相关路径在回放里只会「记录请求、不发送」，因此消息级检测需要在真账号下验证。

## 命令

| 命令 | 场景 | 权限 |
| --- | --- | --- |
| `/adconfig` | 群内 | 群管理员 |
| `/setlogchannel <channel_id>` | 群内 | 群管理员 |
| `/whitelist add\|rm <ID\|@username\|reply>` | 群内 | 群管理员 |
| `/rmwhitelist <ID\|@username\|reply>` | 群内 | 群管理员 |
| `/spam [user_id\|@username]`（回复消息） | 群内 | 群管理员 + Bot 有管理权限 |
| `/qban <tgid\|@username> [limit]` | 任意 | 所有人 |
| `/qadid <ID>` / `/delad <ID>` | 私聊 | Bot 主人/管理员 |
| `/addkw <ads\|bio> [r18] <kw…>` / `/editkw <_id> <kw…>` | 私聊 | Bot 主人/管理员 |
| `/addfwdbl <chat_id\|@username>` / `/rmfwdbl <chat_id>` | 私聊 | Bot 主人/管理员 |
| `/adkew add\|rm\|list [关键词]` | 私聊 | Bot 主人 |
| `/admatch <文本>` / `/adaiadd <文本>` | 私聊 | Bot 主人 |
| `/adaiset add\|rm\|list [chat_id\|@群用户名]` | 私聊 | Bot 主人 |
| `/start aiConfig_<chat_id>` | 私聊 | 目标群管理员 |

命令表（名字 / 说明 / scope / permission）是 `commands/spec.rs` 里的一张静态表，
同时用于分发与 `/help` 登记：装了宿主 help 插件时 `/help` 会列出这 17 条，
没装时插件照常工作（`fuyubot.help.commands` 是软依赖）。私聊命令在群里发会被
**静默忽略**（不回复、不提示权限），与 TS 框架的 `validateCommandAccess` 一致。

群内 `/adconfig` 的「🔑 配置AI接口」按钮会跳到 Bot 私聊，走三步向导
（API Key → 后端地址 → 模型名），最后自动测一次连通性。

## 数据

集合名与 TS 版一致（包括那个拼错的 `flieId_blacklist`）：

| 集合 | 用途 |
| --- | --- |
| `adb_config` | 每个群的配置（开关、处置模式、日志频道、自定义 AI） |
| `message_records` | 48 小时消息索引（`createdAt` 有 TTL 索引），封禁时连带撤回 |
| `ban_records` | 封禁/禁言审计（`/qban`） |
| `mute_review_records` | 禁言待复审（1 小时超时 → 封禁） |
| `new_member_watch_records` | 入群 24h 监视（`expires_at` 有 TTL 索引） |
| `ads_blacklist` / `bio_blacklist` / `name_blacklist` / `username_blacklist` | 关键词 / 简介 / 名称 / 用户名 |
| `forward_blacklist` | 转发来源黑名单 |
| `flieId_blacklist` / `sticker_blacklist` / `sticker_set_blacklist` | 文件 / 单贴纸 / 贴纸集 |
| `ai_fulltext_blacklist` | AI 判定的广告全文（精确匹配） |
| `ai_default_chats` | 内置 AI 授权群 |
| `adblock_settings` | 全局 AI 触发关键词（TS 版是本地 lowdb 文件） |
| `SPAM_Log` | `/spam` 举报流水 |

索引在插件启动时幂等创建（已存在 / 冲突只记 debug）。

## 与 TS 版的有意差异

1. **密钥不硬编码**：原代码里的 API Key 已进仓库，这里只从配置读；
2. **内置 AI 的授权群**用 `ai_default_chats` 管（`/adaiset`），不再有写死的群列表；
3. **总日志频道默认关闭**：原版写死 `-1003040986800` 的固定话题，换部署就是往别人群里灌消息；
4. **「Bot 主人」= `owner_ids`**：FuyuBot 没有框架级 owner 概念；
5. **数据库走 `storage.mongo`**：不自带连接池，也不依赖宿主私有的 db API；
6. **`/setlogchannel` 更严格**：只有测试消息真的发出去了才写配置（原版失败也报成功）；
7. **链接预览自己抓**：不拖 `link-preview-js`，只取 `<title>` 与 description；
8. **HTTP 用 `ureq`（同步）**：插件不拖 async runtime，所有网络调用都在工作线程。

判定阈值、文案、按钮、超时时间、复审规则都按原逻辑保留，各模块头部有对应说明。

## 排障

- 启动日志里先看 `adblock 已启动（库 …）` 与 `storage.mongo 连通（库 …，集合 … 个）`；
- 日志级别调到 `debug` 能看到每条回调查询、跳过原因与索引冲突；
- `/admatch <文本>` 是排查「为什么这条被拦/没被拦」最快的入口；
- 群里没反应时先确认 Bot 有「删除消息 + 限制成员」权限（`/adconfig` 能看到 AI 配置状态）；
- AI 一直失败时会返回明确的 `Reason`（未配置 / HTTP 4xx / JSON 格式不符），不会静默放行。

## 目录

```text
crates/fuyubot-plugin-adblock/
  src/
    lib.rs         插件入口：inject / 订阅 / 周期任务
    runtime.rs     共享状态、线程池、调度器
    store.rs       storage.mongo 客户端 + 集合与查询（兼容旧库）
    api.rs         TDLib 调用（阻塞，只在 worker 里用）
    detect.rs      内容检测
    action.rs      处置与通知
    flow/          消息、入群、资料复检、超时扫描、消息索引、GKY 转发
    commands/      17 个命令
    callbacks/     回调按钮
    ai/            AI 审核 + 内置提示词（include_str!）
    text.rs        文本提取 + MarkdownV2 渲染
```

## License

MIT OR Apache-2.0
