//! A load test with real chat: reads a public Twitch chat firehose (newline JSON in catquery's
//! `jsonBasic` shape) and spreads its messages across the four platforms. Read-only, and nothing
//! it receives is kept.

use std::time::Duration;

use futures_util::StreamExt;
use rill_core::backoff::Backoff;
use rill_core::{
    Adapter, AdapterIo, Author, Badge, Capabilities, ChatMessage, ConnectionStatus, Fragment,
    MessageKind, Platform, Roles, adapter::BoxFuture, merge_text, split_text,
};
use serde_json::Value;
use tokio::time::sleep;
use tokio_tungstenite::tungstenite::Message;

/// Reads chat from a firehose; registered in Twitch's slot.
pub struct FirehoseAdapter {
    url: String,
}

impl FirehoseAdapter {
    pub fn new(url: impl Into<String>) -> FirehoseAdapter {
        FirehoseAdapter { url: url.into() }
    }
}

impl Adapter for FirehoseAdapter {
    fn platform(&self) -> Platform {
        Platform::Twitch
    }

    fn run(self: Box<Self>, io: AdapterIo) -> BoxFuture {
        Box::pin(run(self.url, io))
    }
}

async fn run(url: String, io: AdapterIo) {
    let AdapterIo {
        events,
        mut requests,
    } = io;
    let mut backoff = Backoff::chat();
    loop {
        events
            .status(ConnectionStatus::Connecting, None, Capabilities::NONE)
            .await;
        let socket = tokio_tungstenite::connect_async(url.as_str()).await;
        let mut ws = match socket {
            Ok((ws, _)) => ws,
            Err(e) => {
                let attempt = backoff.attempt() + 1;
                tracing::warn!(err = %e, "Firehose unreachable");
                events
                    .status(
                        ConnectionStatus::Reconnecting { attempt },
                        None,
                        Capabilities::NONE,
                    )
                    .await;
                sleep(backoff.next_delay()).await;
                continue;
            }
        };
        tracing::info!("Firehose connected");
        let opened = tokio::time::Instant::now();
        events
            .status(ConnectionStatus::Connected, None, Capabilities::NONE)
            .await;
        loop {
            tokio::select! {
                msg = ws.next() => match msg {
                    Some(Ok(Message::Text(t))) => {
                        if let Some(m) = parse(&t)
                            && !events.message(m).await
                        {
                            return;
                        }
                    }
                    Some(Ok(_)) => {}
                    Some(Err(e)) => {
                        tracing::warn!(err = %e, "Firehose dropped");
                        break;
                    }
                    None => {
                        tracing::warn!("Firehose closed the connection");
                        break;
                    }
                },
                req = requests.recv() => {
                    let Some(req) = req else { return };
                    events
                        .done(req.id, Err("The firehose is a read-only test feed.".into()))
                        .await;
                }
            }
        }
        // A connection that lasted a while starts the backoff over; quick closes back off.
        if opened.elapsed() > Duration::from_secs(60) {
            backoff.reset();
        }
        sleep(backoff.next_delay()).await;
    }
}

/// Which platform a channel's messages show under, so all four icons and filters get traffic.
fn platform_for(channel: &str) -> Platform {
    let h = channel
        .bytes()
        .fold(0u32, |h, b| h.wrapping_mul(31).wrapping_add(u32::from(b)));
    Platform::ALL[h as usize % Platform::ALL.len()]
}

