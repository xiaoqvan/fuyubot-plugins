# fuyubot-plugins

个人维护的 [FuyuBot](https://github.com/CatMoeCircle/Fuyu-Bot) 插件集合。九个插件，外加一个可以单独使用的 Vue 模板编译库。每个插件是独立的 cdylib，按[插件分发格式](https://catmoecircle.github.io/FuyuBot-docs/docs/plugin/bundle/)打成 zip。

## 插件

| 插件 | id | 说明 |
| --- | --- | --- |
| [sqlite](crates/fuyubot-plugin-sqlite/) | `fuyubot.sqlite` | SQLite 存储服务，给别的插件建表、读写用 |
| [mongo](crates/fuyubot-plugin-mongo/) | `fuyubot.mongo` | MongoDB 存储服务，集合级读写 |
| [adblock](crates/fuyubot-plugin-adblock/) | `xiaoqvan.adblock` | 广告拦截：关键词、文件、贴纸、AI 审核，命中后撤回、禁言或封禁；依赖 mongo |
| [html2img](crates/fuyubot-plugin-html2img/) | `fuyubot.html2img` | HTML / Vue 转图片服务，纯 Rust，不需要 Chromium |
| [help](crates/fuyubot-plugin-help/) | `xiaoqvan.help` | `/help` 图片版，按插件分组；依赖 html2img |
| [douyin](crates/fuyubot-plugin-douyin/) | `xiaoqvan.douyin` | 抖音解析：`/dy` 命令加自动识别链接 |
| [music](crates/fuyubot-plugin-music/) | `xiaoqvan.music` | 网易云、汽水、YouTube Music 的搜索与解析 |
| [message-log](crates/fuyubot-plugin-message-log/) | `xiaoqvan.message-log` | 把新消息写到宿主日志 |
| [x](crates/fuyubot-plugin-x/) | `xiaoqvan.x` | X / Twitter 解析，文字和最多两张图渲染成卡片 |

## 库

| 库 | 说明 |
| --- | --- |
| [vue-template-html](crates/vue-template-html/) | Vue 3 模板编译成 HTML，纯 Rust，无 JS 运行时 |

## 安装

在 TUI 插件商店里直接装，或者把发布页的 zip 解压到机器人的插件目录，一个插件一个目录：

```text
plugins/
├── fuyubot.sqlite/
│   ├── plugin.json
│   └── bin/fuyubot_plugin_sqlite.dll
├── fuyubot.mongo/
│   ├── plugin.json
│   └── bin/fuyubot_plugin_mongo.dll
├── xiaoqvan.adblock/
│   ├── plugin.json
│   └── bin/fuyubot_plugin_adblock.dll
├── fuyubot.html2img/
│   ├── plugin.json
│   └── bin/fuyubot_plugin_html2img.dll
├── xiaoqvan.help/
│   ├── plugin.json
│   └── bin/fuyubot_plugin_help.dll
├── xiaoqvan.douyin/
│   ├── plugin.json
│   └── bin/fuyubot_plugin_douyin.dll
├── xiaoqvan.music/
│   ├── plugin.json
│   └── bin/fuyubot_plugin_music.dll
├── xiaoqvan.message-log/
│   ├── plugin.json
│   └── bin/fuyubot_plugin_message_log.dll
└── xiaoqvan.x/
    ├── plugin.json
    └── bin/fuyubot_plugin_x.dll
```

Windows 用 `.dll`，Linux / Android 用 `.so`，macOS 用 `.dylib`，每个插件的 zip 里已经按平台放好。

`xiaoqvan.help` 和框架内置的 help 都注册 `/help`，不要同时启用。想用图片版就把内置的 `plugins/fuyubot_plugin_help.dll` 改名成 `.disable`（别直接删，`run` 会重新装回来）；想装回内置版就删掉 `plugins/xiaoqvan.help/` 再改回来。

## 配置

每个插件只读 `fuyubot.toml` 里自己名字点分的那一段，键和默认值见各自的 README。主人、管理员写在宿主全局的 `[roles]`，日志级别在 `[plugins].log_level`。

## 兼容性

九个插件都按 ABI v6 编译，需要 FuyuBot 0.1.4-rc.1 或更高。v5 和 v6 互不兼容，升级宿主时插件包要一起换。

`plugin.json` 的 `depends` / `optional_depends` 写的是**插件 id**（如 `fuyubot.mongo`），商店按它算安装计划；
`provides` 列出插件对外提供的服务名，商店靠它把服务名对回插件 id。运行期的依赖仍是服务名，两者不要混。

每个版本按**不可变 Release** 发布，GitHub 会同时生成发布 attestation，这是官方商店的上架要求；
发布后资产和 tag 就锁住了，要改内容只能升版本号重新发。

## 来源

- adblock 从 FuyuBot TS 版的 adblock 插件重写，命令、提示文案、AI 提示词和集合结构沿用原版；`src/simplify.rs` 内联了 `fuyubot-simplify` 的裁剪版。
- douyin 的签名栈移植自 [Evil0ctal/Douyin_TikTok_Download_API](https://github.com/Evil0ctal/Douyin_TikTok_Download_API)（Apache-2.0）。
- music 的网易云 eapi 参考 TS 版 `163music` 插件，汽水接口和音质档位参考 [sodahub-org/libresoda](https://github.com/sodahub-org/libresoda)，代码为本仓库重写。
- html2img 的渲染器是 [hyper-render](https://github.com/thomasmost/hyper-render) 的本地 fork，底层是 Blitz、Stylo、Taffy 和 Vello。
- vue-template-html 的模板语义对照 Vue 3，代码是纯 Rust 实现，不含 Vue 源码。
- x 走 [FxEmbed / FxTwitter](https://github.com/FxEmbed/FxEmbed) 的公开只读接口。
- PNG 转 JPEG 用 `image` crate，MongoDB 驱动用官方 `mongodb` / `bson`。

## 许可

MIT OR Apache-2.0
