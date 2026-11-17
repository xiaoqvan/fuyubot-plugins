# fuyubot-plugin-adblock

FuyuBot 的广告拦截插件：一条消息进来，按「关键词 / 文件 / 贴纸 / 转发来源 / 用户名 / 资料」逐项匹配；命中就撤回消息、禁言或封禁，并把处置记录写进群绑定的日志频道。

没命中的可疑消息（引用、联系人、带全局触发词、入群 24h 内改过资料的人）会再送 **AI 深度审核**。判定为广告后，关键词 / 全文 / 名称 / 简介 / 转发来源 / 贴纸集会被写回黑名单——下一次同样的广告就不用再过 AI 了。

数据全部存在 MongoDB，插件启动时自动建好集合与索引。

## 功能

| 入口 | 说明 |
| --- | --- |
| 消息检测 | 文本（含富消息）、图片、视频、联系人、贴纸；广告关键词组按 **AND** 匹配 |
| 自动点赞机器人 | 群管理员在 `/adconfig` 打开后，连续回应 5 条消息的账号默认只被封禁**回应权限**（仍可正常发言），并清掉它已经点出去的回应；可切换为跟随广告禁言 / 封禁；中间被删除的消息不计 |
| 处置 | 撤回触发消息 + 该用户 48 小时内的历史消息；按群配置 **禁言**（默认）或 **封禁** |
| 通知 | 群内提示（可配置自删延时）、群绑定的日志频道（带 `unban` / `unmute` / `Ban` 按钮）、可选的全局审计频道 |
| 话题群 | 群内提示 / 处置通知跟着触发消息回到**同一条话题**；日志频道的 topic 可配（`log_topic` / `ai_log_topic` / `ai_db_topic`） |
| 转发黑名单 | 来自黑名单频道/超级群的消息一律处置；AI 判定后自动把来源加进去 |
| AI 审核 | OpenAI 兼容接口；群可自定义接口，也可由 Bot 主人给「内置接口」授权 |
| 入群检查 | 新成员按名称 / 简介 / 主页挂载频道查黑名单与 AI；高危直接封，存疑先禁言并给「重新审查」按钮 |
| 入群提示 | 三个开关分别删除「成员加入」「通过社区加入」「通过邀请链接加入」的入群提示消息 |
| 取消频道置顶 | 群绑定频道自动转发到群并被 Telegram 自动置顶的消息，自动取消置顶 |
| 24h 监视 | 入群后改资料会触发复检；复审通过后不再因弱信号被禁言；正常发言超过 4 条自动解除监视 |
| 超时兜底 | 禁言 1 小时内没通过复审 → 自动封禁（每 5 分钟扫一次） |
| 人工举报 | `/spam` 可对消息、用户、`@用户名` 做一次 AI 判定，并把结果沉淀成规则 |

## 依赖

- **`storage.mongo`（硬依赖）**：数据全部通过宿主里的 MongoDB 插件服务读写。插件在加载时声明了它——mongo 插件不在时本插件**不会激活**（停在 Pending），而不是静默地「不拦截」。
- **FuyuBot 运行时**（ABI v6），发布包覆盖 Windows / Linux / macOS / Android，具体平台见 `plugin.json`。
- **命令表由宿主提供**（`fuyubot.host.commands`）：本插件登记的命令带 `role` / `scopes`，宿主在分发前做角色闸门，`/help` 也按同一份能力过滤；宿主服务启动时就在，不需要等 help 插件。
- **主人 / 管理员名单也归宿主**（`fuyubot.toml` 的 `[roles].owners` / `[roles].admins`）：插件不再自带 `owner_ids` / `admin_ids`。宿主闸门和 handler 内的复核读的是同一份角色，不会出现两套名单不一致。

> mongo 插件就在本仓库的 [`crates/fuyubot-plugin-mongo`](../fuyubot-plugin-mongo/)；本插件只认服务名 `storage.mongo` 与它的 JSON 方法约定，两个插件各自独立发布。
>
> AI 审核走 OpenAI 兼容的对话接口，后端与模型由 `ai_base_url` / `ai_model` 决定，群内也可以单独配置；
> 默认接口支持每日 token 上限，超限或调用失败时自动切换备用接口（`ai_fallback_*`）。

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

插件自己的配置全部在 `fuyubot.toml` 的 `[plugin.fuyubot.adblock]` 下，**只有这一个段、全部平铺**，所有项都可选：

