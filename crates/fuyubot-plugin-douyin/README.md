# fuyubot-plugin-douyin

FuyuBot 的抖音解析插件（Rust，cdylib）。从 TypeScript 宿主 `plugins/video_parse`
的**抖音分支**迁来；本仓库（`fuyubot-plugins`）把它和其余六个插件一起按
**ABI v6** 编译、打成 bundle。

```
消息里有抖音链接 ──┐                       ┌─ 命中 SQL 缓存 ─▶ 直接按文件 id 发送（不解析、不下载）
                  ├─▶ detail API（原生签名）┴─失败─▶ 分享页 SSR ─▶ 挑最高清晰度 ─▶ 发送 ─▶ 写 SQL 缓存
/dy <链接> ───────┘
```

- 插件 id：`fuyubot.douyin`，**ABI v6**（`fuyubot-abi` 0.1.3+ / FuyuBot 0.1.4+）
- 命令：`/dy <链接>`（私聊 / 群 / 频道）
- 自动识别：消息里含 `douyin.com` / `iesdouyin.com` 链接就解析（`/` 开头的不抢）；
  群里**发送人是频道身份且这条是转发消息**时跳过（见下）
- 产出：`fuyubot_plugin_douyin.{dll,so,dylib}`

## 依赖

- **编译期只有 `fuyubot-abi`**：本仓库根 `Cargo.toml` 统一给 path 依赖，并带
  `version = "0.1.3"` 守卫（0.1.2 是 ABI v5，硬不兼容）。
- **运行期软依赖**：只有 `fuyubot.sqlite`（文件 id 缓存）。没装时插件照常工作，
  只是没有缓存加速；命令表由宿主预置（`fuyubot.host.commands`），`/dy` 登记时带
  `role = user`、`scopes = private + group + channel`。
- 刻意**不依赖** `fuyubot-simplify`（`publish = false`，外部插件只能 path）：
  「人话参数 → TDLib 请求体」的构造器内联在 `src/simplify.rs`，发送走 ABI 的
  `ResponseRouter::invoke_op`（simplify op 表从 v5 起就有，v6 没改）。

`cargo tree -p fuyubot-plugin-douyin -e normal | grep -i simplify` 必须为空。

## ABI v6 相关

- **服务调用走宿主**：缓存探测用 `host.list_services()`（不再 `get_service` 拿裸
  vtable），调用一律 `host.call_service(...)`——提供方（sqlite / help）因此能拿到
  真实调用者身份。
- **命令回复不引用原消息**：本插件刻意**不用** `fuyubot.host.command` 的 `reply`
  （它会按账号类型给 Bot 账户加 `reply_to`）。用户明确要求不引用原消息，所以
  发送仍走 simplify 的 `send_message` / `send_message_album`，错误提示也一样。

## 执行模型（全部异步）

| 旧 | 新 |
| --- | --- |
| 每个命令 `std::thread::spawn` 一条线程，`stop()` 挨个 `join` | 插件自有 **tokio 多线程 runtime**（4 worker），回调只 `submit` |
| `ureq` 同步阻塞 + `wait_for_update` 干等 TDLib | **`reqwest` 0.13 async** + `ResponseRouter` 的 future（`tokio::time::timeout` 兜底） |
| 下载整份读进内存 | `Response::chunk()` **流式写盘**（块间检查取消标志） |

- 宿主 `on_command` / `updateNewMessage` 回调里只做「克隆参数 + `runtime.submit`」，
  立刻返回——接收循环线程永远不在等网络。
- 所有 TDLib 请求都经 `ResponseRouter` 按 `request_id` 路由回 future；**没有**阻塞等待。
- `stop()` 的顺序：**退订 → 置取消标志 → 关闸并等在飞任务归零（≤30s）→ 关 runtime（5s）**。
  动态库卸载前不会留下跑着本库代码的线程（细节见 `src/runtime.rs` 模块文档）。

## 落点：不引用原消息

发送一律**新发一条**，**不 `reply_to`、不引用原消息**（用户要求去掉引用行为）——
因此当年只为「判群聊 → 决定要不要引用」而存在的 `getChat` 也整个删掉了，
一次解析只剩「短链 GET（可选）→ detail API 或分享页 GET → 发送」这几次请求。

唯一的例外是**频道帖子**：机器人在频道里通常没有发言权、只有编辑权，
所以按 TS 版 `parse/douyin/index.ts` + `utils/videomsg.ts` 的**编辑模式**处理：

