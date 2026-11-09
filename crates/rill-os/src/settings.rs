//! `settings.json` in the data folder: app IDs and channels. Secrets never go here.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::data_dir;

/// Everything Rill reads from `settings.json`. Missing fields use defaults.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Platforms turned off in Settings, by name ("rumble"): not connected and not shown.
    pub disabled_platforms: Vec<String>,
    /// Deleted messages keep their text, faded and struck through, instead of "message deleted".
    pub show_deleted_messages: bool,
    /// Hovering a link shows the page's title, description and picture (fetched on hover).
    pub link_previews: bool,
    /// How many chat lines Rill keeps for scrolling back, searching and user cards.
    pub message_history: usize,
    pub highlights: Highlights,
    pub twitch: TwitchSettings,
    pub youtube: YouTubeSettings,
    pub kick: KickSettings,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            disabled_platforms: Vec::new(),
            show_deleted_messages: false,
            link_previews: true,
            message_history: 5_000,
            highlights: Highlights::default(),
            twitch: TwitchSettings::default(),
            youtube: YouTubeSettings::default(),
            kick: KickSettings::default(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TwitchSettings {
    /// Overrides the built-in app client ID.
    pub client_id: Option<String>,
    /// The channel to join, by login; the signed-in user's own channel when empty.
    pub channel: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct YouTubeSettings {
    /// Overrides the built-in desktop OAuth client.
    pub client_id: Option<String>,
    pub client_secret: Option<String>,
    /// The shortest wait between chat polls, in milliseconds (default 5000). Longer saves quota.
    pub min_poll_ms: Option<u64>,
}

/// Which lines stand out with a colored background and bar, and get a mark on the scroll bar.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Highlights {
    /// Messages that mention you.
    pub mentions: bool,
    /// As `#rrggbb`.
    pub mention_color: String,
    /// Subscriptions, gifts, raids, Rants, redemptions and ad breaks.
    pub events: bool,
    pub event_color: String,
    /// Marks on the scroll bar where highlighted lines are.
    pub scrollbar_marks: bool,
    /// A user's first message in the channel (once Rill has watched it a week).
    pub first_chatters: bool,
    pub first_chatter_color: String,
    /// Colors for each kind of event (`subscription`, `gift`, `raid`, `money`, `redemption`,
    /// `follow`, `announcement`, `ad_break`, `other`); missing ones use Rill's.
    pub event_colors: std::collections::BTreeMap<String, String>,
    /// Kinds of event that aren't highlighted.
    pub events_off: Vec<String>,
}

impl Default for Highlights {
    fn default() -> Self {
        Highlights {
            mentions: true,
            mention_color: "#f23f43".into(),
            events: true,
            event_color: "#a970ff".into(),
            scrollbar_marks: true,
            first_chatters: true,
            first_chatter_color: "#23a55a".into(),
            event_colors: Default::default(),
            events_off: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct KickSettings {
    /// The channel to read, by its URL name (kick.com/<channel>).
    pub channel: Option<String>,
    /// Overrides the built-in app client ID, for sending and moderation.
    pub client_id: Option<String>,
    /// The app's client secret, for development: Rill then signs in to Kick directly.
    pub client_secret: Option<String>,
    /// The sign-in helper's address, which holds the client secret instead of Rill.
    pub auth_helper: Option<String>,
}

/// Where the settings file lives.
pub fn path() -> PathBuf {
    data_dir().join("settings.json")
}

/// Loads the settings; a missing file gives defaults, a broken one gives an error saying where.
pub fn load() -> Result<Settings, String> {
    let path = path();
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Settings::default()),
        Err(e) => return Err(format!("Settings: can't read {}: {e}.", path.display())),
    };
    serde_json::from_str(&text).map_err(|e| {
        format!(
            "Settings: {} isn't valid JSON ({e}). Fix the file or delete it to start over.",
            path.display()
        )
    })
}

/// Writes the settings, replacing the file in one step.
pub fn save(settings: &Settings) -> Result<(), String> {
    let path = path();
    let fail = |e: std::io::Error| format!("Settings: can't write {}: {e}.", path.display());
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(fail)?;
    }
    let text = serde_json::to_string_pretty(settings)
        .map_err(|e| format!("Settings: can't encode the settings: {e}."))?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, text).map_err(fail)?;
    std::fs::rename(&tmp, &path).map_err(fail)
}

/// Loads, changes and saves the settings.
pub fn update(change: impl FnOnce(&mut Settings)) -> Result<(), String> {
    let mut settings = load()?;
    change(&mut settings);
    save(&settings)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partial_settings_fill_in_defaults() {
        let s: Settings = serde_json::from_str(r#"{"kick":{"channel":"xqc"}}"#).unwrap();
        assert_eq!(s.kick.channel.as_deref(), Some("xqc"));
        assert!(s.twitch.client_id.is_none());
        assert!(s.disabled_platforms.is_empty());
        assert!(!s.show_deleted_messages);
        assert!(s.link_previews, "previews are on unless turned off");
        assert_eq!(s.message_history, 5_000);
        assert!(s.highlights.mentions && s.highlights.events && s.highlights.scrollbar_marks);
    }
}
