# fuyubot-plugin-help

`/help` 的图片版：读取宿主命令表，按插件分组渲染成一张图发回聊天，替代框架内置的 `/help` 文字回复。

插件 id 是 `xiaoqvan.help`，内置 help 是 `fuyubot.help`。两者 id 不同，但都注册 `/help`，不要同时启用。

![preview](preview.jpg)

## 命令

| 命令 | 说明 | 场景 | 权限 |
| --- | --- | --- | --- |
| `/help` | 出图片版命令表，按插件分组 | 私聊 / 群 / 频道 | 所有人 |
| `/help text`（`/help t`） | 出同一份数据的文字版，不渲染图片 | 私聊 / 群 / 频道 | 所有人 |

`/help@本Bot` 也认。

## 安装

本插件硬依赖 `fuyubot.html2img`，没装或没启用时停在 Pending，不会退化成文字版。

它和内置 help 抢 `/help`，装之前先把内置的平铺 DLL 移走或改名（改名成 `.disable`，不要直接删，`fuyubot run` 会重新装回来）：

```text
plugins/
├── fuyubot_plugin_help.dll.disable   ← 内置 help，已禁用
├── xiaoqvan.help/                    ← 本插件
│   ├── plugin.json
│   └── bin/fuyubot_plugin_help.dll
└── fuyubot.html2img/                 ← 硬依赖
    ├── plugin.json
    └── bin/fuyubot_plugin_html2img.dll
```

想装回内置 help，删掉 `plugins/xiaoqvan.help/`，再把 DLL 改回 `fuyubot_plugin_help.dll`。

## 配置

配置段 `[plugin.xiaoqvan.help]`，全部可省略：

```toml
[plugin.xiaoqvan.help]
title = "可用命令"
description = "FuyuBot · 一张图看完所有命令"
tips = "可使用 /help t 获取文本版本"
table_desc = "共 {count} 条命令"
image = "https://files.catbox.moe/py80s6.jpg"
width = 760
height = 600
scale = 2.0
format = "jpg"
max_commands = 120
send_timeout_ms = 30000
fallback_text = true
```

| 键 | 默认 | 说明 |
| --- | --- | --- |
| `title` | `可用命令` | 图片大标题 |
| `description` | `FuyuBot · 一张图看完所有命令` | 副标题 |
| `tips` | `可使用 /help t 获取文本版本` | 左下角提示 |
| `table_desc` | `共 {count} 条命令` | 每张卡片的说明，支持 `{count}` / `{plugins}` / `{version}` |
| `image` | catbox 头图 | 头图 URL，支持 `http(s)` / `data:`；置空则不画 |
| `width` | `760` | 渲染宽度（CSS px），夹到 320–1920 |
| `height` | `600` | 高度下限，`auto_height` 会按内容撑开 |
| `scale` | `2.0` | 输出倍率，夹到 1.0–3.0 |
| `format` | `jpg` | 输出格式 |
| `max_commands` | `120` | 单图最多画多少条，超出截断并提示 |
| `send_timeout_ms` | `30000` | 等发送结果的预算 |
| `fallback_text` | `true` | 出图或发送失败时补发文字版 |

`image` 走 html2img 的远程资源能力，只接受公网 `http(s)` 和 `data:`。头图拉取失败不会让整张图失败。完全不想联网就写 `image = ""`。

## 权限与可见性

显示哪些命令由宿主命令表里的 `role` / `scopes` / `hidden` 决定：

- `hidden = true` 不显示。
- `role` 按 `user` / `chat_admin` / `admin` / `owner` 逐级过滤。
- `scopes` 只显示和当前会话类型匹配的命令。

图片版在私聊里放宽一档：主人看到全部，管理员看到除主人命令以外的全部。`/help text` 保持原语义。

## 注意

- 命令表在宿主里，help 没装或被禁用都不影响其它插件登记命令。
- 出图或发送失败时，`fallback_text = true` 会补发一条纯文本命令表。
- 图片底部有一排图例和版本号，颜色对应角色，右侧图标表示命令允许出现在哪种会话。

## 来源与许可

页面布局参照 `help.vue`（Tailwind 版）改成手写 CSS，因为 hyper-render 不跑 JS。图标用 [Lucide](https://lucide.dev) 的线性 SVG 内联，不联网。渲染交给 `fuyubot.html2img`。

MIT OR Apache-2.0
