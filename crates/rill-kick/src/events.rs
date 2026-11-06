//! Kick's chat events, which both of its sockets carry, turned into Rill events.

use rill_core::{
    Author, Badge, ChatMessage, Event, Fragment, MessageKind, Platform, ReplyRef, Roles,
    merge_text, split_text,
};
use serde_json::Value;

/// A Kick emote's image.
pub fn emote_url(id: &str) -> String {
    format!("https://files.kick.com/emotes/{id}/fullsize")
}

/// Subscriber badge images by months, from the channel's data.
pub type SubBadges = Vec<(u64, String)>;

/// A payload as Kick sends it: JSON, or JSON written into a string.
pub fn payload(v: &Value) -> Value {
    match v {
        Value::String(s) => serde_json::from_str(s).unwrap_or(Value::Null),
        other => other.clone(),
    }
}

/// Turns a Kick event into a Rill event. Kick has used two names for messages and deletions.
pub fn to_event(event: &str, data: &Value, sub_badges: &SubBadges) -> Option<Event> {
    let platform = Platform::Kick;
    let s = |v: &Value| match v {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        _ => String::new(),
    };
    match event.strip_prefix("App\\Events\\")? {
        "ChatMessageSentEvent" | "ChatMessageEvent" => {
            Some(Event::Message(chat_message(data, sub_badges)))
        }
        "MessageDeletedEvent" | "ChatMessageDeletedEvent" => {
            let id = [&data["message"]["id"], &data["deletedMessage"]["id"]]
                .into_iter()
                .map(s)
                .find(|id| !id.is_empty())?;
            Some(Event::Deleted {
                platform,
                message_id: id,
            })
        }
        "UserBannedEvent" => Some(Event::UserCleared {
            platform,
            user_id: s(&data["user"]["id"]),
        }),
        "ChatroomClearEvent" => Some(Event::Cleared { platform }),
        _ => None,
    }
}

fn chat_message(d: &Value, sub_badges: &SubBadges) -> ChatMessage {
    let s = |v: &Value| match v {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        _ => String::new(),
    };
    let sender = &d["sender"];
    let mut roles = Roles::default();
    let mut badges = Vec::new();
    for b in sender["identity"]["badges"]
        .as_array()
        .into_iter()
        .flatten()
    {
        let kind = b["type"].as_str().unwrap_or_default();
        let mut image_url = None;
        match kind {
            "broadcaster" => roles.broadcaster = true,
            "moderator" => roles.moderator = true,
            "vip" => roles.vip = true,
            "subscriber" | "founder" => {
                roles.subscriber = true;
                let months = b["count"].as_u64().unwrap_or(0);
                image_url = sub_badges
                    .iter()
                    .filter(|(m, _)| *m <= months)
                    .max_by_key(|(m, _)| *m)
                    .map(|(_, url)| url.clone());
            }
            _ => {}
        }
        badges.push(Badge {
            id: format!("{kind}/1"),
            title: b["text"].as_str().unwrap_or(kind).to_string(),
            image_url,
        });
    }
    let reply_to = (d["type"] == "reply").then(|| ReplyRef {
        message_id: s(&d["metadata"]["original_message"]["id"]),
        author: s(&d["metadata"]["original_sender"]["username"]),
        text: s(&d["metadata"]["original_message"]["content"]),
    });
    ChatMessage {
        platform: Platform::Kick,
        id: s(&d["id"]),
        author: Author {
            id: s(&sender["id"]),
            login: s(&sender["slug"]),
            display_name: s(&sender["username"]),
            color: sender["identity"]["color"]
                .as_str()
                .and_then(|c| u32::from_str_radix(c.trim_start_matches('#'), 16).ok()),
            badges,
            roles,
        },
        fragments: content_fragments(d["content"].as_str().unwrap_or_default()),
        kind: MessageKind::Chat,
        sent_at_ms: d["created_at"]
            .as_str()
            .and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok())
            .map(|t| t.timestamp_millis())
            .unwrap_or_else(rill_core::now_ms),
        reply_to,
    }
}

