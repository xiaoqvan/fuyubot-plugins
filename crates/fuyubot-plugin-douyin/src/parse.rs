//! 解析：链接 → [`VideoInfo`]。
//!
//! 对应 TS 版 `parse/douyin/getvideo.ts`。**算法一个字节都没改**，
//! 只把 `axios` 换成 [`crate::http`]（`reqwest` async）、把 `any` 换成
//! `serde_json::Value` 的取值。所有函数都是 `async fn`：网络往返在插件自己的
//! tokio 运行时里 await，宿主的接收循环线程不参与等待。
//!
//! ## 两条取数路线（API 优先、SSR 兜底）
//!
//! **API 路线**（[`fetch_aweme_item_api`]）：`/aweme/v1/web/aweme/detail/` 原本被
//! ArgusSecurityPlugin 的「`uifid` + SecSDK web 签名」拦着（`a_bogus` 实测不参与
//! 校验——随机伪造与真算返回相同结果）。2026-11 起签名已原生实现（[`crate::sign`]），
//! 从用户的浏览器 cookie 里取 `uifid` / `s_v_web_id`，实测 `status_code=0` 拿到
//! 完整 `aweme_detail`。
//!
//! **SSR 路线**（[`fetch_aweme_item`]）：分享页 SSR 数据 `_ROUTER_DATA`，不依赖
//! 任何签名，是 API 路线失败（签名过期 / 版本变更 / 风控页）时的回退。
//!
//! 两条路给出**同一形状**的 `item`（API 的 `aweme_detail` ≡ SSR 的 `item_list[0]`），
//! [`build_video_info`] 不分彼此。
//!
//! 代价是**必须带 cookie**：分享页在拿不到 `ttwid` 等身份信息时不渲染作品数据。
//!
//! ## cookie 从哪来
//!
//! **配置优先**：`fuyubot.toml` 里本插件自己的段
//! `[plugin.xiaoqvan.douyin] cookies = "ttwid=…; msToken=…"`（别名 `cookie` 也认）。
//! 没配才回退旧文件 `plugins/douyin/cookies.json`（格式同 TS 版，仍兼容）。
//! 读取逻辑见 [`read_cookies`]。

use std::fs;
use std::path::{Path, PathBuf};

use fuyubot_abi::HostApi;
use serde_json::Value;

use crate::http::{self, Agent};
use crate::model::{Author, Coauthor, Image, Music, Video, VideoInfo};

// 分享链接里作品 id 的位置（TS 版那条正则 `/\/(video|note|notes|slides)\/(\d+)/`）：
// `video` / `note` / `notes` / `slides` 都是作品类型，分享页路径与之一一对应。
// 这里不引 `regex` 依赖，模式位置固定，手写扫描就够（见 `target_from_path`）。

/// 重试次数。抖音偶发返回风控页，重来一次通常就成了；再多就是浪费时间。
const ATTEMPTS: usize = 2;

/// 解析失败的原因。文案会直接进给用户看的报错，所以写人话。
#[derive(Debug)]
pub struct ParseError(pub String);

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ParseError {}

/// 解析一条抖音分享链接。
///
/// [`Agent`] 由调用方传：同一条处理链上要发两个请求（短链 + 分享页），
/// 复用连接池才不至于每次握手。
///
/// ⚠️ **这里再挡一次汽水音乐**，即使 [`crate::commands`] 已经挡过。
/// 那道挡住的是「用户消息里的链接」；这一道挡的是**任何**直接调本函数的路径
/// （以后加新命令、或者别的插件借用）。汽水短链没有作品 id，放进来只会
/// HEAD 一趟、重试两次、然后报一个让人看不懂的错误。
pub async fn get_douyin(
    agent: &Agent,
    share_link: &str,
    cookie: &str,
) -> Result<VideoInfo, ParseError> {
    crate::log::debug(&format!(
        "开始解析 {}（Cookie {}）",
        crate::log::redact_url(share_link),
        crate::log::cookie_summary(cookie)
    ));
    if crate::commands::is_qishui_url(share_link) {
        crate::log::info(&format!(
            "跳过汽水音乐链接（由 xiaoqvan.music 处理）：{}",
            crate::log::redact_url(share_link)
        ));
        return Err(ParseError(
            "这是汽水音乐的链接，不属于抖音解析（该由音乐插件处理）".to_owned(),
        ));
    }
    if cookie.is_empty() {
        crate::log::error("解析中止：没有 cookie");
        return Err(ParseError("没有有效的 cookie 信息".to_owned()));
    }

    let mut last = ParseError("未知错误".to_owned());
    for attempt in 1..=ATTEMPTS {
        match once(agent, share_link, cookie).await {
            Ok(info) => {
                crate::log::info(&format!(
                    "解析成功：id={} 标题={:?} 作者={} 视频={} 图集={} 图集视频={}",
                    info.detail_id,
                    crate::log::shorten(&info.title, 40),
                    info.author.name,
                    info.video.is_some(),
                    info.images.as_ref().map(Vec::len).unwrap_or(0),
                    info.image_videos.as_ref().map(Vec::len).unwrap_or(0),
                ));
                return Ok(info);
            }
            Err(error) => {
                crate::log::warn(&format!("第 {attempt}/{ATTEMPTS} 次失败：{error}"));
                last = ParseError(format!("第 {attempt}/{ATTEMPTS} 次失败：{error}"));
            }
        }
    }
    crate::log::error(&format!("解析最终失败：{last}"));
    Err(last)
}

