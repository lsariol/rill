//! The streaming platforms Rill connects to.

use std::fmt;

/// A streaming platform with a chat.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Platform {
    Twitch,
    YouTube,
    Kick,
}

impl Platform {
    /// Every platform, in the order the composer cycles through them.
    pub const ALL: [Platform; 3] = [Platform::Twitch, Platform::YouTube, Platform::Kick];

    /// The platform's display name.
    pub fn name(self) -> &'static str {
        match self {
            Platform::Twitch => "Twitch",
            Platform::YouTube => "YouTube",
            Platform::Kick => "Kick",
        }
    }
}

impl fmt::Display for Platform {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}
