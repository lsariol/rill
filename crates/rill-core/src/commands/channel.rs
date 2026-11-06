//! `/set title`, `/set category` (and `/title`, `/category`): change the stream's details on every
//! platform that can, or only the composer's destination, with one summary line.

use super::usage;
use crate::command::Invocation;
use crate::event::{Action, Command};
use crate::session::{Batch, Session, Submitted};

/// `/set title <text>` or `/set category <name>`.
pub(super) fn set(s: &mut Session, inv: &Invocation) -> Submitted {
    let (what, rest) = inv
        .args
        .split_once(char::is_whitespace)
        .unwrap_or((inv.args, ""));
    match what.to_lowercase().as_str() {
        "title" => apply(s, inv, Field::Title, rest.trim()),
        "category" | "game" => apply(s, inv, Field::Category, rest.trim()),
        _ => usage(inv),
    }
}

pub(super) fn title(s: &mut Session, inv: &Invocation) -> Submitted {
    apply(s, inv, Field::Title, inv.args)
}

pub(super) fn category(s: &mut Session, inv: &Invocation) -> Submitted {
    apply(s, inv, Field::Category, inv.args)
}

/// `/ad [length]`: an ad break on every platform that can (or the destination), 60 s by default.
pub(super) fn ad(s: &mut Session, inv: &Invocation) -> Submitted {
    let seconds = match inv.args.split_whitespace().next() {
        None => 60,
        Some(a) => match crate::command::parse_duration(a) {
            Some(secs) if secs <= 600 => secs,
            _ => return usage(inv),
        },
    };
    let connected = s.connected();
    let targets: Vec<_> = inv
        .destination
        .targets(&connected)
        .into_iter()
        .filter(|p| s.can(*p, Action::Ads))
        .collect();
    if targets.is_empty() {
        return Submitted::Rejected(
            "No connected platform can start ads with this account (it takes the broadcaster)."
                .into(),
        );
    }
    let names: Vec<_> = targets.iter().map(|p| p.name()).collect();
    let batch = Batch {
        label: format!(
            "Ad break requested: {} on {}",
            crate::command::format_duration(seconds),
            names.join(", ")
        ),
        items: targets
            .into_iter()
            .map(|p| (p, Command::StartAds { seconds }))
            .collect(),
        undoable: false,
    };
    s.run_batch(batch, inv.now_ms)
}

#[derive(Clone, Copy)]
enum Field {
    Title,
    Category,
}

fn apply(s: &mut Session, inv: &Invocation, field: Field, value: &str) -> Submitted {
    if value.is_empty() {
        return usage(inv);
    }
    let action = match field {
        Field::Title => Action::Title,
        Field::Category => Action::Category,
    };
    let connected = s.connected();
    let targets: Vec<_> = inv
        .destination
        .targets(&connected)
        .into_iter()
        .filter(|p| s.can(*p, action))
        .collect();
    if targets.is_empty() {
        return Submitted::Rejected(format!(
            "No connected platform can {action} with this account."
        ));
    }
    let names: Vec<_> = targets.iter().map(|p| p.name()).collect();
    let (command, label) = match field {
        Field::Title => (
            Command::SetTitle {
                title: value.to_string(),
            },
            format!("Title set to \"{value}\" on {}", names.join(", ")),
        ),
        Field::Category => (
            Command::SetCategory {
                name: value.to_string(),
            },
            format!("Category set to \"{value}\" on {}", names.join(", ")),
        ),
    };
    let batch = Batch {
        label,
        items: targets.into_iter().map(|p| (p, command.clone())).collect(),
        undoable: false,
    };
    s.run_batch(batch, inv.now_ms)
}

#[cfg(test)]
mod tests {
    use crate::{
        Account, Action, Capabilities, Command, ConnectionStatus, Destination, Event, Platform,
        Session, Submitted,
    };

    fn session() -> Session {
        let mut s = Session::new(10);
        let mut connect = |p, caps| {
            s.apply(
                Event::Status {
                    platform: p,
                    status: ConnectionStatus::Connected,
                    account: Some(Account::default()),
                    capabilities: caps,
                },
                0,
            );
        };
        connect(
            Platform::Twitch,
            Capabilities::of(&[Action::Title, Action::Category]),
        );
        connect(
            Platform::YouTube,
            Capabilities::of(&[Action::Title, Action::Ads]),
        );
        connect(Platform::Rumble, Capabilities::NONE);
        s
    }

    fn sent(s: Submitted) -> Vec<(Platform, Command)> {
        match s {
            Submitted::Send(out) => out
                .into_iter()
                .map(|o| (o.platform, o.request.command))
                .collect(),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn titles_go_everywhere_they_can_and_categories_where_they_exist() {
        let mut s = session();
        let title = |t: &str| Command::SetTitle { title: t.into() };
        assert_eq!(
            sent(s.submit("/set title Ranked grind", Destination::All, 0)),
            [
                (Platform::Twitch, title("Ranked grind")),
                (Platform::YouTube, title("Ranked grind"))
            ]
        );
        assert_eq!(
            sent(s.submit("/title Chill", Destination::One(Platform::YouTube), 0)),
            [(Platform::YouTube, title("Chill"))]
        );
        assert_eq!(
            sent(s.submit("/set category Elden Ring", Destination::All, 0)),
            [(
                Platform::Twitch,
                Command::SetCategory {
                    name: "Elden Ring".into()
                }
            )]
        );
        assert_eq!(
            sent(s.submit("/ad 90s", Destination::All, 0)),
            [(Platform::YouTube, Command::StartAds { seconds: 90 })]
        );
        assert_eq!(
            sent(s.submit("/ad", Destination::All, 0)),
            [(Platform::YouTube, Command::StartAds { seconds: 60 })]
        );
        for bad in [
            "/set",
            "/set colour red",
            "/title",
            "/category   ",
            "/ad soon",
            "/ad 2h",
        ] {
            assert!(
                matches!(s.submit(bad, Destination::All, 0), Submitted::Rejected(_)),
                "{bad}"
            );
        }
    }
}
