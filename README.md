# fuyubot-plugins

个人的 [FuyuBot](https://github.com/CatMoeCircle/Fuyu-Bot) 插件集合：**一个仓库，八个插件**，
外加一个可以独立发布的库 crate。每个插件是一个独立的 cdylib，按
[插件分发格式](https://catmoecircle.github.io/FuyuBot-docs/docs/plugin/bundle/) 打成 bundle zip。

| 插件 | id | 说明 |
| --- | --- | --- |
| [fuyubot-plugin-sqlite](crates/fuyubot-plugin-sqlite/) | `fuyubot.sqlite` | SQLite 存储服务：参数化查询、表结构模板、事务批量写入，按插件隔离库文件 |
| [fuyubot-plugin-mongo](crates/fuyubot-plugin-mongo/) | `fuyubot.mongo` | MongoDB 存储服务（`storage.mongo`）：集合级 CRUD、聚合、索引管理 |
| [fuyubot-plugin-adblock](crates/fuyubot-plugin-adblock/) | `fuyubot.adblock` | 广告拦截：关键词 / 文件 / 贴纸 / AI 审核 / 自动点赞机器人，命中即删除、禁言或封禁；**硬依赖 `storage.mongo`** |
| [fuyubot-plugin-html2img](crates/fuyubot-plugin-html2img/) | `fuyubot.html2img` | HTML / Vue 转图片服务：把 HTML、CSS 或 Vue 模板渲染成 PNG / JPEG（默认 JPEG）并返回本地文件路径（hyper-render，纯 Rust，无 Chromium） |
| [fuyubot-plugin-help](crates/fuyubot-plugin-help/) | `fuyubot.help` | **替代框架内置 `/help`**：读取宿主命令表（`fuyubot.host.commands`）**按插件分组**渲染成图片发送（`/help text` 出文字版），按命令的 `role` / `scopes` / `hidden` 过滤；同 id；**硬依赖 `fuyubot.html2img`** |
| [fuyubot-plugin-douyin](crates/fuyubot-plugin-douyin/) | `fuyubot.douyin` | 抖音解析：`/dy` 命令 + 聊天里自动识别分享链接，发去水印视频 / 图集；**可选** `fuyubot.sqlite` 缓存文件 id |
| [fuyubot-plugin-music](crates/fuyubot-plugin-music/) | `fuyubot.music` | 网易云 / 汽水音乐：`/music163`、`/soda` 搜索与解析，按音质发音频（汽水 UGC 发视频）；**可选** `fuyubot.sqlite` 缓存文件 id |
| [fuyubot-plugin-message-log](crates/fuyubot-plugin-message-log/) | `fuyubot.message-log` | 消息日志：订阅 `updateNewMessage`，把文本 / 媒体 / 回复 / 转发 / 话题拼成一行交给宿主日志（控制台 / 日志页 / `logs/fuyubot.log`） |

库 crate（不是插件、不依赖 `fuyubot-abi`）：

| 库 | 说明 |
| --- | --- |
| [vue-template-html](crates/vue-template-html/) | 轻量级 Vue 3 模板 → HTML 编译器：预编译、无 JS 运行时、无 DOM，含 `.vue` 拆块与 props 读取 |

## 安装

TUI 插件商店里直接装，或者把发布页对应的 zip 解压到机器人的插件目录（一个插件一个目录）：

```text
plugins/
├── fuyubot.sqlite/
│   ├── plugin.json
│   └── bin/fuyubot_plugin_sqlite.dll
├── fuyubot.mongo/
│   ├── plugin.json
│   └── bin/fuyubot_plugin_mongo.dll
├── fuyubot.adblock/
│   ├── plugin.json
│   └── bin/fuyubot_plugin_adblock.dll
├── fuyubot.html2img/
│   ├── plugin.json
│   └── bin/fuyubot_plugin_html2img.dll
├── fuyubot.help/
│   ├── plugin.json
│   └── bin/fuyubot_plugin_help.dll
├── fuyubot.douyin/
│   ├── plugin.json
│   └── bin/fuyubot_plugin_douyin.dll
├── fuyubot.music/
│   ├── plugin.json
│   └── bin/fuyubot_plugin_music.dll
└── fuyubot.message-log/
    ├── plugin.json
    └── bin/fuyubot_plugin_message_log.dll
```

Windows 用 `.dll`，Linux / Android 用 `.so`，macOS 用 `.dylib`；每个插件的 zip 里已经按平台放好。

> `fuyubot.help` 与框架内置 help **同 id**（`fuyubot.help`），不能同时加载。
> 装它之前先禁用内置的平铺 `plugins/fuyubot_plugin_help.dll`
> （禁用就是改名成 `fuyubot_plugin_help.dll.disable`；别只删，`run` 会重新装回来）。
> 反向操作即可装回内置版。细节见
> [`fuyubot-plugin-help/README.md`](crates/fuyubot-plugin-help/README.md)。

## 运行时兼容性

八个插件都按 **ABI v6** 编译：`fuyubot-abi` 0.1.3+（v6 首发版本），
运行时 FuyuBot `0.1.4-rc.1` 起；命令表 / 权限过滤按 **0.1.5** 的宿主命令表
（`fuyubot.host.commands`）实现。v5 → v6 是**硬切**：
v5 宿主会拒绝加载 v6 插件，v6 宿主也拒绝加载 v5 插件（启动日志里有明确报错），
所以升级宿主时要一起换插件包。`plugin.json` 里的 `"abi"` 只是给商店看的建议值，
加载时以插件导出的版本号为准。

v6 对插件作者的两点变化：**服务调用经过宿主**——提供方用 `host.current_plugin()` 问得到
真实调用方（插件在自己的线程上调进来也算数），不用再让调用方自报名字；服务方法返回
`NULL` 表示**失败**，「成功但没有输出」要回 `""`。

命令回复也统一走宿主（`fuyubot.host.command` 服务）：插件只把 `chat_id` / `message_id`
指到命令消息，发新消息、编辑原命令（user 账户自己触发）、ephemeral（群 + Bot + ephemeral
命令）由宿主决定。本仓库里 `fuyubot.help` 与 `fuyubot.adblock` 都按这个约定回复。
**`fuyubot.douyin` / `fuyubot.music` 是例外**：它们按用户要求一律**不引用原消息**，
而宿主的命令回复会给 Bot 账户加 `reply_to`，所以这两个插件仍用 simplify 的
`send_message` / `send_message_album` 自己发。

## 插件之间

- **`fuyubot.adblock` 依赖 `storage.mongo`**（由 `fuyubot.mongo` 提供）：没装 mongo 插件时
  adblock 不会激活（停在 Pending），不会静默失效。
- **`fuyubot.sqlite` / `fuyubot.mongo` 是给别的插件用的存储服务**，自己不接收聊天消息；
  配置和调用方式见各自的 README。
- **`fuyubot.html2img` 只提供服务**（`call_service("fuyubot.html2img", "render", …)`），
  不注册命令、不主动发消息；图片怎么用由调用方决定。
- **`fuyubot.help` 硬依赖 `fuyubot.html2img`**：没装 html2img 时 help 不会激活
  （停在 Pending）。它替代内置 `/help`，但**不再自己持有命令表**：所有插件（含
  `/start`、`/plugins`、adblock）都把命令登记进宿主 `fuyubot.host.commands`，
  help 只读并按 `role` / `scopes` / `hidden` 过滤。
- **`fuyubot.douyin` / `fuyubot.music` 只有软依赖**（`optional_depends`）：
  只有 `fuyubot.sqlite` 是可选的——缺了照常工作，只影响「第二次直接按文件 id
  重发」的加速；命令表服务由宿主预置，不需要等 help。
- **`fuyubot.message-log` 不依赖任何插件**：只订阅 `updateNewMessage`、只写宿主日志，
  不注册命令、不发消息；停用 / 卸载它不会影响任何业务插件。
- 想用 Vue 模板出图，`fuyubot.html2img` 已经内置了本仓库的 `vue-template-html`。

## 测试代码放哪里

**测试代码不写在实现文件里**。每个插件在自己的 `tests/unit/` 下放测试模块，`src/` 里只留声明：

```rust
#[cfg(test)]
#[path = "../tests/unit/<模块名>.rs"]
mod tests;
```

- `tests/unit/*.rs` 是被 `#[cfg(test)]` 引入的模块，Cargo **不会**把它当成集成测试目标
  （只有 `tests/*.rs` 顶层文件才会），所以放多少都不会多出测试二进制。
- 测试仍在原模块的作用域里展开，可以 `use super::*` 直接测私有函数，但实现文件里不再有测试代码。
- 需要宿主 / 网络 / 跨 crate 的端到端测试，仍放在插件的 `tests/*.rs`（Cargo 集成测试）。
- 多模块文件用 `__` 连接路径（如 `src/commands/mod.rs` → `tests/unit/commands__mod.rs`），
  一个文件里有多个测试模块时再加模块名后缀（如 `parse__live_tests.rs`）。

## 命令一览

命令都登记在宿主的 `fuyubot.host.commands` 里，装了 `fuyubot.help` 时 `/help` 按插件分组列出
（`/help text` 出文字版）。权限档：`所有人` / `群管理`（本群管理员）/ `管理员`
（宿主 `[roles].admins`）/ `主人`（宿主 `[roles].owners`）。

| 插件 | 命令 | 一句话 |
| --- | --- | --- |
| adblock | `/adconfig` | 看 / 改本群的拦截开关（群管理） |
| adblock | `/setlogchannel <channel_id>` | 绑定本群日志频道，先发测试消息（群管理） |
| adblock | `/whitelist add\|rm <ID\|@用户名\|回复>` | 加本群白名单（群管理） |
| adblock | `/rmwhitelist <ID\|@用户名\|回复>` | 移出本群白名单（群管理） |
| adblock | `/spam [user_id\|@用户名]` | 对一条消息做一次 AI 判定（任何人，需 Bot 有管理权） |
| adblock | `/qban <tgid\|@用户名> [limit]` | 按 TGID 查封禁记录（所有人） |
| adblock | `/qadid <ID>` | 按 `_id` 查黑名单条目（管理员） |
| adblock | `/delad <ID>` | 按 `_id` 删黑名单条目（管理员） |
| adblock | `/addkw <ads\|bio> [r18] <关键词…>` | 加关键词，同组按 AND 命中（管理员） |
| adblock | `/editkw <_id> <关键词…>` | 按 `_id` 改一条关键词（管理员） |
| adblock | `/addfwdbl <chat_id\|@用户名>` | 把来源频道 / 群加进转发黑名单（管理员） |
| adblock | `/rmfwdbl <chat_id>` | 从转发黑名单里移除（管理员） |
| adblock | `/adkew add\|rm\|list [关键词]` | 管理触发 AI 审核的全局关键词（主人） |
| adblock | `/admatch <文本>` | 看一段文本命中了哪些关键词（主人） |
| adblock | `/adaiadd <文本>` | 让 AI 判一段文本，超过 3 个词就入库（主人） |
| adblock | `/adaiset add\|rm\|list [chat_id]` | 授权群使用内置 AI 接口（主人） |
| adblock | `/start aiConfig_<chat_id>` | 私聊三步向导配置本群 AI 接口（目标群管理） |
| douyin | `/dy <链接>` | 解析抖音分享链接，发视频 / 图集 |
| music | `/music163 <关键词\|链接\|ID> [音质]` | 网易云搜索或直接发音频 |
| music | `/soda <关键词\|链接\|ID> [音质]` | 汽水搜索，或发音频 / UGC 视频 |
| help | `/help` | 出图片版命令表，按插件分组 |
| help | `/help text` | 同一份数据的文字版（`/help t` 同义） |

## 配置一览

每个插件只读自己名字点分的那一段，段内全部平铺。键的含义与默认值见各自 README。

| 插件 | 配置段 | 键 |
| --- | --- | --- |
| sqlite | `[plugin.fuyubot.sqlite]` | `mode` `dir` `file` `table_prefix` `journal_mode` `busy_timeout_ms` `max_rows` `max_sql_bytes` `max_batch` `max_result_bytes` `read_only` |
| mongo | `[plugin.fuyubot.mongo]` | `uri` `db` `timeout_ms` `app_name` |
| adblock | `[plugin.fuyubot.adblock]` | `db` `log_channel` `log_topic` `ai_api_key` `ai_base_url` `ai_model` `ai_token_limit` `ai_fallback_api_key` `ai_fallback_base_url` `ai_fallback_model` `ai_log_channel` `ai_log_topic` `ai_db_channel` `ai_db_topic` |
| html2img | `[plugin.fuyubot.html2img]` | `width` `height` `scale` `auto_height` `dark` `transparent` `padding` `font_family` `temp_dir` `format` `jpeg_quality` `max_width` `max_height` `max_scale` `max_html_bytes` `max_pixels` |
| help | `[plugin.fuyubot.help]` | `title` `description` `tips` `table_desc` `image` `width` `height` `scale` `format` `max_commands` `send_timeout_ms` `fallback_text` |
| douyin | `[plugin.fuyubot.douyin]` | `cookies`（别名 `cookie`） |
| music | `[plugin.fuyubot.music]` | `netease` `soda` `soda_device_id` `soda_iid` `soda_fp` `soda_x_helios` `soda_x_medusa` `soda_user_agent` `soda_signer_url` `soda_signer_token` |
| message-log | `[plugin.fuyubot.message-log]` | `enabled` `ignore_outgoing` `max_text_chars` |

宿主全局的 `[roles].owners` / `[roles].admins`、日志级别 `[plugins].log_level` 不在插件段里。

## 来源与致谢

- **`fuyubot-plugin-message-log`** 把聊天气泡 / 发送者 / 回复 / 转发 / 话题拼成一行，
  交给 FuyuBot 的插件日志（宿主统一加时间、来源与级别），不自带日志文件与按日轮转。
- **`fuyubot-plugin-douyin`** 的签名栈移植自
  [Evil0ctal/Douyin_TikTok_Download_API](https://github.com/Evil0ctal/Douyin_TikTok_Download_API)
  （Apache-2.0）的 `dtk/signing/native/`，并用 `sign-vectors.json` 逐字节对拍。
- **`fuyubot-plugin-music`** 的汽水接口与音质档位参考
  [sodahub-org/libresoda](https://github.com/sodahub-org/libresoda)；代码是独立重写，
  只沿用协议事实（路径、参数形状、eapi / spade / CENC 算法），没有搬 AGPL 源码。
- **`fuyubot.html2img` 的渲染器**是 [hyper-render](https://github.com/thomasmost/hyper-render)
  的本地 vendored fork（`vendor/`），合并了 upstream PR #1（blitz 0.3.0-alpha.4 + `data:` URI
  资源 + StyleThreading）并实现了 PR #3 的外部 URL 资源能力；底层是 Blitz、Stylo、Taffy 与
  Vello。来源与升级方式见 [`vendor/hyper-render/LOCAL.md`](vendor/hyper-render/LOCAL.md)。
- **`vue-template-html` 的模板语义**对照 [Vue 3](https://github.com/vuejs/core)
  （`compiler-core` / `runtime-core`，3.5 行为）；代码是纯 Rust 重写，不含 Vue 源码，也不在
  运行时依赖它。
- **`fuyubot.adblock` 中的 `src/simplify.rs`** 是 FuyuBot 的 `fuyubot-simplify`
  （`publish = false`，不能作为依赖发布）的裁剪内联版。
- PNG → JPEG 转码用 [`image`](https://crates.io/crates/image)；MongoDB 驱动用官方
  `mongodb` / `bson`。

## 许可

MIT OR Apache-2.0。
