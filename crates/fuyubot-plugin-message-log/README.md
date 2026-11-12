# fuyubot-plugin-message-log

FuyuBot 的**消息日志插件**（Rust，cdylib）。订阅 `updateNewMessage`，把收到的新消息
拼成一行，经 `host.log` 打到控制台、TUI 日志页和 `logs/fuyubot.log`。

- 插件 id：`fuyubot.message-log`
- ABI：**v6**（`fuyubot-abi` 0.1.3+ / FuyuBot 0.1.4-rc.1+）
- 无依赖：不注册命令、不发消息、不改消息，也不依赖其它插件
- 产物：`fuyubot_plugin_message_log.{dll,so,dylib}`

它对应旧 TS 框架的 [`plugins/message_log.ts`](../../../FuyuBot/plugins/message_log.ts)，
但**输出交给框架的日志系统**，不再自带 pino 和按日轮转的 `logs/message.*.log`。

## 输出长什么样

宿主会在正文前面补上时间、来源与级别，所以控制台 / 日志页里是：

```text
2026-11-11 13:45:00 [plugins - fuyubot.message-log] INFO  [开发群(id:-1001234567890)][张三(id:123456)]: 你好
2026-11-11 13:45:02 [plugins - fuyubot.message-log] INFO  [Private][李四(id:654321)][reply_chat:654321][reply_msg:42]: 收到\n第二行
2026-11-11 13:45:05 [plugins - fuyubot.message-log] INFO  [公告频道(id:-1009876543210)][频道名(id:-1009876543210)][topic:forum:7][forward_from_channel:-100111…][forward_msg_id:99]: [图片 file_id:AQAD…] 活动海报
```

正文的拼法（与 TS 版同源）：

```text
[聊天][发送者][回复/引用/转发/话题…]: 正文
```

| 片段 | 规则 |
| --- | --- |
| 聊天 | 私聊固定 `Private`（TS 版行为）；群 / 频道 `标题(id:…)` |
| 发送者 | 用户 `名字(id:…)`；频道 / 匿名管理员 `标题(id:…)` |
| 回复 | `[reply_chat:…][reply_msg:…]`；有 `quote` 时再加 `[quote]` 与 `quote_from_*` |
| 转发 | `[forward_from_user:…]`、`[forward_from_channel:…][forward_msg_id:…]` 等 |
| 话题 | `[topic:thread:…]` / `[topic:forum:…]` / `[topic:direct:…]` / `[topic:saved:…]` |
| 正文 | 文本原样；媒体给 `[图片 file_id:…]`、`[视频 00:12 file_id:…]` 这类标签，再接 caption |

## 装

把 zip 解压到机器人的插件目录（或走 TUI 商店）：

```text
plugins/
└── fuyubot.message-log/
    ├── plugin.json
    └── bin/fuyubot_plugin_message_log.dll
```

Windows 用 `.dll`，Linux / Android 用 `.so`，macOS 用 `.dylib`。启动后日志里会出现：

```text
消息日志已就绪：记录自己发的消息=true，正文上限=4096 字符
```

## 配置

所有项都可选；不配也能启动。写错类型不会让插件失败，只会退回默认值并打一条 WARN。

```toml
[plugin.fuyubot.message-log]
enabled = true          # false = 插件照常加载，但不订阅、什么都不打
ignore_outgoing = false # true = 不记录本账号自己发出的消息
max_text_chars = 4096   # 单条正文最多保留多少字符；0 = 不截断
```

> 日志门槛由宿主统一控制：`[plugins].log_level`（默认 `info`）。
> 本插件只打 `INFO`；调成 `warn` 后消息日志会一起消失，需要时调回来即可。
> 想只保留本插件的日志，可以用 `fuyubot run --log-plugin fuyubot.message-log`。

## 覆盖的消息