/// 一轮完整的解析。
async fn once(agent: &Agent, share_link: &str, cookie: &str) -> Result<VideoInfo, ParseError> {
    let target = resolve_target(agent, share_link).await?;
    crate::log::debug(&format!("认出作品：{} {}", target.kind, target.id));
    // API 优先：签名保护区端点，签名原生算（`sign::detail`）；它挂了再回退
    // SSR 分享页——两条路给出同一形状的 `item`，`build_video_info` 不分彼此。
    match fetch_aweme_item_api(agent, &target, cookie).await {
        Ok(item) => return Ok(build_video_info(&item)),
        Err(error) => crate::log::warn(&format!("API 路线失败（{error}），回退 SSR 分享页")),
    }
    let item = fetch_aweme_item(agent, &target, cookie).await?;
    Ok(build_video_info(&item))
}

/// 作品类型 + id。
pub struct Target {
    pub kind: String,
    pub id: String,
}

/// 从分享链接里认出作品：能直接从路径里看出就省一次请求。
///
/// `v.douyin.com` 短链才走第二步——它 302 到 `iesdouyin` 分享页，`Location` 里才有
/// 作品段。这也是**唯一**允许在「缓存命中前」发出的请求：它只为一个作品 id，
/// 不拉分享页、更不下载媒体。
pub async fn resolve_target(agent: &Agent, link: &str) -> Result<Target, ParseError> {
    if let Some(target) = target_from_path(link) {
        return Ok(target);
    }

    let response = http::head_no_redirect(agent, link)
        .await
        .map_err(|error| ParseError(error.0))?;
    let location = response.location.ok_or_else(|| {
        ParseError(format!(
            "短链没有返回 Location（status={}）",
            response.status
        ))
    })?;
    target_from_path(&location)
        .ok_or_else(|| ParseError(format!("无法解析分享链接（status={}）", response.status)))
}

/// 只要作品 id（缓存键用）：本地能看出来就不发请求，短链才 HEAD 一次拿 302。
pub async fn detail_id_of(agent: &Agent, link: &str) -> Result<String, ParseError> {
    resolve_target(agent, link).await.map(|target| target.id)
}

/// 从 URL 路径里抠出 `/(video|note|notes|slides)/<数字>`。
fn target_from_path(link: &str) -> Option<Target> {
    // 不用正则 engine：`)}$` 那点复杂度不值得引一个 `regex` 依赖，
    // 而且这个模式在两侧的位置都很固定。
    let bytes = link.as_bytes();
    let mut start = 0usize;
    while start < bytes.len() {
        let Some(slash) = link[start..].find('/').map(|offset| start + offset) else {
            break;
        };
        // 找到这个片段的后边界
        let end = link[slash + 1..]
            .find(['/', '?', '#'])
            .map(|offset| slash + 1 + offset)
            .unwrap_or(link.len());
        let segment = &link[slash + 1..end];
        if matches!(segment, "video" | "note" | "notes" | "slides") {
            // id 必须是纯数字，且紧跟在 `kind/` 之后
            let rest = &link[end..];
            let rest = rest.strip_prefix('/').unwrap_or(rest);
            let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
            if !digits.is_empty() {
                return Some(Target {
                    kind: segment.to_owned(),
                    id: digits,
                });
            }
        }
        start = end.max(slash + 1);
    }
    None
}