/// One firehose line as a chat message; `None` when it isn't a chat line.
pub fn parse(line: &str) -> Option<ChatMessage> {
    let v: Value = serde_json::from_str(line).ok()?;
    let s = |v: &Value| v.as_str().unwrap_or_default().to_string();
    let text = v["text"].as_str()?;
    let tags = &v["tags"];
    let mut roles = Roles::default();
    let mut badges = Vec::new();
    for b in s(&tags["badges"]).split(',').filter(|b| !b.is_empty()) {
        let set = b.split('/').next().unwrap_or_default();
        match set {
            "broadcaster" => roles.broadcaster = true,
            "moderator" | "lead_moderator" => roles.moderator = true,
            "vip" => roles.vip = true,
            "subscriber" | "founder" => roles.subscriber = true,
            _ => {}
        }
        badges.push(Badge {
            id: b.to_string(),
            title: set.to_string(),
            image_url: None,
        });
    }
    Some(ChatMessage {
        platform: platform_for(v["channel"].as_str().unwrap_or_default()),
        id: s(&v["id"]),
        author: Author {
            id: s(&tags["user-id"]),
            login: s(&v["username"]),
            display_name: s(&v["displayName"]),
            color: tags["color"]
                .as_str()
                .and_then(|c| u32::from_str_radix(c.trim_start_matches('#'), 16).ok()),
            badges,
            roles,
        },
        fragments: fragments(text, tags["emotes"].as_str().unwrap_or_default()),
        kind: MessageKind::Chat,
        sent_at_ms: rill_core::now_ms(),
        reply_to: None,
    })
}

/// Splits text at Twitch's emote ranges (`25:0-4,12-16/1902:6-10`, in characters).
fn fragments(text: &str, emotes: &str) -> Vec<Fragment> {
    let mut ranges: Vec<(usize, usize, &str)> = emotes
        .split('/')
        .filter_map(|e| e.split_once(':'))
        .flat_map(|(id, spots)| {
            spots.split(',').filter_map(move |r| {
                let (a, b) = r.split_once('-')?;
                Some((a.parse().ok()?, b.parse().ok()?, id))
            })
        })
        .collect();
    ranges.sort_unstable();
    let chars: Vec<char> = text.chars().collect();
    let mut out = Vec::new();
    let mut at = 0;
    for (start, end, id) in ranges {
        if start < at || end >= chars.len() || start > end {
            continue;
        }
        out.extend(split_text(&chars[at..start].iter().collect::<String>()));
        let name: String = chars[start..=end].iter().collect();
        out.push(Fragment::emote(
            name,
            Some(format!(
                "https://static-cdn.jtvnw.net/emoticons/v2/{id}/default/dark/1.0"
            )),
        ));
        at = end + 1;
    }
    out.extend(split_text(&chars[at..].iter().collect::<String>()));
    merge_text(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_a_line_with_emotes_badges_and_colour() {
        let line = r##"{"text":"hi Kappa ok Kappa","username":"bob","displayName":"Bob","channel":"somechannel","id":"m1","tags":{"badges":"moderator/1,subscriber/12","color":"#FF0000","emotes":"25:3-7,12-16","user-id":"42"}}"##;
        let m = parse(line).unwrap();
        assert_eq!(m.plain_text(), "hi Kappa ok Kappa");
        let emotes = m
            .fragments
            .iter()
            .filter(|f| matches!(f, Fragment::Emote { .. }))
            .count();
        assert_eq!(emotes, 2);
        assert!(m.author.roles.moderator && m.author.roles.subscriber);
        assert_eq!(m.author.color, Some(0xff0000));
        assert_eq!(m.author.id, "42");
        assert_eq!(m.platform, platform_for("somechannel"));
    }

    #[test]
    fn odd_lines_and_ranges_are_safe() {
        assert!(parse("not json").is_none());
        assert!(parse(r#"{"username":"x"}"#).is_none());
        // Ranges past the end, overlapping or backwards are skipped; multi-byte text is fine.
        let f = fragments("Ахуеть 🙂", "1:0-1,1-3/2:50-60/3:5-2");
        assert_eq!(
            f.iter()
                .map(|f| match f {
                    Fragment::Text(t) => t.clone(),
                    Fragment::Emote { name, .. } => format!("[{name}]"),
                    _ => String::new(),
                })
                .collect::<String>(),
            "[Ах]уеть 🙂"
        );
        let spread: std::collections::HashSet<_> = (0..200)
            .map(|i| platform_for(&format!("chan{i}")))
            .collect();
        assert_eq!(spread.len(), 4, "every platform gets messages");
    }
}
