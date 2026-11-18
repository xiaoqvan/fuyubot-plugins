# fuyubot-plugin-music

网易云音乐、汽水音乐和 YouTube Music 的搜索与解析。发命令，或者直接在聊天里贴分享链接。

- 插件 id：`xiaoqvan.music`
- 命令：`/music163`、`/soda`、`/ytmusic`
- 软依赖：`fuyubot.sqlite`（文件 id 缓存，没装也能用）

## 命令

| 命令 | 说明 |
| --- | --- |
| `/music163 <关键词>` | 搜网易云，回前 8 条（带歌曲 ID） |
| `/music163 <链接或 ID> [音质]` | 解析网易云作品并发音频 |
| `/soda <关键词>` | 搜汽水，回前 8 条（带 track ID） |
| `/soda <链接或 ID> [音质]` | 单曲发音频；UGC 视频发视频 |
| `/ytmusic <关键词>` | 搜 YouTube Music，回前 8 条 |
| `/ytmusic <链接或 ID> [音质]` | 单曲发音频；播放列表回前 10 首 |

聊天里贴 `music.163.com`、`163cn.tv`、`qishui.com`、`qishui.douyin.com`、`music.youtube.com` 的链接会自动按默认音质解析。普通 `youtu.be` / `youtube.com` 视频不自动抢，要用 `/ytmusic <链接>`。`/` 开头的消息不抢，媒体说明文字里的链接也不解析。

链接支持完整分享链接、短链、汽水短链的落地页和纯数字 ID。

## 安装

```text
plugins/
└── xiaoqvan.music/
    ├── plugin.json
    └── bin/fuyubot_plugin_music.dll
```

Windows 用 `.dll`，Linux / Android 用 `.so`，macOS 用 `.dylib`。

## 配置

配置在 `fuyubot.toml` 的 `[plugin.xiaoqvan.music]`，一个段、键平铺：

```toml
[plugin.xiaoqvan.music]
# 整段 Cookie 头
netease = "MUSIC_U=…; os=pc; appver=3.1.28.205001"
soda = "sessionid=…; sid_tt=…"
youtube = "SAPISID=…; SID=…"

# 汽水的设备指纹和签名头（可选；匿名也能用）
# soda_device_id = "16 位数字"
# soda_iid = "16 位数字"
# soda_fp = "一般等于 soda_device_id"
# soda_x_helios = "抓包得到的 x-helios"
# soda_x_medusa = "抓包得到的 x-medusa"
# soda_user_agent = "LunaPC/3.3.0(359450208)"
# soda_signer_url = "http://127.0.0.1:8899/sign"
# soda_signer_token = "可选：签名器的 Bearer token"
```

| 键 | 默认 | 说明 |
| --- | --- | --- |
| `netease` | 空 | 网易云 Cookie 头，不填只能拿匿名可播曲目 |
| `soda` | 空 | 汽水 Cookie 头，用 `scripts/qishui_qr_login.py` 扫码获取 |
| `soda_device_id` | 自动生成 | 汽水设备号，要和抓包那台机器一致 |
| `soda_iid` | 自动生成 | 汽水安装 id |
| `soda_fp` | 同 `soda_device_id` | 汽水设备指纹 |
| `soda_x_helios` | 空 | 抓包得到的签名头，会员整曲用 |
| `soda_x_medusa` | 空 | 另一枚签名头，和上面成对出现 |
| `soda_user_agent` | `LunaPC/3.3.0(359450208)` | 汽水客户端 UA |
| `soda_signer_url` | 空 | 外部签名器地址，比静态头稳定 |
| `soda_signer_token` | 空 | 签名器的 Bearer token |
| `youtube` | 空 | YouTube Cookie 头，只为绕开机器人校验 |

空串按没配处理。配置里有的平台不会再读旧文件；某个平台没配才会回退到 `plugins/music/*.json`，日志里会写明用的哪个来源。

完整的登录和 cookie 获取步骤见 [LOGIN.md](LOGIN.md)。

## 音质

| 平台 | 可选音质 | 默认 |
| --- | --- | --- |
| 网易云 | `standard` / `higher` / `exhigh` / `lossless` / `hires` / `jymaster` / `sky` | `exhigh` |
| 汽水 | `best` / `hires` / `lossless` / `dolby` / `highest` / `medium` / `low` | `best` |
| YouTube Music | `best` / `medium` / `low` | `best` |

别名也认：`sq`、`flac`、`无损`、`hr`、`320k`、`hq`、`母带` 等。接口实际给的档位可能比请求的低（版权或会员限制），文案里显示的是实际档位。

## YouTube Music

- 支持 `music.youtube.com/watch?v=`、`youtu.be/`、`youtube.com/watch?v=`、`/shorts/`、`/live/` 和裸 11 位 ID。
- 搜索走 music.youtube.com 的 WEB_REMIX。
- 播放列表只回前 10 首，不发整个列表。
- 取流按 ANDROID_VR → IOS → ANDROID 三个客户端依次尝试，不需要 PO token 或签名解密。
- 数据中心 IP 上可能遇到「Sign in to confirm you're not a bot」，可以配 `youtube` cookie，或者换住宅网络。

## 缓存

装了 `fuyubot.sqlite` 时，发送成功后把 TDLib 文件 id 和文案写进 `media_cache`。下次同一个「平台 + 作品 id + 音质」直接重发，不再请求接口、不再下载。

缓存失效时删掉记录并自动回落完整流程，用户看不到错误；缓存写入失败也不影响发送。

## 频道帖与群聊

普通会话新发一条消息，不引用原消息。频道帖编辑原帖：先写「正在获取音乐…」，解析完用 `editMessageMedia` 替换；搜索结果、用法和错误也写回原帖。编辑前会验证权限，没有权限就跳过。

群里「发送人是频道身份且这条是转发消息」时跳过自动识别，普通成员转发照常解析。

## 注意

- 网易云 VIP 音质需要有效的 `MUSIC_U`；匿名请求 VIP 曲目会返回空地址，插件会提示没登录、不是 VIP 或版权受限。
- 汽水会员曲目的整曲依赖签名器或抓包得到的签名头，静态头会过期。
- 没有签名头时免费曲目可以整曲，会员曲目通常只有几十秒试听，文案里会标出来。
- 视频按 Telegram 单文件 2GB 检查，下载是流式写盘。
- 汽水 UGC 视频如果被标成付费或地区受限，分享页给的可能是试听片段。

## 来源与许可

网易云的 eapi 接口形状参考 TS 版 `plugins/163music/music.ts`。汽水的接口和音质档位参考 [sodahub-org/libresoda](https://github.com/sodahub-org/libresoda)，代码为本仓库重写，没有引入它的 AGPL 源码。YouTube Music 走 InnerTube 的 ANDROID_VR / IOS / ANDROID 客户端。

MIT OR Apache-2.0
