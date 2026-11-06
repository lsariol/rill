//! EventSub WebSocket messages and their conversion into Rill events.

use rill_core::{
    Author, Badge, ChatMessage, Event, Fragment, MessageKind, Platform, ReplyRef, Roles,
    merge_text, split_text,
};
use serde::Deserialize;
use serde_json::Value;

use crate::helix::Badges;

pub const URL: &str = "wss://eventsub.wss.twitch.tv/ws";

/// The chat events Rill subscribes to.
pub const SUBSCRIPTIONS: [&str; 4] = [
    "channel.chat.message",
    "channel.chat.message_delete",
    "channel.chat.clear_user_messages",
    "channel.chat.clear",
];

/// A decoded WebSocket message.
#[derive(Debug, PartialEq)]
pub enum Frame {
    Welcome {
        session_id: String,
        keepalive_secs: u64,
    },
    Keepalive,
    Reconnect {
        url: String,
    },
    Notification {
        kind: String,
        timestamp: String,
        event: Value,
    },
    Revocation {
        kind: String,
        status: String,
    },
    Other,
}

#[derive(Deserialize)]
struct Envelope {
    metadata: Metadata,
    #[serde(default)]
    payload: Value,
}

#[derive(Deserialize)]
struct Metadata {
    message_type: String,
    #[serde(default)]
    message_timestamp: String,
    #[serde(default)]
    subscription_type: String,
}

