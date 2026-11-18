# 音乐插件使用与登录指南

本文覆盖三家平台的**登录、cookie 获取、配置位置**和常见错误。


---

## 0. 安装与配置位置

```bash
cargo build -p fuyubot-plugin-music --release
python scripts/package.py --plugin fuyubot-plugin-music \
    --platform windows-x86_64 --version 0.2.0
# 解压 dist/xiaoqvan.music-0.2.0-windows-x86_64.zip 到 plugins/xiaoqvan.music/
```

所有凭据都写在全局配置文件的**本插件段**，一个根段、键平铺：

```toml
[plugin.xiaoqvan.music]
netease = "MUSIC_U=...; os=pc; appver=3.1.28.205001"
soda = "sessionid=...; sid_tt=..."
youtube = "SAPISID=...; SID=...; HSID=..."



```

改完**重启宿主**才会重新读配置。日志里会写每个平台的状态：
`YouTube：已读取 cookie` 等。

---

## 1. 命令速查

| 命令 | 作用 |
| --- | --- |
| `/music163 关键词` | 搜网易云，回前 8 条 |
| `/music163 <链接或 ID> [音质]` | 网易云单曲 → 发音频 |
| `/soda 关键词` / `/soda <链接或 ID> [音质]` | 汽水搜索 / 单曲；UGC 视频发视频 |
| `/ytmusic 关键词` / `/ytmusic <链接或 ID> [音质]` | YouTube Music 搜索 / 单曲 |
| 聊天里的分享链接 | 自动识别网易云 / 汽水 / `music.youtube.com`；普通 YouTube 视频不抢 |

音质：网易云 `standard / higher / exhigh / lossless / hires / jymaster / sky`；
汽水 `best / hires / lossless / dolby / highest / medium / low`；
YouTube Music `best / medium / low`。

---

## 2. YouTube / YouTube Music

### 2.1 什么时候需要 cookie

插件走 InnerTube 的 ANDROID_VR → IOS → ANDROID 多客户端，**普通住宅网络不需要 cookie**。
出现下面这条才需要：

```text
YouTube 要求登录验证（Sign in to confirm you're not a bot）
```

这是按 IP 的风控。yt-dlp 在同一台机器上也会报同样的错误，换客户端并不能绕过；
要么配置 cookie，要么换住宅网络。

### 2.2 获取 `youtube` cookie 头

1. Chrome / Edge 登录 <https://www.youtube.com>。
2. F12 → **Network（网络）** → 刷新页面。
3. 点任意一个 `www.youtube.com` 的请求 → **Headers** →
   **Request Headers → Cookie**。
4. 复制整个值（形如 `SAPISID=...; SID=...; HSID=...; SSID=...; APISID=...`），
   去掉开头的 `Cookie: `，确保是**一行**。
5. 贴进配置：

```toml
youtube = "SAPISID=...; SID=...; HSID=...; SSID=...; APISID=..."
```

> 只填 `SAPISID` 通常不够；整段 Cookie 头最稳。
> Cookie 等同账号密码，别提交到 Git / 截图 / 群里。

### 2.3 其他

- `/ytmusic` 支持 `music.youtube.com/watch?v=`、`youtu.be/`、
  `youtube.com/watch?v=`、`/shorts/`、`/live/`、裸 11 位 ID。
- 普通 `youtu.be` / `youtube.com` 视频不会自动抢，用 `/ytmusic <链接>` 显式解析。
- YouTube 音频按 2 MiB `Range` 分段下载；整段请求会被 googlevideo 限速。

---

## 3. 网易云音乐

### 3.1 获取 `MUSIC_U`

1. 浏览器登录 <https://music.163.com>。
2. F12 → **Application → Storage → Cookies** → `https://music.163.com`。
3. 复制 `MUSIC_U` 的 Value。
4. 配置：

```toml
netease = "MUSIC_U=粘贴到这里; os=pc; appver=3.1.28.205001"
```

