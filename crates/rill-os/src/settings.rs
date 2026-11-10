//! `settings.json` in the data folder: app IDs and channels. Secrets never go here.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::data_dir;

/// Everything Rill reads from `settings.json`. Missing fields use defaults.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Platforms turned off in Settings, by name: not connected and not shown. Rumble starts off.
    pub disabled_platforms: Vec<String>,
    /// Deleted messages keep their text, faded and struck through, instead of "message deleted".
    pub show_deleted_messages: bool,
    /// Hovering a link shows the page's title, description and picture (fetched on hover).
    pub link_previews: bool,
    /// Each line's time; a narrow window hides it anyway.
    pub show_times: bool,
    /// Badges before names; a narrow window hides them anyway.
    pub show_badges: bool,
    /// Show only messages that mention you.
    pub mentions_only: bool,
    /// How many chat lines Rill keeps for scrolling back, searching and user cards.
    pub message_history: usize,
    pub highlights: Highlights,
    /// Text and emote size, in percent of normal (90 to 150).
    pub text_size: u32,
    /// Check for new versions now and then, download them, and install on the next start.
    pub auto_update: bool,
    /// The chat overlay for OBS.
    pub overlay: Overlay,
    pub twitch: TwitchSettings,
    pub youtube: YouTubeSettings,
    pub kick: KickSettings,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            disabled_platforms: vec!["rumble".into()],
            show_deleted_messages: false,
            link_previews: true,
            show_times: true,
            show_badges: true,
            mentions_only: false,
            message_history: 5_000,
            highlights: Highlights::default(),
            text_size: 100,
            auto_update: true,
            overlay: Overlay::default(),
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
    /// Highlights a user's first message in the channel; keeps a list of everyone who chats.
    pub first_chatters: bool,
    pub first_chatter_color: String,
    /// Colors for each kind of event, by key; missing ones use Rill's.
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
            first_chatters: false,
            first_chatter_color: "#23a55a".into(),
            event_colors: Default::default(),
            events_off: Vec::new(),
        }
    }
}

/// The chat overlay: a page on this computer that OBS shows as a Browser source.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Overlay {
    /// Serve the overlay while Rill is open. Off by default.
    pub enabled: bool,
    /// The port on 127.0.0.1.
    pub port: u16,
    /// Text size in pixels.
    pub font_size: u16,
    /// Seconds before a line fades away; 0 keeps every line.
    pub fade_seconds: u32,
    pub badges: bool,
    pub platform_icons: bool,
    /// Leave out messages that start with `!`.
    pub hide_commands: bool,
    /// Leave out well-known chat bots.
    pub hide_bots: bool,
    /// A dark backing behind the lines instead of none.
    pub background: bool,
    /// Highlights, separate from the app's own: all off by default.
    pub highlights: OverlayHighlights,
}

impl Default for Overlay {
    fn default() -> Self {
        Overlay {
            enabled: false,
            port: 47832,
            font_size: 18,
            fade_seconds: 0,
            badges: true,
            platform_icons: true,
            hide_commands: false,
            hide_bots: false,
            background: false,
            highlights: OverlayHighlights::default(),
        }
    }
}

/// The overlay's highlights, each with its color as `#rrggbb`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct OverlayHighlights {
    pub mentions: bool,
    pub mention_color: String,
    pub first_chatters: bool,
    pub first_chatter_color: String,
    /// Events, each kind on unless listed in `events_off`.
    pub events: bool,
    /// Colors by kind of event (`subscription`, `gift`, ...); missing ones use Rill's.
    pub event_colors: std::collections::BTreeMap<String, String>,
    pub events_off: Vec<String>,
}

impl Default for OverlayHighlights {
    fn default() -> Self {
        OverlayHighlights {
            mentions: false,
            mention_color: "#f23f43".into(),
            first_chatters: false,
            first_chatter_color: "#23a55a".into(),
            events: false,
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
        assert_eq!(s.disabled_platforms, ["rumble"], "Rumble starts off");
        assert!(s.show_times && s.show_badges && !s.mentions_only);
        assert!(!s.show_deleted_messages);
        assert!(s.link_previews, "previews are on unless turned off");
        assert_eq!(s.message_history, 5_000);
        assert!(s.highlights.mentions && s.highlights.events && s.highlights.scrollbar_marks);
        assert!(!s.overlay.enabled, "the overlay is off unless turned on");
        assert!(!s.overlay.highlights.mentions && !s.overlay.highlights.events);
    }
}