/// Splits Kick's message text, where emotes are written `[emote:ID:NAME]`.
pub fn content_fragments(content: &str) -> Vec<Fragment> {
    let mut out = Vec::new();
    let mut rest = content;
    while let Some(start) = rest.find("[emote:") {
        let after = &rest[start + 7..];
        let parsed = after.find(']').and_then(|end| {
            let (id, name) = after[..end].split_once(':')?;
            id.bytes()
                .all(|b| b.is_ascii_digit())
                .then(|| (id.to_string(), name.to_string(), end))
        });
        match parsed {
            Some((id, name, end)) => {
                out.extend(split_text(&rest[..start]));
                out.push(Fragment::Emote {
                    url: Some(emote_url(&id)),
                    name,
                });
                rest = &after[end + 1..];
            }
            None => {
                out.extend(split_text(&rest[..start + 7]));
                rest = after;
            }
        }
    }
    out.extend(split_text(rest));
    merge_text(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pusher::Frame;
    use serde_json::json;

    #[test]
    fn splits_emotes_out_of_content() {
        assert_eq!(
            content_fragments("hi [emote:37226:KEKW] @bob [emote:x:y]"),
            vec![
                Fragment::Text("hi ".into()),
                Fragment::Emote {
                    name: "KEKW".into(),
                    url: Some(emote_url("37226"))
                },
                Fragment::Text(" ".into()),
                Fragment::Mention("bob".into()),
                Fragment::Text(" [emote:x:y]".into()),
            ]
        );
    }

    #[test]
    fn converts_a_chat_message() {
        let frame = include_str!("../tests/fixtures/chat_message.json");
        let Frame::Kick { event, data } = crate::pusher::parse(frame).unwrap() else {
            panic!("expected a Kick event");
        };
        let subs = vec![(1, "https://sub/1".into()), (6, "https://sub/6".into())];
        let Some(Event::Message(m)) = to_event(&event, &data, &subs) else {
            panic!("expected a message");
        };
        assert_eq!(m.id, "a2c7b5c4-9c71-4e3a-8b1a-0f6c7f2f5d10");
        assert_eq!(m.author.display_name, "SomeViewer");
        assert_eq!(m.author.id, "1234567");
        assert_eq!(m.author.color, Some(0xff9d00));
        assert!(m.author.roles.moderator && m.author.roles.subscriber);
        let sub = m
            .author
            .badges
            .iter()
            .find(|b| b.id == "subscriber/1")
            .unwrap();
        assert_eq!(sub.image_url.as_deref(), Some("https://sub/6"));
        assert_eq!(m.plain_text(), "nice play KEKW");
        assert_eq!(m.sent_at_ms, 1_730_000_000_000);
    }

    #[test]
    fn converts_deletes_and_bans() {
        let del = json!({"id": "x", "message": {"id": "m1"}});
        assert_eq!(
            to_event("App\\Events\\MessageDeletedEvent", &del, &Vec::new()),
            Some(Event::Deleted {
                platform: Platform::Kick,
                message_id: "m1".into()
            })
        );
        let ban = json!({"id": "x", "user": {"id": 42, "username": "b"}, "permanent": true});
        assert_eq!(
            to_event("App\\Events\\UserBannedEvent", &ban, &Vec::new()),
            Some(Event::UserCleared {
                platform: Platform::Kick,
                user_id: "42".into()
            })
        );
    }
    #[test]
    fn reads_both_event_names() {
        let msg = json!({"id": "m1", "content": "hi", "sender": {"id": 7, "username": "V"}});
        for name in ["ChatMessageSentEvent", "ChatMessageEvent"] {
            let e = to_event(&format!("App\\Events\\{name}"), &msg, &Vec::new());
            assert!(
                matches!(e, Some(Event::Message(m)) if m.id == "m1"),
                "{name}"
            );
        }
        let del = json!({"deletedMessage": {"id": "m2", "deleted_by": 1}});
        assert_eq!(
            to_event("App\\Events\\ChatMessageDeletedEvent", &del, &Vec::new()),
            Some(Event::Deleted {
                platform: Platform::Kick,
                message_id: "m2".into()
            })
        );
        assert_eq!(
            to_event(
                "App\\Events\\ChatMessageDeletedEvent",
                &json!({}),
                &Vec::new()
            ),
            None
        );
        assert_eq!(payload(&json!(r#"{"a":1}"#)), json!({"a": 1}));
    }
}
