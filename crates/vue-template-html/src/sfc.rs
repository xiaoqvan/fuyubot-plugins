//! 最小可用的 Vue SFC（`.vue`）辅助：拆块 + 读取 `defineProps`。
//!
//! **不执行 `<script>`**，也不做完整的 TypeScript 解析。它只回答图片生成场景里最需要的
//! 两个问题：
//!
//! 1. `<template>` 在哪（喂给 [`Template::compile`](crate::Template::compile)）；
//! 2. 组件声明了哪些 props（名字 / 类型类别 / 是否可选 / 有没有字面量默认值）。
//!
//! 支持的 `defineProps` 形式：
//!
//! ```ts
//! // 1. 内联类型字面量
//! defineProps<{ title: string; count?: number }>()
//!
//! // 2. 引用同文件里的 interface / type
//! interface Props { title: string }
//! defineProps<Props>()
//!
//! // 3. withDefaults + 字面量默认值
//! withDefaults(defineProps<Props>(), { title: 'hi', count: 1 })
//!
//! // 4. 运行时对象写法（常用构造器 + 字面量 default）
//! defineProps({ title: String, count: { type: Number, default: 0 } })
//!
//! // 5. 数组简写
//! defineProps(['title', 'count'])
//! ```
//!
//! 不支持：`export default { props: ... }`、`PropType<T>` 的深层校验、函数/箭头函数默认值
//! （这类 default 会被忽略）、从其它文件 import 的类型（按 `Any` 处理）。
//!
//! ```no_run
//! use vue_template_html::Sfc;
//!
//! let source = std::fs::read_to_string("Help.vue")?;
//! let sfc = Sfc::parse(&source)?;
//! for prop in sfc.props()? {
//!     println!("{}: {:?}（可选 = {}）", prop.name, prop.kind, prop.optional);
//! }
//! let data = sfc.sample_data();
//! let html = vue_template_html::Template::compile(sfc.template())?.render(&data)?;
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

use serde_json::{Map, Value};

use crate::error::{Error, ErrorKind};

/// prop 的类型类别（只做粗分类，够生成占位数据 / 校验顶层形状）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PropKind {
    String,
    Number,
    Boolean,
    Array,
    Object,
    Function,
    /// 认不出的类型（联合类型、import 进来的类型……）。
    Any,
}

impl PropKind {
    /// 一个能跑通模板的占位值（**不是**业务数据）。
    pub fn placeholder(self) -> Value {
        match self {
            Self::String => Value::String(String::new()),
            Self::Number => Value::from(0),
            Self::Boolean => Value::Bool(false),
            Self::Array => Value::Array(Vec::new()),
            Self::Object => Value::Object(Map::new()),
            Self::Function | Self::Any => Value::Null,
        }
    }
}

/// 一条 prop 声明。
#[derive(Debug, Clone, PartialEq)]
pub struct PropDef {
    pub name: String,
    /// 粗分类；`ty` 里保留原始类型文本。
    pub kind: PropKind,
    /// 原始类型文本（例如 `string[]`、`{ name: string }[]`）。
    pub ty: String,
    /// 类型字面量里带 `?`，或运行时写法里 `required: false`。
    pub optional: bool,
    /// 字面量默认值（对象/数组只支持字面量；函数默认值忽略）。
    pub default: Option<Value>,
}

impl PropDef {
    /// 默认值优先；没有就按类型给占位值。
    pub fn sample(&self) -> Value {
        self.default
            .clone()
            .unwrap_or_else(|| self.kind.placeholder())
    }
}

/// 解析后的 SFC。
#[derive(Debug, Clone)]
pub struct Sfc {
    source: String,
    template: String,
    #[allow(dead_code)]
    template_offset: usize,
    script: Option<String>,
    script_offset: usize,
    styles: Vec<String>,
    /// `defineProps` 的原始声明位置（相对 script），报错定位用。
    props_hint: Option<usize>,
}

