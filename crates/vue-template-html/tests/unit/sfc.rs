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
