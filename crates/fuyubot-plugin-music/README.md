# fuyubot-plugin-music

FuyuBot 的音乐解析插件（Rust，cdylib）：**网易云音乐 + 汽水音乐**，只做三件事——
搜索、解析分享链接、按指定音质发音频；汽水还支持 **UGC 音乐视频**分享
（发视频，文案格式与抖音插件一致）。本仓库（`fuyubot-plugins`）把它和其余六个
插件一起按 **ABI v6** 编译、打成 bundle。

```
/music163 晴天              ──▶ 搜索结果列表（带 ID）
/music163 <链接|ID> 无损     ──▶ 取流 ──▶ 发音频
/soda     <链接|ID> lossless ──▶ 取流 ──▶ （加密则解密）──▶ 发音频
/soda     <UGC 视频链接>       ──▶ 分享页 ──▶ 发视频

聊天里的分享链接（music.163.com / 163cn.tv / qishui.com / qishui.douyin.com）
        ──▶ 自动按默认音质解析
```

- 插件 id：`fuyubot.music`，**ABI v6**（`fuyubot-abi` 0.1.3+ / FuyuBot 0.1.4+）
- 产出：`fuyubot_plugin_music.{dll,so,dylib}`
- 只在**鉴权**与**解密**上花力气：网易云 eapi（AES-128-ECB + MD5），
  汽水 `play_auth`（spade）+ MP4/CENC（AES-128-CTR）
- **全异步**：插件自带 tokio 多线程 runtime，HTTP 走 reqwest 0.13 async，下载流式落盘
- **软依赖 `fuyubot.sqlite`**：装了就把发送成功后的 TDLib 文件 id 缓存下来，
  同一个「音乐 id + 音质」下次直接用文件 id 发，不再请求接口、不再下载

## ABI v6 相关

- **服务调用走宿主**：缓存探测用 `host.list_services()`（不再 `get_service` 拿裸
  vtable），调用一律 `host.call_service(...)`——提供方（sqlite / help）因此能拿到
  真实调用者身份；请求里仍显式带自己的插件名，sqlite 会拿它和调用者比对。
- **命令回复不引用原消息**：本插件刻意**不用** `fuyubot.host.command` 的 `reply`
  （它会按账号类型给 Bot 账户加 `reply_to`）。所有发送仍走 simplify 的
  `send_message`，文案与第一次逐字一致。

## 执行模型

```text
宿主回调（接收循环线程）
  /music163 … / /soda … / 分享链接
        └─ 只做「解析参数 + submit」 ──▶ 插件自有 tokio runtime（4 个 music-worker）
                                          ├─ reqwest async：搜索 / 详情 / 取流（20s 总超时）
                                          ├─ reqwest 流式下载：chunk() 分块写 tokio::fs
                                          ├─ ResponseRouter + timeout：等 TDLib 响应
                                          └─ spawn_blocking：SQL 缓存读写 / CENC 解密
```

- 宿主回调里**不做任何网络 / 磁盘 / TDLib 请求**；`stop()` 的顺序是
  「退订 → 置取消标志 → 关闸并等在飞任务归零（限时 30s）→ `shutdown_timeout(5s)`」。
  下载 / 解密循环每一步都看取消标志，所以卸载动态库前不会有残线程。
- 网络与 TDLib 等待全是 `.await`；没有固定 worker 线程、没有任务队列。
- 旧的同步 `ureq` + 固定 worker 线程 + `ResponseHub`（Condvar 等待）已经全部删除。

## SQL 缓存（软依赖）

