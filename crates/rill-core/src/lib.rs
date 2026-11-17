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
    Event, Level, ModAction, Moderation, Request, RequestId, SignInPrompt, UserRef,
};
pub use feed::{Entry, Feed, Filter, Item};
pub use message::{
    Author, Badge, ChatMessage, EmoteScope, EventKind, Fragment, MessageKind, Rank, ReplyRef,
    Roles, merge_text, replace_words, split_text,
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

/// Unix milliseconds of an RFC 3339 time, such as `2026-11-17T20:15:00.123Z`.
pub fn time_ms(text: &str) -> Option<i64> {
    text.parse::<jiff::Timestamp>()
        .ok()
        .map(|t| t.as_millisecond())
}

#[cfg(test)]
mod tests {
    use super::time_ms;

    #[test]
    fn reads_each_platforms_timestamps() {
        // 2026-11-06 06:00:00 UTC.
        let base = 1_793_944_800_000;
        // Rumble and YouTube: an offset.
        assert_eq!(time_ms("2026-11-06T06:00:00+00:00"), Some(base));
        assert_eq!(time_ms("2026-11-06T06:00:00.000000+00:00"), Some(base));
        assert_eq!(time_ms("2026-11-06T01:00:00-05:00"), Some(base));
        // Kick: Z.
        assert_eq!(time_ms("2026-11-06T06:00:00Z"), Some(base));
        // Twitch: nanoseconds, cut to milliseconds.
        assert_eq!(time_ms("2026-11-06T06:00:00.634234626Z"), Some(base + 634));
        assert_eq!(time_ms("2026-11-06T06:00:00.2Z"), Some(base + 200));
    }

    #[test]
    fn rejects_what_isnt_a_full_time() {
        assert_eq!(time_ms(""), None);
        assert_eq!(time_ms("soon"), None);
        // No offset: which moment it means is unknown.
        assert_eq!(time_ms("2026-11-06T06:00:00"), None);
        assert_eq!(time_ms("2026-11-06"), None);
    }
}
