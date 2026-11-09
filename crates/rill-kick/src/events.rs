//! Kick's chat events, which both of its sockets carry, turned into Rill events.

use rill_core::{
    Author, Badge, ChatMessage, Event, EventKind, Fragment, MessageKind, Platform, ReplyRef, Roles,
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
        "SubscriptionEvent" | "ChannelSubscriptionEvent" => {
            let months = data["months"].as_u64().unwrap_or(0);
            let text = if months > 1 {
                format!("subscribed for {months} months")
            } else {
                "subscribed".into()
            };
            happening(data, &s(&data["username"]), text, EventKind::Subscription)
        }
        "GiftedSubscriptionsEvent" => {
            let n = data["gifted_usernames"].as_array().map_or(1, Vec::len);
            let subs = if n == 1 {
                "sub".to_string()
            } else {
                format!("{n} subs")
            };
            happening(
                data,
                &s(&data["gifter_username"]),
                format!("gifted {subs}"),
                EventKind::Gift,
            )
        }
        "StreamHostEvent" => {
            let viewers = data["number_viewers"].as_u64().unwrap_or(0);
            let mut text = format!("is hosting with {viewers} viewers");
            if let Some(m) = data["optional_message"].as_str().filter(|m| !m.is_empty()) {
                text.push_str(&format!(": {m}"));
            }
            happening(data, &s(&data["host_username"]), text, EventKind::Raid)
        }
        "RewardRedeemedEvent" => {
            let mut text = format!("redeemed {}", s(&data["reward_title"]));
            if let Some(i) = data["user_input"].as_str().filter(|i| !i.is_empty()) {
                text.push_str(&format!(": {i}"));
            }
            happening(data, &s(&data["username"]), text, EventKind::Redemption)
        }
        _ => None,
    }
}

/// The emotes in Kick's emote list for a channel (kick.com/emotes/<channel>): the channel's own,
/// Kick's global ones and its emoji, by name with their IDs. Read loosely: any object with a
/// numeric `id` and a `name` inside an `emotes` list counts.
pub fn emote_list(v: &Value) -> Vec<(String, u64)> {
    fn walk(v: &Value, inside_emotes: bool, out: &mut Vec<(String, u64)>) {
        match v {
            Value::Array(items) => items.iter().for_each(|i| walk(i, inside_emotes, out)),
            Value::Object(o) => {
                if inside_emotes
                    && let (Some(id), Some(name)) = (
                        o.get("id").and_then(Value::as_u64),
                        o.get("name").and_then(Value::as_str),
                    )
                {
                    out.push((name.to_string(), id));
                    return;
                }
                for (k, child) in o {
                    walk(child, k == "emotes", out);
                }
            }
            _ => {}
        }
    }
    let mut out = Vec::new();
    walk(v, false, &mut out);
    let mut seen = std::collections::HashSet::new();
    out.retain(|(name, _)| seen.insert(name.clone()));
    out
}