impl Sfc {
    /// 解析 `.vue` 源码。只要求有 `<template>`；`<script>` / `<style>` 可选。
    pub fn parse(source: &str) -> Result<Self, Error> {
        let blocks = parse_blocks(source)?;
        let mut template = None;
        let mut template_offset = 0;
        let mut script = None;
        let mut script_offset = 0;
        let mut styles = Vec::new();

        for block in blocks {
            match block.name.as_str() {
                "template" => {
                    if template.is_some() {
                        return Err(Error::new(
                            ErrorKind::Parse,
                            source,
                            block.start,
                            "SFC 里出现了多个 `<template>` 块",
                        ));
                    }
                    if let Some(lang) = block.attr("lang")
                        && !lang.is_empty()
                        && !lang.eq_ignore_ascii_case("html")
                    {
                        return Err(Error::new(
                            ErrorKind::Unsupported,
                            source,
                            block.start,
                            format!("暂不支持 `<template lang=\"{lang}\">`"),
                        ));
                    }
                    template = Some(source[block.content_start..block.content_end].to_owned());
                    template_offset = block.content_start;
                }
                "script" => {
                    if script.is_some() {
                        return Err(Error::new(
                            ErrorKind::Parse,
                            source,
                            block.start,
                            "SFC 里出现了多个 `<script>` 块",
                        ));
                    }
                    script = Some(source[block.content_start..block.content_end].to_owned());
                    script_offset = block.content_start;
                }
                "style" => styles.push(source[block.content_start..block.content_end].to_owned()),
                _ => {}
            }
        }

        let Some(template) = template else {
            return Err(Error::new(
                ErrorKind::Parse,
                source,
                0,
                "SFC 缺少 `<template>` 块",
            ));
        };

        // 提前定位 defineProps，供 props() 报错时给出大致位置。
        let props_hint = script
            .as_deref()
            .and_then(|script| find_word(script, "defineProps"));

        Ok(Self {
            source: source.to_owned(),
            template,
            template_offset,
            script,
            script_offset,
            styles,
            props_hint,
        })
    }

    /// `<template>` 内容（不含外层 `<template>` 标签）。
    pub fn template(&self) -> &str {
        &self.template
    }

    /// 原始 `.vue` 源码。
    pub fn source(&self) -> &str {
        &self.source
    }

    /// `<script>` / `<script setup>` 的内容（不含标签）。
    pub fn script(&self) -> Option<&str> {
        self.script.as_deref()
    }

    /// 所有 `<style>` 块的内容。
    pub fn styles(&self) -> &[String] {
        &self.styles
    }

    /// 读取 `defineProps` 声明的 props。
    ///
    /// 找不到 `defineProps`（例如用 `export default { props }`）时返回空数组。
    pub fn props(&self) -> Result<Vec<PropDef>, Error> {
        let Some(script) = &self.script else {
            return Ok(Vec::new());
        };
        parse_props(script).map_err(|error| {
            Error::new(
                ErrorKind::Parse,
                &self.source,
                self.script_offset + error.offset,
                format!("读取 props 失败：{}", error.message),
            )
        })
    }

    /// 按 props 生成一份占位数据：有默认值用默认值，否则按类型给空值。
    ///
    /// 只是为了让模板能跑起来；真实渲染请传业务数据。
    pub fn sample_data(&self) -> Value {
        let mut object = Map::new();
        if let Ok(props) = self.props() {
            for prop in props {
                object.insert(prop.name.clone(), prop.sample());
            }
        }
        Value::Object(object)
    }

    /// `defineProps` 在 script 里的偏移（没有就是 `None`）。
    pub fn props_offset(&self) -> Option<usize> {
        self.props_hint.map(|offset| self.script_offset + offset)
    }
}

// ── 块扫描 ────────────────────────────────────────────────────────

struct Block {
    name: String,
    attrs: Vec<(String, Option<String>)>,
    start: usize,
    content_start: usize,
    content_end: usize,
}

impl Block {
    fn attr(&self, name: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .and_then(|(_, value)| value.as_deref())
    }
}

fn parse_blocks(source: &str) -> Result<Vec<Block>, Error> {
    let bytes = source.as_bytes();
    let mut blocks = Vec::new();
    let mut index = 0;

    while index < bytes.len() {
        if bytes[index] != b'<' {
            index += 1;
            continue;
        }
        if source[index..].starts_with("<!--") {
            index = source[index..]
                .find("-->")
                .map(|offset| index + offset + 3)
                .unwrap_or(bytes.len());
            continue;
        }
        let Some(tag) = scan_open_tag(source, index) else {
            index += 1;
            continue;
        };
        match tag.name.as_str() {
            "template" => {
                let close = find_matching_template(source, tag.content_start).ok_or_else(|| {
                    Error::new(ErrorKind::Parse, source, index, "未闭合的 `<template>` 块")
                })?;
                blocks.push(Block {
                    name: tag.name,
                    attrs: tag.attrs,
                    start: index,
                    content_start: tag.content_start,
                    content_end: close.start,
                });
                index = close.end;
            }
            "script" | "style" => {
                let close =
                    find_block_close(source, tag.content_start, &tag.name).ok_or_else(|| {
                        Error::new(
                            ErrorKind::Parse,
                            source,
                            index,
                            format!("未闭合的 `<{}>` 块", tag.name),
                        )
                    })?;
                blocks.push(Block {
                    name: tag.name,
                    attrs: tag.attrs,
                    start: index,
                    content_start: tag.content_start,
                    content_end: close.start,
                });
                index = close.end;
            }
            _ => index = tag.end,
        }
    }

    Ok(blocks)
}

