//! Third-party emotes for chat: loads 7TV's global set and a channel's set, keeps the channel's
//! set current over 7TV's event socket, and swaps emote names in messages for images.

mod live;
pub mod seven_tv;

use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use rill_core::backoff::Backoff;
use rill_core::{ChatMessage, Enrich, Fragment, replace_words};
use tokio::sync::OnceCell;
use tokio::task::JoinHandle;

pub use seven_tv::Config;

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
    fn fragment(&self) -> Fragment {
        Fragment::Emote {
            name: self.name.clone(),
            url: Some(self.url.clone()),
            size: Some(self.size),
            overlay: self.overlay,
        }
    }
}

/// Emotes by the name chat uses.
pub type EmoteSet = HashMap<String, Emote>;

#[derive(Default)]
struct Sets {
    /// Shared by every chat, so it's downloaded once.
    global: Arc<EmoteSet>,
    channel: EmoteSet,
}

/// The emotes one chat shows. A channel's emote wins over a global one with the same name.
#[derive(Default)]
pub struct Emotes {
    sets: RwLock<Sets>,
}

impl Emotes {
    /// How many global and channel emotes are loaded.
    pub fn counts(&self) -> (usize, usize) {
        let s = self.read();
        (s.global.len(), s.channel.len())
    }

    /// The emote chat means by `word`, if any.
    pub fn find(&self, word: &str) -> Option<Emote> {
        let s = self.read();
        s.channel.get(word).or_else(|| s.global.get(word)).cloned()
    }

    fn read(&self) -> std::sync::RwLockReadGuard<'_, Sets> {
        self.sets.read().unwrap_or_else(|e| e.into_inner())
    }

    fn write(&self) -> std::sync::RwLockWriteGuard<'_, Sets> {
        self.sets.write().unwrap_or_else(|e| e.into_inner())
    }

    pub(crate) fn set_global(&self, set: Arc<EmoteSet>) {
        self.write().global = set;
    }

    pub(crate) fn set_channel(&self, set: EmoteSet) {
        self.write().channel = set;
    }

    pub(crate) fn change_channel(&self, f: impl FnOnce(&mut EmoteSet)) {
        f(&mut self.write().channel);
    }
}

impl Enrich for Emotes {
    fn enrich(&self, message: &mut ChatMessage) {
        let s = self.read();
        if s.global.is_empty() && s.channel.is_empty() {
            return;
        }
        let fragments = std::mem::take(&mut message.fragments);
        message.fragments = replace_words(fragments, |w| {
            s.channel
                .get(w)
                .or_else(|| s.global.get(w))
                .map(Emote::fragment)
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

/// Starts loading global emotes, and a channel's when given. Call on a Tokio runtime.
pub fn follow(http: reqwest::Client, cfg: Config, channel: Option<Channel>) -> Follow {
    let emotes = Arc::new(Emotes::default());
    let task = tokio::spawn(run(http, cfg, channel, emotes.clone()));
    Follow { emotes, task }
}

async fn run(http: reqwest::Client, cfg: Config, channel: Option<Channel>, emotes: Arc<Emotes>) {
    let global = async {
        let mut backoff = Backoff::new(Duration::from_secs(5), Duration::from_secs(600));
        let cell = global_cell(&cfg);
        loop {
            let loaded = cell
                .get_or_try_init(|| async {
                    let set = seven_tv::global(&http, &cfg).await?;
                    tracing::debug!(count = set.len(), "7TV global emotes loaded");
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
    let channel = async {
        if let Some(channel) = channel {
            follow_channel(&http, &cfg, &channel, &emotes).await;
        }
    };
    tokio::join!(global, channel);
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
        let f = follow(reqwest::Client::new(), Config::default(), Some(channel));
        loop {
            let (global, channel) = f.emotes().counts();
            if global > 0 && channel > 1000 {
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
