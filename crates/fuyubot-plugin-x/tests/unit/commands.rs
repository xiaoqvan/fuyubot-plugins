use super::*;
use serde_json::json;

#[test]
fn accepts_the_common_x_link_shapes() {
    for url in [
        "https://x.com/user/status/2007184284944322584",
        "http://x.com/user/status/2007184284944322584?s=20",
        "https://twitter.com/i/status/2007184284944322584",
        "https://mobile.twitter.com/user/status/2007184284944322584/photo/1",
        "https://x.com/i/article/2007184284944322584",
        "x.com/user/status/2007184284944322584",
    ] {
        let link = link_of(url).unwrap_or_else(|| panic!("应认出 {url}"));
        assert_eq!(link.id, "2007184284944322584", "{url}");
    }
}

#[test]
fn lookalike_hosts_are_rejected() {
    for url in [
        "https://x.com.evil.test/user/status/123",
        "https://notx.com/user/status/123",
        "https://evil.test/x.com/user/status/123",
    ] {
        assert!(link_of(url).is_none(), "不该认 {url}");
    }
}

#[test]
fn non_status_paths_are_rejected() {
    for url in [
        "https://x.com/user",
        "https://x.com/user/status/abc",
        "https://x.com/home",
        "ftp://x.com/user/status/123",
    ] {
        assert!(link_of(url).is_none(), "不该认 {url}");
    }
}

#[test]
fn finds_a_link_inside_a_sentence() {
    let link = link_of("看看这篇（https://x.com/author/status/1234567890）。").expect("应认出链接");
    assert_eq!(link.id, "1234567890");
    assert_eq!(link.url(), "https://x.com/i/status/1234567890");
}

#[test]
fn picks_the_first_x_link_in_a_message() {
    let text = "两个链接 https://example.test/a 和 https://x.com/a/status/77 都在";
    assert_eq!(link_of(text).map(|link| link.id), Some("77".to_owned()));
}

#[test]
fn message_of_reads_text_and_flags() {
    let update = json!({
        "message": {
            "chat_id": 42,
            "is_outgoing": true,
            "content": {
                "@type": "messageText",
                "text": { "@type": "formattedText", "text": "hello https://x.com/a/status/1" }
            }
        }
    });
    let message = message_of(&update).expect("应解析出消息");
    assert_eq!(message.chat_id, 42);
    assert!(message.is_outgoing);
    assert!(message.command.is_none());
    assert_eq!(
        link_of(&message.text).map(|link| link.id),
        Some("1".to_owned())
    );
}

#[test]
fn non_text_messages_are_ignored() {
    let update = json!({
        "message": { "chat_id": 1, "content": { "@type": "messagePhoto" } }
    });
    assert!(message_of(&update).is_none());
}

#[test]
fn slash_commands_are_parsed() {
    let update = json!({
        "message": {
            "chat_id": 7,
            "id": 9,
            "content": {
                "@type": "messageText",
                "text": { "text": "/x https://x.com/a/status/55" }
            }
        }
    });
    let message = message_of(&update).expect("命令也是文本消息");
    let command = message.command.expect("应解析出命令");
    assert!(command.is("x"));
    assert_eq!(
        link_of(&command.args).map(|link| link.id),
        Some("55".to_owned())
    );
}
