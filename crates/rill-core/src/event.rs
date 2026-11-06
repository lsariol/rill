//! What adapters report to the core, and what the core asks adapters to do.

use std::fmt;

use crate::{ChatMessage, Platform};

/// An action a platform may support.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Action {
    Send,
    Reply,
    Delete,
    Timeout,
    Ban,
    Unban,
    Poll,
    /// Slow, followers-only, subscribers-only, emote-only and unique chat.
    ChatMode,
    Announce,
    Shoutout,
    /// The stream's title.
    Title,
    /// The stream's category (the game).
    Category,
    /// Starting an ad break.
    Ads,
}

impl Action {
    const fn bit(self) -> u16 {
        1 << self as u16
    }

    /// A short verb for messages, such as "ban".
    pub fn verb(self) -> &'static str {
        match self {
            Action::Send => "send",
            Action::Reply => "reply",
            Action::Delete => "delete messages",
            Action::Timeout => "time out users",
            Action::Ban => "ban users",
            Action::Unban => "unban users",
            Action::Poll => "run polls",
            Action::ChatMode => "change chat modes",
            Action::Announce => "post announcements",
            Action::Shoutout => "give shoutouts",
            Action::Title => "change the stream title",
            Action::Category => "change the stream category",
            Action::Ads => "start ads",
        }
    }
}

/// The set of actions a connected platform supports for the signed-in user.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Capabilities(u16);

impl Capabilities {
    /// Nothing supported (read-only).
    pub const NONE: Capabilities = Capabilities(0);

    /// A set built from a list of actions.
    pub const fn of(actions: &[Action]) -> Capabilities {
        let mut bits = 0;
        let mut i = 0;
        while i < actions.len() {
            bits |= actions[i].bit();
            i += 1;
        }
        Capabilities(bits)
    }

    pub fn has(self, action: Action) -> bool {
        self.0 & action.bit() != 0
    }
}

/// A platform connection's state.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum ConnectionStatus {
    #[default]
    Disconnected,
    Connecting,
    Connected,
    /// Lost the connection; trying again.
    Reconnecting {
        attempt: u32,
    },
    /// Stopped trying; the reason says how to fix it.
    Failed(String),
    /// Signed in, but there's nothing to read yet (YouTube before you go live); the reason says
    /// what it's waiting for.
    Waiting(String),
}

impl ConnectionStatus {
    /// A short label for the UI.
    pub fn label(&self) -> String {
        match self {
            ConnectionStatus::Disconnected => "Not connected".into(),
            ConnectionStatus::Connecting => "Connecting".into(),
            ConnectionStatus::Connected => "Connected".into(),
            ConnectionStatus::Reconnecting { attempt } => format!("Reconnecting (try {attempt})"),
            ConnectionStatus::Failed(why) => format!("Failed: {why}"),
            ConnectionStatus::Waiting(why) => why.clone(),
        }
    }
}

/// The signed-in account on a platform.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Account {
    pub id: String,
    pub login: String,
    pub display_name: String,
}

/// Identifies one request so its result can be matched to it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RequestId(pub u64);

/// A user to act on: the ID when known, else the login for the adapter to look up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserRef {
    pub id: Option<String>,
    pub login: String,
}

/// A chat restriction, turned on or off.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChatMode {
    /// Seconds between one user's messages; `None` turns slow mode off.
    Slow(Option<u32>),
    /// Minutes someone must have followed to chat; `None` turns it off.
    Followers(Option<u32>),
    Subscribers(bool),
    EmoteOnly(bool),
    /// No repeated messages.
    UniqueChat(bool),
}

/// Something the core asks an adapter to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Send {
        text: String,
        reply_to: Option<String>,
    },
    Delete {
        message_id: String,
    },
    Timeout {
        user: UserRef,
        seconds: u32,
        reason: Option<String>,
    },
    Ban {
        user: UserRef,
        reason: Option<String>,
    },
    Unban {
        user: UserRef,
    },
    Poll {
        title: String,
        choices: Vec<String>,
        seconds: u32,
    },
    SetMode {
        mode: ChatMode,
    },
    Announce {
        text: String,
    },
    Shoutout {
        user: UserRef,
    },
    SetTitle {
        title: String,
    },
    /// The category by name; the adapter finds the platform's match.
    SetCategory {
        name: String,
    },
    /// An ad break of about this length; each platform rounds to what it allows.
    StartAds {
        seconds: u32,
    },
    /// Start signing in (shows a code or opens the browser).
    SignIn,
    /// Forget the stored account and disconnect.
    SignOut,
    /// Read this channel (by its name on the platform) instead of the current one.
    Join {
        channel: String,
    },
}

