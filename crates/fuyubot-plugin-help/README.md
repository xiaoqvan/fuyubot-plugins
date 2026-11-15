# fuyubot-plugin-help

FuyuBot 的 **`/help` 图片版**：把命令表渲染成一张图片发回聊天，替代框架内置的
`/help` 文本回复。

它不是「另一个 help」：**插件 id 与内置 help 相同（`fuyubot.help`）**。从
FuyuBot 0.1.5 起命令表归宿主（`fuyubot.host.commands`）：`/start`、`/plugins`、
adblock 等插件各自登记，本插件只读 `list` 并按命令的能力过滤、渲染。

[![](https://img.shields.io/badge/powered_by-dsh-4D6BFE?style=flat-square&logo=deepseek&logoColor=white)](https://github.com/deepseek-ai/deepseek-harness)

![preview](preview.jpg)

## 它做什么

- 订阅 `/help`（含 `/help@本Bot`），问宿主要当前命令表（`fuyubot.host.commands` 的 `list`），交给
  [`fuyubot.html2img`](../fuyubot-plugin-html2img/) 渲染成 JPEG / PNG，再按
  `reply_to_message_id` 发回原消息。
- `/help t` / `/help text` 回**文字版**：同一份分组数据，不渲染、不吃 CPU，图片太大或
  不方便看图时用；其它参数（含裸 `/help`）照旧出图。
- **回复形态交给宿主**：出图后走宿主的命令回复服务（`fuyubot.host.command`）——
  普通的发新消息 + 回原命令；user 账户自己敲的 `/help` 由宿主编辑原消息；
  群里的 ephemeral 命令发只对执行者可见的临时消息。插件不自己判断账号类型。
- **命令按插件分组**：一个登记命令的插件一张卡片，组标题优先用宿主报告的展示名
  （`广告拦截（adblock）`），没有归属的收在最后的「其它」里。归属由宿主判定
  （ABI v6 的调用者栈）——登记方在自己的线程上调进来也认得，插件自报的名字不算数。
- 图片生成 / 发送失败时，按配置补发一条**纯文本命令表**（`fallback_text = true`），
  保证 `/help` 不会什么都不回。
- **按宿主命令表的权限过滤**：`hidden` 不进列表；`role`（user / chat_admin / admin /
  owner）和 `scopes`（private / group / channel）按当前用户与会话裁剪。
- **图片版在私聊里放宽一档**：主人看到全部命令，管理员看到除主人命令以外的全部；
  普通用户 / 群 / 频道仍按 `role` / `scopes` 裁剪。文字版（`/help text`）保持原语义。
- **每条命令带角色描边与会话图标**：紫=主人、绿=管理员、黄=群管、白=普通用户；
  右侧小图标表示这条命令允许在私聊 / 群组 / 频道哪里用。条目阴影整体调淡。
- 不再维护静态兜底：`start` / `help` / `plugins` 都由各自插件登记，没登记就不显示。

## 命令

| 命令 | 一句话 | 场景 | 权限 |
| --- | --- | --- | --- |
| `/help` | 出图片版命令表，按插件分组 | 私聊 / 群 / 频道 | 所有人 |
| `/help text`（`/help t`）| 出同一份数据的文字版，不渲染图片 | 私聊 / 群 / 频道 | 所有人 |

`/help@本Bot` 同样认；其它参数（含裸 `/help`）照旧出图。私聊里主人看到全部命令、
管理员看到除主人命令以外的全部。

## 硬依赖：`fuyubot.html2img`

`Plugin::inject()` 声明了 `fuyubot.html2img`（由
[`fuyubot-plugin-html2img`](../fuyubot-plugin-html2img/) 提供的进程级共享服务）：

- 没装 / 没启用 html2img 时，本插件**停在 Pending**，不会静默退化成文字版；
- html2img 默认输出 JPEG，本插件默认也是 `jpg`（可配 `png`）；
- 交给出图服务的是 `{"html": 模板, "vue": true, "data": {...}}`，模板在
  [`assets/help.html`](assets/help.html)，编译期打进动态库。

## 安装（重要：与内置 help 同 id）

两者 id 相同，**不能同时加载**。装本插件前先移走或禁用内置的平铺 DLL
（禁用就是改名：`fuyubot_plugin_help.dll` → `fuyubot_plugin_help.dll.disable`）：

```text
plugins/
├── fuyubot_plugin_help.dll.disable  ← 内置 help：移走 / 改名 / 用面板禁用
├── fuyubot.help/                    ← 本插件（目录包）
│   ├── plugin.json
│   └── bin/fuyubot_plugin_help.dll
└── fuyubot.html2img/                ← 硬依赖
    ├── plugin.json
    └── bin/fuyubot_plugin_html2img.dll
```

如果内置 DLL 还在，宿主会看到两个同名插件（`fuyubot.help`），加载会因重复
id 失败（或者其中一份被跳过）——这不是本插件的 bug，是「替代」本身的约束。

> ⚠️ **别只删内置 DLL**：`fuyubot run` / `init` 会重新把内置插件装回来。
> 用面板禁用，或把文件改名成 `fuyubot_plugin_help.dll.disable`（加载器只认
> 后缀正好是 `.dll` 的文件，`install_builtins` 也把 `.disable` 当作「已存在」）。

想装回内置 help，反向操作即可：删掉 `plugins/fuyubot.help/`，把内置 DLL 改回
`fuyubot_plugin_help.dll`（或面板里重新启用）。

## 配置

配置段 `[plugin.fuyubot.help]`，全部可省略：

```toml
[plugin.fuyubot.help]
title = "可用命令"
description = "FuyuBot · 一张图看完所有命令"
tips = "可使用 /help t 获取文本版本"
table_desc = "共 {count} 条命令"
image = "https://files.catbox.moe/py80s6.jpg"
width = 760
height = 600
scale = 2.0
format = "jpg"                # jpg / png
max_commands = 120
send_timeout_ms = 30000
fallback_text = true
```

| 键 | 默认 | 说明 |
| --- | --- | --- |
| `title` | `可用命令` | 图片大标题 |
| `description` | `FuyuBot · 一张图看完所有命令` | 副标题 |
| `tips` | `可使用 /help t 获取文本版本` | 左下角小提示 |
| `table_desc` | `共 {count} 条命令` | **每组**（一个插件一张卡）的说明，支持 `{count}`（本组条数）/ `{plugins}`（来源插件数）/ `{version}` |
| `image` | catbox 头图 | 头图 URL；支持 `http(s)` / `data:`，**置空则不画头图** |
| `width` | `760` | 渲染宽度（CSS px），夹到 320–1920 |
| `height` | `600` | 高度下限（`auto_height` 会按内容撑开） |
| `scale` | `2.0` | 输出倍率，夹到 1.0–3.0 |
| `format` | `jpg` | 输出格式 |
| `max_commands` | `120` | 单图最多画多少条，超出截断并提示 |
| `send_timeout_ms` | `30000` | 等 TDLib 发送结果的预算 |
| `fallback_text` | `true` | 出图 / 发送失败时补发文字版 |

### 权限与可见性

「谁能在哪看见哪条命令」不再由 help 自己的白 / 黑名单决定，而是**由命令表里的
`role` / `scopes` / `hidden` 决定**——登记命令的插件写什么，`/help` 就按什么过滤：

| 字段 | 默认 | `/help` 行为 |
| --- | --- | --- |
| `hidden` | `false` | `true` 时不进列表 |
| `role` | `user` | `user` / `chat_admin`（本群管理）/ `admin` / `owner` 逐级过滤 |
| `scopes` | `private + group` | 只显示与当前会话类型（私聊 / 群 / 频道）匹配的命令 |

过滤在 `/help` 的工作线程里做：全局角色问宿主 `role` 方法（纯内存），会话类型与
群管理权先读宿主对象缓存（`fuyubot.host.cache`，`chat_member` 30 秒 TTL），
未命中再问一次 TDLib。因此群管理员能看到 `chat_admin` 命令，普通群员看不到。

**图片版在私聊里放宽一档**（`/help text` 不受影响，保持上表语义）：

| 私聊里的身份 | 图片版能看到 |
| --- | --- |
| 主人 `owner` | **全部命令**（含 `admin` / `owner` 命令、含只在群 / 频道执行的命令） |
| 管理员 `admin` | 除 `owner` 命令以外的**全部命令** |
| 普通用户 | 与文字版一致：按 `role` / `scopes` 裁剪 |

`hidden` 在两种形态下都排除——它表达的是「别列出来」，与角色无关。放宽只改
**展示**，不改变命令本身的执行权限。

#### 图片里的视觉语言

每条命令是一张卡片，**1px 淡色**描边 = 这条命令**要求的角色**：

| 描边 | 角色 | 含义 |
| --- | --- | --- |
| 🟪 淡紫 `#c9a2fb` | `owner` | 主人专用 |
| 🟩 淡绿 `#7fd8a0` | `admin` | 全局管理员 |
| 🟨 淡黄 `#f2d268` | `chat_admin` | 本群管理员 |
| ⬜ 白色（垫一圈淡灰外圈） | `user` | 所有人 |

卡片右侧的彩色小图标 = 这条命令**允许出现的会话**（按宿主给的原始 `scopes`，
与当前会话无关）：`user` 私聊（青 `#0d9488`）、`users` 群组（橙 `#ea580c`）、
`megaphone` 频道（蓝 `#2563eb`）。

图标用 [Lucide](https://lucide.dev) 的线性 SVG（14px、`stroke-width: 2`、
`fill: none` + `round` 端点），内联在 `assets/help.html` 里——不是字体图标、也不是
远程资源，`hyper-render` 直接按 SVG 画，不联网、不依赖系统 emoji 字体。
底部图例里用的是同一套图标。

阴影统一是**中性灰**的轻投影（`0 1px 2px rgba(15,23,42,.04)`），不跟随描边颜色：
同色阴影会把 1px 描边糊成 2px 的粗边。白色描边本身在浅色卡片上不可见，额外垫的
那圈 `rgba(203,213,225,.55)` 也是中性灰，只为勾出轮廓。

图片**底部是一整排**：刷新提示 + 七项图例 + 版本号，从左到右一条线排开
（图例色块与卡片描边同源，见 `commands::legend()`）：

`可使用 /help t 获取文本版本`　`● 主人`　`● 管理员`　`● 群管理`　`○ 用户`　`👤 私聊`　`👥 群组`　`📢 频道`　`v0.1.3`

图例是静态的，不随这张图里实际有哪些命令变化——同一张图在任何会话里读法一致。
`tips` 置空时这一排从图例开始；`legend.items` 为空时整排只剩提示与版本号。

> `image` 走 html2img 的远程资源能力（仅公网 `http(s)` 与 `data:`；不要填
> `file://` 或局域网地址）。头图拉取失败不会让整张图失败，只是没有头图。
> 完全不想联网就写 `image = ""`。`data:` 会算进 html2img 的 `max_html_bytes`
> （默认 128 KiB），大图请用 URL，别塞 base64。

## 和其它插件的关系

- **命令表由宿主提供**：其它插件在 `start()` 里
  `call_service("fuyubot.host.commands", "register", ...)`，登记时带上
  `role` / `scopes` / `hidden`；旧的 `fuyubot.help.commands` 是宿主的兼容别名，
  新代码不要再用，也不需要 `soft_inject` 等 help。
- **本插件硬依赖** `fuyubot.html2img`：图片渲染服务。
- 命令表在宿主里，所以 help 没装、被禁用或卸载都不影响其它插件登记命令。

## 开发

```bash
# 只测本插件（不编译 hyper-render）
cargo test -p fuyubot-plugin-help

# 看效果：本地渲染一张预览图（不连宿主、不发消息）
# 真实链路：build_render_request → 模板 → vue-template-html → hyper-render
cargo run -p fuyubot-plugin-help --example preview
cargo run -p fuyubot-plugin-help --example preview -- /tmp/help-preview.jpg

# 打 bundle（与其它插件同一套脚本）
python scripts/package.py --plugin fuyubot-plugin-help \
    --platform windows-x86_64 --version 0.1.3
```

模板在 [`assets/help.html`](assets/help.html)：布局参照 `help.vue`（Tailwind 版），
这里改写成手写 CSS——`hyper-render` 不跑 JS、也没有 Tailwind 运行时，所以只保留
模板用到的那组布局 / 颜色规则，并把 `body` 内边距压成 0，让渐变背景铺满整张图。
改模板后跑一遍 `cargo test -p fuyubot-plugin-help`，单测会用 `vue-template-html`
把同一份模板编译并渲染，确认 `{{ }}` / `v-for` / 空列表都能正常出 HTML。

## 许可

MIT OR Apache-2.0。
