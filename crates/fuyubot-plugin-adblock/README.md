# fuyubot-plugin-adblock

FuyuBot 的广告拦截插件。群消息进来后按关键词、文件、贴纸、转发来源、用户名和资料逐项检查，命中就撤回消息、禁言或封禁，并把处置记录发到群绑定的日志频道。

没命中的可疑消息会再交给 AI 审核。判定为广告后，关键词、全文、名称、简介、转发来源和贴纸集会被写回黑名单，下次同样的广告直接拦截。

数据全部存在 MongoDB，插件启动时自动建好集合和索引。

## 功能

| 功能 | 说明 |
| --- | --- |
| 消息检测 | 文本（含富消息）、图片、视频、联系人、贴纸；广告关键词组按 AND 匹配 |
| 处置 | 撤回触发消息和该用户 48 小时内的历史消息；按群配置禁言（默认）或封禁 |
| 通知 | 群内提示（可配置自删时间）、群日志频道（带解禁 / 封禁按钮）、可选的全局审计频道 |
| 话题群 | 提示和处置通知回到触发消息所在的话题 |
| 转发黑名单 | 来自黑名单频道或超级群的消息直接处置；AI 判定后自动加入来源 |
| AI 审核 | OpenAI 兼容接口；群可以用自己的接口，也可以由主人授权使用内置接口。群自定义接口失败时**不会**切到内置备用接口，并私聊通知配置人 |
| 入群检查 | 新成员按名称、简介、主页频道查黑名单和 AI；高危直接封，存疑先禁言 |
| 入群提示 | 可分别删除「成员加入」「社区加入」「邀请链接加入」的提示消息 |
| 取消频道置顶 | 群绑定频道被 Telegram 自动置顶的消息，自动取消置顶 |
| 24 小时监视 | 入群后改资料会触发复检；正常发言超过 4 条自动解除 |
| 超时兜底 | 禁言 1 小时内没通过复审，自动封禁；每 5 分钟扫一次 |
| 人工举报 | `/spam` 对消息、用户或 `@用户名` 做一次 AI 判定，并把结果写回规则 |
| 自定义 emoji | 把消息里的 custom emoji 拼成联系表交给 AI 识图，命中后拉黑 emoji 和所属集合 |
| 自动点赞机器人 | 连续回应 5 条消息的账号，默认只封禁回应权限；可改为跟随广告禁言或封禁 |

## 安装

1. 先装 mongo 插件，把 `[plugin.fuyubot.mongo]` 指到你的 MongoDB。本插件硬依赖 `storage.mongo`，没有它不会激活。
2. 把 zip 解压到机器人的插件目录：

   ```text
   plugins/
   └── xiaoqvan.adblock/
       ├── plugin.json
       └── bin/fuyubot_plugin_adblock.dll
   ```

3. 在 `fuyubot.toml` 的 `[roles]` 里填主人和管理员 TGID（不是 `@username`）：

   ```toml
   [roles]
   owners = [123456789]
   admins = [987654321]
   ```

4. 把 Bot 拉进群并设为管理员，在群里发 `/adconfig` 配置开关、处置方式和日志频道。

## 配置

插件配置在 `[plugin.xiaoqvan.adblock]`，全部可选：

```toml
[plugin.xiaoqvan.adblock]
# 覆盖 mongo 插件的默认库；不写就用 mongo 的 db
db = "fuyubot"

# 只在下列群生效（chat id 数组）；留空 = 所有群
# 不在列表里的群完全跳过（消息检测 / 入群检查 / 自动点赞 / 取消置顶 / 群命令）
# chat_allowlist = [-1001234567890, -1009876543210]

# 全局封禁审计频道（可选）
# log_channel = -1001234567890
# log_topic = 1622

# 内置 AI 接口（OpenAI 兼容）
ai_api_key = "sk-…"
ai_base_url = "https://api.openai.com/v1"
ai_model = "gpt-4o-mini"
# 默认接口每天累计 token 上限；0 = 不限
ai_token_limit = 2000000

# 备用接口：内置默认接口超限或失败时切换（群自定义接口失败不会用备用）
# ai_fallback_api_key = "sk-…"
# ai_fallback_base_url = "https://api.deepseek.com/v1"
# ai_fallback_model = "deepseek-chat"

# AI 审查总日志（可选）
# ai_log_channel = -1001234567890
# ai_log_topic = 11740

# /spam 判定写库后的通知（可选）
# ai_db_channel = -1001234567890
# ai_db_topic = 3034
```

