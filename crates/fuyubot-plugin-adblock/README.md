# fuyubot-plugin-adblock

FuyuBot 的广告拦截插件：一条消息进来，按「关键词 / 文件 / 贴纸 / 转发来源 / 用户名 / 资料」逐项匹配；命中就撤回消息、禁言或封禁，并把处置记录写进群绑定的日志频道。

没命中的可疑消息（引用、联系人、带全局触发词、入群 24h 内改过资料的人）会再送 **AI 深度审核**。判定为广告后，关键词 / 全文 / 名称 / 简介 / 转发来源 / 贴纸集会被写回黑名单——下一次同样的广告就不用再过 AI 了。

数据全部存在 MongoDB，插件启动时自动建好集合与索引。

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

- **`storage.mongo`（硬依赖）**：数据全部通过宿主里的 MongoDB 插件服务读写。插件在加载时声明了它——mongo 插件不在时本插件**不会激活**（停在 Pending），而不是静默地「不拦截」。
- **FuyuBot 运行时**（ABI v5），发布包覆盖 Windows / Linux / macOS / Android，具体平台见 `plugin.json`。
- **可选 `fuyubot.help.commands`**：装了宿主 help 插件时 `/help` 会列出下文的命令；没装也能正常使用。

> mongo 插件就在本仓库的 [`crates/fuyubot-plugin-mongo`](../fuyubot-plugin-mongo/)；本插件只认服务名 `storage.mongo` 与它的 JSON 方法约定，两个插件各自独立发布。
>
> AI 审核走 OpenAI 兼容的对话接口，后端与模型由 `ai_base_url` / `ai_model` 决定，群内也可以单独配置。

## 安装

```bash
fuyubot plugin install target/release/fuyubot_plugin_adblock.dll
# 或者直接把动态库拷进项目的 plugins/
```

手工放置时的目录结构：

```text
plugins/
└── fuyubot.adblock/
    ├── plugin.json
    └── bin/fuyubot_plugin_adblock.dll
```

前提：同一个项目里已经装了 mongo 插件（它注册 `storage.mongo`），并且 `fuyubot.toml` 的 `[plugin.fuyubot.mongo]` 指向你的 MongoDB。

## 配置

全部在 `fuyubot.toml` 的 `[plugin.fuyubot.adblock]` 下，**只有这一个段、全部平铺**，所有项都可选：

```toml
[plugin.fuyubot.adblock]
# 能执行「仅 Bot 主人」命令的 TGID（/adkew /admatch /adaiadd /adaiset …）
owner_ids = [123456789]
# 「Bot 管理员」：可执行下面命令表里标了「主人/管理员」的那 6 条
# （/qadid /delad /addkw /editkw /addfwdbl /rmfwdbl）；主人天然也算管理员
admin_ids = [987654321]
# 覆盖 mongo 插件的默认库（不写就用 mongo 插件的 db）
db = "fuyubot"
# 全局封禁审计频道（可选；默认关闭）
log_channel = -1001234567890
log_topic = 1622

# 内置默认 AI 接口（可选；没有它就只在「群自定义了接口」的群里启用 AI）
ai_api_key = "sk-…"
ai_base_url = "https://api.openai.com/v1"
ai_model = "gpt-4o-mini"

# AI 审查总日志（可选）
ai_log_channel = -1001234567890
ai_log_topic = 11740

# /spam 判定后写库的通知（可选）
ai_db_channel = -1001234567890
ai_db_topic = 3034
```

> 密钥不要提交进版本库。本插件只从配置读密钥，仓库里没有任何密钥。

各键的含义与默认值：

| 键 | 不配置时 | 说明 |
| --- | --- | --- |
| `owner_ids` | 空 | Bot 主人；`/adkew` `/admatch` `/adaiadd` `/adaiset` 等主人命令只认这里 |
| `admin_ids` | 空 | Bot 管理员；可执行 `/qadid` `/delad` `/addkw` `/editkw` `/addfwdbl` `/rmfwdbl` |
| `db` | mongo 插件的 db | 覆盖本插件用的库名；多个 bot 共用一套 MongoDB 时用它隔离 |
| `log_channel` / `log_topic` | 不发送 | 全局封禁审计频道及其话题 |
| `ai_api_key` / `ai_base_url` / `ai_model` | 未配置 | 内置默认 AI 接口；没有它时只有「群自定义了接口」的群启用 AI |
| `ai_log_channel` / `ai_log_topic` | 不发送 | AI 审查总日志 |
| `ai_db_channel` / `ai_db_topic` | 不发送 | `/spam` 判定写库后的通知 |

## 快速开始

1. 装好 mongo 插件，把 `[plugin.fuyubot.mongo]` 指到你的 MongoDB；
2. 装本插件，在 `[plugin.fuyubot.adblock]` 里填 `owner_ids`（你自己的 TGID，不是 `@username`）；
3. 把 Bot 拉进群并设为管理员，在群里发 `/adconfig`：开关、处置方式、日志频道都在这里；
4. 要用 AI：在 `/adconfig` 里给本群配接口，或由主人 `/adaiset add <chat_id>` 授权内置接口；
5. 补规则：私聊 `/addkw ads 关键词`（同一组关键词按 AND 匹配，需同时出现），用 `/admatch 一段文本` 验证命中结果。

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

命令名、说明与权限同时用于分发和 `/help` 登记：装了宿主 help 插件时 `/help` 会列出这 17 条，没装时插件照常工作。

群内 `/adconfig` 的「🔑 配置AI接口」按钮会跳到 Bot 私聊，走三步向导（API Key → 后端地址 → 模型名），最后自动测一次连通性。

群管理员的群内命令还会再核一次「真的是本群管理员」；`/spam` 另外要求 Bot 自己有管理权限，否则无法撤回或禁言。

## 数据

集合与索引在插件启动时自动创建（已存在的跳过，冲突不影响启动）：

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
| `adblock_settings` | 全局 AI 触发关键词 |
| `SPAM_Log` | `/spam` 举报流水 |

## 注意

- **Bot 需要管理权限**：撤回要「删除消息」，禁言/封禁要「限制成员」。没有权限时群内不会有任何动作，先确认 Bot 是群管理员。
- **私聊命令在群里发会被静默忽略**：不回复、也不提示权限。这是刻意的——群里可能还装着别的 bot，回一句「你没权限」只会暴露命令存在；主人命令请私聊 Bot。
- **AI 失败不静默放行**：未配置、HTTP 4xx、返回格式不符都会得到明确原因，该消息按「未命中」处理，需要时人工复核。
- **日志频道默认不发**：只有配了 `log_channel` / `log_topic` 才会往审计频道写；群内日志则在 `/adconfig` 或 `/setlogchannel` 里绑。
- **`/setlogchannel` 先验证再落库**：测试消息真的发出去了才写配置，频道填错不会留下半套配置。
- **换库要谨慎**：`db` 指向已有数据的库会直接沿用其中的集合；指向空库时会新建集合与索引，不会导入任何旧数据。
- **封禁只撤回近 48 小时**：可连带撤回的历史消息来自 `message_records` 索引，更早的消息不会被翻出来。

## 来源与致谢

- 本插件是独立实现：检测规则、提示语与按钮交互都在本仓库内维护。
- `src/simplify.rs` 内联了 FuyuBot 的 `fuyubot-simplify`（`publish = false`，不能作为依赖发布）的裁剪版，只保留本插件用到的命令解析与消息渲染部分。
- TDLib 调用与消息发送走 FuyuBot 的 ABI v5；AI 审核走 OpenAI 兼容的对话接口。

## License

MIT OR Apache-2.0