/// 取分享页里的作品数据（`item_list[0]`）。
async fn fetch_aweme_item(
    agent: &Agent,
    target: &Target,
    cookie: &str,
) -> Result<Value, ParseError> {
    let url = format!(
        "https://www.iesdouyin.com/share/{}/{}/",
        target.kind, target.id
    );
    let response = http::get_page(agent, &url, cookie)
        .await
        .map_err(|error| ParseError(error.0))?;

    let Some(raw) = extract_router_data(&response.body) else {
        return Err(ParseError(format!(
            "分享页没有返回数据（status={}）",
            response.status
        )));
    };
    let loader_data: Value = serde_json::from_str(raw)
        .map_err(|error| ParseError(format!("分享页数据不是合法 JSON：{error}")))?;

    // loaderData 的键是路由序号，逐个翻，谁先带 `item_list` 就用谁。
    let mut rejected = String::new();
    let loader = loader_data.get("loaderData");
    if let Some(object) = loader.and_then(Value::as_object) {
        for value in object.values() {
            let info = value.get("videoInfoRes");
            if let Some(item) = info
                .and_then(|i| i.get("item_list"))
                .and_then(item_list_head)
            {
                return Ok(item);
            }
            // 作品不可见时抖音给的是 `filter_list`，把原因记下来比报「解析不出」有用。
            if rejected.is_empty()
                && let Some(filter) = info
                    .and_then(|i| i.get("filter_list"))
                    .and_then(Value::as_array)
                    .and_then(|list| list.first())
            {
                rejected = str_field(filter, "detail_msg")
                    .or_else(|| str_field(filter, "filter_reason"))
                    .unwrap_or_else(|| "作品不可见".to_owned());
            }
        }
    }
    Err(ParseError(if rejected.is_empty() {
        "分享页没有解析到作品数据".to_owned()
    } else {
        rejected
    }))
}

/// detail API 路线：签名保护区端点，`aweme_detail` 与 SSR 的 `item_list[0]` 同形，
/// [`build_video_info`] 不分彼此。
///
/// 身份取自浏览器 cookie（`uifid` + `s_v_web_id` + `ttwid`）；jar 里没有 `uifid`
/// 就直接放弃（调用方回退 SSR）。错误信息要能区分「签名被拒」（身份/版本问题）
/// 与「作品不可见」（内容问题）——两者修法完全不同。
async fn fetch_aweme_item_api(
    agent: &Agent,
    target: &Target,
    cookie: &str,
) -> Result<Value, ParseError> {
    let mut rng = crate::sign::detail::entropy();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| ParseError("系统时钟异常".to_owned()))?;
    let request = crate::sign::detail::build_detail_request(
        &target.id,
        cookie,
        now.as_secs() as i64,
        now.as_millis() as u64,
        &mut rng,
    )
    .ok_or_else(|| ParseError("cookie 里没有 uifid——API 路线需要浏览器身份".to_owned()))?;
    let response = http::get_api(agent, &request.url, cookie, &request.headers)
        .await
        .map_err(|error| ParseError(error.0))?;
    if response.status != 200 {
        return Err(ParseError(format!("detail API HTTP {}", response.status)));
    }
    let data: Value = serde_json::from_str(&response.body)
        .map_err(|error| ParseError(format!("detail API 不是合法 JSON：{error}")))?;
    if data.get("status_code").and_then(Value::as_i64) != Some(0) {
        let message = str_field(&data, "status_msg").unwrap_or_else(|| "未知原因".to_owned());
        return Err(ParseError(format!(
            "detail API 拒绝：{message}（status_code={:?}）",
            data.get("status_code")
        )));
    }
    data.get("aweme_detail")
        .filter(|detail| detail.is_object())
        .cloned()
        .ok_or_else(|| ParseError("detail API 没有 aweme_detail（作品不可见？）".to_owned()))
}

/// `item_list` 的第一条（没有就是 `None`）。
fn item_list_head(info: &Value) -> Option<Value> {
    info.as_array()
        .and_then(|list| list.first())
        .filter(|item| item.is_object())
        .cloned()
}

