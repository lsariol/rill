//! Chat-wide commands: modes, announcements, shoutouts, purging one user, and search.

use super::{rest, usage, user_arg};
use crate::command::{Invocation, format_duration, parse_duration};
use crate::event::{Action, ChatMode, Command};
use crate::session::{Plan, Session, Submitted};

/// The first argument as on or off; `None` when it's something else.
fn switch(inv: &Invocation) -> Option<bool> {
    match inv
        .args
        .split_whitespace()
        .next()
        .map(str::to_lowercase)
        .as_deref()
    {
        None | Some("on") => Some(true),
        Some("off") => Some(false),
        Some(_) => None,
    }
}

fn mode(s: &mut Session, inv: &Invocation, mode: ChatMode, label: String) -> Submitted {
    let plan = Plan::Run {
        label,
        command: Command::SetMode { mode },
    };
    s.target(plan, Action::ChatMode, inv.destination)
}

/// The label for turning a mode on or off.
fn turned(name: &str, on: bool) -> String {
    format!("{name} {}", if on { "on" } else { "off" })
}

pub(super) fn slow(s: &mut Session, inv: &Invocation) -> Submitted {
    let arg = inv.args.split_whitespace().next();
    let secs = match arg {
        None => Some(30),
        Some(a) if a.eq_ignore_ascii_case("off") => None,
        Some(a) => match parse_duration(a) {
            Some(secs) if (3..=120).contains(&secs) => Some(secs),
            _ => return usage(inv),
        },
    };
    let label = match secs {
        Some(secs) => format!("Slow mode on ({})", format_duration(secs)),
        None => "Slow mode off".into(),
    };
    mode(s, inv, ChatMode::Slow(secs), label)
}

pub(super) fn followers(s: &mut Session, inv: &Invocation) -> Submitted {
    let arg = inv.args.split_whitespace().next();
    let minutes = match arg {
        None => Some(0),
        Some(a) if a.eq_ignore_ascii_case("off") => None,
        Some(a) if a.eq_ignore_ascii_case("on") => Some(0),
        // Twitch counts follower age in minutes, up to 3 months.
        Some(a) => match a
            .parse::<u32>()
            .ok()
            .or_else(|| parse_duration(a).map(|s| s / 60))
        {
            Some(m) if m <= 129_600 => Some(m),
            _ => return usage(inv),
        },
    };
    let label = match minutes {
        Some(0) => "Followers-only mode on".to_string(),
        Some(m) => format!(
            "Followers-only mode on (followed {})",
            format_duration(m * 60)
        ),
        None => "Followers-only mode off".to_string(),
    };
    mode(s, inv, ChatMode::Followers(minutes), label)
}

pub(super) fn subscribers(s: &mut Session, inv: &Invocation) -> Submitted {
    match switch(inv) {
        Some(on) => mode(
            s,
            inv,
            ChatMode::Subscribers(on),
            turned("Subscribers-only mode", on),
        ),
        None => usage(inv),
    }
}

pub(super) fn emote_only(s: &mut Session, inv: &Invocation) -> Submitted {
    match switch(inv) {
        Some(on) => mode(
            s,
            inv,
            ChatMode::EmoteOnly(on),
            turned("Emote-only mode", on),
        ),
        None => usage(inv),
    }
}

pub(super) fn unique_chat(s: &mut Session, inv: &Invocation) -> Submitted {
    match switch(inv) {
        Some(on) => mode(s, inv, ChatMode::UniqueChat(on), turned("Unique chat", on)),
        None => usage(inv),
    }
}

pub(super) fn announce(s: &mut Session, inv: &Invocation) -> Submitted {
    let Some(text) = rest(inv, 0) else {
        return usage(inv);
    };
    let plan = Plan::Run {
        label: "Announcement posted".into(),
        command: Command::Announce { text },
    };
    s.target(plan, Action::Announce, inv.destination)
}

pub(super) fn shoutout(s: &mut Session, inv: &Invocation) -> Submitted {
    let Some(user) = user_arg(inv) else {
        return usage(inv);
    };
    let plan = Plan::Run {
        label: format!("Shoutout to {}", user.login),
        command: Command::Shoutout { user },
    };
    s.target(plan, Action::Shoutout, inv.destination)
}