```toml
[plugin.fuyubot.adblock]
# 覆盖 mongo 插件的默认库（不写就用 mongo 插件的 db）
db = "fuyubot"
# 全局封禁审计频道（可选；默认关闭）
log_channel = -1001234567890
log_topic = 1622

# 内置默认 AI 接口（可选；没有它就只在「群自定义了接口」的群里启用 AI）
ai_api_key = "sk-…"
ai_base_url = "https://api.openai.com/v1"
ai_model = "gpt-4o-mini"
# 默认接口每天累计 token 上限；超过后优先走备用接口（0 = 不限制）
ai_token_limit = 2000000

# 备用 AI 接口（可选；默认接口超限或失败时切换）
# ai_fallback_api_key = "sk-…"
# ai_fallback_base_url = "https://api.deepseek.com/v1"
# ai_fallback_model = "deepseek-chat"

# AI 审查总日志（可选）
ai_log_channel = -1001234567890
ai_log_topic = 11740

# /spam 判定后写库的通知（可选）
ai_db_channel = -1001234567890
ai_db_topic = 3034
```

> 密钥不要提交进版本库。本插件只从配置读密钥，仓库里没有任何密钥。

主人 / 管理员**不在这个段里**：它们是宿主全局角色，写在根配置的 `[roles]`
（插件经 `fuyubot.host.commands` 的 `role` 方法读）：

```toml
[roles]
owners = [123456789]     # 主人：/adkew /admatch /adaiadd /adaiset …
admins = [987654321]     # 全局管理员：/qadid /delad /addkw /editkw /addfwdbl /rmfwdbl
```

`[plugin.fuyubot.adblock]` 里各键的含义与默认值：

| 键 | 默认 | 一句话 |
| --- | --- | --- |
| `db` | mongo 插件的 db | 换一个库名放本插件的数据 |
| `log_channel` | 不发送 | 全局封禁审计频道 |
| `log_topic` | 不发送 | 上面那个频道的话题 id |
| `ai_api_key` | 未配置 | 内置 AI 接口的 Key |
| `ai_base_url` | 未配置 | 内置 AI 接口地址（OpenAI 兼容） |
| `ai_model` | `gpt-4o-mini` | 内置 AI 接口的模型名 |
| `ai_token_limit` | `2000000` | 内置接口每天 token 上限，`0` = 不限 |
| `ai_fallback_api_key` | 未配置 | 备用接口的 Key |
| `ai_fallback_base_url` | 未配置 | 备用接口地址 |
| `ai_fallback_model` | `gpt-4o-mini` | 备用接口的模型名 |
| `ai_log_channel` | 不发送 | AI 审查总日志频道 |
| `ai_log_topic` | 不发送 | 上面那个频道的话题 id |
| `ai_db_channel` | 不发送 | `/spam` 判定写库后的通知频道 |
| `ai_db_topic` | 不发送 | 上面那个频道的话题 id |

## 快速开始

1. 装好 mongo 插件，把 `[plugin.fuyubot.mongo]` 指到你的 MongoDB；
2. 装本插件，在 `fuyubot.toml` 的 `[roles].owners` 填你自己的 TGID（不是 `@username`）；
3. 把 Bot 拉进群并设为管理员，在群里发 `/adconfig`：开关、处置方式、日志频道都在这里；
4. 要用 AI：在 `/adconfig` 里给本群配接口，或由主人 `/adaiset add <chat_id>` 授权内置接口；
5. 补规则：私聊 `/addkw ads 关键词`（同一组关键词按 AND 匹配，需同时出现），用 `/admatch 一段文本` 验证命中结果。

## 命令

| 命令 | 一句话 | 场景 | 权限 |
| --- | --- | --- | --- |
| `/adconfig [配置项]` | 看本群状态；给配置项则切换开关 | 群内 | 群管理员 |
| `/setlogchannel <channel_id>` | 绑定本群日志频道 | 群内 | 群管理员 |
| `/whitelist add\|rm <ID\|@username\|reply>` | 加白名单，跳过一切检测 | 群内 | 群管理员 |
| `/rmwhitelist <ID\|@username\|reply>` | 移出白名单 | 群内 | 群管理员 |
| `/spam [user_id\|@username]` | 对这条消息做一次 AI 判定 | 群内 | 所有人（Bot 需有管理权限） |
| `/qban <tgid\|@username> [limit]` | 查这个人的封禁记录 | 任意 | 所有人 |
| `/qadid <ID>` | 按 `_id` 查黑名单条目 | 私聊 | 管理员 |
| `/delad <ID>` | 按 `_id` 删黑名单条目 | 私聊 | 管理员 |
| `/addkw <ads\|bio> [r18] <关键词…>` | 加关键词，同组按 AND 命中 | 私聊 | 管理员 |
| `/editkw <_id> <关键词…>` | 按 `_id` 改一条关键词 | 私聊 | 管理员 |
| `/addfwdbl <chat_id\|@username>` | 把来源频道 / 群加进转发黑名单 | 私聊 | 管理员 |
| `/rmfwdbl <chat_id>` | 从转发黑名单里移除 | 私聊 | 管理员 |
| `/adkew add\|rm\|list [关键词]` | 管理触发 AI 审核的全局关键词 | 私聊 | 主人 |
| `/admatch <文本>` | 看这段文本命中了哪些关键词 | 私聊 | 主人 |
| `/adaiadd <文本>` | AI 判一段文本，超过 3 个词就入库 | 私聊 | 主人 |
| `/adaiset add\|rm\|list [chat_id]` | 授权群用内置 AI 接口 | 私聊 | 主人 |
| `/start aiConfig_<chat_id>` | 私聊三步向导配本群 AI 接口 | 私聊 | 目标群管理员 |

