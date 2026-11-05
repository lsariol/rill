//! `settings.json` in the data folder: app IDs and channels. Secrets never go here.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::data_dir;

/// Everything Rill reads from `settings.json`. Missing fields use defaults.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub twitch: TwitchSettings,
    pub youtube: YouTubeSettings,
    pub kick: KickSettings,
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

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct KickSettings {
    /// The channel to read, by its URL name (kick.com/<channel>).
    pub channel: Option<String>,
    /// Overrides the built-in app client ID, for sending and moderation.
    pub client_id: Option<String>,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partial_settings_fill_in_defaults() {
        let s: Settings = serde_json::from_str(r#"{"kick":{"channel":"xqc"}}"#).unwrap();
        assert_eq!(s.kick.channel.as_deref(), Some("xqc"));
        assert!(s.twitch.client_id.is_none());
    }
}
