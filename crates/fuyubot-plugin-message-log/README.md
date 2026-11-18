# fuyubot-plugin-message-log

FuyuBot 的消息日志插件。订阅 `updateNewMessage`，把新消息拼成一行，经 `host.log` 打到控制台、TUI 日志页和 `logs/fuyubot.log`。

不注册命令、不发消息、不改消息，也不依赖其它插件。

## 输出

宿主会在正文前补时间、来源和级别：

```text
2026-11-11 13:45:00 [plugins - xiaoqvan.message-log] INFO  [开发群(id:-1001234567890)][张三(id:123456)]: 你好
2026-11-11 13:45:02 [plugins - xiaoqvan.message-log] INFO  [Private][李四(id:654321)][reply_chat:654321][reply_msg:42]: 收到\n第二行
2026-11-11 13:45:05 [plugins - xiaoqvan.message-log] INFO  [公告频道(id:-1009876543210)][频道名(id:-1009876543210)][topic:forum:7][forward_from_channel:-100111…][forward_msg_id:99]: [图片 file_id:AQAD…] 活动海报
```

正文的拼法是 `[聊天][发送者][回复 / 引用 / 转发 / 话题…]: 正文`。媒体消息会带 `[图片 file_id:…]`、`[视频 00:12 file_id:…]` 这类标签，再接 caption。

## 安装

```text
plugins/
└── xiaoqvan.message-log/
    ├── plugin.json
    └── bin/fuyubot_plugin_message_log.dll
```

Windows 用 `.dll`，Linux / Android 用 `.so`，macOS 用 `.dylib`。

## 配置

配置段 `[plugin.xiaoqvan.message-log]`，全部可选：

```toml
[plugin.xiaoqvan.message-log]
enabled = true          # false = 插件照常加载，但不订阅、什么都不打
ignore_outgoing = false # true = 不记录本账号自己发出的消息
max_text_chars = 4096   # 单条正文最多保留多少字符；0 = 不截断
```

| 键 | 默认 | 说明 |
| --- | --- | --- |
| `enabled` | `true` | `false` = 插件照常加载但不订阅消息 |
| `ignore_outgoing` | `false` | `true` = 不记录自己发出的消息 |
| `max_text_chars` | `4096` | 正文最多保留多少字符，`0` = 不截断 |

日志级别由宿主的 `[plugins].log_level` 控制，本插件只打 `INFO`。想只看本插件的日志，用 `fuyubot run --log-plugin xiaoqvan.message-log`。

## 覆盖的消息

文本和富消息输出正文；图片、视频、动画、文件、音频、语音、贴纸、联系人、位置、地点、投票、骰子、游戏、故事和通话会输出对应的标签；过期或不支持的类型也会留一个占位，整条消息不丢。

回复、引用、转发和话题的标签和正文类型无关，媒体消息同样会带。

## 注意

- 换行、回车和制表符转成 `\n` `\r` `\t`，一条消息只占一行。
- 名字优先用缓存的 `updateNewChat` / `updateChatTitle` / `updateUser`，取不到才查 TDLib。
- 消息多时回调只入队，队列满了会丢弃并告警，不会把接收线程卡住。

## 许可

MIT OR Apache-2.0