struct ScannedTag {
    name: String,
    attrs: Vec<(String, Option<String>)>,
    content_start: usize,
    end: usize,
}

/// 从 `<` 开始读一个开始标签（只用于 SFC 顶层块，不做 HTML 语法校验）。
fn scan_open_tag(source: &str, at: usize) -> Option<ScannedTag> {
    let bytes = source.as_bytes();
    let mut index = at + 1;
    if index >= bytes.len() || !bytes[index].is_ascii_alphabetic() {
        return None;
    }
    let name_start = index;
    while index < bytes.len()
        && (bytes[index].is_ascii_alphanumeric() || bytes[index] == b'-' || bytes[index] == b'_')
    {
        index += 1;
    }
    let name = source[name_start..index].to_ascii_lowercase();

    let mut attrs = Vec::new();
    loop {
        while index < bytes.len() && bytes[index].is_ascii_whitespace() {
            index += 1;
        }
        if index >= bytes.len() {
            return None;
        }
        if bytes[index] == b'>' {
            return Some(ScannedTag {
                name,
                attrs,
                content_start: index + 1,
                end: index + 1,
            });
        }
        if bytes[index] == b'/' {
            index += 1;
            continue;
        }
        let key_start = index;
        while index < bytes.len()
            && !bytes[index].is_ascii_whitespace()
            && !matches!(bytes[index], b'=' | b'>' | b'/')
        {
            index += 1;
        }
        if key_start == index {
            return None;
        }
        let key = source[key_start..index].to_owned();
        while index < bytes.len() && bytes[index].is_ascii_whitespace() {
            index += 1;
        }
        let mut value = None;
        if index < bytes.len() && bytes[index] == b'=' {
            index += 1;
            while index < bytes.len() && bytes[index].is_ascii_whitespace() {
                index += 1;
            }
            if index >= bytes.len() {
                return None;
            }
            let (parsed, next) = scan_attr_value(source, index)?;
            value = Some(parsed);
            index = next;
        }
        attrs.push((key, value));
    }
}

fn scan_attr_value(source: &str, at: usize) -> Option<(String, usize)> {
    let bytes = source.as_bytes();
    match bytes[at] {
        quote @ (b'"' | b'\'') => {
            let mut index = at + 1;
            let start = index;
            while index < bytes.len() && bytes[index] != quote {
                index += 1;
            }
            if index >= bytes.len() {
                return None;
            }
            Some((source[start..index].to_owned(), index + 1))
        }
        _ => {
            let mut index = at;
            while index < bytes.len()
                && !bytes[index].is_ascii_whitespace()
                && !matches!(bytes[index], b'>' | b'/')
            {
                index += 1;
            }
            Some((source[at..index].to_owned(), index))
        }
    }
}

struct Close {
    start: usize,
    end: usize,
}

/// 模板块的结束位置：要跳过模板内部嵌套的 `<template v-if>`。
fn find_matching_template(source: &str, from: usize) -> Option<Close> {
    let bytes = source.as_bytes();
    let mut depth = 1usize;
    let mut index = from;
    while index < bytes.len() {
        let offset = source[index..].find('<')?;
        let at = index + offset;
        if source[at..].starts_with("</") {
            if tag_follows(source, at + 2, "template") {
                depth -= 1;
                if depth == 0 {
                    let end = source[at..].find('>')? + at + 1;
                    return Some(Close { start: at, end });
                }
            }
        } else if tag_follows(source, at + 1, "template") {
            depth += 1;
        }
        index = at + 1;
    }
    None
}