装了 [`fuyubot.sqlite`](https://github.com/CatMoeCircle/fuyubot-plugins) 服务插件时，
`soft_inject` 会声明 `fuyubot.sqlite`（软依赖：没装照常工作，只是没有缓存加速）：

| 平台 | 缓存键 |
| --- | --- |
| 网易云 | `netease:<歌曲 ID>:<请求音质>` |
| 汽水歌曲 | `soda:<track ID>:<请求音质>` |
| 汽水 UGC 视频 | `soda-video:<ugc_video_id>` |

- 音质用**用户请求的档位 key**（默认 `exhigh` / `best`），不是接口降级后的实际档位。
- 发送成功后从 `sendMessage` 的结果里取 `file.id` / 封面 `file.id`，连同标题、歌手、
  专辑、链接、音质文案、码率、大小、时长等文本信息写进 `media_cache`。
- 命中时**零网络请求、零下载**：直接 `inputFileId` 重发（文案与首次一致）。
- 命中但发送失败（文件被 TDLib 清掉 / id 失效）→ 删掉该条缓存并**自动回落**完整流程，
  用户不会看到错误。
- 缓存写入失败只记 debug，**绝不影响发送结果**；没有 SQL 插件时所有缓存操作是 no-op。


## 命令

| 命令 | 作用 |
| --- | --- |
| `/music163 <关键词>` | 搜索，返回前 8 条（带歌曲 ID） |
| `/music163 <链接或 ID> [音质]` | 解析并发送音频 |
| `/soda <关键词>` | 搜索汽水，返回前 8 条（带 track ID） |
| `/soda <链接或 ID> [音质]` | 单曲：解析并发送音频；UGC 视频：解析并发送视频 |

链接支持：完整分享链接、短链（`163cn.tv` / `qishui.douyin.com`，会跟一次重定向）、
汽水短链的落地页（`music.douyin.com/qishui/share/track?track_id=…` 或
`…/share/ugc_video?ugc_video_id=…`）、以及纯数字 ID。

汽水的 UGC 音乐视频（分享页 `ugc_video_page`）不需要 `track_v2`：直接读 SSR 里的
`_ROUTER_DATA`，发 mp4；文案照抖音插件：标题（相邻 `#话题` 自动拆开）+ 空行 +
`By @作者`。音质参数放在最后一段；不带就用默认值。

### 音质

| 平台 | 可用音质（默认加粗） | 默认 |
| --- | --- | --- |
| 网易云 | `standard` / `higher` / `exhigh` / `lossless` / `hires` / `jymaster` / `sky` | `exhigh` |
| 汽水 | `best` / `hires` / `lossless` / `dolby` / `highest` / `medium` / `low` | `best` |

别名也认：`sq`、`flac`、`无损`、`hr`、`320k`、`hq`、`母带` …
接口实际给的档位可能比请求的低（版权 / 会员限制），文案里显示的是**实际档位**。

- 网易云登录态下，VIP 曲目会按请求档位返回（standard/higher/exhigh/lossless/hires/jymaster/sky
  实测均精确命中）；非 VIP 曲目请求高码率会被服务端降级，编号文案仍按实际档位显示。
- 匿名请求 VIP 曲目时网易云返回 `url/code` 全空，插件会明确提示“没登录 / 不是 VIP / 版权受限”。
- 汽水的 `medium` / `low` 不会反向给出比请求更高的档位；没有更低档位时取最接近的一档，
  而不是直接跳到最高。

## 装

本仓库统一打 bundle（`plugin.json` + `bin/`，`plugin.json` 里 `"abi": 6`）：

```bash
cargo build -p fuyubot-plugin-music --release
python scripts/package.py --plugin fuyubot-plugin-music \
    --platform windows-x86_64 --version 0.1.2
# 解压 dist/fuyubot.music-0.1.2-windows-x86_64.zip 到 plugins/fuyubot.music/
```

也可以直接装裸动态库：`fuyubot plugin install target/release/fuyubot_plugin_music.dll`。

### 依赖

- **编译期只有 `fuyubot-abi`**：本仓库根 `Cargo.toml` 统一给 path 依赖，并带
  `version = "0.1.3"` 守卫（0.1.2 是 ABI v5，硬不兼容）。
- **运行期软依赖**：只有 `fuyubot.sqlite`（文件 id 缓存）；没装时插件照常工作。
  命令表由宿主预置（`fuyubot.host.commands`），两个命令按 `role = user`、
  `scopes = private + group` 登记。

**刻意不依赖 `fuyubot-simplify`**（它 `publish = false`，外部插件只能走 path）：
「人话参数 → TDLib 请求体」的构造器（`Text` / `Media` / `InputFile` / `Thumbnail` /
`send_params` / 命令解析 / 从已发送消息取 `file.id`）内联在 `src/simplify.rs`，
「发出去并等结局」走 `ResponseRouter::invoke_op` + op JSON（v6 的 simplify 通道没变）。
`cargo tree -p fuyubot-plugin-music -e normal | grep -i simplify` 应当为空。

插件是 cdylib，`current_exe()` 拿到的是 `fuyubot.exe`，算不出宿主工作目录。
鉴权材料现在**放在全局配置文件 `fuyubot.toml` 里本插件自己的段**
`[plugin.fuyubot.music]`（见下一节）；旧版 `plugins/music/*.json` 仍然兼容，
只是配置里没配某个平台时才会回退去读。`plugins_dir` 仍然由宿主在
`plugin_start` 的 config 里给——旧文件就从它下面的 `music/` 找。

## 鉴权

### 配置（推荐）

**只用一个根段，所有键平铺**（没有二级子表）。值一律写成字符串：

```toml
[plugin.fuyubot.music]
# 整段 Cookie 头字符串
netease = "MUSIC_U=…; os=pc; appver=3.1.28.205001"
soda = "sessionid=…; sid_tt=…"

# 汽水的设备指纹 / 签名头（可选；匿名可不填，但要和抓包那台机器一致）
soda_device_id = "16 位数字"
soda_iid = "16 位数字"
soda_fp = "一般等于 soda_device_id"
soda_x_helios = "静态抓包的 x-helios（可选）"
soda_x_medusa = "静态抓包的 x-medusa（可选）"
soda_user_agent = "LunaPC/3.3.0(359450208)"
soda_signer_url = "http://127.0.0.1:8899/sign"
soda_signer_token = "可选：签名器的 Bearer token"
```

- `netease` / `soda` → 当成 **Cookie 头**（`MUSIC_U=…; os=pc` 这种）。
- `soda_*` → 汽水的设备指纹 / 签名头；缺省时按现有行为生成指纹（匿名可用）。
- 空串 / 坏 JSON / **非字符串** → 按**未配置**处理（不会 panic；非字符串会 warn）。
- **配置优先**：配置里有的平台不会再读旧文件；某个平台没配才回退，
  日志里会写明「鉴权材料来自 fuyubot.toml」还是「回退旧文件（建议迁移）」。
- 换 cookie 后重启宿主即可，不用重编译。

### 旧文件格式（兼容，建议迁移到上面）

```text
plugins/
  fuyubot_plugin_music.dll
  music/
    163cookie.json      ← 网易云（可选，不填就匿名）
    soda.json           ← 汽水（可选，不填就匿名）
```

**读不到不算故障**：插件照常加载，匿名也能搜索、也能拿免费曲目的明文流；
真正用到时再把原因报给用户。

#### 网易云：`163cookie.json`

```json
{
  "MUSIC_U": "粘贴自己的 MUSIC_U",
  "os": "pc",
  "appver": "3.1.28.205001",
  "deviceId": "可选"
}
```

也接受单字段写法：`{"cookie": "MUSIC_U=…; os=pc; appver=…"}`。`os` / `appver` 缺省时
自动补成 PC 客户端形状（eapi 认这个身份）。

- `MUSIC_U` 是登录态；没有它只能拿匿名可播放的曲目（VIP 曲目会失败）。
- 每个 eapi 请求都会**加密并签名**：

  ```text
  digest = MD5("nobody" + /api/路径 + "use" + JSON + "md5forencrypt")
  明文   = /api/路径 + "-36cd479b6b5-" + JSON + "-36cd479b6b5-" + digest
  params = HEX(AES-128-ECB(明文, key = "e82ckenh8dichen8"))
  ```

  然后以 `params=<hex>` 表单 POST 到 `/eapi/...`，请求头带 `Cookie` 与
  客户端 UA（`NeteaseMusicDesktop`）。响应的 `data[].url` 就是播放地址。

#### 汽水：`soda.json`

```json
{
  "cookie": "sessionid=…; sid_tt=…",
  "device_id": "16 位数字",
  "iid": "16 位数字",
  "fp": "一般等于 device_id",
  "x_helios": "静态抓包的 x-helios（可选）",
  "x_medusa": "静态抓包的 x-medusa（可选）",
  "user_agent": "LunaPC/3.3.0(359450208)",
  "signer_url": "可选：http://127.0.0.1:8899/sign",
  "signer_token": "可选：签名器的 Bearer token"
}
```

字段别名（`deviceId` / `install_id` / `X-Helios` / `ua` …）也认。

- **搜索 / 详情 / 免费曲目**：只要 cookie（甚至不要）就能走 Web 侧接口。
- **整曲 / 会员音质**：`POST /luna/pc/track_v2` 需要 `x-helios` + `x-medusa`
  这一对**逐请求**的应用签名头，以及和它们出自同一台机器的 `device_id` / `iid` / `fp`。
  签名由官方客户端的原生安全组件算，纯 Rust 算不出来，所以有两条路：

  1. **签名器（推荐）**：`signer_url` 指向一个能算签名的 HTTP 服务（例如基于
     [libresoda](https://github.com/sodahub-org/libresoda) 的 `SIGNER-SERVICE`、
     mssdk 桥接器）。插件每次请求把 `{"url","method","body","ts_ms","headers"}`
     POST 过去，回包收 `{"headers":{"x-helios":…,"x-medusa":…},"msToken":…,"a_bogus":…}`；
     也兼容 Meting-API 那种 `{"ok":true,"X-Helios":…,"X-Medusa":…}` 扁平写法。
  2. **静态抓包头**：只填 `x_helios` / `x_medusa`。签名通常和 body / track_id 绑定，
     能用是运气；失败时自动回退 Web 侧流。

  两条都没有也能用：免费曲目照样拿整曲，会员曲目通常只有几十秒试听，文案里会标出来。
- 设备指纹缺省时会按当前时间生成一个——匿名够用，但**和签名头不匹配**，所以
  只要填了 `x_helios` / `x_medusa`，就一定要把抓包时的三个指纹一起填上。

## 解密

### 汽水：`play_auth` → AES 密钥（spade）

加密流的播放地址带一个 `play_auth`。先 base64 解出一段字节，按 spade 的
「异或 + 减 popcount + 回卷」规则还原出一串十六进制，再解码成 16 字节 AES-128 密钥。
`src/soda_crypto.rs` 里是完整实现，并用「逆向构造的 play_auth」做了往返测试。

### 汽水：MP4/CENC（AES-128-CTR）

拿到密钥后解 `mdat`：

1. `moov → trak → mdia → minf → stbl` 里找 `stsz`（每个样本的长度）、`senc`
   （每个样本的 IV / subsample 描述）、`tenc`（IV 长度，缺省 8）；
2. 逐样本做 AES-128-CTR：计数器 128 位大端，从样本 IV 起；同一 subsample 内
   明文段不消耗密钥流、密文段连续消耗；
3. `stsd` 里的 `enca` 按 `frma` 改回原始编码（`mp4a` / `fLaC` …），否则播放器不认；
4. 写出的明文扩展名按真实编码给（`.m4a` / `.flac` / …），TDLib 靠它判 MIME。

样本表全部来自文件头且做了上限钳制，损坏 / 截断文件会报错而不会巨量分配。
解密是流式的（只把 `mdat` 之前的前导 box 读进内存），并逐样本检查取消标志。

网易云的流是明文，不需要解密。

## 发出去的是什么

- 单曲：落盘 →（必要时解密）→ `sendMessage` + `inputMessageAudio`：
  标题 / 歌手 / 时长 / 专辑封面，文案里带「音质 · 码率 · 大小 · 原曲链接」。
- 汽水 UGC 视频：落盘 mp4 + 封面缩略图 → `inputMessageVideo`：
  标题 / 作者 / 时长 / 宽高 / `supports_streaming`，文案为「标题 + `By @作者`」。
- **不引用 / 不回复原消息**：`sendMessage` 不带 `reply_to`，也不再为判断群聊发
  TDLib 方法请求（旧版的 `getChat` 已删除）。

临时文件在发送结束后删除（Windows 上句柄未释放会重试几次）。封面下载失败只记日志。

### 大文件

- **下载只落盘，不进内存**：`http::download` 用 reqwest 的 `response.chunk().await`
  分块写 `tokio::fs` 文件，峰值内存与视频/音频大小无关；块与块之间检查取消标志，
  `stop` 时能及时中断。建连 15s / 单次读 2h 分项限时，不用「整次传输 5 分钟」的
  总预算把大文件硬切断。
- **加密音频流式解密**：只把 `mdat` 之前的前导 box（几十 KB）读进内存，
  `mdat` 按样本逐块读、解、写；`moov` 在 `mdat` 之后的少数文件才退回整份解密。
  解密整体放 `spawn_blocking`，逐样本检查取消标志。
- 视频上限按 Telegram 单文件 2GB 检查；封面统一 320×320 / 540p 小图当缩略图。

## 与参考实现的关系

- 网易云命令与 eapi 行为参考 `F:/nodejs/tdlib/plugin/music163`（TypeScript 版）。
- 汽水接口与音质档位参考 [`sodahub-org/libresoda`](https://github.com/sodahub-org/libresoda)。
- **代码是本仓库独立写的**（没有把 AGPL 的 libresoda 源码搬进来），只沿用协议事实：
  接口路径、参数形状、eapi / spade / CENC 的算法。
- 不做参考实现里的其余内容：内联查询、搜索结果图 + 回调按钮、歌单 / 专辑 / 歌词、
  扫码登录、播放上报。

## 自检

```bash
cargo build -p fuyubot-plugin-music
cargo clippy -p fuyubot-plugin-music --all-targets
cargo fmt -p fuyubot-plugin-music -- --check
cargo test -p fuyubot-plugin-music

# 联网冒烟（只做匿名请求）：
cargo test -p fuyubot-plugin-music --test network_smoke -- --ignored --nocapture
```

不接账号也能验插件本身：

```bash
fuyubot plugin list --plugin-dir plugins    # 看 ABI 版本、manifest 版本、启动日志
```

## 目录

```text
crates/fuyubot-plugin-music/
  src/
    lib.rs          define_plugin! + 模块清单 + 执行模型说明
    runtime.rs      tokio 执行引擎（闸门 / 在飞计数）+ ResponseRouter + 缓存句柄
    auth.rs         两个 JSON 的读取与归一化
    http.rs         reqwest async 请求 / 流式下载落盘
    cache.rs        SQL 文件 id 缓存（软依赖 fuyubot.sqlite）
    simplify.rs     内联裁剪版 simplify：参数构造 / 命令解析 / 取已发送文件 id
    netease.rs      eapi 加密签名 + 搜索 / 详情 / 取流
    soda.rs         搜索 / 分享链接 / 详情 / 音质择优
    soda_crypto.rs  spade 密钥还原 + MP4/CENC 解密（可取消）
    soda_sign.rs    汽水 App 端点的逐请求签名（外部签名器）
    soda_video.rs   汽水 UGC 音乐视频（分享页 _ROUTER_DATA）
    send.rs         音频 / 视频 / 文本消息（本地文件或 TDLib 文件 id）
    commands.rs     命令、自动识别、异步处理链、缓存命中 / 回落
  tests/
    host_stub.rs      假宿主：响应竞态 + start/stop 生命周期
    cache_hit.rs      端到端：缓存命中 → inputFileId 发送（假 sqlite + 假 simplify_invoke）
    network_smoke.rs  #[ignore] 联网用例
```

旧版鉴权文件仍在宿主工作目录的 `plugins/music/` 下读取（`163cookie.json` /
`soda.json`，见「鉴权 → 旧文件格式」），本仓库不放样例；新部署直接用
`fuyubot.toml` 的 `[plugin.fuyubot.music]` 段。

## 已知限制

- 汽水会员曲目的整曲 / 无损依赖签名器或抓包得到的 `x-helios` / `x-medusa`；
  静态头会过期，现象是 App 端点返回空 body（日志里会写明），重新抓或改用签名器。
- 网易云 VIP 音质需要有效的 `MUSIC_U`；接口降级时按实际档位发送。
- 汽水某些流不带时长，无法判断是不是试听片段；这时按整曲发送（参考实现同样如此）。
- UGC 视频如果被标成付费/地区受限，分享页给的是试听片段（`previewEnd` 明显小于
  `duration` 时文案会标出来）；下载仍可能受 CDN 时效限制。
- 频道里自动识别链接只按普通消息发（不编辑原帖）。
- 关停时 `stop` 先退订、置取消标志（下载 / 解密尽快退出），再关闸等在飞任务归零
  （限时 30s）并 `shutdown_timeout(5s)`，不会把进程卡在上传预算里。
- 搜索建议也支持把音质写在最后：`/music163 186016 无损`、`/soda <链接> lossless`；
  关键词本身以音质词结尾不会被误判（`/music163 Shape of You high` 仍是搜索）。
- 只处理文本消息里的链接；媒体说明文字里的链接不解析。
