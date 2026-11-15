use super::*;

#[test]
fn text_message_uses_input_message_text() {
    let params = SendMessageParams::text(Text::plain("你好"));
    let payload = send_params(42, &params, &|text| plain_text(text));
    assert_eq!(payload["chat_id"], 42);
    assert_eq!(
        payload["input_message_content"]["@type"],
        "inputMessageText"
    );
    assert_eq!(payload["input_message_content"]["text"]["text"], "你好");
}

#[test]
fn video_content_nests_input_video() {
    let media = Media::video(InputFile::remote("file-1"))
        .duration(24)
        .size(1920, 1080)
        .streaming(true)
        .caption(Text::plain("第 1 集"));
    let params = SendMessageParams::media(media);
    let payload = send_params(7, &params, &|text| plain_text(text));
    let content = &payload["input_message_content"];
    assert_eq!(content["@type"], "inputMessageVideo");
    assert_eq!(content["video"]["@type"], "inputVideo");
    assert_eq!(content["video"]["video"]["@type"], "inputFileRemote");
    assert_eq!(content["video"]["duration"], 24);
    assert_eq!(content["video"]["supports_streaming"], true);
    assert_eq!(content["caption"]["text"], "第 1 集");
}

#[test]
fn reply_markup_base64_encodes_callback_data() {
    let params = SendMessageParams {
        reply_markup: vec![vec![ReplyButton::callback("好", "ok")]],
        ..SendMessageParams::default()
    };
    let payload = send_params(1, &params, &|text| plain_text(text));
    assert_eq!(
        payload["reply_markup"]["rows"][0][0]["type"]["data"],
        "b2s="
    );
}

#[test]
fn command_of_parses_mention_and_args() {
    let update = json!({
        "message": {
            "chat_id": -100,
            "id": 9,
            "is_outgoing": false,
            "content": {
                "@type": "messageText",
                "text": { "text": "/Help@SomeBot  参数 " }
            }
        }
    });
    let command = command_of(&update).expect("是命令");
    assert_eq!(command.name, "help");
    assert_eq!(command.args, "参数");
    assert_eq!(command.chat_id, -100);
}

#[test]
fn outgoing_or_plain_messages_are_not_commands() {
    let outgoing = json!({
        "message": {
            "chat_id": 1, "id": 2, "is_outgoing": true,
            "content": { "@type": "messageText", "text": { "text": "/help" } }
        }
    });
    assert!(command_of(&outgoing).is_none());

    let plain = json!({
        "message": {
            "chat_id": 1, "id": 2,
            "content": { "@type": "messageText", "text": { "text": "hello" } }
        }
    });
    assert!(command_of(&plain).is_none());
}