/// 从 HTML 里抠出 `_ROUTER_DATA = {...}</script>` 的那个对象体。
fn extract_router_data(html: &str) -> Option<&str> {
    let marker = "_ROUTER_DATA";
    let start = html.find(marker)? + marker.len();
    // 跳过 `=` 与空白
    let rest = html[start..].trim_start();
    let rest = rest.strip_prefix('=')?.trim_start();
    if !rest.starts_with('{') {
        return None;
    }
    // 花括号配对：对象里嵌着字符串和转义，但不嵌套 `}` 到同一深度之外。
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    for (index, ch) in rest.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        match ch {
            '\\' if in_string => escaped = true,
            '"' => in_string = !in_string,
            '{' if !in_string => depth += 1,
            '}' if !in_string => {
                depth -= 1;
                if depth == 0 {
                    return Some(&rest[..=index]);
                }
            }
            _ => {}
        }
    }
    None
}

/// `url_list` 的最后一项。
///
/// 抖音给的候选 CDN  list 里，**最后一条**通常是主源（`getvideo.ts` 的实测结论），
/// 取第一条反而常常拿到备用域名。
fn last_url(list: Option<&Value>) -> Option<String> {
    list.and_then(Value::as_array)
        .and_then(|items| items.last())
        .and_then(Value::as_str)
        .map(str::to_owned)
}

/// 从 `bit_rate` 里挑清晰度最高的一条：优先 60fps，其次按分辨率面积。
fn pick_best_bit_rate(bit_rates: Option<&Value>) -> Option<Value> {
    let usable: Vec<Value> = bit_rates
        .and_then(Value::as_array)?
        .iter()
        .filter(|bit_rate| {
            bit_rate
                .pointer("/play_addr/url_list")
                .and_then(Value::as_array)
                .is_some_and(|list| !list.is_empty())
        })
        .cloned()
        .collect();

    let fps60: Vec<Value> = usable
        .iter()
        .filter(|bit_rate| bit_rate.get("FPS").and_then(Value::as_i64) == Some(60))
        .cloned()
        .collect();
    let pool = if fps60.is_empty() { usable } else { fps60 };

    pool.into_iter().max_by_key(|bit_rate| {
        let play_addr = bit_rate.pointer("/play_addr");
        num_at(play_addr, "width").unwrap_or(0) * num_at(play_addr, "height").unwrap_or(0)
    })
}

/// 分享页给的是带水印的 `playwm` 链接，换成 `play` 并把码率抬到 1080p。
pub fn unwatermark(url: &str) -> String {
    url.replace("/playwm/", "/play/")
        .replace("ratio=720p", "ratio=1080p")
}