/// Clears someone's messages without keeping them out: a one-second timeout.
pub(super) fn purge(s: &mut Session, inv: &Invocation) -> Submitted {
    let Some(user) = user_arg(inv) else {
        return usage(inv);
    };
    let plan = Plan::Run {
        label: format!("Purged {}'s messages", user.login),
        command: Command::Timeout {
            user,
            seconds: 1,
            reason: rest(inv, 1).or_else(|| Some("purged".into())),
        },
    };
    s.target(plan, Action::Timeout, inv.destination)
}

pub(super) fn search(_: &mut Session, inv: &Invocation) -> Submitted {
    Submitted::Search(inv.args.to_string())
}

#[cfg(test)]
mod tests {
    use crate::{
        Account, Action, Capabilities, ChatMode, Command, ConnectionStatus, Destination, Event,
        Platform, Session, Submitted,
    };

    fn session() -> Session {
        let mut s = Session::new(10);
        let caps = Capabilities::of(&[
            Action::Send,
            Action::Timeout,
            Action::ChatMode,
            Action::Announce,
            Action::Shoutout,
        ]);
        s.apply(
            Event::Status {
                platform: Platform::Twitch,
                status: ConnectionStatus::Connected,
                account: Some(Account::default()),
                capabilities: caps,
            },
            0,
        );
        s.apply(
            Event::Status {
                platform: Platform::Kick,
                status: ConnectionStatus::Connected,
                account: None,
                capabilities: Capabilities::of(&[Action::Send, Action::Timeout]),
            },
            0,
        );
        s
    }

    /// The command the only (or chosen) platform would get.
    fn command(s: &mut Session, input: &str) -> Command {
        match s.submit(input, Destination::All, 0) {
            Submitted::Send(out) => out[0].request.command.clone(),
            other => panic!("{input}: {other:?}"),
        }
    }

    #[test]
    fn modes_run_where_they_can_without_asking() {
        let mut s = session();
        // Only Twitch has chat modes, so there's nothing to choose.
        let mode = |c: Command| match c {
            Command::SetMode { mode } => mode,
            c => panic!("{c:?}"),
        };
        assert_eq!(mode(command(&mut s, "/slow")), ChatMode::Slow(Some(30)));
        assert_eq!(mode(command(&mut s, "/slow 2m")), ChatMode::Slow(Some(120)));
        assert_eq!(mode(command(&mut s, "/slow off")), ChatMode::Slow(None));
        assert_eq!(
            mode(command(&mut s, "/followers")),
            ChatMode::Followers(Some(0))
        );
        assert_eq!(
            mode(command(&mut s, "/followers 10m")),
            ChatMode::Followers(Some(10))
        );
        assert_eq!(
            mode(command(&mut s, "/followers 1d")),
            ChatMode::Followers(Some(1440))
        );
        assert_eq!(
            mode(command(&mut s, "/followers off")),
            ChatMode::Followers(None)
        );
        assert_eq!(
            mode(command(&mut s, "/subs off")),
            ChatMode::Subscribers(false)
        );
        assert_eq!(
            mode(command(&mut s, "/emoteonly")),
            ChatMode::EmoteOnly(true)
        );
        assert_eq!(mode(command(&mut s, "/r9k")), ChatMode::UniqueChat(true));
        for bad in ["/slow 1s", "/slow 5m", "/subs maybe", "/followers soon"] {
            assert!(
                matches!(s.submit(bad, Destination::All, 0), Submitted::Rejected(_)),
                "{bad}"
            );
        }
        assert!(matches!(
            command(&mut s, "/announce Stream starts soon"),
            Command::Announce { text } if text == "Stream starts soon"
        ));
        assert!(matches!(
            command(&mut s, "/so @Friend"),
            Command::Shoutout { user } if user.login == "Friend"
        ));
    }

    #[test]
    fn purge_is_a_one_second_timeout_and_asks_where() {
        let mut s = session();
        let Submitted::NeedsTarget { plan, choices, .. } =
            s.submit("/purge spammer", Destination::All, 0)
        else {
            panic!("two platforms can time out");
        };
        assert_eq!(choices, [Platform::Twitch, Platform::Kick]);
        let Submitted::Send(out) = s.execute(Platform::Kick, plan) else {
            panic!()
        };
        assert!(matches!(
            &out[0].request.command,
            Command::Timeout { seconds: 1, user, .. } if user.login == "spammer"
        ));
    }

    #[test]
    fn search_opens_the_search_bar() {
        let mut s = session();
        assert_eq!(
            s.submit("/search buy followers", Destination::All, 0),
            Submitted::Search("buy followers".into())
        );
    }
}
