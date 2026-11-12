# fuyubot-plugins

个人的 [FuyuBot](https://github.com/CatMoeCircle/Fuyu-Bot) 插件集合：**一个仓库，八个插件**，
外加一个可以独立发布的库 crate。每个插件是一个独立的 cdylib，按
[插件分发格式](https://catmoecircle.github.io/FuyuBot-docs/docs/plugin/bundle/) 打成 bundle zip。

| 插件 | id | 说明 |
| --- | --- | --- |
| [fuyubot-plugin-sqlite](crates/fuyubot-plugin-sqlite/) | `fuyubot.sqlite` | SQLite 存储服务：参数化查询、表结构模板、事务批量写入，按插件隔离库文件 |
| [fuyubot-plugin-mongo](crates/fuyubot-plugin-mongo/) | `fuyubot.mongo` | MongoDB 存储服务（`storage.mongo`）：集合级 CRUD、聚合、索引管理 |
| [fuyubot-plugin-adblock](crates/fuyubot-plugin-adblock/) | `fuyubot.adblock` | 广告拦截：关键词 / 文件 / 贴纸 / AI 审核，命中即删除、禁言或封禁；**硬依赖 `storage.mongo`** |
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

## 来源与致谢

- **`fuyubot-plugin-message-log`** 从 TypeScript 宿主 `plugins/message_log.ts` 迁来：
  聊天气泡 / 发送者 / 回复 / 转发 / 话题的拼法沿用原版，输出改走 FuyuBot 的插件日志
  （宿主统一加时间、来源与级别），不再自带 pino 与按日轮转文件。
- **`fuyubot-plugin-douyin`** 从 TypeScript 宿主 `plugins/video_parse` 的抖音分支迁来；
  签名栈移植自 [Evil0ctal/Douyin_TikTok_Download_API](https://github.com/Evil0ctal/Douyin_TikTok_Download_API)
  （Apache-2.0）的 `dtk/signing/native/`，并用 `sign-vectors.json` 逐字节对拍。
- **`fuyubot-plugin-music`** 的网易云命令 / eapi 行为参考旧 TS 实现，汽水接口与音质
  档位参考 [sodahub-org/libresoda](https://github.com/sodahub-org/libresoda)；代码是独立
  重写，只沿用协议事实（路径、参数形状、eapi / spade / CENC 算法），没有搬 AGPL 源码。
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
