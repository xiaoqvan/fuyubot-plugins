//! 解析结果的形状（对应 TS 版 `types/douyin.d.ts`）。
//!
//! 字段与 JSON 里的键名**故意保持一致**（`detail_id` / `image_videos` / `cover_url` …），
//! 好让它和未来可能出现的其它数据源对得上；只有两处按 Rust 的习惯收了类型：
//!
//! - TS 里「可选字段 + 可空」写成 `xxx?: T | null`，这里统一用 `Option`
//!   ——要不要发 = 有没有值，`null` 不参与讨论；
//! - 数字一律具体宽度而不是 TS 的笼统 `number`（抖音的 `duration` 是毫秒整数，
//!   这里在 [`Video::duration`] 上就已经换算成秒了）。

use serde::Serialize;

/// 一个抖音作品的解析结果。
#[derive(Debug, Clone, Serialize, PartialEq, Eq, Default)]
pub struct VideoInfo {
    /// 作品 id（`aweme_id`）。
    pub detail_id: String,
    /// 文案（抖音里叫 `desc`，用户看到的就是「标题」）。
    pub title: String,
    /// 封面链接。
    pub cover_url: String,
    /// 主视频。纯图片的作品没有它。
    pub video: Option<Video>,
    /// 图集的图片（图集作品才有）。
    pub images: Option<Vec<Image>>,
    /// 图集里夹着的视频——抖音的「图文」允许某些页是视频。
    pub image_videos: Option<Vec<Video>>,
    /// 作者。
    pub author: Author,
    /// 背景音乐。
    pub music: Option<Music>,
    /// 共创作者（合拍 / 合作作品）。
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub cooperation_info: Vec<Coauthor>,
}

/// 一段视频。
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Video {
    /// 直链（已去水印，见 [`crate::parse::unwatermark`]）。
    pub video_url: String,
    /// 帧率。抖音不一定给。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fps: Option<i64>,
    /// 时长（**秒**）。源数据里是毫秒，转换在解析层做完了。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration: Option<i64>,
    pub height: i64,
    pub width: i64,
}

/// 图集里的一张图。
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Image {
    pub height: i64,
    pub width: i64,
    pub url: String,
}

/// 作者。
#[derive(Debug, Clone, Serialize, PartialEq, Eq, Default)]
pub struct Author {
    /// 头像。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub avatar: Option<String>,
    /// 昵称。
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sec_uid: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub uid: Option<String>,
    /// 主页链接。解析层无条件按 `sec_uid` 拼出，
    /// 即使 `sec_uid` 缺失（那时指向一个不存在的主页）——这一点与 TS 版一致，
    /// 因为文案构造里「没有主页」要退化成纯文本 `@昵称`，判据是 `home_url` 是否为空。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub home_url: Option<String>,
}

/// 背景音乐。
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Music {
    pub id: i64,
    pub id_str: String,
    pub name: String,
    pub url: String,
    pub author: String,
    pub sec_uid: String,
    pub avatar: String,
    pub cover: String,
}

/// 共创作者。
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Coauthor {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub avatar: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nickname: Option<String>,
    /// 角色（`role_title`，例如「摄影」）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub uid: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sec_uid: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub home_url: Option<String>,
}
