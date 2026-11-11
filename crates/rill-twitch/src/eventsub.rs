//! EventSub WebSocket messages and their conversion into Rill events.

use rill_core::{
    Author, Badge, ChatMessage, EmoteScope, Event, EventKind, Fragment, MessageKind, ModAction,
    Moderation, Platform, ReplyRef, Roles, merge_text, split_text,
};
use serde::Deserialize;
use serde_json::Value;

use crate::helix::Badges;

pub const URL: &str = "wss://eventsub.wss.twitch.tv/ws";

/// The chat events Rill subscribes to.
pub const SUBSCRIPTIONS: [&str; 5] = [
    "channel.chat.message",
    "channel.chat.message_delete",
    "channel.chat.clear_user_messages",
    "channel.chat.clear",
    "channel.chat.notification",
];

/// Subscriptions only the broadcaster's own token can make: type and version.
pub const BROADCASTER_SUBSCRIPTIONS: [(&str, &str); 3] = [
    ("channel.ad_break.begin", "1"),
    ("channel.channel_points_custom_reward_redemption.add", "1"),
    (
        "channel.channel_points_automatic_reward_redemption.add",
        "2",
    ),
];

/// An ad break that began: its length and why ("automatic" or who started it).
pub fn ad_break(kind: &str, e: &Value) -> Option<(u32, String)> {
    if kind != "channel.ad_break.begin" {
        return None;
    }
    let seconds = e["duration_seconds"]
        .as_u64()
        .or_else(|| e["duration_seconds"].as_str()?.parse().ok())?;
    let why = if e["is_automatic"].as_bool().unwrap_or(false) {
        "automatic".to_string()
    } else {
        match e["requester_user_name"]
            .as_str()
            .or(e["requester_user_login"].as_str())
        {
            Some(who) if !who.is_empty() => format!("started by {who}"),
            _ => "started from Twitch".to_string(),
        }
    };
    Some((u32::try_from(seconds).unwrap_or(u32::MAX), why))
}

/// "Foo redeemed Hydrate: what they typed", as an event line.
fn redemption(kind: &str, timestamp: &str, e: &Value) -> Option<ChatMessage> {
    let s = |v: &Value| v.as_str().unwrap_or_default().to_string();
    let title = match kind {
        "channel.channel_points_custom_reward_redemption.add" => s(&e["reward"]["title"]),
        "channel.channel_points_automatic_reward_redemption.add" => {
            match e["reward"]["type"].as_str().unwrap_or_default() {
                "send_highlighted_message" => "Highlight My Message".into(),
                "single_message_bypass_sub_mode" => "Send a Message in Sub-Only Mode".into(),
                "random_sub_emote_unlock" => "Unlock a Random Sub Emote".into(),
                "chosen_sub_emote_unlock" => "Choose an Emote to Unlock".into(),
                "chosen_modified_sub_emote_unlock" => "Modify a Single Emote".into(),
                "message_effect" => "Message Effects".into(),
                "gigantify_an_emote" => "Gigantify an Emote".into(),
                "celebration" => "On-Screen Celebration".into(),
                other => other.replace('_', " "),
            }
        }
        _ => return None,
    };
    let input = e["user_input"]
        .as_str()
        .or(e["message"]["text"].as_str())
        .unwrap_or_default()
        .trim();
    let mut fragments = vec![Fragment::Text(format!("redeemed {title}"))];
    if !input.is_empty() {
        fragments.push(Fragment::Text(": ".into()));
        fragments.extend(split_text(input));
    }
    Some(ChatMessage {
        platform: Platform::Twitch,
        id: s(&e["id"]),
        author: Author {
            id: s(&e["user_id"]),
            login: s(&e["user_login"]),
            display_name: s(&e["user_name"]),
            ..Author::default()
        },
        fragments: merge_text(fragments),
        kind: MessageKind::Event(EventKind::Redemption),
        sent_at_ms: chrono::DateTime::parse_from_rfc3339(timestamp)
            .map(|t| t.timestamp_millis())
            .unwrap_or_else(|_| rill_core::now_ms()),
        reply_to: None,
    })
}

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
        "channel.chat.notification" => notification(timestamp, event, badges).map(Event::Message),
        "channel.moderate" => moderation(timestamp, event).map(|moderation| Event::Moderation {
            platform,
            moderation,
        }),
        k if k.starts_with("channel.channel_points_") => {
            redemption(k, timestamp, event).map(Event::Message)
        }
        _ => None,
    }
}

