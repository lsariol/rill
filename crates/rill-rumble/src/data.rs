//! Reads the Live Stream API's reply: the account, and the live stream's recent chat and Rants.
//!
//! The API returns the latest messages on every poll, without message IDs, so a message is known
//! by its time, author, text and amount.

use rill_core::{
    Account, Author, ChatMessage, EventKind, Fragment, MessageKind, Platform, Roles, split_text,
};
use serde_json::Value;

/// One poll's reply.
#[derive(Debug, Clone, PartialEq)]
pub struct Snapshot {
    pub account: Account,
    /// The stream that's live now, if any.
    pub live: Option<Live>,
    /// Recent follows and subscriptions, oldest first.
    pub supporters: Vec<Supporter>,
}

/// Someone who followed or subscribed.
#[derive(Debug, Clone, PartialEq)]
pub struct Supporter {
    pub username: String,
    /// "followed" or "subscribed".
    pub action: &'static str,
    pub at: String,
}

impl Supporter {
    pub fn key(&self) -> String {
        format!("{}\u{1f}{}\u{1f}{}", self.action, self.username, self.at)
    }

    /// An event line: "newfan followed".
    pub fn to_message(&self) -> ChatMessage {
        ChatMessage {
            platform: Platform::Rumble,
            id: format!("rumble-{:016x}", fnv(&self.key())),
            author: Author {
                id: self.username.to_lowercase(),
                login: self.username.clone(),
                display_name: self.username.clone(),
                ..Author::default()
            },
            fragments: vec![Fragment::Text(self.action.to_string())],
            kind: MessageKind::Event(match self.action {
                "followed" => EventKind::Follow,
                "subscribed" => EventKind::Subscription,
                _ => EventKind::Other,
            }),
            sent_at_ms: chrono::DateTime::parse_from_rfc3339(&self.at)
                .map(|t| t.timestamp_millis())
                .unwrap_or_else(|_| rill_core::now_ms()),
            reply_to: None,
        }
    }
}

fn supporters(v: &Value) -> Vec<Supporter> {
    let list = |group: &str, items: &str, at: &str, action: &'static str| {
        v[group][items]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(move |s| {
                let username = text(&s["username"]);
                (!username.is_empty()).then(|| Supporter {
                    username,
                    action,
                    at: text(&s[at]),
                })
            })
            .collect::<Vec<_>>()
    };
    let mut out = list("followers", "recent_followers", "followed_on", "followed");
    out.extend(list(
        "subscribers",
        "recent_subscribers",
        "subscribed_on",
        "subscribed",
    ));
    out.sort_by(|a, b| a.at.cmp(&b.at));
    out
}

#[derive(Debug, Clone, PartialEq)]
pub struct Live {
    pub id: String,
    /// Chat messages and Rants, oldest first.
    pub messages: Vec<Said>,
}

/// A chat message or a Rant.
#[derive(Debug, Clone, PartialEq)]
pub struct Said {
    pub username: String,
    pub text: String,
    pub created_on: String,
    pub badges: Vec<String>,
    /// A Rant's amount, in cents.
    pub rant_cents: Option<u64>,
}

impl Said {
    /// What identifies the message across polls.
    pub fn key(&self) -> String {
        format!(
            "{}\u{1f}{}\u{1f}{}\u{1f}{:?}",
            self.created_on, self.username, self.text, self.rant_cents
        )
    }

    /// The message as Rill shows it.
    pub fn to_message(&self) -> ChatMessage {
        let has = |b: &str| self.badges.iter().any(|x| x.eq_ignore_ascii_case(b));
        let roles = Roles {
            broadcaster: has("admin") || has("broadcaster") || has("owner"),
            moderator: has("moderator"),
            vip: has("vip"),
            subscriber: has("premium") || has("recurring_subscription") || has("local"),
        };
        let (kind, fragments) = match self.rant_cents {
            Some(cents) => {
                let mut f = vec![Fragment::Text(format!(
                    "sent a ${}.{:02} Rant: ",
                    cents / 100,
                    cents % 100
                ))];
                f.extend(split_text(&self.text));
                (MessageKind::Event(EventKind::Money), f)
            }
            None => (MessageKind::Chat, split_text(&self.text)),
        };
        // A stable ID from the message's identity, so deletions and the feed can refer to it.
        let id = format!("rumble-{:016x}", fnv(&self.key()));
        ChatMessage {
            platform: Platform::Rumble,
            id,
            author: Author {
                id: self.username.to_lowercase(),
                login: self.username.clone(),
                display_name: self.username.clone(),
                color: None,
                badges: Vec::new(),
                roles,
            },
            fragments,
            kind,
            sent_at_ms: chrono::DateTime::parse_from_rfc3339(&self.created_on)
                .map(|t| t.timestamp_millis())
                .unwrap_or_else(|_| rill_core::now_ms()),
            reply_to: None,
        }
    }
}