fn find_block_close(source: &str, from: usize, name: &str) -> Option<Close> {
    let lower = source[from..].to_ascii_lowercase();
    let needle = format!("</{name}");
    let mut index = 0;
    while let Some(offset) = lower[index..].find(&needle) {
        let at = from + index + offset;
        if tag_follows(source, at + 2, name) {
            let end = source[at..].find('>')? + at + 1;
            return Some(Close { start: at, end });
        }
        index += offset + needle.len();
    }
    None
}

/// `at` 处是否是 `name`（大小写不敏感）且后面是标签边界。
fn tag_follows(source: &str, at: usize, name: &str) -> bool {
    let bytes = source.as_bytes();
    let end = at + name.len();
    if end > bytes.len() {
        return false;
    }
    if !source[at..end].eq_ignore_ascii_case(name) {
        return false;
    }
    match bytes.get(end) {
        None => true,
        Some(byte) => byte.is_ascii_whitespace() || matches!(*byte, b'>' | b'/'),
    }
}

// ── defineProps ───────────────────────────────────────────────────

struct PropsError {
    offset: usize,
    message: String,
}

fn parse_props(script: &str) -> Result<Vec<PropDef>, PropsError> {
    let Some(start) = find_word(script, "defineProps") else {
        return Ok(Vec::new());
    };
    let after = skip_ws(script, start + "defineProps".len());

    if script[after..].starts_with('<') {
        let (type_text, type_end) = take_angle_group(script, after)?;
        let defaults = if wrapped_in_with_defaults(script, start) {
            parse_with_defaults_object(script, type_end)?
        } else {
            Vec::new()
        };
        let members = resolve_type_members(script, type_text.trim());
        return Ok(merge_defaults(parse_type_members(members), defaults));
    }

    if script[after..].starts_with('(') {
        let (inside, _) = take_paren_group(script, after)?;
        let inside = inside.trim();
        if inside.starts_with('{') {
            return Ok(parse_runtime_props(inside));
        }
        if inside.starts_with('[') {
            return Ok(parse_array_props(inside));
        }
        return Ok(Vec::new());
    }

    Ok(Vec::new())
}

fn parse_type_members(block: &str) -> Vec<PropDef> {
    // 内联类型字面量带外层 `{}`；interface 的 body 不带。两种都接受。
    let block = block.trim();
    let block = block
        .strip_prefix('{')
        .and_then(|inner| inner.strip_suffix('}'))
        .unwrap_or(block);
    let cleaned = strip_comments(block);
    let mut props = Vec::new();
    for member in split_top_level(&cleaned) {
        let member = member.trim();
        if member.is_empty() {
            continue;
        }
        let member = member.strip_prefix("readonly ").unwrap_or(member);
        let Some(colon) = find_top_level_char(member, ':') else {
            continue;
        };
        let mut name = member[..colon].trim();
        let mut optional = false;
        if let Some(stripped) = name.strip_suffix('?') {
            name = stripped.trim();
            optional = true;
        }
        let name = unquote(name);
        if name.is_empty() {
            continue;
        }
        let ty = member[colon + 1..].trim().to_owned();
        props.push(PropDef {
            name,
            kind: classify_type(&ty),
            ty,
            optional,
            default: None,
        });
    }
    props
}

/// `defineProps<Props>()` / `defineProps<Props | Other>()` 里解析出类型成员。
fn resolve_type_members<'a>(script: &'a str, type_text: &'a str) -> &'a str {
    if type_text.starts_with('{') {
        return type_text;
    }
    // 取第一个标识符（联合类型只认第一个）
    let name: String = type_text
        .chars()
        .take_while(|ch| ch.is_ascii_alphanumeric() || *ch == '_' || *ch == '$')
        .collect();
    if name.is_empty() {
        return "{}";
    }
    find_interface_body(script, &name).unwrap_or("{}")
}

fn find_interface_body<'a>(script: &'a str, name: &str) -> Option<&'a str> {
    for keyword in ["interface", "type"] {
        let mut search = 0;
        while let Some(offset) = find_word(&script[search..], keyword) {
            let at = search + offset;
            let after = skip_ws(script, at + keyword.len());
            if script[after..].starts_with(name)
                && script[after + name.len()..]
                    .chars()
                    .next()
                    .is_some_and(|ch| ch.is_whitespace() || ch == '<' || ch == '=')
            {
                let mut cursor = skip_ws(script, after + name.len());
                if script[cursor..].starts_with('<') {
                    // 泛型接口跳过类型参数
                    let (_, end) = take_angle_group(script, cursor).ok()?;
                    cursor = skip_ws(script, end);
                }
                if script[cursor..].starts_with('=') {
                    cursor = skip_ws(script, cursor + 1);
                }
                if script[cursor..].starts_with('{') {
                    let (body, _) = take_brace_group(script, cursor).ok()?;
                    return Some(body);
                }
            }
            search = at + keyword.len();
        }
    }
    None
}

