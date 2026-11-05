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