/// `Raw`（豆瓣原始 item）→ [`VideoInfo`]。
fn build_video_info(item: &Value) -> VideoInfo {
    let video = item.get("video");

    // ── 主视频 ──────────────────────────────────────────────
    // 诊断：`bit_rate` 的真实形状随抖音版本变（宽高有时在 `play_addr` 下、
    // 有时在 bit_rate 根层），字段也可能改名叫 `bit_rate_list`。
    // 把候选的键名打出来，下次再变就能一眼看出该改哪。
    if let Some(v) = video {
        crate::log::debug(&format!(
            "video 字段：{}；bit_rate 项数={}",
            v.as_object()
                .map(|o| o.keys().cloned().collect::<Vec<_>>().join(","))
                .unwrap_or_default(),
            v.get("bit_rate")
                .or_else(|| v.get("bit_rate_list"))
                .and_then(Value::as_array)
                .map(Vec::len)
                .unwrap_or(0)
        ));
        if let Some(list) = v
            .get("bit_rate")
            .or_else(|| v.get("bit_rate_list"))
            .and_then(Value::as_array)
            && let Some(first) = list.first()
        {
            crate::log::debug(&format!(
                "bit_rate[0] 字段：{}；play_addr 字段：{}",
                first
                    .as_object()
                    .map(|o| o.keys().cloned().collect::<Vec<_>>().join(","))
                    .unwrap_or_default(),
                first
                    .pointer("/play_addr")
                    .and_then(Value::as_object)
                    .map(|o| o.keys().cloned().collect::<Vec<_>>().join(","))
                    .unwrap_or_else(|| "（无 play_addr）".to_owned())
            ));
        }
    }
    let best_bit_rate = pick_best_bit_rate(
        video
            .and_then(|v| v.get("bit_rate"))
            .or_else(|| video.and_then(|v| v.get("bit_rate_list"))),
    );
    let raw_url = last_url(
        best_bit_rate
            .as_ref()
            .and_then(|b| b.pointer("/play_addr/url_list")),
    )
    .or_else(|| last_url(video.and_then(|v| v.pointer("/play_addr/url_list"))));
    // 挑中的清晰度：这是「解析到底成没成」最有用的一行——
    // 有直链 + 分辨率，后面发送失败就只可能是防盗链/体积问题，不是解析问题。
    if let Some(url) = raw_url.as_deref() {
        crate::log::debug(&format!(
            "挑中直链 {}x{} {}fps：{}",
            best_bit_rate
                .as_ref()
                .and_then(|b| num_at(b.pointer("/play_addr"), "width"))
                .unwrap_or(0),
            best_bit_rate
                .as_ref()
                .and_then(|b| num_at(b.pointer("/play_addr"), "height"))
                .unwrap_or(0),
            best_bit_rate
                .as_ref()
                .and_then(|b| num_field(b, "FPS"))
                .unwrap_or(0),
            crate::log::redact_url(url)
        ));
    } else {
        crate::log::warn("没挑出任何直链（纯图集作品，或 bit_rate 里没有可用项）");
    }
    let main_video = raw_url.map(|url| Video {
        video_url: unwatermark(&url),
        fps: best_bit_rate.as_ref().and_then(|b| num_field(b, "FPS")),
        duration: video
            .and_then(|v| num_field(v, "duration"))
            .map(|ms| ms / 1000),
        height: best_bit_rate
            .as_ref()
            .and_then(|b| num_at(b.pointer("/play_addr"), "height"))
            .or_else(|| video.and_then(|v| num_field(v, "height")))
            .unwrap_or(0),
        width: best_bit_rate
            .as_ref()
            .and_then(|b| num_at(b.pointer("/play_addr"), "width"))
            .or_else(|| video.and_then(|v| num_field(v, "width")))
            .unwrap_or(0),
    });

    // ── 图集：图片里可能夹着视频 ────────────────────────────
    let mut images: Option<Vec<Image>> = None;
    let mut image_videos: Option<Vec<Video>> = None;
    let source_images = item.get("images").and_then(Value::as_array);
    if let Some(list) = source_images.filter(|list| !list.is_empty()) {
        let mut videos = Vec::new();
        let mut pictures = Vec::new();
        for img in list {
            if let Some(best) = pick_best_bit_rate(img.pointer("/video/bit_rate"))
                && let Some(url) = last_url(best.pointer("/play_addr/url_list"))
            {
                videos.push(Video {
                    video_url: unwatermark(&url),
                    fps: num_field(&best, "FPS"),
                    duration: None,
                    height: best
                        .pointer("/play_addr")
                        .and_then(|p| num_field(p, "height"))
                        .unwrap_or(0),
                    width: best
                        .pointer("/play_addr")
                        .and_then(|p| num_field(p, "width"))
                        .unwrap_or(0),
                });
            }
            pictures.push(Image {
                height: num_field(img, "height").unwrap_or(0),
                width: num_field(img, "width").unwrap_or(0),
                url: last_url(img.get("url_list")).unwrap_or_default(),
            });
        }
        if !videos.is_empty() {
            image_videos = Some(videos);
        }
        images = Some(pictures);
    }

    // `author` 缺失时用 `Null`：下面一片字段取值都不用再写 `and_then`。
    let author = item.pointer("/author").unwrap_or_else(|| null());

    VideoInfo {
        detail_id: str_field(item, "aweme_id").unwrap_or_default(),
        title: str_field(item, "desc").unwrap_or_default(),
        cover_url: last_url(video.and_then(|v| v.pointer("/origin_cover/url_list")))
            .or_else(|| last_url(video.and_then(|v| v.pointer("/cover/url_list"))))
            .unwrap_or_default(),
        video: main_video,
        images,
        image_videos,
        author: Author {
            avatar: last_url(item.pointer("/author/avatar_thumb/url_list")),
            name: str_field(author, "nickname").unwrap_or_default(),
            sec_uid: str_field(author, "sec_uid"),
            uid: str_field(author, "uid"),
            home_url: Some(format!(
                "https://www.douyin.com/user/{}",
                str_field(author, "sec_uid").unwrap_or_default()
            )),
        },
        music: item.get("music").map(|music| Music {
            id: num_field(music, "id").unwrap_or(0),
            id_str: str_field(music, "mid").unwrap_or_default(),
            name: str_field(music, "title").unwrap_or_default(),
            url: last_url(music.pointer("/play_url/url_list")).unwrap_or_default(),
            author: str_field(music, "author").unwrap_or_default(),
            sec_uid: str_field(music, "sec_uid").unwrap_or_default(),
            avatar: last_url(music.pointer("/avatar_large/url_list")).unwrap_or_default(),
            cover: last_url(music.pointer("/cover_hd/url_list")).unwrap_or_default(),
        }),
        cooperation_info: item
            .pointer("/cooperation_info/co_creators")
            .and_then(Value::as_array)
            .map(|creators| {
                creators
                    .iter()
                    .map(|creator| {
                        let sec_uid = str_field(creator, "sec_uid");
                        Coauthor {
                            avatar: last_url(creator.pointer("/avatar_thumb/url_list")),
                            nickname: str_field(creator, "nickname"),
                            role: str_field(creator, "role_title"),
                            uid: str_field(creator, "uid"),
                            home_url: Some(format!(
                                "https://www.douyin.com/user/{}",
                                sec_uid.clone().unwrap_or_default()
                            )),
                            sec_uid,
                        }
                    })
                    .collect()
            })
            .unwrap_or_default(),
    }
}