命令名、说明与权限同时用于分发和 `/help` 登记：装了宿主 help 插件时 `/help` 会列出这 17 条，没装时插件照常工作。

群内 `/adconfig` 的「🔑 配置AI接口」按钮会跳到 Bot 私聊，走三步向导（API Key → 后端地址 → 模型名），最后自动测一次连通性。

`/adconfig`、`/setlogchannel`、`/whitelist`、`/rmwhitelist` 会在 handler 里再核一次「真的是本群管理员」。

`/spam` **不限制发起人**：群里任何人都能举报（举报管理员会被拒、举报白名单用户会被拒），
但 Bot 自己必须有「删除消息 + 限制成员」，否则命令直接不响应。

## 自动点赞机器人

有些群里会挂着一批「点赞机器人」：每来一条新消息就挨个点表情。这个功能默认**关闭**，由群管理员在群里打开：

```text
/adconfig like_guard          # 打开 / 关闭（别名，等同 /adconfig like_guard_enabled）
```

开启后，插件拿该群**最近 20 条消息**（cache 里本来就有）当基准，看某个账号是否**连续回应了 5 条**；判定时被删除的消息已经从最近消息里摘掉，所以「A B C(已删) D E F」会连成 A B D E F，不会被删掉的那条打断。

命中后的处置默认**只封禁「回应权限」**：`setChatMemberStatus` 设为受限成员，只关 `can_react_to_messages`，发言、图片、链接预览这些照常。群管理员可以改成**跟随广告处置**，按本群的广告模式（`/adconfig limit ban|mute`）禁言或封禁：

```text
/adconfig like_guard_mode reactions   # 默认：只封禁回应权限，仍可正常发言
/adconfig like_guard_mode ad          # 跟随广告：按 limit_mode 禁言或封禁
```

两种方式都会：

- 写一条 `ban_records`（`spam_type = reaction_bot`）；
- 清掉它在本群已经点出去的全部回应（`deleteAllRecentMessageReactionsFromSender`，需要「删除消息」权限；失败只记日志，不回滚处置）；
- 群里发一条 `#ReactionBot` 提示（`**User ID**: 小王` + `**Reason**: reaction_bot 连续回应 5 条消息`），带「解除限制」/「Ban」按钮，删除时间与其它提示共用 `spam_tips_meg_delay`（`spam_tips` 关闭时提示常驻）；
- 群绑定的日志频道同步一份（带「解除限制」/「unban+白名单」按钮），配了全局审计频道时也写一条。

判定状态只在内存里（插件重启即清空）；同一账号命中后 30 分钟内不会重复处置；管理员、白名单成员不会被处置。

> 检测依赖 TDLib 的 `updateMessageInteractionInfo`：`recent_sender_ids` 每种表情最多只给 3 个最近的点赞者。同一条消息被 4 个以上账号点同一种表情时，这一轮可能只看得到 3 个——处置掉之后，剩下的账号会在后续消息里浮现。
>
> Bot 需要「限制成员」权限；没有权限时不会统计，也不会动作。

## 入群提示消息

群管理员可以在 `/adconfig` 里分别关闭三种入群提示（默认都保留）：

```text
/adconfig join            # 成员加入（被添加 / 审批通过）
/adconfig join_community  # 成员通过社区加入
/adconfig join_invite     # 成员通过邀请链接加入
```

对应的群配置项是 `delete_join_message` / `delete_join_community_message` / `delete_join_invite_message`。打开后，插件收到对应的服务消息（TDLib 的 `messageChatAddMembers`、`messageChatJoinByRequest`、`messageChatJoinFromCommunity`、`messageChatJoinByLink`）就撤回；Bot 需要群的「删除消息」权限。

> 能不能收到这条服务消息由 Telegram 决定：不同账号类型 / 入群方式可能不下发，收不到就删不了。

## 取消频道置顶

群绑定（linked）的频道发帖后，Telegram 会把帖子系统转发到群里并**自动置顶**。打开这个开关后，插件会把这类置顶撤掉：

