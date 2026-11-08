# fuyubot-plugins

个人的 [FuyuBot](https://github.com/CatMoeCircle/Fuyu-Bot) 插件集合：**一个仓库，四个插件**，
外加一个可以独立发布的库 crate。每个插件是一个独立的 cdylib，按
[插件分发格式](https://catmoecircle.github.io/FuyuBot-docs/docs/plugin/bundle/) 打成 bundle zip。

| 插件 | id | 说明 |
| --- | --- | --- |
| [fuyubot-plugin-sqlite](crates/fuyubot-plugin-sqlite/) | `fuyubot.sqlite` | SQLite 存储服务：参数化查询、表结构模板、事务批量写入，按插件隔离库文件 |
| [fuyubot-plugin-mongo](crates/fuyubot-plugin-mongo/) | `fuyubot.mongo` | MongoDB 存储服务（`storage.mongo`）：集合级 CRUD、聚合、索引管理 |
| [fuyubot-plugin-adblock](crates/fuyubot-plugin-adblock/) | `fuyubot.adblock` | 广告拦截：关键词 / 文件 / 贴纸 / AI 审核，命中即删除、禁言或封禁；**硬依赖 `storage.mongo`** |
| [fuyubot-plugin-html2img](crates/fuyubot-plugin-html2img/) | `fuyubot.html2img` | HTML / Vue 转图片服务：把 HTML、CSS 或 Vue 模板渲染成 PNG / JPEG（默认 JPEG）并返回本地文件路径（hyper-render，纯 Rust，无 Chromium） |

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
└── fuyubot.html2img/
    ├── plugin.json
    └── bin/fuyubot_plugin_html2img.dll
```

Windows 用 `.dll`，Linux / Android 用 `.so`，macOS 用 `.dylib`；每个插件的 zip 里已经按平台放好。

## 插件之间

- **`fuyubot.adblock` 依赖 `storage.mongo`**（由 `fuyubot.mongo` 提供）：没装 mongo 插件时
  adblock 不会激活（停在 Pending），不会静默失效。
- **`fuyubot.sqlite` / `fuyubot.mongo` 是给别的插件用的存储服务**，自己不接收聊天消息；
  配置和调用方式见各自的 README。
- **`fuyubot.html2img` 只提供服务**（`call_service("fuyubot.html2img", "render", …)`），
  不注册命令、不主动发消息；图片怎么用由调用方决定。
- 想用 Vue 模板出图，`fuyubot.html2img` 已经内置了本仓库的 `vue-template-html`。

## 来源与致谢

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