| 键 | 默认 | 说明 |
| --- | --- | --- |
| `db` | mongo 插件的 db | 换一个库名放本插件的数据 |
| `chat_allowlist` | 空（所有群） | 只在这些群里生效；不在列表的群完全跳过 |
| `log_channel` | 不发送 | 全局封禁审计频道 |
| `log_topic` | 不发送 | 上面那个频道的话题 id |
| `ai_api_key` | 未配置 | 内置 AI 接口的 Key |
| `ai_base_url` | 未配置 | 内置 AI 接口地址 |
| `ai_model` | `gpt-4o-mini` | 内置 AI 接口的模型名 |
| `ai_token_limit` | `2000000` | 内置接口每天 token 上限，`0` = 不限 |
| `ai_fallback_api_key` | 未配置 | 备用接口的 Key（只服务内置默认接口） |
| `ai_fallback_base_url` | 未配置 | 备用接口地址 |
| `ai_fallback_model` | `gpt-4o-mini` | 备用接口的模型名 |
| `ai_log_channel` | 不发送 | AI 审查总日志频道 |
| `ai_log_topic` | 不发送 | 上面那个频道的话题 id |
| `ai_db_channel` | 不发送 | `/spam` 判定写库后的通知频道 |
| `ai_db_topic` | 不发送 | 上面那个频道的话题 id |

主人和管理员不在插件段里，它们是宿主全局角色，写在 `[roles]`。

## 命令

| 命令 | 说明 | 场景 | 权限 |
| --- | --- | --- | --- |
| `/adconfig [配置项]` | 看本群状态；带配置项则切换开关 | 群内 | 群管理员 |
| `/setlogchannel <channel_id>` | 绑定本群日志频道 | 群内 | 群管理员 |
| `/whitelist add\|rm <ID\|@username\|reply>` | 加白名单，跳过所有检测 | 群内 | 群管理员 |
| `/rmwhitelist <ID\|@username\|reply>` | 移出白名单 | 群内 | 群管理员 |
| `/spam [user_id\|@username]` | 对这条消息做一次 AI 判定 | 群内 | 所有人 |
| `/qban <tgid\|@username> [limit]` | 查这个人的封禁记录 | 任意 | 所有人 |
| `/qadid <ID>` | 按 `_id` 查黑名单条目 | 私聊 | 管理员 |
| `/delad <ID>` | 按 `_id` 删黑名单条目 | 私聊 | 管理员 |
| `/addkw <ads\|bio> [r18] <关键词…>` | 加关键词，同组按 AND 命中 | 私聊 | 管理员 |
| `/editkw <_id> <关键词…>` | 按 `_id` 改一条关键词 | 私聊 | 管理员 |
| `/addfwdbl <chat_id\|@username>` | 把来源频道或群加进转发黑名单 | 私聊 | 管理员 |
| `/rmfwdbl <chat_id>` | 从转发黑名单里移除 | 私聊 | 管理员 |
| `/adkew add\|rm\|list [关键词]` | 管理触发 AI 审核的全局关键词 | 私聊 | 主人 |
| `/admatch <文本>` | 看这段文本命中了哪些关键词 | 私聊 | 主人 |
| `/adaiadd <文本>` | AI 判一段文本，超过 3 个词就入库 | 私聊 | 主人 |
| `/adaiset add\|rm\|list [chat_id]` | 授权群使用内置 AI 接口 | 私聊 | 主人 |
| `/start aiConfig_<chat_id>` | 私聊三步向导配置本群 AI 接口 | 私聊 | 目标群管理员 |

群内 `/adconfig` 的「配置 AI 接口」按钮会跳到私聊，走三步向导（API Key、后端地址、模型名），最后自动测一次连通性。

## 使用要点

- Bot 需要「删除消息」和「限制成员」权限；没有权限时群内不会有任何动作。
- `/spam` 不限制发起人，群里任何人都能举报；举报管理员或白名单用户会被拒绝。
- 私聊命令在群里发会被静默忽略，不回复也不提示。
- 自动点赞默认关闭，由群管理员用 `/adconfig like_guard` 打开。误判时点提示上的「解除限制」即可恢复。
- AI 调用失败不会静默放行，日志里会写明原因；消息按未命中处理，需要时人工复核。
- 想知道某条消息为什么没被检查，看日志里的 `[检查跳过]`，会写明是缺权限、聊天类型不对，还是发送者在白名单里。

## 数据

数据都在 MongoDB。集合和索引在启动时自动创建，已存在的跳过。`db` 换到已有数据的库会直接沿用其中的集合；换到空库会新建，不会导入旧数据。

## 来源与许可

本插件从 FuyuBot TS 版的 adblock 插件重写而来，检测流程、命令、提示文案、AI 提示词和集合结构都沿用原版。

MIT OR Apache-2.0
