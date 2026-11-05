//! Slash commands: parsing, autocomplete and the command table.

use crate::Action;

/// Whether a command runs in Rill or on a platform.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandKind {
    /// Handled by Rill itself; nothing is sent.
    Local,
    /// Sent to a platform's API; needs this capability there.
    Platform(Action),
}

/// One slash command.
#[derive(Debug, PartialEq, Eq)]
pub struct CommandSpec {
    pub name: &'static str,
    pub aliases: &'static [&'static str],
    pub usage: &'static str,
    pub summary: &'static str,
    pub kind: CommandKind,
}

/// Every slash command, in the order autocomplete lists them.
pub const COMMANDS: &[CommandSpec] = &[
    CommandSpec {
        name: "ban",
        aliases: &[],
        usage: "/ban <user> [reason]",
        summary: "Ban a user",
        kind: CommandKind::Platform(Action::Ban),
    },
    CommandSpec {
        name: "timeout",
        aliases: &["to"],
        usage: "/timeout <user> [duration] [reason]",
        summary: "Time out a user (default 10m)",
        kind: CommandKind::Platform(Action::Timeout),
    },
    CommandSpec {
        name: "unban",
        aliases: &["untimeout"],
        usage: "/unban <user>",
        summary: "Lift a ban or timeout",
        kind: CommandKind::Platform(Action::Unban),
    },
    CommandSpec {
        name: "poll",
        aliases: &[],
        usage: "/poll",
        summary: "Start a poll",
        kind: CommandKind::Platform(Action::Poll),
    },
    CommandSpec {
        name: "clear",
        aliases: &[],
        usage: "/clear",
        summary: "Clear Rill's view of the chat (local only)",
        kind: CommandKind::Local,
    },
    CommandSpec {
        name: "help",
        aliases: &["commands"],
        usage: "/help",
        summary: "List the commands",
        kind: CommandKind::Local,
    },
];

impl CommandSpec {
    /// Finds a command by name or alias, ignoring case.
    pub fn find(name: &str) -> Option<&'static CommandSpec> {
        let name = name.to_lowercase();
        COMMANDS
            .iter()
            .find(|c| c.name == name || c.aliases.contains(&name.as_str()))
    }
}

/// What the composer's text means.
#[derive(Debug, PartialEq, Eq)]
pub enum Parsed<'a> {
    Empty,
    /// A chat message (a leading `//` sends one literal `/`).
    Text(String),
    Command {
        spec: &'static CommandSpec,
        /// Everything after the command name, trimmed.
        args: &'a str,
    },
    Unknown(&'a str),
}

/// Parses composer text.
pub fn parse(input: &str) -> Parsed<'_> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Parsed::Empty;
    }
    if let Some(rest) = trimmed.strip_prefix("//") {
        return Parsed::Text(format!("/{rest}"));
    }
    let Some(rest) = trimmed.strip_prefix('/') else {
        return Parsed::Text(trimmed.to_string());
    };
    let (name, args) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
    match CommandSpec::find(name) {
        Some(spec) => Parsed::Command {
            spec,
            args: args.trim(),
        },
        None => Parsed::Unknown(name),
    }
}

/// Commands matching what's typed so far, while only the name is typed.
pub fn completions(input: &str) -> Vec<&'static CommandSpec> {
    let Some(prefix) = input.strip_prefix('/') else {
        return Vec::new();
    };
    if prefix.starts_with('/') || prefix.contains(char::is_whitespace) {
        return Vec::new();
    }
    let prefix = prefix.to_lowercase();
    COMMANDS
        .iter()
        .filter(|c| c.name.starts_with(&prefix) || c.aliases.iter().any(|a| a.starts_with(&prefix)))
        .collect()
}

/// The longest timeout platforms allow: 14 days.
pub const MAX_TIMEOUT_SECS: u32 = 14 * 24 * 3600;

/// Parses a duration like `30`, `30s`, `10m`, `1h`, `2d` or `1w` into seconds.
pub fn parse_duration(s: &str) -> Option<u32> {
    let s = s.trim().to_lowercase();
    let split = s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len());
    let (num, unit) = s.split_at(split);
    let n: u32 = num.parse().ok()?;
    let mult = match unit {
        "" | "s" => 1,
        "m" => 60,
        "h" => 3600,
        "d" => 86_400,
        "w" => 604_800,
        _ => return None,
    };
    let secs = n.checked_mul(mult)?;
    (1..=MAX_TIMEOUT_SECS).contains(&secs).then_some(secs)
}

/// Formats seconds as the largest whole unit, such as `10m`.
pub fn format_duration(secs: u32) -> String {
    for (unit, size) in [("w", 604_800), ("d", 86_400), ("h", 3600), ("m", 60)] {
        if secs >= size && secs % size == 0 {
            return format!("{}{unit}", secs / size);
        }
    }
    format!("{secs}s")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_text_commands_and_literals() {
        assert_eq!(parse("  "), Parsed::Empty);
        assert_eq!(parse("hello"), Parsed::Text("hello".into()));
        assert_eq!(parse("//me waves"), Parsed::Text("/me waves".into()));
        assert_eq!(parse("/nope x"), Parsed::Unknown("nope"));
        let Parsed::Command { spec, args } = parse("/TO bob 5m spam") else {
            panic!("expected a command");
        };
        assert_eq!((spec.name, args), ("timeout", "bob 5m spam"));
    }

    #[test]
    fn completes_only_the_name() {
        let names: Vec<_> = completions("/t").iter().map(|c| c.name).collect();
        assert_eq!(names, ["timeout"]);
        assert_eq!(completions("/").len(), COMMANDS.len());
        assert!(completions("/ban bob").is_empty());
        assert!(completions("hi").is_empty());
        assert!(completions("//").is_empty());
        let names: Vec<_> = completions("/un").iter().map(|c| c.name).collect();
        assert_eq!(names, ["unban"]);
    }

    #[test]
    fn durations() {
        assert_eq!(parse_duration("30"), Some(30));
        assert_eq!(parse_duration("10m"), Some(600));
        assert_eq!(parse_duration("1H"), Some(3600));
        assert_eq!(parse_duration("2w"), Some(1_209_600));
        assert_eq!(parse_duration("3w"), None);
        assert_eq!(parse_duration("0"), None);
        assert_eq!(parse_duration("5x"), None);
        assert_eq!(parse_duration("m"), None);
        assert_eq!(format_duration(600), "10m");
        assert_eq!(format_duration(90), "90s");
        assert_eq!(format_duration(86_400), "1d");
    }
}
