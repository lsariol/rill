//! A load test with real chat from a public Twitch firehose, spread across four platforms.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::StreamExt;
use rill_core::backoff::Backoff;
use rill_core::{
    Adapter, AdapterIo, Author, Badge, Capabilities, ChatMessage, ConnectionStatus, Fragment,
    MessageKind, Platform, Roles, adapter::BoxFuture, merge_text, split_text,
};
use serde_json::Value;
use tokio::sync::mpsc;
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
    let images = BadgeImages::start();
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
                        if let Some((mut m, room)) = parse(&t) {
                            images.apply(&room, &mut m.author.badges);
                            if !events.message(m).await {
                                return;
                            }
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

/// Badges by `set/version`: their title and image.
type Badges = HashMap<String, (String, String)>;

/// Badge images without a Twitch sign-in: ivr.fi mirrors Twitch's global and channel badges.
const BADGES_API: &str = "https://api.ivr.fi/v2/twitch/badges";
/// Channels' badge sets kept before starting over; the firehose covers thousands of channels.
const CHANNELS_KEPT: usize = 3_000;
/// The pause between channel lookups, so a busy firehose doesn't flood the service.
const LOOKUP_GAP: Duration = Duration::from_millis(500);

/// Badge images for the firehose: Twitch's global set, and each channel's once it's seen.
#[derive(Clone)]
struct BadgeImages {
    known: Arc<Mutex<Known>>,
    lookups: mpsc::UnboundedSender<String>,
}

#[derive(Default)]
struct Known {
    global: Badges,
    channels: HashMap<String, Badges>,
    asked: HashSet<String>,
}

impl BadgeImages {
    /// Starts the background lookups: the global set first, then channels as they're asked for.
    fn start() -> BadgeImages {
        let (lookups, mut queue) = mpsc::unbounded_channel::<String>();
        let known = Arc::new(Mutex::new(Known::default()));
        let images = BadgeImages {
            known: known.clone(),
            lookups,
        };
        tokio::spawn(async move {
            let http = match reqwest::Client::builder()
                .user_agent(concat!("Rill/", env!("CARGO_PKG_VERSION")))
                .timeout(Duration::from_secs(10))
                .build()
            {
                Ok(h) => h,
                Err(e) => {
                    tracing::warn!(err = %e, "Firehose badges: no HTTP client");
                    return;
                }
            };
            let global = fetch_badges(&http, &format!("{BADGES_API}/global")).await;
            lock(&known).global = global;
            while let Some(room) = queue.recv().await {
                let set = fetch_badges(&http, &format!("{BADGES_API}/channel?id={room}")).await;
                {
                    let mut k = lock(&known);
                    if k.channels.len() >= CHANNELS_KEPT {
                        k.channels.clear();
                        k.asked.clear();
                    }
                    k.channels.insert(room, set);
                }
                sleep(LOOKUP_GAP).await;
            }
        });
        images
    }

    /// Fills in the badges' titles and images: the channel's own where known, else global.
    fn apply(&self, room: &str, badges: &mut [Badge]) {
        if badges.is_empty() {
            return;
        }
        let mut k = lock(&self.known);
        if !room.is_empty() && !k.channels.contains_key(room) && k.asked.insert(room.to_string()) {
            let _ = self.lookups.send(room.to_string());
        }
        for b in badges.iter_mut() {
            let found = k
                .channels
                .get(room)
                .and_then(|c| c.get(&b.id))
                .or_else(|| k.global.get(&b.id));
            if let Some((title, url)) = found {
                // Keep the months the line carried ("Subscriber · 1 year").
                b.title = match b.title.split_once(" · ") {
                    Some((_, months)) => format!("{title} · {months}"),
                    None => title.clone(),
                };
                b.image_url = Some(url.clone());
            }
        }
    }
}

fn lock(known: &Mutex<Known>) -> std::sync::MutexGuard<'_, Known> {
    known.lock().unwrap_or_else(|e| e.into_inner())
}

/// Reads one badge list (Twitch's shape: sets with versions); empty when it can't be had.
async fn fetch_badges(http: &reqwest::Client, url: &str) -> Badges {
    let body = match http.get(url).send().await {
        Ok(r) if r.status().is_success() => r.json::<Value>().await.ok(),
        Ok(r) => {
            tracing::debug!(status = %r.status(), "Firehose badges not found");
            None
        }
        Err(e) => {
            tracing::debug!(err = %e, "Firehose badges unreachable");
            None
        }
    };
    body.map(|v| badge_sets(&v)).unwrap_or_default()
}

