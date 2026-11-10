//! Third-party emotes: 7TV's global and channel sets, kept current, swapped into messages.

mod live;
mod others;
pub mod seven_tv;

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use rill_core::backoff::Backoff;
use rill_core::{
    ChatMessage, EmoteOption, EmoteSource, Enrich, Event, EventSink, Fragment, replace_words,
};
use tokio::sync::OnceCell;
use tokio::task::JoinHandle;

pub use others::RoomBadges;
pub use seven_tv::Config;

/// A Twitch channel's custom moderator and VIP badges from FrankerFaceZ, if any.
pub async fn room_badges(http: &reqwest::Client, twitch_id: &str) -> RoomBadges {
    let base = Config::default().ffz;
    others::ffz_room_badges(http, &base, twitch_id)
        .await
        .unwrap_or_else(|e| {
            tracing::warn!(err = %e, "FrankerFaceZ badges not loaded");
            RoomBadges::default()
        })
}

/// One emote: the name chat uses, its 1x image, and its size there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Emote {
    pub id: String,
    pub name: String,
    pub url: String,
    pub size: (u16, u16),
    /// Zero-width: drawn over the emote before it.
    pub overlay: bool,
}

impl Emote {
    fn fragment(&self, scope: rill_core::EmoteScope) -> Fragment {
        Fragment::Emote {
            name: self.name.clone(),
            url: Some(self.url.clone()),
            // A provider that doesn't give sizes leaves them 0: drawn at emote height.
            size: (self.size.0 > 0 && self.size.1 > 0).then_some(self.size),
            overlay: self.overlay,
            scope,
        }
    }
}

/// Emotes by the name chat uses.
pub type EmoteSet = HashMap<String, Emote>;

#[derive(Default)]
struct Sets {
    /// 7TV, BetterTTV and FrankerFaceZ globals, shared by every chat so they're downloaded once.
    global: Arc<EmoteSet>,
    /// The channel's 7TV set, kept current.
    channel: EmoteSet,
    /// The channel's BetterTTV and FrankerFaceZ emotes (Twitch only).
    others: EmoteSet,
}

/// The emotes one chat shows. A channel's emote wins over a global one with the same name.
#[derive(Default)]
pub struct Emotes {
    sets: RwLock<Sets>,
    /// Bumped on every change, so the composer's list is sent again.
    version: AtomicU64,
}

impl Emotes {
    /// How many global and channel emotes are loaded.
    pub fn counts(&self) -> (usize, usize) {
        let s = self.read();
        (s.global.len(), s.channel.len() + s.others.len())
    }

    /// The emote chat means by `word`, if any.
    pub fn find(&self, word: &str) -> Option<Emote> {
        self.read().find(word).cloned()
    }

    fn read(&self) -> std::sync::RwLockReadGuard<'_, Sets> {
        self.sets.read().unwrap_or_else(|e| e.into_inner())
    }

    fn write(&self) -> std::sync::RwLockWriteGuard<'_, Sets> {
        self.version.fetch_add(1, Ordering::Relaxed);
        self.sets.write().unwrap_or_else(|e| e.into_inner())
    }

    /// Every emote by name, the channel's first, for the composer's `:` suggestions.
    pub fn options(&self) -> Vec<EmoteOption> {
        let s = self.read();
        let mut seen = std::collections::HashSet::new();
        s.channel
            .values()
            .chain(s.others.values())
            .chain(s.global.values())
            .filter(|e| !e.overlay && seen.insert(e.name.clone()))
            .map(|e| EmoteOption {
                name: e.name.clone(),
                url: Some(e.url.clone()),
                size: (e.size.0 > 0 && e.size.1 > 0).then_some(e.size),
            })
            .collect()
    }

    pub(crate) fn set_global(&self, set: Arc<EmoteSet>) {
        self.write().global = set;
    }

    pub(crate) fn set_channel(&self, set: EmoteSet) {
        self.write().channel = set;
    }

    pub(crate) fn set_others(&self, set: EmoteSet) {
        self.write().others = set;
    }

    pub(crate) fn change_channel(&self, f: impl FnOnce(&mut EmoteSet)) {
        f(&mut self.write().channel);
    }
}

impl Sets {
    /// The channel's own emotes first (7TV, then BetterTTV and FrankerFaceZ), then the globals.
    fn find(&self, word: &str) -> Option<&Emote> {
        self.scoped(word).map(|(e, _)| e)
    }

    /// The emote and whether it's the channel's own or global.
    fn scoped(&self, word: &str) -> Option<(&Emote, rill_core::EmoteScope)> {
        use rill_core::EmoteScope::{Channel, Global};
        self.channel
            .get(word)
            .or_else(|| self.others.get(word))
            .map(|e| (e, Channel))
            .or_else(|| self.global.get(word).map(|e| (e, Global)))
    }
}

impl Enrich for Emotes {
    fn enrich(&self, message: &mut ChatMessage) {
        let s = self.read();
        if s.global.is_empty() && s.channel.is_empty() && s.others.is_empty() {
            return;
        }
        let fragments = std::mem::take(&mut message.fragments);
        message.fragments = replace_words(fragments, |w| {
            s.scoped(w).map(|(e, scope)| e.fragment(scope))
        });
    }
}

