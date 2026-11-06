//! The slash command table. Each command's run function lives in the file for its group.

mod channel;
mod chat;
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
        name: "purge",
        aliases: &[],
        usage: "/purge <user> [reason]",
        summary: "Clear a user's messages (a 1-second timeout)",
        needs: Some(Action::Timeout),
        run: chat::purge,
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
        name: "slow",
        aliases: &["slowmode"],
        usage: "/slow [3s-2m|off]",
        summary: "Slow mode: seconds between each user's messages (default 30s)",
        needs: Some(Action::ChatMode),
        run: chat::slow,
    },
    CommandSpec {
        name: "followers",
        aliases: &["followersonly"],
        usage: "/followers [followed for, e.g. 10m|off]",
        summary: "Followers-only chat",
        needs: Some(Action::ChatMode),
        run: chat::followers,
    },
    CommandSpec {
        name: "subscribers",
        aliases: &["subs", "subsonly"],
        usage: "/subscribers [on|off]",
        summary: "Subscribers-only chat",
        needs: Some(Action::ChatMode),
        run: chat::subscribers,
    },
    CommandSpec {
        name: "emoteonly",
        aliases: &["emotes"],
        usage: "/emoteonly [on|off]",
        summary: "Emote-only chat",
        needs: Some(Action::ChatMode),
        run: chat::emote_only,
    },
    CommandSpec {
        name: "uniquechat",
        aliases: &["r9k"],
        usage: "/uniquechat [on|off]",
        summary: "No repeated messages",
        needs: Some(Action::ChatMode),
        run: chat::unique_chat,
    },
    CommandSpec {
        name: "announce",
        aliases: &[],
        usage: "/announce <message>",
        summary: "Post a highlighted announcement",
        needs: Some(Action::Announce),
        run: chat::announce,
    },
    CommandSpec {
        name: "shoutout",
        aliases: &["so"],
        usage: "/shoutout <user>",
        summary: "Give another streamer a shoutout",
        needs: Some(Action::Shoutout),
        run: chat::shoutout,
    },
    CommandSpec {
        name: "set",
        aliases: &[],
        usage: "/set title <text> | /set category <name>",
        summary: "Change the stream's title or category everywhere you can",
        needs: Some(Action::Title),
        run: channel::set,
    },
    CommandSpec {
        name: "title",
        aliases: &[],
        usage: "/title <text>",
        summary: "Change the stream title",
        needs: Some(Action::Title),
        run: channel::title,
    },
    CommandSpec {
        name: "category",
        aliases: &["game"],
        usage: "/category <name>",
        summary: "Change the stream category (Twitch and Kick)",
        needs: Some(Action::Category),
        run: channel::category,
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
        name: "search",
        aliases: &["find"],
        usage: "/search [text]",
        summary: "Show only messages containing the text (Ctrl+F)",
        needs: None,
        run: chat::search,
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