fn parse_runtime_props(block: &str) -> Vec<PropDef> {
    let Some(body) = brace_body(block) else {
        return Vec::new();
    };
    let cleaned = strip_comments(body);
    let mut props = Vec::new();
    for member in split_top_level(&cleaned) {
        let member = member.trim();
        if member.is_empty() {
            continue;
        }
        let Some(colon) = find_top_level_char(member, ':') else {
            continue;
        };
        let name = unquote(member[..colon].trim());
        if name.is_empty() {
            continue;
        }
        let rest = member[colon + 1..].trim();
        let (kind, default, optional) = if rest.starts_with('{') {
            let Some(config) = brace_body(rest) else {
                continue;
            };
            let kind = config
                .split(',')
                .find_map(|entry| {
                    let entry = entry.trim();
                    entry
                        .strip_prefix("type:")
                        .map(|value| classify_ctor(value.trim()))
                })
                .unwrap_or(PropKind::Any);
            let default = config.split(',').find_map(|entry| {
                let entry = entry.trim();
                entry
                    .strip_prefix("default:")
                    .and_then(|value| parse_literal(value.trim()))
            });
            let optional = config
                .split(',')
                .any(|entry| entry.trim().starts_with("required: false"));
            (kind, default, optional)
        } else {
            (classify_ctor(rest), None, false)
        };
        props.push(PropDef {
            name,
            kind,
            ty: rest.to_owned(),
            optional,
            default,
        });
    }
    props
}

fn parse_array_props(block: &str) -> Vec<PropDef> {
    let inner = block.trim();
    let inner = inner
        .strip_prefix('[')
        .and_then(|text| text.strip_suffix(']'))
        .unwrap_or(inner);
    inner
        .split(',')
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(|name| {
            let name = unquote(name);
            PropDef {
                name,
                kind: PropKind::Any,
                ty: "any".to_owned(),
                optional: false,
                default: None,
            }
        })
        .collect()
}

fn classify_ctor(text: &str) -> PropKind {
    let text = text.trim();
    let first = text.trim_start_matches(['[', ' ']).trim_start();
    let name: String = first
        .chars()
        .take_while(|ch| ch.is_ascii_alphanumeric())
        .collect();
    match name.as_str() {
        "String" => PropKind::String,
        "Number" => PropKind::Number,
        "Boolean" => PropKind::Boolean,
        "Array" => PropKind::Array,
        "Object" => PropKind::Object,
        "Function" => PropKind::Function,
        _ => PropKind::Any,
    }
}

fn classify_type(text: &str) -> PropKind {
    let text = text.trim();
    // 去掉 `| null` / `| undefined`，取第一个非空类型。
    let mut parts = split_union(text);
    parts.retain(|part| {
        let part = part.trim();
        !part.is_empty() && part != "null" && part != "undefined"
    });
    let text = parts.first().copied().unwrap_or(text).trim();

    // 先判数组：`{ name: string }[]` 以 `[]` 结尾，不能因为以 `{` 开头就当对象。
    if text.ends_with("[]") || text.starts_with("Array<") || text.starts_with("ReadonlyArray<") {
        return PropKind::Array;
    }
    if text.starts_with('{') || text.starts_with("Record<") || text == "object" {
        return PropKind::Object;
    }
    if text.starts_with("() =>") || text == "Function" || text.starts_with("Function") {
        return PropKind::Function;
    }
    match text {
        "string" => PropKind::String,
        "number" | "bigint" => PropKind::Number,
        "boolean" => PropKind::Boolean,
        _ => PropKind::Any,
    }
}

fn split_union(text: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut depth = 0i32;
    let mut start = 0;
    let bytes = text.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'{' | b'[' | b'(' | b'<' => depth += 1,
            b'}' | b']' | b')' | b'>' => depth -= 1,
            b'|' if depth == 0 => {
                parts.push(&text[start..index]);
                start = index + 1;
            }
            _ => {}
        }
        index += 1;
    }
    parts.push(&text[start..]);
    parts
}