/// Decodes one text frame.
pub fn parse(text: &str) -> Result<Frame, String> {
    let env: Envelope = serde_json::from_str(text)
        .map_err(|e| format!("Twitch: unreadable EventSub message: {e}"))?;
    let p = env.payload;
    Ok(match env.metadata.message_type.as_str() {
        "session_welcome" => Frame::Welcome {
            session_id: p["session"]["id"].as_str().unwrap_or_default().to_string(),
            keepalive_secs: p["session"]["keepalive_timeout_seconds"]
                .as_u64()
                .unwrap_or(10),
        },
        "session_keepalive" => Frame::Keepalive,
        "session_reconnect" => Frame::Reconnect {
            url: p["session"]["reconnect_url"]
                .as_str()
                .unwrap_or(URL)
                .to_string(),
        },
        "notification" => Frame::Notification {
            kind: env.metadata.subscription_type,
            timestamp: env.metadata.message_timestamp,
            event: p["event"].clone(),
        },
        "revocation" => Frame::Revocation {
            kind: p["subscription"]["type"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
            status: p["subscription"]["status"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
        },
        _ => Frame::Other,
    })
}

/// Turns a notification into a Rill event.
pub fn to_event(kind: &str, timestamp: &str, event: &Value, badges: &Badges) -> Option<Event> {
    let platform = Platform::Twitch;
    let s = |v: &Value| v.as_str().unwrap_or_default().to_string();
    match kind {
        "channel.chat.message" => Some(Event::Message(chat_message(timestamp, event, badges))),
        "channel.chat.message_delete" => Some(Event::Deleted {
            platform,
            message_id: s(&event["message_id"]),
        }),
        "channel.chat.clear_user_messages" => Some(Event::UserCleared {
            platform,
            user_id: s(&event["target_user_id"]),
        }),
        "channel.chat.clear" => Some(Event::Cleared { platform }),
        _ => None,
    }
}

/// The Twitch CDN image for an emote: animated when available, dark theme, 2x.
pub fn emote_url(id: &str) -> String {
    format!("https://static-cdn.jtvnw.net/emoticons/v2/{id}/default/dark/2.0")
}

fn chat_message(timestamp: &str, e: &Value, badges: &Badges) -> ChatMessage {
    let s = |v: &Value| v.as_str().unwrap_or_default().to_string();
    let mut roles = Roles::default();
    let mut author_badges = Vec::new();
    for b in e["badges"].as_array().into_iter().flatten() {
        let set = b["set_id"].as_str().unwrap_or_default();
        let key = format!("{set}/{}", b["id"].as_str().unwrap_or_default());
        match set {
            "broadcaster" => roles.broadcaster = true,
            "moderator" => roles.moderator = true,
            "vip" => roles.vip = true,
            "subscriber" | "founder" => roles.subscriber = true,
            _ => {}
        }
        let (title, url) = badges
            .get(&key)
            .map(|(t, u)| (t.clone(), Some(u.clone())))
            .unwrap_or_else(|| (set.to_string(), None));
        author_badges.push(Badge {
            id: key,
            title,
            image_url: url,
        });
    }

    let mut fragments = Vec::new();
    for f in e["message"]["fragments"].as_array().into_iter().flatten() {
        let text = s(&f["text"]);
        match f["type"].as_str() {
            Some("emote") => fragments.push(Fragment::emote(
                text,
                f["emote"]["id"].as_str().map(emote_url),
            )),
            Some("mention") => fragments.push(Fragment::Mention(
                f["mention"]["user_login"]
                    .as_str()
                    .map(str::to_string)
                    .unwrap_or_else(|| text.trim_start_matches('@').to_string()),
            )),
            _ => fragments.extend(split_text(&text)),
        }
    }
    if fragments.is_empty() {
        fragments = split_text(e["message"]["text"].as_str().unwrap_or_default());
    }
    let kind = strip_action(&mut fragments);

    let reply_to = e["reply"]["parent_message_id"].as_str().map(|id| ReplyRef {
        message_id: id.to_string(),
        author: s(&e["reply"]["parent_user_name"]),
        text: s(&e["reply"]["parent_message_body"]),
    });

    ChatMessage {
        platform: Platform::Twitch,
        id: s(&e["message_id"]),
        author: Author {
            id: s(&e["chatter_user_id"]),
            login: s(&e["chatter_user_login"]),
            display_name: s(&e["chatter_user_name"]),
            color: e["color"]
                .as_str()
                .and_then(|c| u32::from_str_radix(c.trim_start_matches('#'), 16).ok()),
            badges: author_badges,
            roles,
        },
        fragments: merge_text(fragments),
        kind,
        sent_at_ms: chrono::DateTime::parse_from_rfc3339(timestamp)
            .map(|t| t.timestamp_millis())
            .unwrap_or_else(|_| rill_core::now_ms()),
        reply_to,
    }
}

/// Removes the `\x01ACTION …\x01` wrapper of `/me` messages, if present.
fn strip_action(fragments: &mut [Fragment]) -> MessageKind {
    const START: &str = "\u{1}ACTION ";
    let starts = matches!(fragments.first(), Some(Fragment::Text(t)) if t.starts_with(START));
    if !starts {
        return MessageKind::Chat;
    }
    if let Some(Fragment::Text(t)) = fragments.first_mut() {
        *t = t[START.len()..].to_string();
    }
    if let Some(Fragment::Text(t)) = fragments.last_mut()
        && t.ends_with('\u{1}')
    {
        t.pop();
    }
    MessageKind::Action
}

#[cfg(test)]
mod tests {
    use super::*;

    const MESSAGE: &str = include_str!("../tests/fixtures/chat_message.json");

    #[test]
    fn parses_welcome_and_keepalive() {
        let w = r#"{"metadata":{"message_id":"1","message_type":"session_welcome","message_timestamp":"2023-07-19T14:56:51.634234626Z"},"payload":{"session":{"id":"AQoQ","status":"connected","connected_at":"2023-07-19T14:56:51.616329898Z","keepalive_timeout_seconds":10,"reconnect_url":null}}}"#;
        assert_eq!(
            parse(w).unwrap(),
            Frame::Welcome {
                session_id: "AQoQ".into(),
                keepalive_secs: 10
            }
        );
        let k = r#"{"metadata":{"message_id":"2","message_type":"session_keepalive","message_timestamp":"2023-07-19T10:11:12.634234626Z"},"payload":{}}"#;
        assert_eq!(parse(k).unwrap(), Frame::Keepalive);
    }

    #[test]
    fn parses_reconnect_url() {
        let r = r#"{"metadata":{"message_id":"3","message_type":"session_reconnect","message_timestamp":"2022-11-18T09:10:11.634234626Z"},"payload":{"session":{"id":"AQoQ","status":"reconnecting","keepalive_timeout_seconds":null,"reconnect_url":"wss://eventsub.wss.twitch.tv?x=1","connected_at":"2022-11-16T10:11:12.634234626Z"}}}"#;
        assert_eq!(
            parse(r).unwrap(),
            Frame::Reconnect {
                url: "wss://eventsub.wss.twitch.tv?x=1".into()
            }
        );
    }

    #[test]
    fn converts_a_chat_message() {
        let Frame::Notification {
            kind,
            timestamp,
            event,
        } = parse(MESSAGE).unwrap()
        else {
            panic!("expected a notification");
        };
        let mut badges = Badges::new();
        badges.insert(
            "moderator/1".into(),
            ("Moderator".into(), "https://badge/mod".into()),
        );
        let Some(Event::Message(m)) = to_event(&kind, &timestamp, &event, &badges) else {
            panic!("expected a message");
        };
        assert_eq!(m.id, "cc106a89-1814-919d-454c-f4f2f970aae7");
        assert_eq!(m.author.display_name, "Cool_User");
        assert_eq!(m.author.color, Some(0x00ff00));
        assert!(m.author.roles.moderator && m.author.roles.subscriber);
        assert_eq!(
            m.author.badges[0].image_url.as_deref(),
            Some("https://badge/mod")
        );
        assert_eq!(
            m.fragments,
            vec![
                Fragment::Text("Hi chat ".into()),
                Fragment::emote("Kappa", Some(emote_url("25"))),
                Fragment::Text(" ".into()),
                Fragment::Mention("rill_dev".into()),
                Fragment::Text(" see ".into()),
                Fragment::Link("https://rill.mobasity.com".into()),
            ]
        );
        assert_eq!(m.sent_at_ms, 1_689_778_611_634);
        assert_eq!(m.kind, MessageKind::Chat);
    }

    #[test]
    fn converts_deletes_and_clears() {
        let del = serde_json::json!({"message_id": "abc", "target_user_id": "7"});
        assert_eq!(
            to_event("channel.chat.message_delete", "", &del, &Badges::new()),
            Some(Event::Deleted {
                platform: Platform::Twitch,
                message_id: "abc".into()
            })
        );
        assert_eq!(
            to_event("channel.chat.clear_user_messages", "", &del, &Badges::new()),
            Some(Event::UserCleared {
                platform: Platform::Twitch,
                user_id: "7".into()
            })
        );
    }

    #[test]
    fn strips_me_actions() {
        let mut f = vec![Fragment::Text("\u{1}ACTION waves\u{1}".into())];
        assert_eq!(strip_action(&mut f), MessageKind::Action);
        assert_eq!(f, vec![Fragment::Text("waves".into())]);
    }
}
