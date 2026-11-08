//! 读取一个 `.vue` 文件：打印 `defineProps` 声明的 props，渲染 `<template>`。
//!
//! ```bash
//! cargo run --example render_sfc -- path/to/Help.vue
//! cargo run --example render_sfc -- path/to/Help.vue data.json
//! ```
//!
//! `data.json` 是可选的 props 数据，会覆盖按默认值/类型生成的占位数据。

use std::env;
use std::fs;

use serde_json::Value;
use vue_template_html::{Sfc, Template};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = env::args().nth(1).unwrap_or_else(|| "Help.vue".to_owned());
    let source = fs::read_to_string(&path)?;
    let sfc = Sfc::parse(&source)?;

    let props = sfc.props()?;
    println!("props（{} 个）：", props.len());
    for prop in &props {
        println!(
            "  {:<14} {:<8} optional={:<5} default={}",
            prop.name,
            format!("{:?}", prop.kind),
            prop.optional,
            prop.default
                .as_ref()
                .map(Value::to_string)
                .unwrap_or_else(|| "-".to_owned())
        );
    }

    let mut data = sfc.sample_data();
    if let Some(data_path) = env::args().nth(2) {
        let overrides: Value = serde_json::from_str(&fs::read_to_string(data_path)?)?;
        merge(&mut data, &overrides);
    }

    let html = Template::compile(sfc.template())?.render(&data)?;
    println!("\n{html}");
    Ok(())
}

/// 把 `overrides` 合并进 `data`（顶层浅合并，够演示 props 覆盖）。
fn merge(data: &mut Value, overrides: &Value) {
    let (Some(target), Some(source)) = (data.as_object_mut(), overrides.as_object()) else {
        return;
    };
    for (key, value) in source {
        target.insert(key.clone(), value.clone());
    }
}