1. 解析前 `editMessageText` 把原帖改成「原文 + 分隔线 + 正在解析抖音视频...」；
2. 单条视频 / 单条图集视频 → `editMessageMedia` 编辑原帖；
3. 图集（多图 / 多视频）→ 先 `deleteMessages` 删原帖，再 `sendMessageAlbum` 发相册；
4. 解析失败 → 编辑原帖写 `❌ …`，不往频道另发错误消息；
5. `/dy` 在频道里同样走这套（命令消息就是原帖）。

**编辑前先验证权限**：用 TDLib `getMessageProperties` 查 `can_edit_media` /
`can_be_edited`。没有编辑媒体权限 → **整段跳过**（不解析、不写占位、不编辑、不新发）；
没有文本编辑权限 → 不写占位/报错，真的解析失败只写日志。避免在频道里反复做
必然失败的编辑尝试。

频道帖**不受 `is_outgoing` 限制**：在自己频道里发链接 / 敲 `/dy` 也要生效——
编辑不会产生新的 `updateNewMessage`，不存在「解析自己发的消息」的套娃；
群聊 / 私聊里自己发的消息仍然跳过。

频道判定用 TDLib 的 `message.is_channel_post`（“All messages to channels are channel
posts, all other messages are not channel posts”）：群里匿名频道马甲（发送者也是
`messageSenderChat`）不算频道帖，避免去编辑一条不属于自己的群消息。

## 自动识别的过滤

群里**发送人是频道身份（`messageSenderChat`）且这条消息是转发**时，自动识别直接
跳过——那不是群友自己发的链接，解析它等于替别人的转发链自动产出内容。普通群成员
的转发、频道身份但非转发的消息、以及频道帖子本身都照常解析。

## SQL 缓存（软依赖）

