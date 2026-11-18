use super::*;
use serde_json::json;

#[test]
fn html_text_json_carries_the_parse_mode() {
    let value = text_json(&Text::html("<b>hi</b>"));
    assert_eq!(value["parse_mode"], json!("html"));
    assert_eq!(value["text"], json!("<b>hi</b>"));
}

#[test]
fn command_of_parses_name_args_and_addressing() {
    let update = json!({
        "message": {
            "chat_id": 1,
            "id": 2,
            "content": {
                "@type": "messageText",
                "text": { "text": "/X@SomeBot  https://x.com/a/status/9" }
            }
        }
    });
    let command = command_of(&update).expect("应解析出命令");
    assert_eq!(command.name, "x");
    assert_eq!(command.args, "https://x.com/a/status/9");
    assert_eq!(command.addressed_to.as_deref(), Some("SomeBot"));
    assert!(command.is("x"));
}

#[test]
fn addressed_commands_only_answer_our_own_name() {
    let mut command = Command {
        chat_id: 1,
        message_id: 2,
        name: "x".to_owned(),
        args: String::new(),
        addressed_to: Some("OtherBot".to_owned()),
        text: "/x@OtherBot".to_owned(),
        is_channel_post: false,
    };
    assert!(!command.is_for(&["FuyuBot".to_owned()]));
    assert!(command.is_for(&["OtherBot".to_owned()]));

    // 裸命令谁都可以响应。
    command.addressed_to = None;
    assert!(command.is_for(&["FuyuBot".to_owned()]));
}

#[test]
fn album_params_carry_every_item_and_one_caption() {
    let params = SendAlbumParams {
        medias: vec![
            Media::photo(InputFile::remote("https://example.test/1.jpg")).size(100, 100),
            Media::photo(InputFile::remote("https://example.test/2.jpg")).size(100, 100),
        ],
        caption: Some(Text::html("<b>标题</b>")),
        timeout_ms: 1000,
    };
    let render = |text: &Text| text_json(text);
    let args = album_params(-100, &params, &render);
    assert_eq!(args["chat_id"], json!(-100));
    let contents = args["input_message_contents"].as_array().expect("应是数组");
    assert_eq!(contents.len(), 2);
    for content in contents {
        assert_eq!(content["caption"]["parse_mode"], json!("html"));
    }
}