fn wrapped_in_with_defaults(script: &str, props_at: usize) -> bool {
    let prefix = &script[..props_at];
    let Some(offset) = prefix.rfind("withDefaults") else {
        return false;
    };
    let between = prefix[offset + "withDefaults".len()..].trim();
    between == "("
}

fn parse_with_defaults_object(
    script: &str,
    type_end: usize,
) -> Result<Vec<(String, Value)>, PropsError> {
    let mut cursor = skip_ws(script, type_end);
    if !script[cursor..].starts_with('(') {
        return Ok(Vec::new());
    }
    let (_, end) = take_paren_group(script, cursor)?;
    cursor = skip_ws(script, end);
    if !script[cursor..].starts_with(',') {
        return Ok(Vec::new());
    }
    cursor = skip_ws(script, cursor + 1);
    if !script[cursor..].starts_with('{') {
        return Ok(Vec::new());
    }
    let (body, _) = take_brace_group(script, cursor)?;
    let cleaned = strip_comments(body);
    let mut defaults = Vec::new();
    for member in split_top_level(&cleaned) {
        let member = member.trim();
        if member.is_empty() {
            continue;
        }
        let Some(colon) = find_top_level_char(member, ':') else {
            continue;
        };
        let name = unquote(member[..colon].trim());
        if name.is_empty() {
            continue;
        }
        if let Some(value) = parse_literal(member[colon + 1..].trim()) {
            defaults.push((name, value));
        }
    }
    Ok(defaults)
}

fn merge_defaults(mut props: Vec<PropDef>, defaults: Vec<(String, Value)>) -> Vec<PropDef> {
    for (name, value) in defaults {
        if let Some(prop) = props.iter_mut().find(|prop| prop.name == name) {
            prop.default = Some(value);
        }
    }
    props
}

// ── 字面量 / 文本工具 ─────────────────────────────────────────────

fn parse_literal(text: &str) -> Option<Value> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    if let Some(rest) = text.strip_prefix("() =>") {
        return parse_literal(rest.trim());
    }
    let first = text.chars().next()?;
    match first {
        '"' | '\'' => parse_string_literal(text).map(Value::String),
        '[' => {
            let body = text.strip_prefix('[')?.strip_suffix(']')?;
            let mut items = Vec::new();
            for item in split_top_level(body) {
                let item = item.trim();
                if item.is_empty() {
                    continue;
                }
                items.push(parse_literal(item)?);
            }
            Some(Value::Array(items))
        }
        '{' => {
            let body = text.strip_prefix('{')?.strip_suffix('}')?;
            let mut object = Map::new();
            for member in split_top_level(body) {
                let member = member.trim();
                if member.is_empty() {
                    continue;
                }
                let colon = find_top_level_char(member, ':')?;
                let key = unquote(member[..colon].trim());
                if key.is_empty() {
                    continue;
                }
                object.insert(key, parse_literal(member[colon + 1..].trim())?);
            }
            Some(Value::Object(object))
        }
        '-' | '0'..='9' => {
            if let Ok(int) = text.parse::<i64>() {
                return Some(Value::from(int));
            }
            let number: f64 = text.parse().ok()?;
            serde_json::Number::from_f64(number).map(Value::Number)
        }
        _ => match text {
            "true" => Some(Value::Bool(true)),
            "false" => Some(Value::Bool(false)),
            "null" | "undefined" => Some(Value::Null),
            _ => None,
        },
    }
}

fn parse_string_literal(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let quote = *bytes.first()?;
    if quote != b'"' && quote != b'\'' {
        return None;
    }
    let mut out = String::new();
    let mut index = 1;
    while index < bytes.len() {
        match bytes[index] {
            b'\\' => {
                index += 1;
                let escaped = *bytes.get(index)?;
                match escaped {
                    b'n' => out.push('\n'),
                    b'r' => out.push('\r'),
                    b't' => out.push('\t'),
                    b'\\' => out.push('\\'),
                    b'\'' => out.push('\''),
                    b'"' => out.push('"'),
                    other => out.push(other as char),
                }
                index += 1;
            }
            byte if byte == quote => return Some(out),
            byte => {
                // 逐字符处理（保留 UTF-8）
                let ch = text[index..].chars().next()?;
                out.push(ch);
                index += ch.len_utf8();
                let _ = byte;
            }
        }
    }
    None
}

fn brace_body(text: &str) -> Option<&str> {
    let text = text.trim();
    let inner = text.strip_prefix('{')?;
    let end = inner.rfind('}')?;
    Some(&inner[..end])
}

