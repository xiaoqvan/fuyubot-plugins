//! AI 深度审核：OpenAI 兼容的 `/chat/completions` + 结构化判定结果。
//!
//! TS 版用 `openai` SDK、客户端里写死两把 Key（已进仓库）。这一版：
//!
//! - **任何密钥都不进源码**：接口地址 / Key / 模型来自
//!   `[plugin.fuyubot.adblock.ai]`（默认接口）或各群 `/adconfig` 配的自定义接口；
//! - HTTP 用 `ureq`（同步阻塞），只在工作线程里发——回调线程一秒都不能停；
//! - 提示词用 `include_str!` 嵌进动态库（见 [`prompt`]）；
//! - 判定结果的字段名与 TS 版一致（`Reason` / `quoted_message` / `PersonalChannel`…），
//!   提示词、老日志、人工排查口径都不用改。
//!
//! 失败语义：**永远返回一个 [`ModerationResult`]**，`ad_detected=false` 且
//! `Reason` 里写清失败原因。上层据此区分「AI 说不是广告」与「AI 没跑成」，
//! 后者不会触发任何处置。

pub mod prompt;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::api::Sender;
use crate::config::AiEndpoint;
use crate::log;
use crate::runtime::Runtime;
use crate::store::GroupConfig;
use crate::text;
use crate::util::{base64_encode, truncate};

/// 单次请求的最大尝试次数（与 TS 版一致）。
const MAX_ATTEMPTS: usize = 3;
/// 结果里 JSON 的最大输出 token。
const MAX_TOKENS: u32 = 2048;
/// AI 总日志里单段 JSON 的截断长度。
const LOG_JSON_LIMIT: usize = 3500;