```text
/adconfig unpin          # 打开 / 关闭（别名，等同 /adconfig unpin_channel_pins）
```

判定只认**本群绑定的那个频道**（`linked_chat_id`）：频道帖子本身被置顶、频道身份执行的置顶、以及 `updateMessageIsPinned` 里绑定频道的消息，都会 `unpinChatMessage`。需要 Bot 有群的「置顶消息」权限。配置项：`unpin_channel_pins`，默认关闭。

## 数据

集合与索引在插件启动时自动创建（已存在的跳过，冲突不影响启动）：

| 集合 | 用途 |
| --- | --- |
| `adb_config` | 每个群的配置（开关、处置模式、日志频道、自定义 AI） |
| `message_records` | 48 小时消息索引（`createdAt` 有 TTL 索引），封禁时连带撤回 |
| `message_texts` | 48 小时消息文本历史（AI 审核的「刚才在聊什么」上下文） |
| `ban_records` | 封禁/禁言审计（`/qban`） |
| `mute_review_records` | 禁言待复审（1 小时超时 → 封禁） |
| `new_member_watch_records` | 入群 24h 监视（`expires_at` 有 TTL 索引） |
| `ads_blacklist` / `bio_blacklist` / `name_blacklist` / `username_blacklist` | 关键词 / 简介 / 名称 / 用户名 |
| `forward_blacklist` | 转发来源黑名单 |
| `flieId_blacklist` / `sticker_blacklist` / `sticker_set_blacklist` | 文件 / 单贴纸 / 贴纸集 |
| `ai_fulltext_blacklist` | AI 判定的广告全文（精确匹配） |
| `ai_default_chats` | 内置 AI 授权群 |
| `adblock_settings` | 全局 AI 触发关键词、每日默认接口 token 用量 |
| `SPAM_Log` | `/spam` 举报流水 |

## 注意

- **Bot 需要管理权限**：撤回要「删除消息」，禁言/封禁要「限制成员」。没有权限时群内不会有任何动作，先确认 Bot 是群管理员。
- **私聊命令在群里发会被静默忽略**：不回复、也不提示权限。这是刻意的——群里可能还装着别的 bot，回一句「你没权限」只会暴露命令存在；主人命令请私聊 Bot。
- **AI 失败不静默放行**：未配置、HTTP 4xx、返回格式不符都会得到明确原因，该消息按「未命中」处理，需要时人工复核；配置了备用接口时，默认接口超限或失败会自动切过去。
- **命令回复交给宿主**：所有回命令的消息都走宿主的命令回复服务（`fuyubot.host.command`）——发新消息、编辑原命令消息还是发 ephemeral 全由宿主按账号类型与命令形态决定。`/adconfig`、`/setlogchannel` 这类「只该给操作人看」的回复因此在群里是 ephemeral 消息（只对执行者可见）；user 账户自己敲的命令则由宿主编辑原消息，不再多发一条。
- **回复的保留策略**：私聊命令的回复永不自动删除；群内只有封禁提示、`/spam`、`/adconfig`、`/setlogchannel`、`/whitelist`(`/rmwhitelist`) 的回复会按配置的秒数自删，`/qban` 等查询结果保留。**编辑进原命令消息的回复同样到点自删**（删掉的就是那条命令消息）。
- **日志频道默认不发**：只有配了 `log_channel` / `log_topic` 才会往审计频道写；群内日志则在 `/adconfig` 或 `/setlogchannel` 里绑。
- **`/setlogchannel` 先验证再落库**：测试消息真的发出去了才写配置，频道填错不会留下半套配置。
- **换库要谨慎**：`db` 指向已有数据的库会直接沿用其中的集合；指向空库时会新建集合与索引，不会导入任何旧数据。
- **封禁只撤回近 48 小时**：可连带撤回的历史消息来自 `message_records` 索引，更早的消息不会被翻出来。
- **自动点赞机器人默认关闭**：只有群管理员 `/adconfig like_guard` 打开后才统计；判定是连续 5 条回应（被删除的消息会被剔除，不算中断），误判时点提示上的「解除限制」即可恢复。

## 来源与致谢

- 本插件是独立实现：检测规则、提示语与按钮交互都在本仓库内维护。
- `src/simplify.rs` 内联了 FuyuBot 的 `fuyubot-simplify`（`publish = false`，不能作为依赖发布）的裁剪版，只保留本插件用到的命令解析与消息渲染部分。
- TDLib 调用与消息发送走 FuyuBot 的 ABI v6（`simplify_invoke` op 表 + 响应通道）；AI 审核走 OpenAI 兼容的对话接口。

## License

MIT OR Apache-2.0