/// A ban, timeout, unban, mod or VIP change by anyone; other moderation actions are left out.
pub fn moderation(timestamp: &str, e: &Value) -> Option<Moderation> {
    let action_name = e["action"].as_str()?;
    // Twitch names the object after the action.
    let target = &e[action_name];
    let action = match action_name {
        "ban" => ModAction::Ban,
        "unban" => ModAction::Unban,
        "timeout" => {
            let ends = chrono::DateTime::parse_from_rfc3339(target["expires_at"].as_str()?).ok()?;
            let at = chrono::DateTime::parse_from_rfc3339(timestamp).ok()?;
            let secs = (ends - at).num_seconds().max(1);
            // Twitch's two times are a moment apart: 599 s is a 10m timeout.
            let secs = if secs >= 60 {
                (secs + 30) / 60 * 60
            } else {
                secs
            };
            ModAction::Timeout {
                seconds: secs.min(u32::MAX as i64) as u32,
            }
        }
        "untimeout" => ModAction::Untimeout,
        "mod" => ModAction::Mod,
        "unmod" => ModAction::Unmod,
        "vip" => ModAction::Vip,
        "unvip" => ModAction::Unvip,
        _ => return None,
    };
    let s = |v: &Value| v.as_str().unwrap_or_default().to_string();
    let moderator = s(&e["moderator_user_name"]);
    Some(Moderation {
        moderator: (!moderator.is_empty()).then_some(moderator),
        user: s(&target["user_name"]),
        user_login: s(&target["user_login"]),
        user_id: s(&target["user_id"]),
        action,
        reason: target["reason"].as_str().map(str::to_string),
    })
}