/// 取一个整数字段（缺字段 / 非数字都当没有）。
///
/// 收 `&Value` 而不是 `Option<&Value>`：`Value::Null` 本身就表示「这一层没有」，
/// 传 `Null` 进来拿到的自然是 `None`——调用方因此不必为每个 `pointer()` 写一层
/// `and_then`。
fn num_field(value: &Value, key: &str) -> Option<i64> {
    value.get(key)?.as_i64()
}

/// 取一个字符串字段。
fn str_field(value: &Value, key: &str) -> Option<String> {
    value.get(key)?.as_str().map(str::to_owned)
}

/// 缺失时用 `Null` 顶上，好直接喂给上面两个取字段的函数。
fn null() -> &'static Value {
    static EMPTY: std::sync::OnceLock<Value> = std::sync::OnceLock::new();
    EMPTY.get_or_init(|| Value::Null)
}

/// `num_field` 的「路径版」：`pointer()` 返回 `Option<&Value>`，去掉那一层
/// `unwrap_or_else(null)` 的噪音。
fn num_at(value: Option<&Value>, key: &str) -> Option<i64> {
    match value {
        Some(value) => num_field(value, key),
        None => None,
    }
}

// ── cookie 读取 ────────────────────────────────────────────────────

/// 旧格式的 cookie 文件名（`plugins_dir/douyin/cookies.json`，仅作后备）。
const COOKIES_FILE: &str = "cookies.json";

/// 本插件在全局配置里的**段名**（`[plugin.xiaoqvan.douyin]` 去掉 `plugin.` 前缀）。
///
/// 与 `define_plugin!` 里的插件名一致；`HostApi::get_config` 的 `section` 就是插件名。
const CONFIG_SECTION: &str = "xiaoqvan.douyin";

/// 配置里的主键。
const CONFIG_KEY: &str = "cookies";

/// 兼容的别名键（早期文档里可能有人写成单数）。
const CONFIG_KEY_ALIAS: &str = "cookie";

/// 从「配置读取器」里取 cookie（纯函数：注入 `get_config` 便于单测，不碰宿主）。
///
/// `read(section, key)` 返回的是**JSON 文本**：配置里的字符串值会被 JSON 引号包住
/// （`cookies = "abc"` 读出来是 `"abc"`），所以先 `serde_json::from_str::<Value>`
/// 再 `as_str()`。
///
/// 读取顺序：`cookies` → 别名 `cookie`；空串 / 非字符串 / 不是 JSON 一律当
/// **未配置**（继续试下一个键），不会把 `""` 当成有效 cookie。
pub fn cookie_from_config(read: impl Fn(&str, &str) -> Option<String>) -> Option<String> {
    for key in [CONFIG_KEY, CONFIG_KEY_ALIAS] {
        if let Some(cookies) = read(CONFIG_SECTION, key)
            .as_deref()
            .and_then(cookie_from_json)
        {
            return Some(cookies);
        }
    }
    None
}

