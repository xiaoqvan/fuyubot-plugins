# fuyubot-plugin-douyin

FuyuBot 的抖音解析插件。聊天里出现抖音分享链接，或者发 `/dy <链接>`，插件就把视频或图集发出来，去水印。

- 插件 id：`xiaoqvan.douyin`
- 命令：`/dy <链接>`
- 自动识别：消息里含 `douyin.com` / `iesdouyin.com` 链接就解析

## 命令

| 命令 | 说明 | 场景 | 权限 |
| --- | --- | --- | --- |
| `/dy <链接>` | 解析抖音分享链接，发视频或图集 | 私聊 / 群 / 频道 | 所有人 |

聊天里直接贴链接也会解析，`/` 开头的消息不抢。普通会话新发一条消息，不引用原消息；频道帖编辑原帖。

## 安装

```text
plugins/
└── xiaoqvan.douyin/
    ├── plugin.json
    └── bin/fuyubot_plugin_douyin.dll
```

Windows 用 `.dll`，Linux / Android 用 `.so`，macOS 用 `.dylib`。

## 配置

抖音分享页在拿不到 `ttwid` 等身份信息时不会返回作品数据，所以必须配 cookie。写在 `fuyubot.toml`：

```toml
[plugin.xiaoqvan.douyin]
cookies = "ttwid=…; msToken=…; odin_tt=…"
```

`cookies` 就是浏览器里复制出来的整串 `Cookie` 请求头。别名 `cookie` 也认，两个都写时 `cookies` 优先；空串按没配处理。

| 键 | 默认 | 说明 |
| --- | --- | --- |
| `cookies` | 空 | 整串 Cookie 请求头，解析抖音必需 |
| `cookie` | 空 | 上面那个键的别名，`cookies` 优先 |

旧版 `plugins/douyin/cookies.json` 仍然兼容，只在配置里没写时才会读，日志会提示迁移。改完重启宿主生效。

## 频道帖

频道里 Bot 通常没有发言权，只有编辑权，所以按编辑原帖处理：解析前把原帖改成「原文 + 正在解析…」，视频或图集视频用 `editMessageMedia` 替换；图集先删原帖再发相册；失败时编辑原帖写错误，不另发消息。

编辑前会查 `can_edit_media` / `can_be_edited`，没有权限就整段跳过，不解析也不写占位。

## 缓存

装了 `fuyubot.sqlite` 时，发送成功后按作品 id 缓存 TDLib 文件 id 和文案。下次同一个作品直接用文件 id 重发，不再解析、不再下载。缓存失效时删掉记录，自动回落到完整解析，用户看不到错误。

没装 sqlite 也能正常用，只是没有缓存加速。

## 解析方式

优先调用 detail API（带原生签名），失败时回退分享页的 `_ROUTER_DATA`，两条路给出同样的结果。签名代码在 `src/sign/`。

## 注意

- 群里「发送人是频道身份且这条是转发消息」时跳过自动识别，普通成员转发照常解析。
- 自己发的消息不解析，频道帖除外。
- 下载是流式写盘，大文件不会占满内存。

## 来源与许可

签名部分移植自 [Evil0ctal/Douyin_TikTok_Download_API](https://github.com/Evil0ctal/Douyin_TikTok_Download_API) 的 `dtk/signing/native/`（Apache-2.0），与 Python 版做了逐字节对拍。其余代码在本仓库实现。

MIT OR Apache-2.0