/// Badges from a list of sets: `[{set_id, versions: [{id, title, image_url_2x}]}]`.
fn badge_sets(v: &Value) -> Badges {
    let mut out = Badges::new();
    for set in v.as_array().into_iter().flatten() {
        let Some(set_id) = set["set_id"].as_str() else {
            continue;
        };
        for ver in set["versions"].as_array().into_iter().flatten() {
            if let (Some(id), Some(url)) = (ver["id"].as_str(), ver["image_url_2x"].as_str()) {
                let title = ver["title"].as_str().unwrap_or(set_id).to_string();
                out.insert(format!("{set_id}/{id}"), (title, url.to_string()));
            }
        }
    }
    out
}

/// One firehose line as a chat message and its channel's Twitch ID, if it's a chat line.
pub fn parse(line: &str) -> Option<(ChatMessage, String)> {
    let v: Value = serde_json::from_str(line).ok()?;
    let s = |v: &Value| v.as_str().unwrap_or_default().to_string();
    let text = v["text"].as_str()?;
    let tags = &v["tags"];
    let mut roles = Roles::default();
    let mut badges = Vec::new();
    // `badge-info` says how many months a subscriber has subscribed: `subscriber/14`.
    let months = s(&tags["badge-info"])
        .split(',')
        .find_map(|b| {
            let (set, n) = b.split_once('/')?;
            matches!(set, "subscriber" | "founder")
                .then(|| n.parse::<u64>().ok())
                .flatten()
        })
        .filter(|n| *n > 0);
    for b in s(&tags["badges"]).split(',').filter(|b| !b.is_empty()) {
        let set = b.split('/').next().unwrap_or_default();
        match set {
            "broadcaster" => roles.broadcaster = true,
            "moderator" | "lead_moderator" => roles.moderator = true,
            "vip" => roles.vip = true,
            "subscriber" | "founder" => roles.subscriber = true,
            _ => {}
        }
        let title = match months {
            Some(n) if matches!(set, "subscriber" | "founder") => {
                format!("{set} · {}", rill_core::message::tenure(n))
            }
            _ => set.to_string(),
        };
        badges.push(Badge {
            id: b.to_string(),
            title,
            image_url: None,
        });
    }
    let room = s(&tags["room-id"]);
    let message = ChatMessage {
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
    };
    Some((message, room))
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
        let (m, _) = parse(line).unwrap();
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
    fn badges_take_the_channels_images_and_keep_the_months() {
        let line = r##"{"text":"hi","username":"bob","displayName":"Bob","channel":"c","id":"m1","tags":{"badges":"subscriber/12,vip/1","badge-info":"subscriber/14","room-id":"99","user-id":"42"}}"##;
        let (mut m, room) = parse(line).unwrap();
        assert_eq!(room, "99");
        assert_eq!(m.author.badges[0].title, "subscriber · 1 year, 2 months");
        let channel = badge_sets(&serde_json::json!([
            {"set_id": "subscriber", "versions": [
                {"id": "12", "title": "1-Year Subscriber", "image_url_2x": "https://c/sub12"}]}
        ]));
        let global = badge_sets(&serde_json::json!([
            {"set_id": "vip", "versions": [{"id": "1", "title": "VIP", "image_url_2x": "https://g/vip"}]}
        ]));
        let (lookups, mut asked) = mpsc::unbounded_channel();
        let images = BadgeImages {
            known: Arc::new(Mutex::new(Known {
                global,
                channels: HashMap::from([("99".to_string(), channel)]),
                asked: HashSet::new(),
            })),
            lookups,
        };
        images.apply(&room, &mut m.author.badges);
        let b = &m.author.badges;
        assert_eq!(b[0].title, "1-Year Subscriber · 1 year, 2 months");
        assert_eq!(b[0].image_url.as_deref(), Some("https://c/sub12"));
        assert_eq!(
            b[1].image_url.as_deref(),
            Some("https://g/vip"),
            "global fallback"
        );
        assert!(
            asked.try_recv().is_err(),
            "a known channel isn't looked up again"
        );
        // An unknown channel is looked up once.
        let mut other = m.author.badges.clone();
        images.apply("7", &mut other);
        images.apply("7", &mut other);
        assert_eq!(asked.try_recv().ok().as_deref(), Some("7"));
        assert!(asked.try_recv().is_err());
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