/// 一段配置值（JSON 文本）→ cookie 字符串；空串 / 非字符串视为未配置。
pub fn cookie_from_json(raw: &str) -> Option<String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    let value: Value = serde_json::from_str(raw).ok()?;
    let cookies = value.as_str()?.trim();
    (!cookies.is_empty()).then(|| cookies.to_owned())
}

/// 读旧格式文件 `plugins_dir/douyin/cookies.json`。
///
/// `Ok(None)` = 文件不存在（没配旧文件，不算错误）；文件存在但读不了 / 格式坏了
/// 才返回 `Err`（那是真配置错误，得说清楚）。
fn read_cookies_file(plugins_dir: &Path) -> Result<Option<String>, ParseError> {
    let path: PathBuf = plugins_dir.join("douyin").join(COOKIES_FILE);
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        // ⚠️ 文件不存在是**正常情况**（大家都改用配置了），不是错误。
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(ParseError(format!(
                "读旧 cookie 文件 {} 失败：{error}",
                path.display()
            )));
        }
    };
    let parsed: Value = serde_json::from_str(&text)
        .map_err(|error| ParseError(format!("解析 {} 失败：{error}", path.display())))?;
    let cookies = str_field(&parsed, "cookies")
        .unwrap_or_default()
        .trim()
        .to_owned();
    if cookies.is_empty() {
        return Err(ParseError(format!(
            "{} 里没有 cookies 字段",
            path.display()
        )));
    }
    Ok(Some(cookies))
}

/// 读 cookie：**配置优先**，旧 `plugins/douyin/cookies.json` 只作后备。
///
/// 顺序：
/// 1. `[plugin.xiaoqvan.douyin] cookies = "…"`（别名 `cookie` 也认）；
/// 2. 都没有 → 旧文件 `plugins/douyin/cookies.json`（格式同 TS 版 `{"cookies": "…"}`）；
/// 3. 两者都没有 → 报错，并在错误里写清「该在 fuyubot.toml 里怎么配」。
///
/// 为什么还留旧文件：老用户已经按旧文档放了文件，升级插件不该让他们突然解析失败。
/// 日志会明确区分「已从配置读取」与「回退旧文件（建议迁移）」。
///
/// ⚠️ **为什么配置读取走 `HostApi`**：插件拿不到全局配置文件的路径，只有宿主知道。
pub fn read_cookies(host: HostApi, plugins_dir: &Path) -> Result<String, ParseError> {
    read_cookies_with(&|section, key| host.get_config(section, key), plugins_dir)
}

/// [`read_cookies`] 的可测形态：配置读取器由调用方注入（单测不构造假宿主）。
fn read_cookies_with(
    read: &dyn Fn(&str, &str) -> Option<String>,
    plugins_dir: &Path,
) -> Result<String, ParseError> {
    if let Some(cookies) = cookie_from_config(read) {
        crate::log::info(&format!(
            "cookie 已从 fuyubot.toml 的 [plugin.{CONFIG_SECTION}] 读取（{}）",
            crate::log::cookie_summary(&cookies)
        ));
        return Ok(cookies);
    }
    match read_cookies_file(plugins_dir)? {
        Some(cookies) => {
            crate::log::warn(&format!(
                "fuyubot.toml 的 [plugin.{CONFIG_SECTION}] 没有配 cookies，回退旧文件 {}（建议迁移到配置）",
                plugins_dir.join("douyin").join(COOKIES_FILE).display()
            ));
            Ok(cookies)
        }
        None => Err(ParseError(format!(
            "没有可用的 cookie：在 fuyubot.toml 的 [plugin.{CONFIG_SECTION}] 段配 cookies = \"…\"\
             （旧 cookies.json 仍兼容）"
        ))),
    }
}

#[cfg(test)]
#[path = "../tests/unit/parse.rs"]
mod tests;

/// 联网实测（默认不跑）：
///
/// ```text
/// TEST_DOUYIN_COOKIES=…/cookies.json TEST_DOUYIN_AWEME_ID=<作品id> \
///   cargo test -p fuyubot-plugin-douyin -- --ignored live_detail_api --nocapture
/// ```
///
/// 走完整 API 路线（签名 + 身份 + 桌面画像），断言拿回 `aweme_detail`。
#[cfg(test)]
#[path = "../tests/unit/parse__live_tests.rs"]
mod live_tests;
