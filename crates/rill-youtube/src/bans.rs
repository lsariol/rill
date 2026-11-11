//! `youtube-bans.txt`: the bans and timeouts Rill gave on YouTube, so they can be lifted after a restart.
//!
//! YouTube lifts a ban only by the ID it gave when banning, and has no way to look bans up.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// A ban Rill gave: YouTube's ID for it, and when a timeout ends.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Saved {
    pub ban: String,
    /// When a timeout ends, in Unix milliseconds; `None` for a ban.
    pub until_ms: Option<i64>,
}

pub fn path() -> PathBuf {
    rill_os::data_dir().join("youtube-bans.txt")
}

/// Reads the file: channel ID → ban, leaving out ended timeouts and lines it can't read.
pub fn load(path: &Path, now_ms: i64) -> HashMap<String, Saved> {
    std::fs::read_to_string(path)
        .map(|t| parse(&t, now_ms))
        .unwrap_or_default()
}

/// Writes the file; a failure is only logged, since it only matters for a later unban.
pub fn save(path: &Path, bans: &HashMap<String, Saved>) {
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Err(e) = std::fs::write(path, render(bans)) {
        tracing::warn!(err = %e, path = %path.display(), "YouTube bans not saved");
    }
}

/// One `channel<TAB>ban<TAB>until` line each, `until` being `ban` or a timeout's end in Unix ms.
fn parse(text: &str, now_ms: i64) -> HashMap<String, Saved> {
    text.lines()
        .filter_map(|line| {
            let mut parts = line.split('\t');
            let (channel, ban, until) = (parts.next()?, parts.next()?, parts.next()?);
            let until_ms = match until {
                "ban" => None,
                t => Some(t.parse().ok()?),
            };
            let current = until_ms.is_none_or(|u| u > now_ms);
            (current && !channel.is_empty() && !ban.is_empty()).then(|| {
                (
                    channel.to_string(),
                    Saved {
                        ban: ban.to_string(),
                        until_ms,
                    },
                )
            })
        })
        .collect()
}

fn render(bans: &HashMap<String, Saved>) -> String {
    let mut lines: Vec<String> = bans
        .iter()
        .map(|(channel, s)| {
            let until = s.until_ms.map_or("ban".to_string(), |u| u.to_string());
            format!("{channel}\t{}\t{until}\n", s.ban)
        })
        .collect();
    lines.sort();
    lines.concat()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_what_it_writes_and_drops_ended_timeouts() {
        let bans = HashMap::from([
            (
                "UCa".to_string(),
                Saved {
                    ban: "b1".into(),
                    until_ms: None,
                },
            ),
            (
                "UCb".to_string(),
                Saved {
                    ban: "b2".into(),
                    until_ms: Some(2_000),
                },
            ),
            (
                "UCc".to_string(),
                Saved {
                    ban: "b3".into(),
                    until_ms: Some(500),
                },
            ),
        ]);
        let back = parse(&format!("{}odd line\n", render(&bans)), 1_000);
        assert_eq!(
            back.len(),
            2,
            "the ended timeout and the odd line are left out"
        );
        assert_eq!(back["UCa"], bans["UCa"]);
        assert_eq!(back["UCb"], bans["UCb"]);
    }
}
