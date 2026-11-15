use super::*;

#[test]
fn splits_for_source() {
    assert_eq!(
        split_for_source("(item, i) in items.filter").map(|(l, r)| (l.trim(), r.trim())),
        Some(("(item, i)", "items.filter"))
    );
    assert_eq!(
        split_for_source("value of object").map(|(l, r)| (l.trim(), r.trim())),
        Some(("value", "object"))
    );
}

#[test]
fn parses_aliases() {
    assert_eq!(parse_aliases("item").unwrap(), ["item"]);
    assert_eq!(parse_aliases("(item, index)").unwrap(), ["item", "index"]);
    assert_eq!(
        parse_aliases("(value, key, index)").unwrap(),
        ["value", "key", "index"]
    );
    assert!(parse_aliases("(item, item)").is_err());
    assert!(parse_aliases("(1, index)").is_err());
}

#[test]
fn classifies_directives() {
    assert_eq!(classify("v-if"), AttrClass::If);
    assert_eq!(classify("v-else-if"), AttrClass::ElseIf);
    assert_eq!(classify(":src"), AttrClass::Bind { name: "src" });
    assert_eq!(classify("v-bind:class"), AttrClass::Bind { name: "class" });
    assert_eq!(classify("@click"), AttrClass::Unsupported("@click"));
    assert_eq!(classify("class"), AttrClass::Static);
    assert_eq!(classify("v-model"), AttrClass::Unsupported("v-model"));
}

#[test]
fn finds_value_offset() {
    let source = r#"<img :src=" url ">"#;
    let attr = Attribute {
        name: ":src".to_owned(),
        value: Some(" url ".to_owned()),
        span: Span::new(5, 17),
    };
    // 值从引号后开始（含两侧空格），到结束引号前结束。
    let offset = attribute_value_offset(source, &attr);
    assert_eq!(offset, 11);
    assert_eq!(&source[offset..attr.span.end - 1], " url ");
}