| 类型 | 输出 |
| --- | --- |
| 文本 / 富消息 | 文本本体（富消息递归收集纯文本块） |
| 图片 / 视频 / 动画 | `[图片 file_id:…]`、`[视频 00:12 file_id:…]` | 
| 文件 / 音频 / 语音 / 视频消息 | `[文件 报表.pdf file_id:…]`、`[音频 晴天 - 周杰伦 03:45 file_id:…]` |
| 贴纸 / 动态表情 | `[贴纸 🙂 id:… set_id:… file_id:…]` |
| 联系人 / 位置 / 地点 | `[联系人 张三 +86…]`、`[位置 31.23,121.47]`、`[地点 外滩 中山东一路]` |
| 投票 / 骰子 / 游戏 / 故事 / 通话 | `[投票 问题]`、`[骰子 🎲 6]`、`[游戏 …]`、`[故事 id:…]`、`[通话 视频 00:30]` |
| 过期 / 不支持 / 未来新增类型 | `[图片已过期]`、`[不支持的消息类型]`、`[messageXxx]`，**整条不丢** |

回复 / 引用 / 转发 / 话题的标签不依赖正文类型，媒体消息同样会带。

## 与 TS 版的差异

| | TS 版 `message_log.ts` | 本插件 |
| --- | --- | --- |
| 输出 | `console.log`（彩色）+ pino 按日轮转文件 | `host.log`：控制台、TUI 日志页、`logs/fuyubot.log` 一次写全 |
| 时间戳 | 正文里再打一遍消息时间 | 宿主日志行已有时间，正文不再重复 |
| 换行 | 想转义 `\n`，实际只替换了字面量 `\\n` | 真正的换行 / 回车 / 制表符都转成 `\n` `\r` `\t` |
| 控制字符 | 原样输出（可注入 ANSI） | 转义成 `\u{…}`，一条消息只占一行 |
| 名字 | `first_name + last_name`（无空格），取不到会拼出 `undefined…` | `first last`（有空格），取不到回退「未知用户」 |
| 聊天身份 | 频道发言缺 `: ` 分隔 | 统一 `[标签]: 正文` |
| 话题 | 认不出的话题类型会把回复 / 转发标签清空 | 只追加，不清空 |
| 正文上限 | 无 | `max_text_chars` 可配，按字符截断并注明原长度 |
| 线程 | 回调里直接 `await` TDLib | 回调只入队，工作线程顺序处理；队列满时丢弃并告警 |
| 开关 | 无 | `enabled` / `ignore_outgoing` / `max_text_chars` |

## 线程与生命周期

```text
宿主接收循环线程
  └─ on(updateNewMessage)：只 try_send（队列上限 1024，满了丢并告警）
        └─ messagelog-worker（current-thread tokio）
             ├─ getChat / getUser（按 id 缓存 10 分钟；失败负缓存 30 秒）
             └─ render → host.log(INFO, 一行)
stop()：退订 → 置停机标志 → 断开队列 → 处理完当前这条 → join
```

- 私聊不查 `getChat`（`chat_id > 0` 固定 `Private`），少一次 TDLib 往返；
- 所有 TDLib 请求都有 3 秒超时；停机只等**当前这一条**（最多两个超时），
  队列里还没处理的直接丢弃，不会被上千条积压拖住；
- 工作线程不持有发送端，`recv()` 能看到 `Disconnected`，不会在 `join` 上死锁；
- 名字缓存超过 4096 条整表清空，避免随「见过的用户数」无限涨。

## 开发

```bash
# 在仓库根目录；fuyubot-abi 走 ../FuyuBot 的 path 依赖
cargo test -p fuyubot-plugin-message-log
cargo build -p fuyubot-plugin-message-log --release
python scripts/package.py --plugin fuyubot-plugin-message-log --platform windows-x86_64 --version 0.1.0
```

单测只覆盖纯函数（渲染、转义、截断、配置、缓存），不需要 TDLib / 宿主。

## 许可

MIT OR Apache-2.0。