/// FNV-1a: a small stable hash, so IDs stay the same across runs.
fn fnv(s: &str) -> u64 {
    s.bytes().fold(0xcbf2_9ce4_8422_2325, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
    })
}

fn text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        _ => String::new(),
    }
}

/// Badges arrive as a list of names or as an object keyed by name.
fn badges(v: &Value) -> Vec<String> {
    match v {
        Value::Array(a) => a.iter().map(text).filter(|s| !s.is_empty()).collect(),
        Value::Object(o) => o.keys().cloned().collect(),
        _ => Vec::new(),
    }
}

fn said(v: &Value, rant: bool) -> Option<Said> {
    let username = text(&v["username"]);
    let body = text(&v["text"]);
    if username.is_empty() || body.is_empty() {
        return None;
    }
    let rant_cents = rant.then(|| {
        v["amount_cents"]
            .as_u64()
            .or_else(|| {
                v["amount_dollars"]
                    .as_f64()
                    .map(|d| (d * 100.0).round() as u64)
            })
            .unwrap_or(0)
    });
    Some(Said {
        username,
        text: body,
        created_on: text(&v["created_on"]),
        badges: badges(&v["badges"]),
        rant_cents,
    })
}

/// Reads a reply. `None` when it isn't the Live Stream API's shape.
pub fn parse(v: &Value) -> Option<Snapshot> {
    let username = text(&v["username"]);
    let channel = text(&v["channel_name"]);
    if username.is_empty() && channel.is_empty() {
        return None;
    }
    let is_channel = v["type"] == "channel" && !channel.is_empty();
    let account = Account {
        id: if is_channel {
            text(&v["channel_id"])
        } else {
            text(&v["user_id"])
        },
        login: username.clone(),
        display_name: if is_channel { channel } else { username },
    };
    let live = v["livestreams"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|s| s["is_live"].as_bool().unwrap_or(false))
        .map(|s| {
            let chat = &s["chat"];
            let mut messages: Vec<Said> = chat["recent_messages"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|m| said(m, false))
                .chain(
                    chat["recent_rants"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(|m| said(m, true)),
                )
                .collect();
            // RFC 3339 times in one zone sort as text.
            messages.sort_by(|a, b| a.created_on.cmp(&b.created_on));
            Live {
                id: text(&s["id"]),
                messages,
            }
        });
    Some(Snapshot {
        account,
        live,
        supporters: supporters(v),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const DATA: &str = include_str!("../tests/fixtures/get_data.json");

    #[test]
    fn reads_the_account_and_the_live_streams_chat_and_rants() {
        let v: Value = serde_json::from_str(DATA).unwrap();
        let s = parse(&v).unwrap();
        assert_eq!(s.account.display_name, "Some Streamer");
        assert_eq!(s.account.id, "_c1");
        let live = s.live.unwrap();
        assert_eq!(live.id, "v2live");
        let texts: Vec<_> = live.messages.iter().map(|m| m.text.as_str()).collect();
        assert_eq!(
            texts,
            ["first @somestreamer", "be nice", "great stream", "second"]
        );

        let first = live.messages[0].to_message();
        assert_eq!(first.plain_text(), "first @somestreamer");
        assert_eq!(first.kind, MessageKind::Chat);
        assert_eq!(first.id, live.messages[0].to_message().id, "IDs are stable");
        assert!(live.messages[1].to_message().author.roles.moderator);
        assert!(live.messages[3].to_message().author.roles.subscriber);
        let rant = live.messages[2].to_message();
        assert_eq!(rant.kind, MessageKind::Event(EventKind::Money));
        assert_eq!(rant.plain_text(), "sent a $5.00 Rant: great stream");
    }

    #[test]
    fn reads_follows_and_subscriptions() {
        let v: Value = serde_json::from_str(DATA).unwrap();
        let s = parse(&v).unwrap();
        assert_eq!(s.supporters.len(), 1);
        let m = s.supporters[0].to_message();
        assert_eq!(
            (m.author.display_name.as_str(), m.plain_text().as_str()),
            ("newfan", "followed")
        );
        assert!(m.kind.is_event());
    }

    #[test]
    fn nothing_live_and_odd_shapes() {
        let v = serde_json::json!({"type": "user", "username": "me", "livestreams": []});
        let s = parse(&v).unwrap();
        assert!(s.live.is_none());
        assert_eq!(s.account.display_name, "me");
        assert!(parse(&serde_json::json!({"error": "bad key"})).is_none());
        let b = badges(&serde_json::json!({"moderator": {"label": "Mod"}}));
        assert_eq!(b, ["moderator"]);
    }
}
