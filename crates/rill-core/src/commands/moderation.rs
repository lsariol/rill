//! One-user moderation commands, and opening the poll form.

use super::{rest, usage, user_arg};
use crate::command::{Invocation, format_duration, parse_duration};
use crate::event::{Action, Command};
use crate::session::{Plan, Session, Submitted};

pub(super) fn ban(s: &mut Session, inv: &Invocation) -> Submitted {
    let Some(user) = user_arg(inv) else {
        return usage(inv);
    };
    let plan = Plan::Run {
        label: format!("Ban {}", user.login),
        command: Command::Ban {
            user,
            reason: rest(inv, 1),
        },
    };
    s.target(plan, Action::Ban, inv.destination)
}

pub(super) fn unban(s: &mut Session, inv: &Invocation) -> Submitted {
    let Some(user) = user_arg(inv) else {
        return usage(inv);
    };
    let plan = Plan::Run {
        label: format!("Unban {}", user.login),
        command: Command::Unban { user },
    };
    s.target(plan, Action::Unban, inv.destination)
}

pub(super) fn timeout(s: &mut Session, inv: &Invocation) -> Submitted {
    let Some(user) = user_arg(inv) else {
        return usage(inv);
    };
    let words: Vec<&str> = inv.args.split_whitespace().collect();
    let (seconds, skip) = match words.get(1).map(|w| parse_duration(w)) {
        Some(Some(secs)) => (secs, 2),
        // A lone second word must be a duration; with more words it starts the reason.
        Some(None) if words.len() == 2 => return usage(inv),
        _ => (600, 1),
    };
    let plan = Plan::Run {
        label: format!("Time out {} for {}", user.login, format_duration(seconds)),
        command: Command::Timeout {
            user,
            seconds,
            reason: rest(inv, skip),
        },
    };
    s.target(plan, Action::Timeout, inv.destination)
}

pub(super) fn poll(s: &mut Session, inv: &Invocation) -> Submitted {
    s.target(Plan::PollDialog, Action::Poll, inv.destination)
}