/// Which channel's 7TV emotes to load, by the IDs 7TV might know it under.
#[derive(Debug, Clone)]
pub struct Channel {
    /// 7TV's name for the platform: "twitch" or "kick".
    pub platform: &'static str,
    /// Tried in order until one has a 7TV account.
    pub ids: Vec<String>,
}

/// Keeps a chat's emotes loaded and current until dropped.
pub struct Follow {
    emotes: Arc<Emotes>,
    task: JoinHandle<()>,
}

impl Follow {
    pub fn emotes(&self) -> &Arc<Emotes> {
        &self.emotes
    }

    /// The emotes as an [`Enrich`] for an adapter's event sink.
    pub fn enrich(&self) -> Arc<dyn Enrich> {
        self.emotes.clone()
    }
}

impl Drop for Follow {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Starts loading global and channel emotes, sending the usable list to `events` as it changes.
pub fn follow(
    http: reqwest::Client,
    cfg: Config,
    channel: Option<Channel>,
    events: Option<EventSink>,
) -> Follow {
    let emotes = Arc::new(Emotes::default());
    let task = tokio::spawn(run(http, cfg, channel, emotes.clone(), events));
    Follow { emotes, task }
}

/// Sends the emote list when it has changed, at most every 2 s (7TV changes can come in bursts).
async fn publish(events: EventSink, emotes: Arc<Emotes>) {
    let mut sent = u64::MAX;
    loop {
        let now = emotes.version.load(Ordering::Relaxed);
        if now != sent {
            sent = now;
            let list = emotes.options();
            let event = Event::Emotes {
                platform: events.platform(),
                source: EmoteSource::ThirdParty,
                emotes: list,
            };
            if !events.send(event).await {
                return;
            }
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

async fn run(
    http: reqwest::Client,
    cfg: Config,
    channel: Option<Channel>,
    emotes: Arc<Emotes>,
    events: Option<EventSink>,
) {
    if let Some(events) = events {
        tokio::spawn(publish(events, emotes.clone()));
    }
    let global = async {
        let mut backoff = Backoff::new(Duration::from_secs(5), Duration::from_secs(600));
        let cell = global_cell(&cfg);
        loop {
            let loaded = cell
                .get_or_try_init(|| async {
                    let mut set = seven_tv::global(&http, &cfg).await?;
                    // BetterTTV and FrankerFaceZ are extras: a failure there doesn't hold up 7TV's.
                    let (bttv, ffz) = tokio::join!(
                        others::bttv_global(&http, &cfg.bttv),
                        others::ffz_global(&http, &cfg.ffz)
                    );
                    for extra in [bttv, ffz] {
                        match extra {
                            Ok(extra) => {
                                for (name, e) in extra {
                                    set.entry(name).or_insert(e);
                                }
                            }
                            Err(e) => tracing::warn!(err = %e, "Global emotes not loaded"),
                        }
                    }
                    tracing::debug!(count = set.len(), "Global emotes loaded");
                    Ok::<_, String>(Arc::new(set))
                })
                .await;
            match loaded {
                Ok(set) => {
                    emotes.set_global(set.clone());
                    return;
                }
                Err(e) => {
                    tracing::warn!(err = %e, "7TV global emotes not loaded");
                    tokio::time::sleep(backoff.next_delay()).await;
                }
            }
        }
    };
    let others = async {
        // BetterTTV and FrankerFaceZ know Twitch channels only.
        let Some(id) = channel
            .as_ref()
            .filter(|c| c.platform == "twitch")
            .and_then(|c| c.ids.first())
        else {
            return;
        };
        let (bttv, ffz) = tokio::join!(
            others::bttv_channel(&http, &cfg.bttv, id),
            others::ffz_channel(&http, &cfg.ffz, id)
        );
        let mut set = EmoteSet::new();
        for (what, loaded) in [("BetterTTV", bttv), ("FrankerFaceZ", ffz)] {
            match loaded {
                Ok(extra) => {
                    tracing::debug!(count = extra.len(), source = what, "Channel emotes loaded");
                    for (name, e) in extra {
                        set.entry(name).or_insert(e);
                    }
                }
                Err(e) => tracing::warn!(err = %e, "Channel emotes not loaded"),
            }
        }
        emotes.set_others(set);
    };
    let channel = async {
        if let Some(channel) = channel.as_ref() {
            follow_channel(&http, &cfg, channel, &emotes).await;
        }
    };
    tokio::join!(global, others, channel);
}

/// A global set that loads once and is then shared.
type GlobalCell = Arc<OnceCell<Arc<EmoteSet>>>;

/// The global set for one 7TV address, loaded once per run and shared by every chat.
fn global_cell(cfg: &Config) -> GlobalCell {
    static CELLS: Mutex<Vec<(String, GlobalCell)>> = Mutex::new(Vec::new());
    let mut cells = CELLS.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((_, cell)) = cells.iter().find(|(api, _)| *api == cfg.api) {
        return cell.clone();
    }
    let cell = Arc::new(OnceCell::new());
    cells.push((cfg.api.clone(), cell.clone()));
    cell
}

/// Loads the channel's set, then follows its changes; after a lost connection, loads it again.
async fn follow_channel(http: &reqwest::Client, cfg: &Config, channel: &Channel, emotes: &Emotes) {
    let mut backoff = Backoff::new(Duration::from_secs(5), Duration::from_secs(600));
    loop {
        let found = match seven_tv::channel(http, cfg, channel.platform, &channel.ids).await {
            Ok(found) => found,
            Err(e) => {
                tracing::warn!(platform = channel.platform, err = %e, "7TV channel emotes not loaded");
                tokio::time::sleep(backoff.next_delay()).await;
                continue;
            }
        };
        let Some(found) = found else {
            tracing::info!(platform = channel.platform, "Channel has no 7TV account");
            emotes.set_channel(EmoteSet::new());
            return;
        };
        tracing::info!(
            platform = channel.platform,
            count = found.emotes.len(),
            "7TV channel emotes loaded"
        );
        emotes.set_channel(found.emotes);
        let Some(set_id) = found.set_id else {
            return;
        };
        let started = tokio::time::Instant::now();
        match live::follow(http, cfg, &set_id, found.user_id.as_deref(), emotes).await {
            live::End::Changed => backoff.reset(),
            live::End::Lost(why) => {
                tracing::debug!(err = %why, "7TV event socket lost");
                if started.elapsed() > Duration::from_secs(60) {
                    backoff.reset();
                }
                tokio::time::sleep(backoff.next_delay()).await;
            }
            live::End::Wait(wait, why) => {
                tracing::info!(err = %why, "7TV event socket closed for maintenance");
                tokio::time::sleep(wait).await;
            }
            live::End::Stop(why) => {
                // The set stays as loaded; only live changes stop.
                tracing::warn!(err = %why, "7TV event socket refused Rill");
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rill_core::{Author, MessageKind, Platform};

    fn emote(name: &str, id: &str) -> Emote {
        Emote {
            id: id.into(),
            name: name.into(),
            url: format!("https://cdn.7tv.app/emote/{id}/1x.webp"),
            size: (32, 32),
            overlay: false,
        }
    }

    fn message(text: &str) -> ChatMessage {
        ChatMessage {
            platform: Platform::Twitch,
            id: "m".into(),
            author: Author::default(),
            fragments: vec![Fragment::Text(text.into())],
            kind: MessageKind::Chat,
            sent_at_ms: 0,
            reply_to: None,
        }
    }

    #[test]
    fn channel_emotes_win_over_global_ones() {
        let e = Emotes::default();
        e.set_global(Arc::new(HashMap::from([
            ("EZ".into(), emote("EZ", "G1")),
            ("Clap".into(), emote("Clap", "G2")),
        ])));
        e.set_channel(HashMap::from([("EZ".into(), emote("EZ", "C1"))]));
        let mut m = message("EZ Clap ez");
        e.enrich(&mut m);
        let urls: Vec<_> = m
            .fragments
            .iter()
            .filter_map(|f| match f {
                Fragment::Emote { url, .. } => url.clone(),
                _ => None,
            })
            .collect();
        assert_eq!(
            urls,
            [
                "https://cdn.7tv.app/emote/C1/1x.webp",
                "https://cdn.7tv.app/emote/G2/1x.webp"
            ]
        );
        assert_eq!(m.plain_text(), "EZ Clap ez");
    }

    #[test]
    fn nothing_loaded_leaves_messages_alone() {
        let mut m = message("EZ");
        Emotes::default().enrich(&mut m);
        assert_eq!(m.fragments, vec![Fragment::Text("EZ".into())]);
    }
}

/// Against the real 7TV: `cargo test -p rill-emotes live -- --ignored --nocapture`.
#[cfg(test)]
mod live_7tv {
    use super::*;

    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "talks to 7tv.io"]
    async fn live_loads_a_big_channel_and_the_global_set() {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        let started = std::time::Instant::now();
        // VulpesHD on Twitch: a channel with thousands of 7TV emotes.
        let channel = Channel {
            platform: "twitch",
            ids: vec!["42163359".into()],
        };
        let f = follow(
            reqwest::Client::new(),
            Config::default(),
            Some(channel),
            None,
        );
        loop {
            let (global, channel) = f.emotes().counts();
            // 7TV's 3,000-odd plus the channel's BetterTTV and FrankerFaceZ emotes.
            if global > 100 && channel > 3_210 {
                println!(
                    "{global} global and {channel} channel emotes in {:?}",
                    started.elapsed()
                );
                break;
            }
            assert!(
                started.elapsed() < Duration::from_secs(60),
                "only {global} global, {channel} channel"
            );
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        // Stay subscribed a little, so a refused subscription would show in the log.
        tokio::time::sleep(Duration::from_secs(5)).await;
        assert!(!f.task.is_finished(), "the follower stopped");
    }
}