// ── 输入 ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ModerationInput {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(rename = "quoteMessage", skip_serializing_if = "Option::is_none")]
    pub quote_message: Option<String>,
    #[serde(rename = "link_Preview", skip_serializing_if = "Option::is_none")]
    pub link_preview: Option<LinkPreview>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile: Option<Profile>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub contact: Option<Contact>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sticker: Option<Sticker>,
    #[serde(rename = "PersonalChannel", skip_serializing_if = "Option::is_none")]
    pub personal_channel: Option<Channel>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mentioned_username: Option<Mention>,
    #[serde(rename = "historyMessages", skip_serializing_if = "Option::is_none")]
    pub history_messages: Option<Vec<String>>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LinkPreview {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Profile {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bio: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Contact {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub phone: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Sticker {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Channel {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Mention {
    pub username: String,
    pub name: String,
    pub bio: String,
    #[serde(rename = "type")]
    pub kind: String,
}

impl ModerationInput {
    pub fn of_kind(kind: &str) -> Self {
        Self {
            kind: kind.to_owned(),
            ..Self::default()
        }
    }

    pub fn text(message: impl Into<String>) -> Self {
        Self {
            kind: "text".to_owned(),
            message: Some(message.into()),
            ..Self::default()
        }
    }

    pub fn profile(name: impl Into<String>, bio: impl Into<String>) -> Self {
        Self {
            kind: "user".to_owned(),
            profile: Some(Profile {
                name: Some(name.into()),
                bio: Some(bio.into()),
            }),
            ..Self::default()
        }
    }

    pub fn with_quote(mut self, quote: impl Into<String>) -> Self {
        let quote = quote.into();
        if !quote.trim().is_empty() {
            self.quote_message = Some(quote);
        }
        self
    }

    pub fn with_personal_channel(
        mut self,
        name: impl Into<String>,
        description: impl Into<String>,
    ) -> Self {
        let channel = Channel {
            name: Some(name.into()),
            description: Some(description.into()),
        };
        if channel
            .name
            .as_deref()
            .is_some_and(|name| !name.trim().is_empty())
            || channel
                .description
                .as_deref()
                .is_some_and(|description| !description.trim().is_empty())
        {
            self.personal_channel = Some(channel);
        }
        self
    }

    pub fn with_history(mut self, history: Vec<String>) -> Self {
        if !history.is_empty() {
            self.history_messages = Some(history);
        }
        self
    }
}

// ── 输出 ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Keywords {
    pub keywords: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct QuotedMessage {
    pub has_ad: bool,
    pub keywords: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct MediaFlags {
    pub image_ad: bool,
    pub sticker_ad: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ProfileFlags {
    pub name_ad: bool,
    pub bio_ad: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ContactFlags {
    pub contact_ad: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ChannelFlags {
    #[serde(rename = "Channel_ad")]
    pub channel_ad: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct MentionFlags {
    pub username_ad: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct LinkPreviewFlags {
    pub title_ad: bool,
    pub description_ad: bool,
}

/// AI 的判定结果。字段名与 TS 版一致（`Reason` 大写、`PersonalChannel` 驼峰）。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ModerationResult {
    pub ad_detected: bool,
    pub confidence: f64,
    #[serde(rename = "Reason")]
    pub reason: String,
    pub r18: bool,
    pub message: Keywords,
    pub quoted_message: QuotedMessage,
    pub media: MediaFlags,
    pub profile: ProfileFlags,
    pub contact: ContactFlags,
    #[serde(rename = "PersonalChannel")]
    pub personal_channel: ChannelFlags,
    pub mentioned_username: MentionFlags,
    #[serde(rename = "link_Preview")]
    pub link_preview: LinkPreviewFlags,
}

impl ModerationResult {
    /// 失败 / 未配置的兜底结果（与 TS 版 `defaultErrorResult` 同形）。
    pub fn failure(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
            ..Self::default()
        }
    }

    /// 「资料 / 频道 / 正文」里任一被判为广告。
    pub fn profile_hit(&self) -> bool {
        self.profile.name_ad
            || self.profile.bio_ad
            || self.personal_channel.channel_ad
            || self.ad_detected
    }

    /// 从模型返回的 JSON 解析。
    ///
    /// 校验与 TS 版 `isValidModerationResult`（`aiAdReview.ts`:219-234）逐条对齐：
    /// 12 个嵌套字段缺一个就判「格式不符」，让上层重试；三次都不合格返回失败结果、
    /// **不处置任何人**。放宽校验会把「模型漏字段」变成静默漏判（`profile.bio_ad`
    /// 之类的 false 会被当成真实判断），比一次失败重试危险得多。
    pub fn from_ai_json(value: &Value) -> Option<Self> {
        if !is_complete_result(value) {
            return None;
        }
        let mut result: Self = serde_json::from_value(value.clone()).unwrap_or_default();
        // 以防 serde 默认值覆盖掉真实值（类型被写成数字 / 字符串时）。
        result.ad_detected = value
            .get("ad_detected")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        result.confidence = value
            .get("confidence")
            .and_then(Value::as_f64)
            .unwrap_or(0.0)
            .clamp(0.0, 1.0);
        if let Some(reason) = value.get("Reason").and_then(Value::as_str) {
            result.reason = reason.to_owned();
        }
        // 与 TS 版一致：引用里含广告 → 本次也必须有广告标记。
        if result.quoted_message.has_ad {
            result.ad_detected = true;
        }
        Some(result)
    }
}

/// 与 TS `isValidModerationResult` 一一对应的结构校验。
fn is_complete_result(value: &Value) -> bool {
    fn boolean(value: &Value, key: &str) -> bool {
        value.get(key).and_then(Value::as_bool).is_some()
    }
    fn object<'a>(value: &'a Value, key: &str) -> Option<&'a Value> {
        value.get(key).filter(|nested| nested.is_object())
    }
    fn keywords(value: &Value) -> bool {
        value.get("keywords").and_then(Value::as_array).is_some()
    }

    value.get("Reason").and_then(Value::as_str).is_some()
        && value.get("ad_detected").and_then(Value::as_bool).is_some()
        && value.get("confidence").and_then(Value::as_f64).is_some()
        && object(value, "message").is_some_and(keywords)
        && object(value, "quoted_message")
            .is_some_and(|nested| boolean(nested, "has_ad") && keywords(nested))
        && object(value, "media")
            .is_some_and(|nested| boolean(nested, "image_ad") && boolean(nested, "sticker_ad"))
        && object(value, "profile")
            .is_some_and(|nested| boolean(nested, "name_ad") && boolean(nested, "bio_ad"))
        && object(value, "contact").is_some_and(|nested| boolean(nested, "contact_ad"))
        && object(value, "PersonalChannel").is_some_and(|nested| boolean(nested, "Channel_ad"))
        && object(value, "mentioned_username").is_some_and(|nested| boolean(nested, "username_ad"))
        && object(value, "link_Preview")
            .is_some_and(|nested| boolean(nested, "title_ad") && boolean(nested, "description_ad"))
}

/// 「这次 AI 没跑成」的判据（TS 版用正则，这里是同一组关键词）。
pub fn is_failure_reason(reason: &str) -> bool {
    [
        "失败",
        "未配置",
        "为空",
        "解析",
        "不符合",
        "需人工",
        "提示词",
    ]
    .iter()
    .any(|keyword| reason.contains(keyword))
}

// ── 配置解析 ──────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AiSource {
    /// 群管理员自定义的接口。
    Chat,
    /// 插件配置里的内置默认接口。
    Default,
    /// 都没有：不能审。
    None,
}

#[derive(Debug, Clone)]
pub struct ResolvedAi {
    pub api_key: String,
    pub base_url: String,
    pub model: String,
    pub source: AiSource,
}

impl ResolvedAi {
    pub fn usable(&self) -> bool {
        self.source != AiSource::None && !self.api_key.is_empty() && !self.base_url.is_empty()
    }

    fn from_endpoint(endpoint: &AiEndpoint, source: AiSource) -> Self {
        Self {
            api_key: endpoint.api_key.clone(),
            base_url: endpoint.base_url.clone(),
            model: endpoint.model.clone(),
            source,
        }
    }

    fn unusable() -> Self {
        Self {
            api_key: String::new(),
            base_url: String::new(),
            model: String::new(),
            source: AiSource::None,
        }
    }
}

/// 解析某次审核该用哪个接口。
///
/// 顺序与 TS 版一致：**群自定义 > 内置默认**；区别在于「何时允许用默认接口」——
/// TS 版还有一张写死的群名单，这里只认 `/adaiset` 维护的授权列表（以及强制默认的场景）。
pub fn resolve_ai(rt: &Runtime, chat_id: Option<i64>, force_default: bool) -> ResolvedAi {
    // `force_default` **先判**：`/spam`、`/adaiadd` 这类场景故意绕开群自定义接口，
    // 用内置模型做判定（TS `aiAdReview.ts` 的 `meta.forceDefault` 同款）。
    // 反过来写（先查群配置）会让这些命令花群自己的 key，结论也和 TS 不一致。
    if force_default {
        return match rt.cfg.ai.as_ref().filter(|endpoint| endpoint.is_usable()) {
            Some(endpoint) => ResolvedAi::from_endpoint(endpoint, AiSource::Default),
            None => ResolvedAi::unusable(),
        };
    }

    if let Some(chat_id) = chat_id
        && let Some(config) = rt.db.config(chat_id)
        && config.has_custom_ai()
    {
        return ResolvedAi {
            api_key: config.ai_api_key.unwrap_or_default(),
            base_url: trim_base_url(config.ai_base_url.unwrap_or_default()),
            model: config
                .ai_model
                .filter(|model| !model.trim().is_empty())
                .unwrap_or_else(|| "gpt-4o-mini".to_owned()),
            source: AiSource::Chat,
        };
    }

    let Some(endpoint) = rt.cfg.ai.as_ref().filter(|endpoint| endpoint.is_usable()) else {
        return ResolvedAi::unusable();
    };

    // 内置接口只供给授权群：没有授权记录的群必须自己配。
    let authorized = chat_id.is_some_and(|chat_id| rt.db.is_ai_default_chat(chat_id));
    if !authorized && chat_id.is_some() {
        return ResolvedAi::unusable();
    }
    ResolvedAi::from_endpoint(endpoint, AiSource::Default)
}

fn trim_base_url(mut base_url: String) -> String {
    while base_url.ends_with('/') {
        base_url.pop();
    }
    base_url
}

// ── 调用元信息 ────────────────────────────────────────────────────

/// 调用方补充的展示信息（写总日志、区分场景用）。
#[derive(Debug, Clone, Default)]
pub struct AiMeta {
    pub scene: Option<&'static str>,
    pub message_type: Option<String>,
    pub force_default: bool,
}

// ── 主入口 ────────────────────────────────────────────────────────

/// AI 审核：**永远返回结果**，失败信息在 `Reason` 里。
///
/// `photo` 是本地图片路径（多模态）；`chat_id` 用于选接口与写日志。
pub fn review(
    rt: &Runtime,
    input: &ModerationInput,
    photo: Option<&str>,
    chat_id: Option<i64>,
    meta: &AiMeta,
) -> ModerationResult {
    let has_image = photo.is_some();
    let system_prompt = prompt::build(input, has_image);
    let ai = resolve_ai(rt, chat_id, meta.force_default);

    if !ai.usable() {
        let reason = if chat_id.is_some() {
            "该群未配置AI接口，请管理员在群内使用 /adconfig 并点击「配置AI接口」完成配置"
        } else {
            "未配置默认AI接口，请在 [plugin.fuyubot.adblock.ai] 里填写 api_key / base_url"
        };
        log::warn(
            &rt.host,
            format!("AI 审查跳过（chat={chat_id:?}）：{reason}"),
        );
        let result = ModerationResult::failure(reason_owned(reason));
        write_log(
            rt,
            meta,
            chat_id,
            input,
            Some(&result),
            false,
            Some(reason),
            None,
            has_image,
        );
        return result;
    }

    let user_prompt = format!(
        "{}\nReturn json result in the format specified by the system prompt (strict JSON only).",
        serde_json::to_string_pretty(input).unwrap_or_else(|_| "{}".to_owned())
    );

    let image_base64 = photo.and_then(|path| match std::fs::read(path) {
        Ok(bytes) => Some(base64_encode(&bytes)),
        Err(error) => {
            log::debug(
                &rt.host,
                format!("读图片 {path} 失败（{error}），按纯文本审核"),
            );
            None
        }
    });

    let mut last_error = String::new();
    for attempt in 0..MAX_ATTEMPTS {
        let messages = build_messages(&system_prompt, &user_prompt, image_base64.as_deref());
        match call_api(rt, &ai, &messages) {
            Ok(content) => {
                let string = match content {
                    Some(content) => content,
                    None => {
                        last_error = "AI返回内容为空".to_owned();
                        log::warn(
                            &rt.host,
                            format!("AI 第 {} 次返回空内容（模型 {}）", attempt + 1, ai.model),
                        );
                        continue;
                    }
                };
                let parsed: Value = match serde_json::from_str(&string) {
                    Ok(value) => value,
                    Err(error) => {
                        last_error = "AI返回JSON解析失败".to_owned();
                        log::warn(
                            &rt.host,
                            format!(
                                "AI 第 {} 次返回不是 JSON（{error}）：{}",
                                attempt + 1,
                                truncate(&string, 200)
                            ),
                        );
                        continue;
                    }
                };
                let Some(result) = ModerationResult::from_ai_json(&parsed) else {
                    last_error = "AI返回格式不符合ModerationResult规范".to_owned();
                    log::warn(
                        &rt.host,
                        format!(
                            "AI 第 {} 次返回格式不符：{}",
                            attempt + 1,
                            truncate(&string, 200)
                        ),
                    );
                    continue;
                };
                write_log(
                    rt,
                    meta,
                    chat_id,
                    input,
                    Some(&result),
                    true,
                    None,
                    Some(&ai.model),
                    has_image,
                );
                return result;
            }
            Err(error) => {
                last_error = error;
                log::warn(
                    &rt.host,
                    format!(
                        "AI 第 {} 次调用失败（{} / {}）：{last_error}",
                        attempt + 1,
                        ai.base_url,
                        ai.model
                    ),
                );
            }
        }
    }

    let reason = format!("AI服务{MAX_ATTEMPTS}次调用均失败: {last_error}");
    let result = ModerationResult::failure(reason.clone());
    write_log(
        rt,
        meta,
        chat_id,
        input,
        Some(&result),
        false,
        Some(&reason),
        Some(&ai.model),
        has_image,
    );
    result
}

fn reason_owned(text: &str) -> String {
    text.to_owned()
}

fn build_messages(system: &str, user: &str, image_base64: Option<&str>) -> Value {
    let mut messages = vec![json!({ "role": "system", "content": system })];
    match image_base64 {
        Some(image) => messages.push(json!({
            "role": "user",
            "content": [
                { "type": "text", "text": user },
                {
                    "type": "image_url",
                    "image_url": {
                        "url": format!("data:image/jpeg;base64,{image}"),
                        "detail": "auto"
                    }
                }
            ]
        })),
        None => messages.push(json!({ "role": "user", "content": user })),
    }
    Value::Array(messages)
}

/// gpt-5 / o1 / o3 / o4 只接受默认 temperature，传了就是 400。
fn forbids_custom_temperature(model: &str) -> bool {
    let model = model.to_lowercase();
    model.starts_with("gpt-5")
        || model.starts_with("o1")
        || model.starts_with("o3")
        || model.starts_with("o4")
}

/// 发一次 `/chat/completions`，返回 `choices[0].message.content`。
fn call_api(rt: &Runtime, ai: &ResolvedAi, messages: &Value) -> Result<Option<String>, String> {
    let url = format!("{}/chat/completions", ai.base_url);
    let mut body = json!({
        "model": ai.model,
        "messages": messages,
        "max_tokens": MAX_TOKENS,
        "response_format": { "type": "json_object" },
    });
    let mut has_temperature = false;
    if !forbids_custom_temperature(&ai.model) {
        body["temperature"] = json!(0.2);
        has_temperature = true;
    }

    let (status, text) = send_json(rt, &url, &ai.api_key, &body)?;
    let (status, text) = if status != 200 && has_temperature && text.contains("temperature") {
        // 模型不认自定义 temperature：去掉重试一次（TS 版同款补救）。
        log::warn(
            &rt.host,
            format!("模型 {} 不支持自定义 temperature，去掉后重试", ai.model),
        );
        let mut retry = body.clone();
        if let Some(object) = retry.as_object_mut() {
            object.remove("temperature");
        }
        send_json(rt, &url, &ai.api_key, &retry)?
    } else {
        (status, text)
    };

    if status != 200 {
        return Err(format!("HTTP {status}：{}", truncate(text.trim(), 300)));
    }
    let value: Value =
        serde_json::from_str(&text).map_err(|error| format!("响应不是 JSON（{error}）"))?;
    Ok(value
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|choices| choices.first())
        .and_then(|choice| choice.get("message"))
        .and_then(|message| message.get("content"))
        .and_then(Value::as_str)
        .map(str::to_owned))
}

/// 发 JSON 并读回正文（非 2xx 也读，错误信息在正文里才有用）。
fn send_json(
    rt: &Runtime,
    url: &str,
    api_key: &str,
    body: &Value,
) -> Result<(u16, String), String> {
    let response = rt
        .http
        .post(url)
        .header("Authorization", &format!("Bearer {api_key}"))
        .header("Content-Type", "application/json")
        .header("Accept", "application/json")
        .send(body.to_string())
        .map_err(|error| format!("请求失败：{error}"))?;
    let status = response.status().as_u16();
    let mut response = response;
    let text = response
        .body_mut()
        .read_to_string()
        .map_err(|error| format!("读响应失败：{error}"))?;
    Ok((status, text))
}

// ── 总日志 ────────────────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
fn write_log(
    rt: &Runtime,
    meta: &AiMeta,
    chat_id: Option<i64>,
    input: &ModerationInput,
    result: Option<&ModerationResult>,
    ok: bool,
    error: Option<&str>,
    model: Option<&str>,
    has_image: bool,
) {
    let Some(channel) = rt.cfg.ai_log_channel else {
        return;
    };
    let topic = rt.cfg.ai_log_topic.map(crate::simplify::Topic::forum);

    let chat_label = chat_id
        .map(|chat_id| rt.td.sender_mention(Sender::Chat(chat_id), false))
        .unwrap_or_else(|| "-".to_owned());
    let status = if ok {
        match result {
            Some(result) if result.ad_detected => "✅ 成功 · 判定为广告",
            _ => "✅ 成功 · 未判定为广告",
        }
    } else {
        "❌ 失败"
    };

    let mut lines = vec![
        "#AI审查".to_owned(),
        format!("**场景**: {}", scene_name(meta.scene)),
        format!("**状态**: {status}"),
        format!("**Chat**: {chat_label}"),
    ];
    if let Some(message_type) = &meta.message_type {
        lines.push(format!("**消息类型**: {message_type}"));
    }
    if let Some(model) = model {
        lines.push(format!("**模型**: {model}"));
    }
    if has_image {
        lines.push("**含图片**: 是".to_owned());
    }
    if let Some(error) = error {
        lines.push(format!("**错误**: {error}"));
    }
    lines.push("**输入**:".to_owned());
    lines.push("```json".to_owned());
    lines.push(clip_json(
        &serde_json::to_value(input).unwrap_or(Value::Null),
    ));
    lines.push("```".to_owned());
    if let Some(result) = result {
        lines.push("**结果**:".to_owned());
        lines.push("```json".to_owned());
        lines.push(clip_json(
            &serde_json::to_value(result).unwrap_or(Value::Null),
        ));
        lines.push("```".to_owned());
    }

    let _ = rt.td.send_text(
        channel,
        text::markdown(&lines.join("\n")),
        None,
        Vec::new(),
        topic,
        None,
    );
}

fn scene_name(scene: Option<&str>) -> &str {
    match scene {
        Some("message") => "消息审查",
        Some("message_contact") => "联系人消息",
        Some("new_member") => "入群资料",
        Some("profile_change") => "入群后资料变更复检",
        Some("new_member_review") => "禁言复审",
        Some("spam_user") => "/spam 用户",
        Some("spam_message") => "/spam 消息举报",
        Some("spam_mention") => "/spam @提及",
        Some("adaiadd") => "/adaiadd 文本",
        Some(other) => other,
        None => "未标注场景",
    }
}

fn clip_json(value: &Value) -> String {
    let text = serde_json::to_string_pretty(value).unwrap_or_default();
    if text.chars().count() > LOG_JSON_LIMIT {
        format!("{}\n…(截断)", truncate(&text, LOG_JSON_LIMIT))
    } else {
        text
    }
}

/// 给 `/adconfig` 展示用的「这个群会不会用内置接口」。
pub fn uses_default_ai(rt: &Runtime, chat_id: i64) -> bool {
    rt.cfg
        .ai
        .as_ref()
        .is_some_and(|endpoint| endpoint.is_usable())
        && rt.db.is_ai_default_chat(chat_id)
}

/// 群自定义接口的掩码展示（`sk-a***wxyz`）。
pub fn mask_secret(secret: &str) -> String {
    let chars: Vec<char> = secret.chars().collect();
    if chars.is_empty() {
        return String::new();
    }
    if chars.len() <= 8 {
        return format!("{}***", chars.iter().take(2).collect::<String>());
    }
    format!(
        "{}***{}",
        chars[..4].iter().collect::<String>(),
        chars[chars.len() - 4..].iter().collect::<String>()
    )
}

/// 群的「AI 接口状态」一行（`/adconfig` 用）。
pub fn ai_status_line(rt: &Runtime, chat_id: i64, config: Option<&GroupConfig>) -> String {
    if let Some(config) = config
        && config.has_custom_ai()
    {
        return format!(
            "  → ✅ 已配置（群自定义）\n  → 模型: `{}`\n  → 后端: `{}`\n  → API Key: `{}`",
            config.ai_model.as_deref().unwrap_or("未指定"),
            config.ai_base_url.as_deref().unwrap_or(""),
            mask_secret(config.ai_api_key.as_deref().unwrap_or(""))
        );
    }
    if uses_default_ai(rt, chat_id) {
        let model = rt
            .cfg
            .ai
            .as_ref()
            .map(|endpoint| endpoint.model.as_str())
            .unwrap_or("未知");
        return format!(
            "  → 🟢 使用内置默认接口（模型 {model}）\n  → 群主可点击下方按钮配置自定义接口覆盖默认配置"
        );
    }
    "  → ❌ 未配置，AI 审查当前不可用\n  → 请点击下方按钮跳转到私聊完成配置".to_owned()
}

/// 用一次极小请求验证接口配置（`/start` 配置流程的最后一步）。
///
/// 与 TS 版 `testAiConnection` 等价：`max_tokens = 1`，只关心 HTTP 是否成功。
pub fn test_connection(
    rt: &Runtime,
    api_key: &str,
    base_url: &str,
    model: &str,
) -> Result<(), String> {
    let url = format!("{}/chat/completions", trim_base_url(base_url.to_owned()));
    let body = json!({
        "model": model,
        "messages": [{ "role": "user", "content": "hi" }],
        "max_tokens": 1,
    });
    let (status, text) = send_json(rt, &url, api_key, &body)?;
    if (200..300).contains(&status) {
        Ok(())
    } else {
        Err(format!("HTTP {status}：{}", truncate(text.trim(), 200)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 一份「模型正常返回」的完整结果：严格校验要求 12 个字段齐全。
    fn complete_result() -> Value {
        json!({
            "ad_detected": true,
            "confidence": 0.9,
            "Reason": "广告",
            "message": {"keywords": ["a"]},
            "quoted_message": {"has_ad": false, "keywords": []},
            "media": {"image_ad": false, "sticker_ad": false},
            "profile": {"name_ad": false, "bio_ad": false},
            "contact": {"contact_ad": false},
            "PersonalChannel": {"Channel_ad": false},
            "mentioned_username": {"username_ad": false},
            "link_Preview": {"title_ad": false, "description_ad": false}
        })
    }

    #[test]
    fn ai_json_needs_core_fields() {
        assert!(ModerationResult::from_ai_json(&json!({"Reason": "x"})).is_none());
        // 少一个嵌套字段 = 格式不符 → 让上层重试（TS `isValidModerationResult` 同款）。
        let mut partial = complete_result();
        partial.as_object_mut().expect("对象").remove("profile");
        assert!(ModerationResult::from_ai_json(&partial).is_none());

        let result = ModerationResult::from_ai_json(&complete_result()).expect("合法结果");
        assert!(result.ad_detected);
        assert_eq!(result.message.keywords, vec!["a"]);
    }

    #[test]
    fn quoted_ad_forces_detection() {
        let mut value = complete_result();
        value["ad_detected"] = json!(false);
        value["confidence"] = json!(0.5);
        value["quoted_message"] = json!({"has_ad": true, "keywords": []});
        let result = ModerationResult::from_ai_json(&value).unwrap();
        assert!(result.ad_detected);
    }

    #[test]
    fn failure_reasons_are_recognized() {
        assert!(is_failure_reason("AI服务3次调用均失败: timeout"));
        assert!(is_failure_reason("该群未配置AI接口"));
        assert!(!is_failure_reason("就是普通聊天"));
    }

    #[test]
    fn masking_keeps_edges() {
        assert_eq!(mask_secret("sk-1234567890abcd"), "sk-1***abcd");
        assert_eq!(mask_secret("short"), "sh***");
        assert_eq!(mask_secret(""), "");
    }

    #[test]
    fn temperature_rule() {
        assert!(forbids_custom_temperature("gpt-5.6-luna"));
        assert!(forbids_custom_temperature("o3-mini"));
        assert!(!forbids_custom_temperature("gpt-4o-mini"));
        assert!(!forbids_custom_temperature("deepseek-chat"));
    }

    #[test]
    fn base_url_is_trimmed() {
        assert_eq!(
            trim_base_url("https://api.openai.com/v1/".to_owned()),
            "https://api.openai.com/v1"
        );
    }
}