装了 [`fuyubot.sqlite`](https://github.com/CatMoeCircle/fuyubot-plugins) 时，
发送成功后按**作品 id** 缓存 TDLib 文件 id 与最终文案；下次同一个作品**直接
`inputFileId` 重发——不再解析、不再下载**。没装 / 坏了时插件一切照常，只是没有加速。

- 软依赖：`soft_inject = ["fuyubot.sqlite"]`
  （bundle 的 `plugin.json` 里写作 `optional_depends`）；
- 缓存键：`douyin:<作品 id>`（`src/cache.rs` 的 `key`）；
- 文档三种（版本 1，见 `src/cached.rs`）：
  - `{"v":1,"kind":"video","file_id":…,"cover_id":…,"width":…,"height":…,"duration":…,"caption":…}`
  - `{"v":1,"kind":"photos","items":[{"file_id":…,"width":…,"height":…}],"caption":…}`
  - `{"v":1,"kind":"videos","items":[{"file_id":…,"width":…,"height":…,"duration":…}],"caption":…}`
- `caption` 是**最终发出去的 markdown 文本**原样：命中时直接重发，文案与第一次逐字一致；
- 命中后发送失败（文件 id 失效）→ **删缓存 → 自动回落完整解析**，用户看不到错误；
- 缓存写入失败只记日志，**不影响发送结果**。

## 装

本仓库统一打 bundle（`plugin.json` + `bin/`，`plugin.json` 里 `"abi": 6`）：

```bash
cargo build -p fuyubot-plugin-douyin --release
python scripts/package.py --plugin fuyubot-plugin-douyin \
    --platform windows-x86_64 --version 0.1.3
# 解压 dist/fuyubot.douyin-0.1.3-windows-x86_64.zip 到 plugins/fuyubot.douyin/
```

也可以直接装裸动态库：`fuyubot plugin install target/release/fuyubot_plugin_douyin.dll`。

### cookie（配在 `fuyubot.toml` 里）

抖音的分享页在拿不到 `ttwid` 等身份信息时不渲染作品数据，**没有 cookie 就解析不出东西**。
cookie 配在全局配置文件里**本插件自己的段**：

```toml
# fuyubot.toml
[plugin.fuyubot.douyin]
cookies = "ttwid=…; msToken=…; odin_tt=…"
```

- 值就是整串 `Cookie` 请求头（与 TS 版 `cookies.json` 里那个字段一模一样）；
- 别名 `cookie` 也认（`cookie = "…"`），`cookies` 优先；
- 空串视为**没配**，会继续回退旧文件；
- 改完重启宿主生效，不用重编译。

**旧方式仍兼容**：`plugins/douyin/cookies.json`（格式 `{"cookies": "<整串 Cookie 头>"}`，
与 TS 版完全一致）只在配置里没配时才会被读，日志会提示「回退旧文件（建议迁移）」。
两种都没有时插件**照常加载**，只在真正用到时才报错——错误里会写清楚该在
`[plugin.fuyubot.douyin]` 下配什么。

> ⚠️ 插件是宿主进程 `dlopen` 出来的动态库，`current_exe()` 拿到的是 `fuyubot.exe`；
> 全局配置文件只有宿主读得到，所以配置由宿主经 `get_config` 交给插件。

## 两条取数路线：API 优先、SSR 兜底

**API 路线**：`/aweme/v1/web/aweme/detail/`。这条端点的门禁是「`uifid` + SecSDK web 签名」
（`x-secsdk-web-signature`）——2026-11 已在 `src/sign/` **原生实现**（签名 = 对
`{uifid}_{timestamp}_{SALT}_{query}` 取 MD5；a_bogus 一并生成，实测它当前不参与校验）。
身份（`UIFID` / `s_v_web_id` / `ttwid`）从你配的**浏览器 cookie** 里取——抖音的 UIFID
由页面脚本下发，纯 HTTP 铸不出来。实测 `status_code=0` 拿到完整 `aweme_detail`。

**SSR 兜底**：分享页 `_ROUTER_DATA`（不依赖任何签名）。API 路线失败（cookie 没有
`uifid` / 版本参数过期 / 风控）时自动回退，用户无感知。两条路给出同一形状的 `item`。

> 签名栈移植自 [Evil0ctal/Douyin_TikTok_Download_API](https://github.com/Evil0ctal/Douyin_TikTok_Download_API)
> （Apache-2.0）的 `dtk/signing/native/`，与 Python 版逐字节对拍（`sign-vectors.json`）。
> ⚠️ SALT 是「SecSDK VM 表第 39 个字符串」、`version_code` 跟随网页端大版本——
> 抖音换表/升版时改 `src/sign/websign.rs` 的 `SALT` 与 `src/sign/detail.rs` 的版本三行。

## 与 TS 版的差异（刻意的）

| TS 版 | 这里 | 为什么 |
| --- | --- | --- |
| `ffprobe` 补宽高时长 | 用抖音给的值 | `bit_rate` 里就有，省一个外部进程依赖 |
| 消息写 `json` 前缀 → 吐原始 JSON | 不做 | TS 版特有的调试通道 |
| 下载整文件进内存再写盘 | **流式写盘** | `ureq` 的 `read_to_vec()` 写死 10 MiB 上限，超过就报 `larger than request limit`；流式之后体积无上限、内存恒定 |
| 快手 / YouTube / 内联查询 | **没迁** | 本仓库只覆盖抖音那一条分支 |
| 每次重新解析、重新下载 | **SQL 缓存命中直接发** | 同一个作品第二次是「发文件 id」，不是「重做一遍」 |

## 自检

```bash
cargo build -p fuyubot-plugin-douyin
cargo clippy -p fuyubot-plugin-douyin --all-targets
cargo fmt -p fuyubot-plugin-douyin -- --check
cargo test -p fuyubot-plugin-douyin
cargo tree -p fuyubot-plugin-douyin -e normal | grep -i simplify   # 必须为空
```

> `tests/old_behavior_proof.rs` 现在是**大文件下载回归测试**：起一个本地 HTTP 服务器吐
> 30 MiB（三倍于旧的 10 MiB 上限），走本插件的下载器断言完整落盘；另外两条验
> 「取消时不留半截文件」「被拒时不建空壳」。
>
> `tests/lifecycle.rs` 用假宿主 VTable 跑 `start` / `stop`：没有 SQL / help 插件、
> 没有 cookie、没接 TDLib 时也要能启动，且 `stop()` 快速返回。
>
> `tests/cache_hit.rs` 是**缓存命中的端到端证据**：假宿主 + 假 `fuyubot.sqlite` 预置
> 一行 `douyin:…` 缓存，投一条 `updateNewMessage` 后断言全程**只有一条**发送 op、
> 视频与封面都是 `inputFileId`、op JSON 里没有任何 URL（不解析、不下载、不先试远程直链）。

不用账号也能端到端验一遍（宿主 CLI 会真 `dlopen` 它）：

```bash
fuyubot plugin list --plugin-dir plugins          # 看 ABI 版本与 manifest 版本
fuyubot run --replay replay.json --plugin-dir plugins --replay-wait 3000
```

## 相关

- [插件文档](https://catmoecircle.github.io/FuyuBot-docs/docs/plugin/)
- [运行时仓库](https://github.com/CatMoeCircle/Fuyu-Bot)
