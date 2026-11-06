//! The streaming platforms Rill connects to.

use std::fmt;

/// A streaming platform with a chat.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Platform {
    Twitch,
    YouTube,
    Kick,
    Rumble,
}

impl Platform {
    /// Every platform, in the order the composer cycles through them.
    pub const ALL: [Platform; 4] = [
        Platform::Twitch,
        Platform::YouTube,
        Platform::Kick,
        Platform::Rumble,
    ];

    /// The platform with this name, in any case.
    pub fn from_name(name: &str) -> Option<Platform> {
        Platform::ALL
            .into_iter()
            .find(|p| p.name().eq_ignore_ascii_case(name.trim()))
    }

    /// The platform's display name.
    pub fn name(self) -> &'static str {
        match self {
            Platform::Twitch => "Twitch",
            Platform::YouTube => "YouTube",
            Platform::Kick => "Kick",
            Platform::Rumble => "Rumble",
        }
    }
}

impl Platform {
    /// Platforms Rill can only read: nothing to sign in for beyond reading.
    pub fn is_read_only(self) -> bool {
        self == Platform::Rumble
    }

    /// The longest chat message the platform takes, in characters.
    pub fn max_message_chars(self) -> Option<usize> {
        match self {
            Platform::Twitch | Platform::Kick => Some(500),
            Platform::YouTube => Some(200),
            Platform::Rumble => None,
        }
    }
}

impl fmt::Display for Platform {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}
