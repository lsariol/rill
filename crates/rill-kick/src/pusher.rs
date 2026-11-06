//! Kick's chat socket: the Pusher protocol and Kick's chat events.

use rill_core::{
    Author, Badge, ChatMessage, Event, Fragment, MessageKind, Platform, ReplyRef, Roles,
    merge_text, split_text,
};
use serde_json::{Value, json};

/// Where Kick hands out chat socket settings, per channel. Unofficial: Kick may change it.
pub fn connection_url(channel_id: u64) -> String {
    format!("https://web.kick.com/api/v1/realtime/channels/{channel_id}/chat/connection")
}

/// The body Kick's web player sends: an anonymous client and the providers it accepts.
pub fn connection_body(client_id: &str) -> Value {
    json!({
        "client": {"id": client_id, "type": "web"},
        "capabilities": {"accepted_providers": [{"provider": "pusher"}]},
    })
}

/// A Pusher socket URL for an app key and cluster.
pub fn socket_url(app_key: &str, cluster: &str) -> String {
    format!(
        "wss://ws-{cluster}.pusher.com/app/{app_key}?protocol=7&client=js&version=8.4.0&flash=false"
    )
}

/// Reads Kick's connection settings into a socket URL. Accepts the body bare or under `data`.
pub fn socket_from_settings(v: &Value) -> Result<String, String> {
    let d = if v["provider"].is_string() {
        v
    } else {
        &v["data"]
    };
    match d["provider"].as_str() {
        Some("pusher") => {
            let c = &d["credentials"];
            match (c["app_key"].as_str(), c["cluster"].as_str()) {
                (Some(key), Some(cluster)) if !key.is_empty() && !cluster.is_empty() => {
                    Ok(socket_url(key, cluster))
                }
                _ => Err("Kick's chat settings have no Pusher key or cluster".into()),
            }
        }
        Some(other) => Err(format!(
            "Kick offered the \"{other}\" chat provider, which Rill doesn't support yet"
        )),
        None => Err(format!(
            "Kick's chat settings weren't in the expected shape (fields: {})",
            field_names(v)
        )),
    }
}

/// Top-level field names, for diagnosing a changed response without logging its values.
fn field_names(v: &Value) -> String {
    v.as_object()
        .map(|o| o.keys().cloned().collect::<Vec<_>>().join(", "))
        .unwrap_or_else(|| "none".into())
}

/// A decoded socket frame.
#[derive(Debug, PartialEq)]
pub enum Frame {
    Connected { activity_timeout: u64 },
    Subscribed,
    Ping,
    Pong,
    Error(String),
    Kick { event: String, data: Value },
}

/// Decodes one text frame. Pusher sends `data` as a JSON string inside JSON.
pub fn parse(text: &str) -> Result<Frame, String> {
    let v: Value =
        serde_json::from_str(text).map_err(|e| format!("Kick: unreadable socket message: {e}"))?;
    let event = v["event"].as_str().unwrap_or_default();
    let data = match &v["data"] {
        Value::String(s) => serde_json::from_str(s).unwrap_or(Value::Null),
        other => other.clone(),
    };
    Ok(match event {
        "pusher:connection_established" => Frame::Connected {
            activity_timeout: data["activity_timeout"].as_u64().unwrap_or(120),
        },
        "pusher_internal:subscription_succeeded" => Frame::Subscribed,
        "pusher:ping" => Frame::Ping,
        "pusher:pong" => Frame::Pong,
        "pusher:error" => Frame::Error(
            data["message"]
                .as_str()
                .unwrap_or("unknown error")
                .to_string(),
        ),
        _ => Frame::Kick {
            event: event.to_string(),
            data,
        },
    })
}

/// The frame that subscribes to a chatroom.
pub fn subscribe(chatroom_id: u64) -> String {
    json!({"event": "pusher:subscribe", "data": {"auth": "", "channel": format!("chatrooms.{chatroom_id}.v2")}})
        .to_string()
}

pub fn ping() -> String {
    json!({"event": "pusher:ping", "data": {}}).to_string()
}

pub fn pong() -> String {
    json!({"event": "pusher:pong", "data": {}}).to_string()
}

/// A Kick emote's image.
pub fn emote_url(id: &str) -> String {
    format!("https://files.kick.com/emotes/{id}/fullsize")
}

/// Subscriber badge images by months, from the channel's data.
pub type SubBadges = Vec<(u64, String)>;

/// Turns a Kick event into a Rill event.
pub fn to_event(event: &str, data: &Value, sub_badges: &SubBadges) -> Option<Event> {
    let platform = Platform::Kick;
    let s = |v: &Value| match v {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        _ => String::new(),
    };
    match event {
        "App\\Events\\ChatMessageEvent" => Some(Event::Message(chat_message(data, sub_badges))),
        "App\\Events\\MessageDeletedEvent" => Some(Event::Deleted {
            platform,
            message_id: s(&data["message"]["id"]),
        }),
        "App\\Events\\UserBannedEvent" => Some(Event::UserCleared {
            platform,
            user_id: s(&data["user"]["id"]),
        }),
        "App\\Events\\ChatroomClearEvent" => Some(Event::Cleared { platform }),
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

    #[test]
    fn parses_pusher_frames_with_string_data() {
        let c = r#"{"event":"pusher:connection_established","data":"{\"socket_id\":\"1.2\",\"activity_timeout\":120}"}"#;
        assert_eq!(
            parse(c).unwrap(),
            Frame::Connected {
                activity_timeout: 120
            }
        );
        assert_eq!(
            parse(r#"{"event":"pusher:ping","data":{}}"#).unwrap(),
            Frame::Ping
        );
        let sub = r#"{"event":"pusher_internal:subscription_succeeded","data":"{}","channel":"chatrooms.668.v2"}"#;
        assert_eq!(parse(sub).unwrap(), Frame::Subscribed);
    }

    #[test]
    fn reads_socket_settings() {
        let pusher =
            json!({"provider": "pusher", "credentials": {"app_key": "abc", "cluster": "us2"}});
        assert_eq!(
            socket_from_settings(&pusher).unwrap(),
            socket_url("abc", "us2")
        );
        let wrapped = json!({ "data": pusher });
        assert!(socket_from_settings(&wrapped).is_ok());
        let centrifugo = json!({"provider": "centrifugo", "credentials": {"url": "wss://x"}});
        assert!(
            socket_from_settings(&centrifugo)
                .unwrap_err()
                .contains("centrifugo")
        );
        let odd = json!({"token": "secret-value"});
        let err = socket_from_settings(&odd).unwrap_err();
        assert!(err.contains("token") && !err.contains("secret-value"));
    }

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
        let Frame::Kick { event, data } = parse(frame).unwrap() else {
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
}
