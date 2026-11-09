//! Converts YouTube liveChatMessage resources into Rill events.

use rill_core::{
    Author, Badge, ChatMessage, Event, EventKind, MessageKind, Platform, Roles, split_text,
};
use serde_json::Value;

/// One page of messages: the events, where to continue, and how long to wait.
#[derive(Debug, PartialEq)]
pub struct Page {
    pub events: Vec<Event>,
    pub next_page_token: Option<String>,
    pub wait_ms: u64,
    /// The chat went offline.
    pub offline: bool,
}

/// Reads a liveChatMessages.list response.
pub fn page(v: &Value) -> Page {
    let events = v["items"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(to_event)
        .collect();
    Page {
        events,
        next_page_token: v["nextPageToken"].as_str().map(str::to_string),
        wait_ms: v["pollingIntervalMillis"].as_u64().unwrap_or(5_000),
        offline: v["offlineAt"].is_string(),
    }
}

/// Turns one liveChatMessage into a Rill event; `None` for kinds Rill doesn't show.
pub fn to_event(item: &Value) -> Option<Event> {
    let s = |v: &Value| v.as_str().unwrap_or_default().to_string();
    let snippet = &item["snippet"];
    let platform = Platform::YouTube;
    let kind = snippet["type"].as_str()?;
    match kind {
        "messageDeletedEvent" => {
            return Some(Event::Deleted {
                platform,
                message_id: s(&snippet["messageDeletedDetails"]["deletedMessageId"]),
            });
        }
        "userBannedEvent" => {
            return Some(Event::UserCleared {
                platform,
                user_id: s(&snippet["userBannedDetails"]["bannedUserDetails"]["channelId"]),
            });
        }
        "textMessageEvent"
        | "superChatEvent"
        | "superStickerEvent"
        | "newSponsorEvent"
        | "memberMilestoneChatEvent"
        | "membershipGiftingEvent"
        | "giftMembershipReceivedEvent" => {}
        _ => return None,
    }
    let a = &item["authorDetails"];
    let roles = Roles {
        broadcaster: a["isChatOwner"].as_bool().unwrap_or(false),
        moderator: a["isChatModerator"].as_bool().unwrap_or(false),
        vip: false,
        subscriber: a["isChatSponsor"].as_bool().unwrap_or(false),
    };
    let mut badges = Vec::new();
    for (has, id, title) in [
        (roles.broadcaster, "broadcaster/1", "Owner"),
        (roles.moderator, "moderator/1", "Moderator"),
        (roles.subscriber, "subscriber/1", "Member"),
    ] {
        if has {
            badges.push(Badge {
                id: id.into(),
                title: title.into(),
                image_url: None,
            });
        }
    }
    let text = snippet["displayMessage"]
        .as_str()
        .or_else(|| snippet["textMessageDetails"]["messageText"].as_str())
        .unwrap_or_default();
    let name = s(&a["displayName"]);
    Some(Event::Message(ChatMessage {
        platform,
        id: s(&item["id"]),
        author: Author {
            id: s(&a["channelId"]),
            login: name.trim_start_matches('@').to_string(),
            display_name: name,
            color: None,
            badges,
            roles,
        },
        fragments: split_text(text),
        kind: match kind {
            "textMessageEvent" => MessageKind::Chat,
            "superChatEvent" | "superStickerEvent" | "fanFundingEvent" => {
                MessageKind::Event(EventKind::Money)
            }
            "newSponsorEvent" | "memberMilestoneChatEvent" => {
                MessageKind::Event(EventKind::Subscription)
            }
            "membershipGiftingEvent" | "giftMembershipReceivedEvent" => {
                MessageKind::Event(EventKind::Gift)
            }
            _ => MessageKind::Event(EventKind::Other),
        },
        sent_at_ms: snippet["publishedAt"]
            .as_str()
            .and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok())
            .map(|t| t.timestamp_millis())
            .unwrap_or_else(rill_core::now_ms),
        reply_to: None,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_a_page() {
        let v: Value =
            serde_json::from_str(include_str!("../tests/fixtures/messages.json")).unwrap();
        let p = page(&v);
        assert_eq!(p.next_page_token.as_deref(), Some("GKjVnqvLs"));
        assert_eq!(p.wait_ms, 3_000);
        assert!(!p.offline);
        assert_eq!(p.events.len(), 3);
        let Event::Message(m) = &p.events[0] else {
            panic!("expected a message");
        };
        assert_eq!(m.author.display_name, "@viewer");
        assert_eq!(m.author.login, "viewer");
        assert!(m.author.roles.moderator);
        assert_eq!(m.plain_text(), "hello @rill_dev");
        assert_eq!(m.sent_at_ms, 1_730_000_000_000);
        assert!(matches!(&p.events[1], Event::Message(m) if m.kind.is_event()));
        assert_eq!(
            p.events[2],
            Event::Deleted {
                platform: Platform::YouTube,
                message_id: "LCC.old".into()
            }
        );
    }
}
