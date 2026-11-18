# fuyubot-plugin-x

FuyuBot 的 X / Twitter 解析插件。聊天里出现 X 链接，或者发 `/x <链接>`，插件把帖子或长文解析出来。默认渲染成一张 X 帖子风格的卡片：正文 + 最多两张图（视频只画封面），配文只放原链接；没装 html2img 或关掉 `image_card` 时，按相册 / 文字发送。

- 插件 id：`xiaoqvan.x`
- 命令：`/x <链接>`
- 软依赖：`fuyubot.html2img`（没有它时退回相册 / 文字发送）

## 能做什么

| 输入 | 输出 |
| --- | --- |
| 带图片或视频的帖子 / 文章 | 一张卡片：正文 + 最多前 2 张图（视频画封面），配文是原链接 |
| 没有媒体的纯文本文章 | 一张卡片（装了 html2img 时） |
| 没装 html2img，或出图失败 | 原来的文字 + 相册发送 |
| 只有文字、没有媒体的推文 | 文本消息，不渲染卡片 |

解析用 [FxTwitter / FxEmbed](https://docs.fxembed.com/api/introduction) 的公开只读接口，不需要 token。

## 命令

| 命令 | 说明 | 场景 | 权限 |
| --- | --- | --- | --- |
| `/x <链接>` | 解析 X 帖子或文章 | 私聊 / 群 / 频道 | 所有人 |

聊天里贴链接也会自动解析；`/x@别的Bot` 不抢，裸 `/x` 和 `/x@自己` 都响应。

## 安装

```text
plugins/
├── xiaoqvan.x/
│   ├── plugin.json
│   └── bin/fuyubot_plugin_x.dll
└── fuyubot.html2img/        ← 软依赖，要卡片图才需要
    ├── plugin.json
    └── bin/fuyubot_plugin_html2img.dll
```

Windows 用 `.dll`，Linux / Android 用 `.so`，macOS 用 `.dylib`。

## 配置

配置段 `[plugin.xiaoqvan.x]`，全部可选：

```toml
[plugin.xiaoqvan.x]
api_base = "https://api.fxtwitter.com"
auto_parse = true
album_limit = 10
max_media = 100
send_text = true
text_chunk = 3500
send_timeout_ms = 120000
image_card = true
fallback_text = true
card_width = 512
card_scale = 2.0
card_format = "jpg"
card_dark = false
card_max_chars = 6000
```

| 键 | 默认 | 说明 |
| --- | --- | --- |
| `api_base` | `https://api.fxtwitter.com` | 解析接口根地址，可换成自建实例 |
| `auto_parse` | `true` | 聊天里的 X 链接自动解析 |
| `album_limit` | `10` | 相册每组个数，夹到 2–10 |
| `max_media` | `100` | 一条文章最多发多少个媒体，超出截断并记 WARN |
| `send_text` | `true` | 退回相册发送时，有媒体是否再单独发正文 |
| `text_chunk` | `3500` | 单条文字消息的字符上限，夹到 200–4000 |
| `send_timeout_ms` | `120000` | 一次发送的等待预算，夹到 5000–600000 |
| `image_card` | `true` | 是否把帖子渲染成卡片（图片最多两张，视频只画封面） |
| `fallback_text` | `true` | 纯文本文章出图失败时补发文字；有媒体的帖子会回退到相册 |
| `card_width` | `512` | 卡片宽度，夹到 240–1600 |
| `card_scale` | `2.0` | 输出倍率，夹到 0.5–3.0 |
| `card_format` | `jpg` | `jpg` / `png` |
| `card_dark` | `false` | 深色卡片 |
| `card_max_chars` | `6000` | 卡片最多画多少字符，超出截断并提示看原链接 |

## 相册分组

卡片关闭、没有 html2img 或出图失败时，才按这里的相册规则发原图。Telegram 的相册只接受 2 到 10 项：1 项走单条消息，2 到 10 项一条相册，超过 10 个每 10 个一条，最后不足 2 个的那组走单条。比如 23 张图会发成 10 + 10 + 3 三条相册。

标题只挂在第一条上，因为相册只有首项能带 caption。文章正文太长，装不进 caption，所以正文单独发，相册标题只留「标题 + 作者 + 原链接」。

## 卡片

整条帖子会排成一份自包含 HTML：作者头像，右边是名字、认证，@handle 在下一行；非中文帖子会先调谷歌机翻：原文保留在上面，下面用分隔线隔开，接一行 Google 图标 + 「翻译自 英语」+「原文对照」，再画中文译文；谷歌不通时只显示原文和「显示翻译」入口。正文里的 URL、#话题、@用户名 都是蓝色链接，URL 不显示 `http(s)://` 前缀。正文下面是媒体，再下面是「8:52 · 2026年11月17日 · 6.6万 浏览」，底部是评论、转发、点赞、收藏、分享（接口返回多少画多少）。

媒体最多画两张：一张图按原比例，宽度上限 450、竖图限高 460 后收窄；两张图各占 45.8% 宽、间距 6px、限高 520；视频只用封面（带播放键），GIF 用静态封面并带 GIF 角标，没有封面的视频不画。

右上角只有「···」，不画 X 标志——那是 X 的顶栏，不属于推文。超过两张的帖子只显示前两张，剩下的图片 / 视频不会再单独发——要保留原图或视频，把 `image_card` 关掉。

卡片消息的配文只有原链接，正文已经在图里，不再重复。

卡片最下面一行是发送者署名：分隔线左右铺满，内容居中，`@` 旁边带 bot 头像，`fuyubot_plugin` 前带 16px 插件图标，文案是 `@当前bot · fuyubot_plugin.xiaoqvan.x`。bot 用户名每次出图前走宿主的 `fuyubot.host.identity` 服务取（`usernames` 的第一个），所以改名后会跟着变；头像从 `getMe` 的 `profile_photo.small` 取：`local.is_downloading_completed` 且 `local.path` 存在就直接读，没下载就调 `download_file` op 下载后再读，图片转成 base64 内嵌 SVG（圆形裁剪），按 file id 缓存。没有头像或下载失败就不画头像。插件图标是 `assets/plugin-icon-inline.svg`：上传图裁成 64px 头像后以 base64 PNG 内嵌进 SVG，再用 `include_str!` 带进二进制，不依赖外部图床。没登录 / 回放拿不到身份时只留插件标识。

### 外部接口

插件通过进程级 service `xiaoqvan.x.render` 把卡片版式借给其他插件：

- `html`：入参 `{"api": <FxTwitter JSON>}`，返回 `{"ok":true,"html":"..."}`
- `render`：同一份入参，调 `fuyubot.html2img` 出图，返回 `{"ok":true,"path":"..."}`

入参也可以直接是 FxTwitter 根对象，或 `{"api":"<JSON 字符串>"}`。失败统一返回 `{"ok":false,"error":"..."}`。服务不机翻、不加 bot 署名的 footer，只复用卡片本体；`render` 需要 `fuyubot.html2img`，`html` 不依赖它。返回的 path 是本机文件路径，调用方直接拿它发图即可。

```rust
let input = serde_json::json!({ "api": api }).to_string();
let reply = host.call_service("xiaoqvan.x.render", "render", &input)?;
```

### 本地预览

两个 example 都不用连宿主：

```bash
# 生成 HTML，用浏览器看版式
cargo run -p fuyubot-plugin-x --example preview -- card.html

# 真的渲染成 PNG（与宿主里同一个 hyper-render）
cargo run -p fuyubot-plugin-x --example render_card -- \
    crates/fuyubot-plugin-x/tests/fixtures/status_article.json target/card.png
```

第二个需要 `hyper-render`（挂在 `[dev-dependencies]`，不进插件产物）。渲染时可能看到
`ICU4X data error: No segmentation model for complex script`——那是 hyper-render 内置
ICU4X 缺中日韩分词模型，只影响断行细节，字符照常渲染（html2img 出中文命令表时也会打这条）。

## 自动识别的过滤

- 自己发的消息不解析，频道帖除外。
- 一条消息里有多个链接时只认第一条 X 链接。
- 群里「发送人是频道身份且是转发消息」时跳过。
- 域名按白名单判断，`x.com.evil.test` 这类不会误认。

## 支持与不支持

支持普通推文的图片、视频、GIF；X 文章的长文块、正文图片视频、头图，以及加粗、斜体、行内代码、@ 提及和话题。嵌入推文在文字消息里是链接，在卡片里是一个带边框的小块。

不支持需要登录才能看的内容、转推和引用推文的原文展开、投票、直播和外链卡片。频道帖一律新发一条消息，不编辑原帖。

## 来源与许可

解析接口是 FxTwitter / FxEmbed 的公开 API。卡片渲染交给本仓库的 `xiaoqvan.html2img`。

MIT OR Apache-2.0
