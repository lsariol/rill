//! `/nuke`, `/meganuke` and `/unnuke`: act on everyone who said a phrase recently, such as a bot
//! raid, after one confirmation.

use std::collections::{BTreeMap, HashSet};

use super::usage;
use crate::command::{Invocation, format_duration, parse_duration};
use crate::event::{Action, Command};
use crate::feed::Item;
use crate::session::{Batch, Session, Submitted};
use crate::{Platform, UserRef};

/// How far back a nuke looks unless told otherwise.
const LOOKBACK_SECS: u32 = 5 * 60;
/// How long `/nuke` times out unless told otherwise.
const TIMEOUT_SECS: u32 = 24 * 3600;
/// The most users one nuke acts on per platform.
const MAX_USERS: usize = 1_000;

/// A nuke's arguments.
#[derive(Debug, PartialEq, Eq)]
struct Args {
    platform: Option<Platform>,
    phrase: String,
    /// The timeout; `None` for a ban.
    seconds: Option<u32>,
    lookback: u32,
}

fn platform_word(w: &str) -> Option<Platform> {
    match w.to_lowercase().as_str() {
        "twitch" => Some(Platform::Twitch),
        "youtube" | "yt" => Some(Platform::YouTube),
        "kick" => Some(Platform::Kick),
        _ => None,
    }
}

/// A duration with a unit (`48h`), so a bare number stays part of the phrase.
fn unit_duration(w: &str) -> Option<u32> {
    w.ends_with(|c: char| c.is_ascii_alphabetic())
        .then(|| parse_duration(w))
        .flatten()
}

/// Reads `[platform] <phrase> [duration] [lookback]`; a ban takes only a lookback.
/// A phrase in quotes may contain anything, including a platform name or a duration.
fn parse(args: &str, timed: bool) -> Option<Args> {
    let mut rest = args.trim();
    let mut platform = None;
    if let Some((first, after)) = rest.split_once(char::is_whitespace)
        && let Some(p) = platform_word(first)
    {
        platform = Some(p);
        rest = after.trim_start();
    }
    let (quoted, mut words): (Option<String>, Vec<&str>) = match rest.strip_prefix('"') {
        Some(q) => {
            let end = q.find('"')?;
            (
                Some(q[..end].to_string()),
                q[end + 1..].split_whitespace().collect(),
            )
        }
        None => (None, rest.split_whitespace().collect()),
    };
    // Durations come off the end; an unquoted phrase keeps at least one word.
    let keep = usize::from(quoted.is_none());
    let mut durations = Vec::new();
    while durations.len() < if timed { 2 } else { 1 } && words.len() > keep {
        let Some(d) = words.last().and_then(|w| unit_duration(w)) else {
            break;
        };
        words.pop();
        durations.insert(0, d);
    }
    let phrase = match quoted {
        Some(q) if words.is_empty() => q,
        Some(_) => return None,
        None => words.join(" "),
    };
    if phrase.trim().is_empty() {
        return None;
    }
    let (seconds, lookback) = match (timed, durations.as_slice()) {
        (true, [d, back]) => (Some(*d), *back),
        (true, [d]) => (Some(*d), LOOKBACK_SECS),
        (true, _) => (Some(TIMEOUT_SECS), LOOKBACK_SECS),
        (false, [back]) => (None, *back),
        (false, _) => (None, LOOKBACK_SECS),
    };
    Some(Args {
        platform,
        phrase,
        seconds,
        lookback,
    })
}

pub(super) fn nuke(s: &mut Session, inv: &Invocation) -> Submitted {
    match parse(inv.args, true) {
        Some(args) => plan(s, inv, args),
        None => usage(inv),
    }
}

pub(super) fn meganuke(s: &mut Session, inv: &Invocation) -> Submitted {
    match parse(inv.args, false) {
        Some(args) => plan(s, inv, args),
        None => usage(inv),
    }
}

/// Finds who said the phrase and asks before acting on them.
fn plan(s: &mut Session, inv: &Invocation, args: Args) -> Submitted {
    let action = if args.seconds.is_some() {
        Action::Timeout
    } else {
        Action::Ban
    };
    let connected = s.connected();
    let platforms: Vec<Platform> = match args.platform {
        Some(p) if !connected.contains(&p) => {
            return Submitted::Rejected(format!("{p} isn't connected."));
        }
        Some(p) if !s.can(p, action) => {
            return Submitted::Rejected(format!("{p} can't {action} with this account."));
        }
        Some(p) => vec![p],
        None => connected,
    };
    let since = inv.now_ms - i64::from(args.lookback) * 1000;
    let found = said(s, &platforms, &args.phrase, since);
    let window = format_duration(args.lookback);
    if found.is_empty() {
        return Submitted::Rejected(format!(
            "No one said \"{}\" in the last {window}. Add a longer lookback, such as /{} {} 30m.",
            args.phrase,
            inv.spec.name,
            if args.seconds.is_some() {
                "<phrase> 24h"
            } else {
                "<phrase>"
            },
        ));
    }
    let mut items = Vec::new();
    let mut counts = Vec::new();
    let mut skipped = Vec::new();
    for (platform, users) in found {
        if !s.can(platform, action) {
            skipped.push(platform.name());
            continue;
        }
        counts.push(format!("{platform} {}", users.len().min(MAX_USERS)));
        let reason = Some(format!("Rill nuke: {}", args.phrase));
        for user in users.into_iter().take(MAX_USERS) {
            items.push((
                platform,
                match args.seconds {
                    Some(seconds) => Command::Timeout {
                        user,
                        seconds,
                        reason: reason.clone(),
                    },
                    None => Command::Ban {
                        user,
                        reason: reason.clone(),
                    },
                },
            ));
        }
    }
    if items.is_empty() {
        return Submitted::Rejected(format!(
            "Only {} had matches, and this account can't {action} there.",
            skipped.join(" and ")
        ));
    }
    let n = items.len();
    let users = if n == 1 {
        "1 user".to_string()
    } else {
        format!("{n} users")
    };
    let (ask, done) = match args.seconds {
        Some(secs) => (
            format!("Time out {users} for {}", format_duration(secs)),
            format!("Timed out {users} for {}", format_duration(secs)),
        ),
        None => (format!("Ban {users}"), format!("Banned {users}")),
    };
    let mut prompt = format!(
        "{ask} who said \"{}\" in the last {window}? {}.",
        args.phrase,
        counts.join(", ")
    );
    if !skipped.is_empty() {
        prompt.push_str(&format!(
            " Skipped {}: no permission.",
            skipped.join(" and ")
        ));
    }
    Submitted::Confirm {
        prompt,
        batch: Batch {
            label: format!("{done} who said \"{}\"", args.phrase),
            items,
            undoable: true,
        },
    }
}