/// Turns emote names in outgoing text into Kick's `[emote:id:name]` codes.
pub fn encode_emotes(text: &str, emotes: &std::collections::HashMap<String, u64>) -> String {
    text.split(' ')
        .map(|w| match emotes.get(w) {
            Some(id) => format!("[emote:{id}:{w}]"),
            None => w.to_string(),
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Kick events Rill knows and shows nothing for.
const QUIET: &[&str] = &[
    "LuckyUsersWhoGotGiftSubscriptionsEvent",
    "PinnedMessageCreatedEvent",
    "PinnedMessageDeletedEvent",
    "UserUnbannedEvent",
    "ChatroomUpdatedEvent",
    "StreamerIsLive",
    "StopStreamBroadcast",
    "PollUpdateEvent",
    "PollDeleteEvent",
    "ChatMoveToSupportedChannelEvent",
];

/// Whether Rill knows this event, shown or not; unknown ones are worth a log line.
pub fn is_known(event: &str) -> bool {
    let name = event.strip_prefix("App\\Events\\").unwrap_or(event);
    QUIET.contains(&name)
        || matches!(
            name,
            "ChatMessageSentEvent"
                | "ChatMessageEvent"
                | "MessageDeletedEvent"
                | "ChatMessageDeletedEvent"
                | "UserBannedEvent"
                | "ChatroomClearEvent"
                | "SubscriptionEvent"
                | "ChannelSubscriptionEvent"
                | "GiftedSubscriptionsEvent"
                | "StreamHostEvent"
                | "RewardRedeemedEvent"
        )
}

/// A sub, gift, host or redemption as an event line by `name`.
fn happening(data: &Value, name: &str, text: String, kind: EventKind) -> Option<Event> {
    if name.is_empty() {
        return None;
    }
    // Kick sends no ID for these; one from their content keeps repeats recognisable.
    let id = format!(
        "kick-event-{:x}",
        format!("{name}{text}{data}")
            .bytes()
            .fold(0xcbf2_9ce4_8422_2325u64, |h, b| (h ^ u64::from(b))
                .wrapping_mul(0x0100_0000_01b3))
    );
    Some(Event::Message(ChatMessage {
        platform: Platform::Kick,
        id,
        author: Author {
            id: format!("name:{}", name.to_lowercase()),
            login: name.to_lowercase(),
            display_name: name.to_string(),
            ..Author::default()
        },
        fragments: split_text(&text),
        kind: MessageKind::Event(kind),
        sent_at_ms: rill_core::now_ms(),
        reply_to: None,
    }))
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
        let mut title = b["text"].as_str().unwrap_or(kind).to_string();
        let months = b["count"].as_u64().unwrap_or(0);
        if matches!(kind, "subscriber" | "founder") && months > 0 {
            title = format!("{title} · {}", rill_core::message::tenure(months));
        }
        badges.push(Badge {
            id: format!("{kind}/1"),
            title,
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
                out.push(Fragment::emote(name, Some(emote_url(&id))));
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
                Fragment::emote("KEKW", Some(emote_url("37226"))),
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
    fn reads_the_emote_list_and_encodes_names_when_sending() {
        let v = json!([
            {"id": 8, "slug": "somechannel", "emotes": [
                {"id": 101, "channel_id": 8, "name": "myHype", "subscribers_only": true},
                {"id": 102, "channel_id": 8, "name": "myWave"}
            ]},
            {"name": "Global", "id": "Global", "emotes": [{"id": 37226, "name": "KEKW"}]},
            {"name": "Emojis", "id": "Emoji", "emotes": [{"id": 1730762, "name": "emojiAngel"}, {"id": 1, "name": "KEKW"}]}
        ]);
        let list = emote_list(&v);
        let names: Vec<_> = list.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, ["myHype", "myWave", "KEKW", "emojiAngel"]);
        let map: std::collections::HashMap<_, _> = list.into_iter().collect();
        assert_eq!(
            encode_emotes("hi myWave and KEKW KEKWs", &map),
            "hi [emote:102:myWave] and [emote:37226:KEKW] KEKWs"
        );
    }

    #[test]
    fn subs_gifts_hosts_and_redemptions_read_as_event_lines() {
        let line = |event: &str, data: Value| match to_event(event, &data, &SubBadges::new()) {
            Some(Event::Message(m)) => {
                assert!(m.kind.is_event());
                format!("{} {}", m.author.display_name, m.plain_text())
            }
            other => panic!("{event}: {other:?}"),
        };
        assert_eq!(
            line(
                "App\\Events\\SubscriptionEvent",
                json!({"username": "Ann", "months": 3})
            ),
            "Ann subscribed for 3 months"
        );
        assert_eq!(
            line(
                "App\\Events\\GiftedSubscriptionsEvent",
                json!({"gifter_username": "Bob", "gifted_usernames": ["a", "b", "c"]})
            ),
            "Bob gifted 3 subs"
        );
        assert_eq!(
            line(
                "App\\Events\\StreamHostEvent",
                json!({"host_username": "Cat", "number_viewers": 42, "optional_message": "hi"})
            ),
            "Cat is hosting with 42 viewers: hi"
        );
        assert_eq!(
            line(
                "App\\Events\\RewardRedeemedEvent",
                json!({"username": "Dee", "reward_title": "Hydrate", "user_input": ""})
            ),
            "Dee redeemed Hydrate"
        );
        assert!(
            to_event(
                "App\\Events\\SubscriptionEvent",
                &json!({}),
                &SubBadges::new()
            )
            .is_none()
        );
        assert!(is_known("App\\Events\\PinnedMessageCreatedEvent"));
        assert!(!is_known("App\\Events\\SomethingNewEvent"));
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
