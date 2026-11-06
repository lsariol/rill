//! Commands that run inside Rill: nothing is sent to a platform.

use super::COMMANDS;
use crate::command::Invocation;
use crate::event::Level;
use crate::session::{Session, Submitted};

pub(super) fn clear(s: &mut Session, _: &Invocation) -> Submitted {
    s.feed.clear();
    Submitted::Local
}

pub(super) fn help(s: &mut Session, inv: &Invocation) -> Submitted {
    let mut out = String::from("Commands:");
    for c in COMMANDS {
        out.push_str(&format!("\n{}  {}", c.usage, c.summary));
    }
    out.push_str("\nStart a message with // to send a literal slash.");
    s.notice(None, Level::Info, &out, inv.now_ms);
    Submitted::Local
}
