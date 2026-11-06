//! The slash command table. Each command's run function lives in the file for its group.

mod local;
mod moderation;
mod nuke;

use crate::command::{CommandSpec, Invocation};
use crate::session::Submitted;
use crate::{Action, UserRef};

/// Every slash command, in the order autocomplete and `/help` list them.
pub const COMMANDS: &[CommandSpec] = &[
    CommandSpec {
        name: "ban",
        aliases: &[],
        usage: "/ban <user> [reason]",
        summary: "Ban a user",
        needs: Some(Action::Ban),
        run: moderation::ban,
    },
    CommandSpec {
        name: "timeout",
        aliases: &["to"],
        usage: "/timeout <user> [duration] [reason]",
        summary: "Time out a user (default 10m)",
        needs: Some(Action::Timeout),
        run: moderation::timeout,
    },
    CommandSpec {
        name: "unban",
        aliases: &["untimeout"],
        usage: "/unban <user>",
        summary: "Lift a ban or timeout",
        needs: Some(Action::Unban),
        run: moderation::unban,
    },
    CommandSpec {
        name: "nuke",
        aliases: &[],
        usage: "/nuke [platform] <phrase> [duration] [lookback]",
        summary: "Time out everyone who said a phrase recently (default 24h, last 5m)",
        needs: Some(Action::Timeout),
        run: nuke::nuke,
    },
    CommandSpec {
        name: "meganuke",
        aliases: &[],
        usage: "/meganuke [platform] <phrase> [lookback]",
        summary: "Ban everyone who said a phrase recently (last 5m)",
        needs: Some(Action::Ban),
        run: nuke::meganuke,
    },
    CommandSpec {
        name: "unnuke",
        aliases: &[],
        usage: "/unnuke",
        summary: "Lift the bans and timeouts of the last nuke",
        needs: Some(Action::Unban),
        run: nuke::unnuke,
    },
    CommandSpec {
        name: "poll",
        aliases: &[],
        usage: "/poll",
        summary: "Start a poll",
        needs: Some(Action::Poll),
        run: moderation::poll,
    },
    CommandSpec {
        name: "clear",
        aliases: &[],
        usage: "/clear",
        summary: "Clear Rill's view of the chat (local only)",
        needs: None,
        run: local::clear,
    },
    CommandSpec {
        name: "help",
        aliases: &["commands"],
        usage: "/help",
        summary: "List the commands",
        needs: None,
        run: local::help,
    },
];

/// The command's usage line, as a rejection that keeps the composer text.
fn usage(inv: &Invocation) -> Submitted {
    Submitted::Rejected(format!("Usage: {}", inv.spec.usage))
}

/// The first argument as a user (a leading `@` is dropped).
fn user_arg(inv: &Invocation) -> Option<UserRef> {
    inv.args
        .split_whitespace()
        .next()
        .map(|w| w.trim_start_matches('@'))
        .filter(|w| !w.is_empty())
        .map(|login| UserRef {
            id: None,
            login: login.to_string(),
        })
}

/// The arguments after the first `skip` words, if any.
fn rest(inv: &Invocation, skip: usize) -> Option<String> {
    let r = inv
        .args
        .split_whitespace()
        .skip(skip)
        .collect::<Vec<_>>()
        .join(" ");
    (!r.is_empty()).then_some(r)
}
