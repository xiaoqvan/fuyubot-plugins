use super::*;

#[test]
fn core_always_present() {
    let prompt = build(&ModerationInput::default(), false);
    assert!(prompt.contains("广告"), "{prompt}");
}

#[test]
fn quote_adds_quote_rules() {
    let input = ModerationInput {
        quote_message: Some("引用内容".to_owned()),
        ..ModerationInput::default()
    };
    let prompt = build(&input, false);
    assert!(prompt.contains("引用"), "{prompt}");
}

#[test]
fn photo_adds_media_rules() {
    let input = ModerationInput {
        kind: "photo".to_owned(),
        ..ModerationInput::default()
    };
    let prompt = build(&input, false);
    assert!(
        prompt.contains("图片") || prompt.contains("照片"),
        "{prompt}"
    );
}