`os` / `appver` 不填时插件会补默认值；`MUSIC_U` 是登录态，
没有它只能拿匿名可播曲目。

### 3.2 音质

- 登录 + VIP：`standard / higher / exhigh / lossless / hires / jymaster / sky`
  按请求档位返回。
- 非 VIP / 无版权：服务端会降级或返回空 `url`，插件会提示“没登录 / 不是 VIP / 版权受限”。

旧版文件仍兼容：`plugins/music/163cookie.json`。

---

## 4. 汽水音乐

### 4.1 扫码登录（唯一方式）

汽水网页版没有可用的登录入口，拿不到 `sessionid`，所以插件只支持扫码。
仓库自带脚本，只用 Python 标准库：

```bash
python scripts/qishui_qr_login.py
```

- 脚本从汽水官方 passport 接口拿二维码，保存为 `qishui_qr.png` 并自动打开。
- 用**已登录的抖音 APP**扫码（接口文案明确要求抖音；汽水 App 扫码通常不确认），
  在手机上点确认。
- 成功后终端会打印：

```toml
soda = "sessionid=...; sid_tt=...; uid_tt=...; ..."
```

把它贴进 `[plugin.xiaoqvan.music]`，重启宿主。

无图形界面时：

```bash
python scripts/qishui_qr_login.py --no-open --output /tmp/qishui_qr.png
```

把 PNG 下载到手机相册再扫，或把 `qrcode_index_url` 生成二维码。

脚本只拿登录态，不修改配置文件；**会员无损还需要下面的签名头**。

### 4.2 会员无损 / 签名头

汽水会员整曲走 `POST /luna/pc/track_v2`，必须有官方客户端的逐请求签名头
`x-helios` / `x-medusa`，以及**同一台机器**的设备指纹：

```toml
soda_device_id = "16 位数字"
soda_iid = "16 位数字"
soda_fp = "一般等于 soda_device_id"
soda_x_helios = "抓包得到的 x-helios"
soda_x_medusa = "抓包得到的 x-medusa"
# 推荐：外部签名器，每次请求现算
soda_signer_url = "http://127.0.0.1:8899/sign"
soda_signer_token = "可选 Bearer"
```

- 签名头来自汽水官方客户端原生安全组件，纯 Rust 算不出来。
- 没有签名头：免费曲目可以整曲；会员曲目通常只有几十秒试听（文案会标注）。
- 三个指纹必须和签名头出自同一台机器，否则签名对不上。
- 旧版文件仍兼容：`plugins/music/soda.json`。

---

## 5. 安全与排错

### Cookie 安全

- Cookie 等同登录凭证，**不要提交到 Git、不要发群、不要贴日志**。
- 建议把 `fuyubot.toml` 权限限制为当前用户可读。
- 失效后重新登录、替换配置、重启宿主。

### 常见错误

| 报错 | 原因 | 处理 |
| --- | --- | --- |



| `YouTube 要求登录验证` | 数据中心 IP 风控 | 配 `youtube` cookie 或换住宅网络 |
| `YouTube 不能播放这个视频` | 地区 / 版权 / 直播 | 换视频；这是上游限制 |
| 网易云提示没登录 / 不是 VIP | `MUSIC_U` 缺失或过期 | 重新登录取 `MUSIC_U` |
| 汽水会员曲目只出试听 | 缺 `x-helios` / `x-medusa` | 配签名器或静态签名头 + 设备指纹 |
| 汽水扫码一直 status=new | 用汽水 App 扫了 / 二维码过期 | 用抖音 App 扫；过期重跑脚本 |

### 验证命令

```text
/music163 https://music.163.com/song?id=186016
/soda https://www.qishui.com/track/7304719759323564095

/ytmusic https://music.youtube.com/watch?v=dQw4w9WgXcQ
```

先看日志：成功会打 `YouTube ... 用 ANDROID_VR 客户端取流成功`，
失败会打具体 HTTP / 业务错误。