/// Everyone who said `phrase` (any case) since `since_ms`, per platform, leaving out the
/// broadcaster, moderators, VIPs and the signed-in account.
fn said(
    s: &Session,
    platforms: &[Platform],
    phrase: &str,
    since_ms: i64,
) -> BTreeMap<Platform, Vec<UserRef>> {
    let phrase = phrase.to_lowercase();
    let mut seen = HashSet::new();
    let mut out: BTreeMap<Platform, Vec<UserRef>> = BTreeMap::new();
    for e in s.feed.entries() {
        let Item::Chat(m) = &e.item else { continue };
        let a = &m.author;
        let me = s
            .link(m.platform)
            .and_then(|l| l.account.as_ref())
            .is_some_and(|acc| acc.id == a.id);
        if e.received_ms < since_ms
            || !platforms.contains(&m.platform)
            || a.roles.broadcaster
            || a.roles.moderator
            || a.roles.vip
            || me
            || !m.plain_text().to_lowercase().contains(&phrase)
            || !seen.insert((m.platform, a.id.clone()))
        {
            continue;
        }
        out.entry(m.platform).or_default().push(UserRef {
            id: Some(a.id.clone()),
            login: a.login.clone(),
        });
    }
    out
}

pub(super) fn unnuke(s: &mut Session, _: &Invocation) -> Submitted {
    if s.last_nuke.is_empty() {
        return Submitted::Rejected("There's no nuke to undo in this session.".into());
    }
    let mut items = Vec::new();
    let mut skipped = HashSet::new();
    for (platform, user) in s.last_nuke.clone() {
        if s.connected().contains(&platform) && s.can(platform, Action::Unban) {
            items.push((platform, Command::Unban { user }));
        } else {
            skipped.insert(platform.name());
        }
    }
    if items.is_empty() {
        return Submitted::Rejected(
            "The platforms of the last nuke aren't connected, or this account can't unban there."
                .into(),
        );
    }
    let n = items.len();
    let users = if n == 1 {
        "1 user".to_string()
    } else {
        format!("{n} users")
    };
    let mut prompt = format!("Lift the bans and timeouts of the last nuke for {users}?");
    if !skipped.is_empty() {
        let mut names: Vec<_> = skipped.into_iter().collect();
        names.sort_unstable();
        prompt.push_str(&format!(
            " Skipped {}: not connected or no permission.",
            names.join(" and ")
        ));
    }
    Submitted::Confirm {
        prompt,
        batch: Batch {
            label: format!("Lifted the last nuke for {users}"),
            items,
            undoable: false,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(platform: Option<Platform>, phrase: &str, seconds: Option<u32>, lookback: u32) -> Args {
        Args {
            platform,
            phrase: phrase.into(),
            seconds,
            lookback,
        }
    }

    #[test]
    fn parses_platform_phrase_and_durations() {
        let day = Some(TIMEOUT_SECS);
        assert_eq!(parse("haha", true), Some(args(None, "haha", day, 300)));
        assert_eq!(
            parse("haha 48h", true),
            Some(args(None, "haha", Some(48 * 3600), 300))
        );
        assert_eq!(
            parse("twitch buy followers 1h 10m", true),
            Some(args(
                Some(Platform::Twitch),
                "buy followers",
                Some(3600),
                600
            ))
        );
        assert_eq!(
            parse("Kick haha", false),
            Some(args(Some(Platform::Kick), "haha", None, 300))
        );
        assert_eq!(
            parse("haha 10m", false),
            Some(args(None, "haha", None, 600)),
            "a ban takes only a lookback"
        );
        // A lone word stays the phrase, even when it looks like something else.
        assert_eq!(parse("twitch", true), Some(args(None, "twitch", day, 300)));
        assert_eq!(parse("10m", true), Some(args(None, "10m", day, 300)));
        assert_eq!(parse("gg 2", true), Some(args(None, "gg 2", day, 300)));
        assert_eq!(
            parse("\"kick rocks 5m\" 1h", true),
            Some(args(None, "kick rocks 5m", Some(3600), 300))
        );
        assert_eq!(parse("", true), None);
        assert_eq!(parse("twitch \"\"", true), None);
        assert_eq!(parse("\"open", true), None);
    }
}