impl Command {
    /// The capability this command needs; none for account commands.
    pub fn action(&self) -> Option<Action> {
        Some(match self {
            Command::Send {
                reply_to: Some(_), ..
            } => Action::Reply,
            Command::Send { .. } => Action::Send,
            Command::Delete { .. } => Action::Delete,
            Command::Timeout { .. } => Action::Timeout,
            Command::Ban { .. } => Action::Ban,
            Command::Unban { .. } => Action::Unban,
            Command::Poll { .. } => Action::Poll,
            Command::SetMode { .. } => Action::ChatMode,
            Command::Announce { .. } => Action::Announce,
            Command::Shoutout { .. } => Action::Shoutout,
            Command::SetTitle { .. } => Action::Title,
            Command::SetCategory { .. } => Action::Category,
            Command::StartAds { .. } => Action::Ads,
            Command::SignIn | Command::SignOut | Command::Join { .. } => return None,
        })
    }
}

/// A command with the ID its result will carry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    pub id: RequestId,
    pub command: Command,
}

/// What the user must do to finish signing in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignInPrompt {
    /// The page to open.
    pub url: String,
    /// The code to enter there, for device-code sign-in.
    pub code: Option<String>,
    /// When the code stops working, in Unix milliseconds.
    pub expires_at_ms: i64,
}

/// How serious a notice is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Info,
    Warn,
    Error,
    /// A channel event told as a notice, such as an ad break.
    Event,
}

/// Something an adapter reports.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    Status {
        platform: Platform,
        status: ConnectionStatus,
        account: Option<Account>,
        capabilities: Capabilities,
    },
    Message(ChatMessage),
    /// Signing in needs the user: show this prompt until the status changes.
    SignIn {
        platform: Platform,
        prompt: SignInPrompt,
    },
    Deleted {
        platform: Platform,
        message_id: String,
    },
    /// A user's messages were removed (timeout or ban).
    UserCleared {
        platform: Platform,
        user_id: String,
    },
    /// The whole chat was cleared.
    Cleared {
        platform: Platform,
    },
    Notice {
        platform: Platform,
        level: Level,
        text: String,
    },
    /// The result of a request: the new message ID for a send, if the platform gave one.
    Done {
        platform: Platform,
        request: RequestId,
        result: Result<Option<String>, String>,
    },
    /// The connection works but something needs the user: an older sign-in without newer
    /// permissions, for example. `None` clears it.
    Attention {
        platform: Platform,
        text: Option<String>,
    },
    /// Emotes the signed-in user can use on a platform, from one source; replaces that source's
    /// last list.
    Emotes {
        platform: Platform,
        source: EmoteSource,
        emotes: Vec<EmoteOption>,
    },
}

impl Event {
    /// The platform the event came from.
    pub fn platform(&self) -> Platform {
        match self {
            Event::Message(m) => m.platform,
            Event::Status { platform, .. }
            | Event::SignIn { platform, .. }
            | Event::Deleted { platform, .. }
            | Event::UserCleared { platform, .. }
            | Event::Cleared { platform, .. }
            | Event::Notice { platform, .. }
            | Event::Done { platform, .. }
            | Event::Attention { platform, .. }
            | Event::Emotes { platform, .. } => *platform,
        }
    }
}

/// Where a platform's usable emotes come from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum EmoteSource {
    /// The platform's own emotes.
    Platform,
    /// 7TV, BetterTTV and FrankerFaceZ.
    ThirdParty,
}

/// An emote the composer can offer: typing its name sends it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmoteOption {
    pub name: String,
    pub url: Option<String>,
    /// Pixels at 1x, when known.
    pub size: Option<(u16, u16)>,
}

impl fmt::Display for Action {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.verb())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capabilities_hold_their_actions() {
        let c = Capabilities::of(&[Action::Send, Action::Ban]);
        assert!(c.has(Action::Send) && c.has(Action::Ban));
        assert!(!c.has(Action::Poll));
        assert!(!Capabilities::NONE.has(Action::Send));
    }
}
