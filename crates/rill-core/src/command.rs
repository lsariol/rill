//! Slash commands: parsing, autocomplete and the command table ([`CommandSpec`]).

use crate::Action;
use crate::destination::Destination;
use crate::session::{Session, Submitted};

pub use crate::commands::COMMANDS;

/// What a command's run function gets.
#[derive(Debug, Clone, Copy)]
pub struct Invocation<'a> {
    pub spec: &'static CommandSpec,
    /// Everything after the command name, trimmed.
    pub args: &'a str,
    /// Where the composer sends right now.
    pub destination: Destination,
    pub now_ms: i64,
}

/// Runs a command.
pub type Run = fn(&mut Session, &Invocation) -> Submitted;

/// One slash command.
pub struct CommandSpec {
    pub name: &'static str,
    pub aliases: &'static [&'static str],
    pub usage: &'static str,
    pub summary: &'static str,
    /// The platform permission it needs; `None` runs inside Rill.
    pub needs: Option<Action>,
    pub run: Run,
}

impl std::fmt::Debug for CommandSpec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "/{}", self.name)
    }
}

impl PartialEq for CommandSpec {
    fn eq(&self, other: &Self) -> bool {
        self.name == other.name
    }
}

impl Eq for CommandSpec {}

impl CommandSpec {
    /// Finds a command by name or alias, ignoring case.
    pub fn find(name: &str) -> Option<&'static CommandSpec> {
        let name = name.to_lowercase();
        COMMANDS
            .iter()
            .find(|c| c.name == name || c.aliases.contains(&name.as_str()))
    }

    /// Whether it runs on a platform rather than inside Rill.
    pub fn is_platform(&self) -> bool {
        self.needs.is_some()
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
        if secs >= size && secs.is_multiple_of(size) {
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
        let names: Vec<_> = completions("/ti").iter().map(|c| c.name).collect();
        assert_eq!(names, ["timeout", "title"]);
        assert_eq!(completions("/").len(), COMMANDS.len());
        assert!(completions("/ban bob").is_empty());
        assert!(completions("hi").is_empty());
        assert!(completions("//").is_empty());
        let names: Vec<_> = completions("/un").iter().map(|c| c.name).collect();
        assert_eq!(names, ["unban", "unnuke", "uniquechat"]);
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