/// Subs, resubs, gifts, raids, announcements and the like: the system line, then any message.
fn notification(timestamp: &str, e: &Value, badges: &Badges) -> Option<ChatMessage> {
    let mut m = chat_message(timestamp, e, badges);
    let system = e["system_message"].as_str().unwrap_or_default().trim();
    // The system line starts with the user's name, which the row already shows.
    let lead = [&m.author.display_name, &m.author.login]
        .into_iter()
        .filter(|n| !n.is_empty())
        .find_map(|n| {
            system
                .get(..n.len())
                .filter(|start| start.eq_ignore_ascii_case(n))
                .map(|_| system[n.len()..].trim_start())
        })
        .unwrap_or(system);
    let lead = match e["notice_type"].as_str() {
        Some("announcement") if lead.is_empty() => "announced:",
        _ => lead,
    };
    let mut fragments = Vec::new();
    if !lead.is_empty() {
        fragments.push(Fragment::Text(lead.to_string()));
        if !m.fragments.is_empty() {
            fragments.push(Fragment::Text(" ".into()));
        }
    }
    fragments.append(&mut m.fragments);
    m.fragments = merge_text(fragments);
    m.kind = MessageKind::Event(match e["notice_type"].as_str().unwrap_or_default() {
        "sub" | "resub" | "prime_paid_upgrade" | "gift_paid_upgrade" | "pay_it_forward" => {
            EventKind::Subscription
        }
        "sub_gift" | "community_sub_gift" => EventKind::Gift,
        "raid" => EventKind::Raid,
        "announcement" => EventKind::Announcement,
        "charity_donation" | "bits_badge_tier" => EventKind::Money,
        _ => EventKind::Other,
    });
    (!m.plain_text().trim().is_empty()).then_some(m)
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
            "moderator" | "lead_moderator" => roles.moderator = true,
            "vip" => roles.vip = true,
            "subscriber" | "founder" => roles.subscriber = true,
            _ => {}
        }
        let (mut title, url) = badges
            .get(&key)
            .map(|(t, u)| (t.clone(), Some(u.clone())))
            .unwrap_or_else(|| (set.to_string(), None));
        // For subscribers, `info` is how many months they've subscribed.
        if matches!(set, "subscriber" | "founder")
            && let Some(months) = b["info"].as_str().and_then(|m| m.parse::<u64>().ok())
            && months > 0
        {
            title = format!("{title} · {}", rill_core::message::tenure(months));
        }
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
            Some("emote") => {
                let mut emote = Fragment::emote(text, f["emote"]["id"].as_str().map(emote_url));
                // Twitch says whose emote it is: "0" is global.
                if let Fragment::Emote { scope, .. } = &mut emote {
                    *scope = match f["emote"]["owner_id"].as_str() {
                        Some("0") => EmoteScope::Global,
                        Some(o) if o == e["broadcaster_user_id"].as_str().unwrap_or("") => {
                            EmoteScope::Channel
                        }
                        Some(o) if !o.is_empty() => EmoteScope::OtherChannel,
                        _ => EmoteScope::Unknown,
                    };
                }
                fragments.push(emote)
            }
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
    use serde_json::json;

    const MESSAGE: &str = include_str!("../tests/fixtures/chat_message.json");

    #[test]
    fn redemptions_and_ad_breaks() {
        let custom = json!({"id": "r1", "user_id": "5", "user_login": "foo", "user_name": "Foo",
            "user_input": "drink water", "reward": {"title": "Hydrate", "cost": 100}});
        let Some(Event::Message(m)) = to_event(
            "channel.channel_points_custom_reward_redemption.add",
            "",
            &custom,
            &Badges::new(),
        ) else {
            panic!("expected a message");
        };
        assert!(m.kind.is_event());
        assert_eq!(m.plain_text(), "redeemed Hydrate: drink water");
        assert_eq!(m.author.display_name, "Foo");
        let auto = json!({"id": "r2", "user_name": "Bar", "reward": {"type": "send_highlighted_message"},
            "message": {"text": "look at me"}});
        let Some(Event::Message(m)) = to_event(
            "channel.channel_points_automatic_reward_redemption.add",
            "",
            &auto,
            &Badges::new(),
        ) else {
            panic!("expected a message");
        };
        assert_eq!(m.plain_text(), "redeemed Highlight My Message: look at me");

        let auto_ad = json!({"duration_seconds": 90, "is_automatic": true});
        assert_eq!(
            ad_break("channel.ad_break.begin", &auto_ad),
            Some((90, "automatic".into()))
        );
        let by = json!({"duration_seconds": "60", "is_automatic": false, "requester_user_login": "mod1"});
        assert_eq!(
            ad_break("channel.ad_break.begin", &by),
            Some((60, "started by mod1".into()))
        );
        assert_eq!(ad_break("channel.chat.message", &by), None);
    }

    #[test]
    fn notifications_read_as_one_line() {
        let sub = json!({
            "chatter_user_id": "9", "chatter_user_login": "bob", "chatter_user_name": "Bob",
            "badges": [], "notice_type": "resub", "message_id": "n1",
            "system_message": "Bob subscribed at Tier 1. They've subscribed for 5 months!",
            "message": {"text": "love it", "fragments": [{"type": "text", "text": "love it"}]}
        });
        let Some(Event::Message(m)) =
            to_event("channel.chat.notification", "", &sub, &Badges::new())
        else {
            panic!("expected a message");
        };
        assert!(m.kind.is_event());
        assert_eq!(
            m.plain_text(),
            "subscribed at Tier 1. They've subscribed for 5 months! love it"
        );
        let raid = json!({
            "chatter_user_id": "8", "chatter_user_login": "raider", "chatter_user_name": "Raider",
            "notice_type": "raid", "system_message": "42 raiders from Raider have joined!",
            "message": {"text": "", "fragments": []}
        });
        let Some(Event::Message(m)) =
            to_event("channel.chat.notification", "", &raid, &Badges::new())
        else {
            panic!("expected a message");
        };
        assert_eq!(m.plain_text(), "42 raiders from Raider have joined!");
        let announcement = json!({
            "chatter_user_id": "1", "chatter_user_login": "mod", "chatter_user_name": "Mod",
            "notice_type": "announcement", "system_message": "",
            "message": {"text": "Giveaway at 9", "fragments": [{"type": "text", "text": "Giveaway at 9"}]}
        });
        let Some(Event::Message(m)) = to_event(
            "channel.chat.notification",
            "",
            &announcement,
            &Badges::new(),
        ) else {
            panic!("expected a message");
        };
        assert_eq!(m.plain_text(), "announced: Giveaway at 9");
        let empty =
            json!({"chatter_user_name": "X", "system_message": "", "message": {"text": ""}});
        assert_eq!(
            to_event("channel.chat.notification", "", &empty, &Badges::new()),
            None
        );
    }

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
                Fragment::Emote {
                    name: "Kappa".into(),
                    url: Some(emote_url("25")),
                    size: None,
                    overlay: false,
                    scope: EmoteScope::Global,
                },
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

    #[test]
    fn reads_moderation_by_anyone() {
        let e = json!({
            "moderator_user_name": "ModFoo",
            "action": "timeout",
            "timeout": {"user_id": "9", "user_login": "bar", "user_name": "Bar",
                "reason": "spam", "expires_at": "2026-11-10T20:10:00.2Z"}
        });
        let m = moderation("2026-11-10T20:00:00.5Z", &e).unwrap();
        assert_eq!(m.line(), "ModFoo timed out Bar for 10m. Reason: spam");
        assert_eq!((m.user_login.as_str(), m.user_id.as_str()), ("bar", "9"));
        let e = json!({"moderator_user_name": "ModFoo", "action": "vip",
            "vip": {"user_id": "9", "user_login": "bar", "user_name": "Bar"}});
        assert_eq!(moderation("", &e).unwrap().line(), "ModFoo VIP'd Bar.");
        let e = json!({"moderator_user_name": "ModFoo", "action": "slow", "slow": {}});
        assert!(
            moderation("", &e).is_none(),
            "chat modes aren't moderation lines"
        );
    }
}
