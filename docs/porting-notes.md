# 从 TS 版 adblock 迁移到 Rust 的记录

这份文档是给「以后要改这个插件」的人看的：哪些是原样搬的、哪些故意改了口径、
遇到旧数据怎么接。

## 1. 模块对照

| TS 版 | Rust 版 | 说明 |
| --- | --- | --- |
| `index.ts` | `lib.rs` + `commands/mod.rs` + `callbacks/mod.rs` | 命令/更新/回调三张表 |
| `index.ts` 的 `cmdHandlers` | `commands/spec.rs` | 17 条命令的 scope/permission/`/help` 描述（单一真相） |
| `Ads/index.ts` | `flow/message.rs` + `action.rs` | 判定流水线与处置 |
| `Ads/message*.ts` | `detect.rs` | 六种内容检测 |
| `Database/query.ts` 等 | `store.rs` | 集合、查询、索引 |
| `cmd/*.ts` | `commands/*.rs` | 17 个命令 |
| `CallbackQuery/*.ts` | `callbacks/*.rs` | 7 种按钮 |
| `function/aiAdReview.ts` | `ai/mod.rs` | 接口调用 + 结果校验 + 总日志 |
| `function/buildSystemPrompt.ts` | `ai/prompt.rs` | 提示词拼装（`include_str!`） |
| `function/prompts/**` | `ai/prompts/**` | **原样复制** |
| `utils/newMemberCheck.ts` | `flow/new_member.rs` | 入群检查 |
| `utils/profileChangeWatch.ts` | `flow/profile.rs` | 24h 资料复检 |
| `utils/muteReviewSweeper.ts` | `flow/sweeper.rs` | 超时封禁 |
| `update/index.ts` | `flow/records.rs` | 消息索引 |
| `update/gkyBanForward.ts` | `flow/gky.rs` | GKY 封禁转发 |
| `utils/messageCache.ts` / `userMessageHistory.ts` | `cache.rs` | 内存缓存 |
| `utils/richText.ts` | `text.rs` | 富消息取文本 |
| `utils/sender.ts` | `flow/mod.rs::sender_details` | 发送者画像 |
| `utils/unbanWhitelist.ts` | `api.rs::unban_callback` | 回调解封数据 |
| `utils/aiCheckLog.ts` | `ai/mod.rs::write_log` | AI 总日志 |
| `Database/lowdb.ts`（全局触发词） | `adblock_settings` 集合 | 从本地文件搬进库 |

## 2. 数据兼容

**集合名、字段名全部沿用**，包括拼错的 `flieId_blacklist`。所以：

- 把本插件的 `db` 配成 TS 版那个库，`adb_config` 里的群配置、黑名单、封禁记录直接生效；
- 时间字段仍是 BSON `Date`：写入用 `{"$date":{"$numberLong":"…"}}` 的 Extended JSON，
  读取同时兼容 relaxed（ISO 字符串）、canonical v2（`$numberLong`）和 v1（毫秒数字）；
- `_id` 由 Mongo 生成（ObjectId）。原版里个别「手写字符串 `_id`」的老记录，
  `/qadid` 与 `/delad` 会先按 ObjectId 查、再按字符串查。

TS 版里唯一的「非 Mongo」数据是全局 AI 触发词（lowdb 文件）。新版放进
`adblock_settings`（`_id = "global_ai_keywords"`），需要迁移就手工把 JSON 里的数组
抄过去（`/adkew list` 能看到当前值）。

## 3. 故意改掉的口径

| 位置 | TS 版 | 本版 | 原因 |
| --- | --- | --- | --- |
| 内置 AI 接口 | 源码里硬编码两把 Key + 写死的授权群列表 | 只读配置；授权群用 `ai_default_chats` + `/adaiset` | 密钥不进仓库；写死的群是某次部署的产物 |
| 「Bot 管理员」权限档 | 框架 `admin[]` / `super_admin` | 插件配置 `owner_ids` / `admin_ids` | FuyuBot 没有框架级管理员名单；`admin` 档 = 主人 ∪ `admin_ids` |
| 命令 scope/permission 校验 | 框架 `PluginValidator.ts`（`cmdHandlers` 上的元数据） | `commands/spec.rs` 一张表 + `dispatch` 里校验 | 宿主没有命令权限层，只能在插件内补；拒绝行为与 TS 一致（静默） |
| `/help` 命令列表 | 框架自动枚举 `cmdHandlers` | 登记到宿主 `fuyubot.help.commands`（软依赖） | 没装 help 插件时只是 `/help` 少几行，不影响命令本身 |
| 总日志频道 | 写死 `-1003040986800` 的固定话题 | `log_channel` / `log_topic`，默认关闭 | 换人部署不该骚扰别的群 |
| `/setlogchannel` | 测试消息失败也写配置并回「成功」 | 只有测试消息真发出去才写 | 明显的 bug |
| `/start aiConfig_…` 的默认后端 | `https://api.openai.com/v1` | 同 | — |
| 链接预览 | `link-preview-js` | 自己抓 HTML 的 `<title>` / description | 少一个依赖；够 AI 判断 |
| 数据库连接 | 宿主私有 db API | `storage.mongo` 服务（JSON 方法） | FuyuBot 的插件模型 |
| 飞书式「delay」参数 | `/adconfig delay` | 同 | 兼容旧用法 |

其余判定阈值（0.95 / 0.9 / 0.8 / 0.75）、复审 1 小时、提示 30 秒自删、5 分钟扫描、
`spam_tips` 默认开、100 条消息一批删除……都按原值保留。

## 4. 线程与生命周期

```text
宿主接收循环线程
  └─ on(updateNewMessage) 回调：只 clone update + 投递任务
        └─ 工作线程池（4 个）
             ├─ TDLib 请求（同步等响应）
             ├─ storage.mongo 调用（同步等回复）
             ├─ AI HTTP（ureq，同步）
             └─ 业务分支
调度器线程
  └─ 提示自删 / 复审扫描 → 到期后交给工作线程执行
```

`stop()`：退订 → 停调度器（丢弃未到期任务）→ join 工作线程。
**顺序不能反**：动态库卸载后线程还在跑库里的代码就是 UB。

## 5. 已知取舍

- 消息类型只覆盖 TS 版支持的六种（文本 / 富消息 / 图片 / 视频 / 联系人 / 贴纸）。
  文档、音频、动画只参与「取文本」与历史缓存，不会触发内容检测（与原版一致）。
- AI 审核是同步阻塞的：一次最长 90 秒，占一个 worker。突发大量可疑消息时后面的消息
  会排队；这是「不拖 async runtime」的代价。
- `getChat` / 管理员权限有 60 秒缓存：刚给 Bot 提权后最多等 1 分钟生效。
- 群配置的写入是「读整份 → 改 → 写回」：两个管理员同时改不同开关时，后写的会覆盖前者
  的一个字段（原版是 Mongo `$set`，没有这个问题）。管理操作频率极低，接受。