fn take_angle_group(text: &str, at: usize) -> Result<(&str, usize), PropsError> {
    take_group(text, at, '<', '>')
}

fn take_paren_group(text: &str, at: usize) -> Result<(&str, usize), PropsError> {
    take_group(text, at, '(', ')')
}

fn take_brace_group(text: &str, at: usize) -> Result<(&str, usize), PropsError> {
    take_group(text, at, '{', '}')
}

fn take_group(text: &str, at: usize, open: char, close: char) -> Result<(&str, usize), PropsError> {
    let bytes = text.as_bytes();
    if bytes.get(at).copied() != Some(open as u8) {
        return Err(PropsError {
            offset: at,
            message: format!("期望 `{open}`"),
        });
    }
    let mut depth = 0i32;
    let mut index = at;
    while index < bytes.len() {
        match bytes[index] as char {
            ch if ch == open => depth += 1,
            ch if ch == close => {
                depth -= 1;
                if depth == 0 {
                    return Ok((&text[at + 1..index], index + 1));
                }
            }
            _ => {}
        }
        index += 1;
    }
    Err(PropsError {
        offset: at,
        message: format!("未闭合的 `{open}`"),
    })
}

fn strip_comments(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '/' && chars.peek() == Some(&'/') {
            for next in chars.by_ref() {
                if next == '\n' {
                    break;
                }
            }
        } else if ch == '/' && chars.peek() == Some(&'*') {
            chars.next();
            let mut previous = '\u{0}';
            for next in chars.by_ref() {
                if previous == '*' && next == '/' {
                    break;
                }
                previous = next;
            }
        } else if ch == '"' || ch == '\u{0027}' {
            out.push(ch);
            let quote = ch;
            while let Some(next) = chars.next() {
                out.push(next);
                if next == '\u{005C}' {
                    if let Some(escaped) = chars.next() {
                        out.push(escaped);
                    }
                } else if next == quote {
                    break;
                }
            }
        } else {
            out.push(ch);
        }
    }
    out
}

/// 按顶层 `;` / `,` / 换行切分类型成员。
fn split_top_level(text: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut depth = 0i32;
    let mut start = 0;
    let bytes = text.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'"' | b'\'' => {
                let quote = bytes[index];
                index += 1;
                while index < bytes.len() {
                    if bytes[index] == b'\\' {
                        index += 2;
                        continue;
                    }
                    if bytes[index] == quote {
                        break;
                    }
                    index += 1;
                }
            }
            b'{' | b'[' | b'(' | b'<' => depth += 1,
            b'}' | b']' | b')' | b'>' => depth -= 1,
            b';' | b',' if depth == 0 => {
                parts.push(&text[start..index]);
                start = index + 1;
            }
            b'\n' if depth == 0 => {
                parts.push(&text[start..index]);
                start = index + 1;
            }
            _ => {}
        }
        index += 1;
    }
    parts.push(&text[start..]);
    parts
}

fn find_top_level_char(text: &str, needle: char) -> Option<usize> {
    let mut depth = 0i32;
    let bytes = text.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] as char {
            ch if ch == needle && depth == 0 => return Some(index),
            '{' | '[' | '(' | '<' => depth += 1,
            '}' | ']' | ')' | '>' => depth -= 1,
            _ => {}
        }
        index += 1;
    }
    None
}

fn skip_ws(text: &str, mut index: usize) -> usize {
    let bytes = text.as_bytes();
    while index < bytes.len() && bytes[index].is_ascii_whitespace() {
        index += 1;
    }
    index
}

fn unquote(text: &str) -> String {
    let text = text.trim();
    let bytes = text.as_bytes();
    if bytes.len() >= 2 && matches!(bytes[0], b'"' | b'\'') && bytes[bytes.len() - 1] == bytes[0] {
        return text[1..text.len() - 1].to_owned();
    }
    text.to_owned()
}

