//! Platform-neutral chat core for Rill. Nothing here depends on the UI or the OS.

pub mod adapter;
pub mod backoff;
pub mod chatters;
pub mod command;
mod commands;
pub mod destination;
pub mod event;
pub mod feed;
pub mod message;
pub mod platform;
pub mod session;

pub use adapter::{Adapter, AdapterIo, Enrich, EventSink, Hub};
pub use destination::Destination;
pub use event::{
    Account, Action, Capabilities, ChatMode, Command, ConnectionStatus, EmoteOption, EmoteSource,
    Event, Level, Request, RequestId, SignInPrompt, UserRef,
};
pub use feed::{Entry, Feed, Filter, Item};
pub use message::{
    Author, Badge, ChatMessage, EmoteScope, EventKind, Fragment, MessageKind, ReplyRef, Roles,
    merge_text, replace_words, split_text,
};
pub use platform::Platform;
pub use session::{Health, Outgoing, Plan, Session, Submitted};

/// Rill's website: downloads, updates, and the pages a sign-in ends on.
pub const WEBSITE: &str = "https://rill.mobasity.com";

/// The reply to a sign-in's local redirect: on to the website's page for how it ended.
pub fn sign_in_reply(platform: &str, ok: bool) -> String {
    let page = if ok { "signed-in" } else { "sign-in-failed" };
    format!(
        "HTTP/1.1 303 See Other\r\nLocation: {WEBSITE}/{page}/{platform}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
    )
}

/// The current time in Unix milliseconds.
pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}
