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
    /// The shortest wait between chat polls, in milliseconds (default 5000). Longer saves quota.
    pub min_poll_ms: Option<u64>,
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
    }
}