fn find_word(text: &str, word: &str) -> Option<usize> {
    let bytes = text.as_bytes();
    let needle = word.as_bytes();
    let mut index = 0;
    while index + needle.len() <= bytes.len() {
        if bytes[index..].starts_with(needle) {
            let before_ok =
                index == 0 || !bytes[index - 1].is_ascii_alphanumeric() && bytes[index - 1] != b'_';
            let after = index + word.len();
            let after_ok = after == bytes.len()
                || !bytes[after].is_ascii_alphanumeric() && bytes[after] != b'_';
            if before_ok && after_ok {
                return Some(index);
            }
        }
        index += 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const HELP: &str = r#"<template>
  <div><span v-if="ok">{{ title }}</span><template v-if="x"><b>nested</b></template></div>
</template>
<script setup lang="ts">
defineProps<{
    title: string;
    description: string;
    tips?: string;
    imgSrc: string;
    data: {
        name: string;
        commands: { name: string; desc: string }[];
    }[]
}>()
</script>
<style>.x { color: red }</style>
"#;

    #[test]
    fn parses_blocks_and_props() {
        let sfc = Sfc::parse(HELP).unwrap();
        assert!(sfc.template().contains("{{ title }}"));
        assert!(sfc.template().contains("<template v-if=\"x\">"));
        assert!(sfc.template().ends_with("</div>\n"));
        assert!(sfc.script().unwrap().contains("defineProps"));
        assert_eq!(sfc.styles().len(), 1);

        let props = sfc.props().unwrap();
        let names: Vec<_> = props.iter().map(|prop| prop.name.as_str()).collect();
        assert_eq!(names, ["title", "description", "tips", "imgSrc", "data"]);
        assert_eq!(props[0].kind, PropKind::String);
        assert!(props[2].optional);
        assert_eq!(props[4].kind, PropKind::Array);
    }

    #[test]
    fn named_interface_props() {
        let source = r#"<template><i/></template>
<script setup lang="ts">
interface Props {
    title: string
    count?: number
    list: string[]
}
const props = defineProps<Props>()
</script>"#;
        let props = Sfc::parse(source).unwrap().props().unwrap();
        assert_eq!(props.len(), 3);
        assert_eq!(props[0].kind, PropKind::String);
        assert_eq!(props[1].kind, PropKind::Number);
        assert!(props[1].optional);
        assert_eq!(props[2].kind, PropKind::Array);
    }

    #[test]
    fn with_defaults_literals() {
        let source = r#"<template><i/></template>
<script setup>
withDefaults(defineProps<{ title?: string; count?: number; list?: string[] }>(), {
    title: 'hi',
    count: 3,
    list: []
})
</script>"#;
        let props = Sfc::parse(source).unwrap().props().unwrap();
        assert_eq!(props[0].default, Some(Value::String("hi".into())));
        assert_eq!(props[1].default, Some(Value::from(3)));
        assert_eq!(props[2].default, Some(Value::Array(vec![])));
    }

    #[test]
    fn runtime_object_props() {
        let source = r#"<template><i/></template>
<script>
defineProps({
    title: String,
    count: { type: Number, default: 0 },
    list: { type: Array, default: () => [] }
})
</script>"#;
        let props = Sfc::parse(source).unwrap().props().unwrap();
        assert_eq!(props[0].kind, PropKind::String);
        assert_eq!(props[1].kind, PropKind::Number);
        assert_eq!(props[1].default, Some(Value::from(0)));
        assert_eq!(props[2].kind, PropKind::Array);
        assert_eq!(
            props[2].default,
            Some(Value::Array(vec![])),
            "`() => []` 这种简单箭头默认值会按字面量解析"
        );
    }

    #[test]
    fn array_shorthand_props() {
        let source = r#"<template><i/></template>
<script setup>defineProps(['title', 'count'])</script>"#;
        let props = Sfc::parse(source).unwrap().props().unwrap();
        assert_eq!(props.len(), 2);
        assert_eq!(props[0].kind, PropKind::Any);
    }

    #[test]
    fn sample_data_uses_defaults_and_placeholders() {
        let source = r#"<template><i/></template>
<script setup>defineProps<{ title: string; count?: number }>()</script>"#;
        let sfc = Sfc::parse(source).unwrap();
        let data = sfc.sample_data();
        assert_eq!(data["title"], "");
        assert_eq!(data["count"], 0);
    }

    #[test]
    fn missing_template_is_an_error() {
        let error = Sfc::parse("<script setup></script>").unwrap_err();
        assert_eq!(error.kind, ErrorKind::Parse);
        assert!(error.message.contains("template"), "{error}");
    }

    #[test]
    fn script_only_and_no_props() {
        let source = "<template><i/></template>";
        let sfc = Sfc::parse(source).unwrap();
        assert!(sfc.props().unwrap().is_empty());
        assert_eq!(sfc.sample_data(), Value::Object(Map::new()));
    }
}
