//! Platform-neutral chat core for Rill. Nothing here depends on the UI or the OS.

pub mod adapter;
pub mod backoff;
pub mod command;
pub mod destination;
pub mod event;
pub mod feed;
pub mod message;
pub mod platform;
pub mod session;

pub use adapter::{Adapter, AdapterIo, EventSink, Hub};
pub use destination::Destination;
pub use event::{
    Account, Action, Capabilities, Command, ConnectionStatus, Event, Level, Request, RequestId,
    UserRef,
};
pub use feed::{Entry, Feed, Filter, Item};
pub use message::{
    Author, Badge, ChatMessage, Fragment, MessageKind, ReplyRef, Roles, merge_text, split_text,
};
pub use platform::Platform;
pub use session::{Outgoing, Plan, Session, Submitted};

/// The current time in Unix milliseconds.
pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}
