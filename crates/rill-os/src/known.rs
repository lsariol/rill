//! `known-chatters.txt` in the data folder: who has chatted in the user's channels before, for
//! first-time chatter highlights. One line per user (`twitch<TAB>12345`), plus when Rill
//! started watching each platform (`since<TAB>twitch<TAB>1731100000000`). Lines are only added.

use std::io::Write;
use std::path::PathBuf;

use crate::data_dir;

/// What the file holds, by platform name.
#[derive(Debug, Default, PartialEq)]
pub struct Known {
    /// When Rill started watching each platform, in Unix milliseconds.
    pub since: Vec<(String, i64)>,
    /// Users who have chatted, by platform and user ID.
    pub ids: Vec<(String, String)>,
}

pub fn path() -> PathBuf {
    data_dir().join("known-chatters.txt")
}

/// Reads the file; a missing one is empty, and lines it can't read are skipped.
pub fn load() -> Result<Known, String> {
    let text = match std::fs::read_to_string(path()) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Known::default()),
        Err(e) => {
            return Err(format!(
                "Known chatters: can't read {} ({e}). First-time chatter highlights start over.",
                path().display()
            ));
        }
    };
    Ok(parse(&text))
}

fn parse(text: &str) -> Known {
    let mut known = Known::default();
    for line in text.lines() {
        let parts: Vec<&str> = line.split('\t').collect();
        match parts.as_slice() {
            ["since", platform, ms] => {
                if let Ok(ms) = ms.parse() {
                    known.since.push((platform.to_string(), ms));
                }
            }
            [platform, id] if !id.is_empty() => {
                known.ids.push((platform.to_string(), id.to_string()));
            }
            _ => {}
        }
    }
    known
}

/// The file's size in bytes, or `None` when there's no file.
pub fn size() -> Option<u64> {
    std::fs::metadata(path()).ok().map(|m| m.len())
}

/// Deletes the file; a missing one is already deleted.
pub fn delete() -> Result<(), String> {
    match std::fs::remove_file(path()) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(format!(
            "Known chatters: can't delete {} ({e}). Close anything that has it open and try again.",
            path().display()
        )),
        _ => Ok(()),
    }
}

/// Adds lines to the end of the file, creating it if needed.
pub fn append(known: &Known) -> Result<(), String> {
    if known.since.is_empty() && known.ids.is_empty() {
        return Ok(());
    }
    let mut text = String::new();
    for (platform, ms) in &known.since {
        text.push_str(&format!("since\t{platform}\t{ms}\n"));
    }
    for (platform, id) in &known.ids {
        // IDs never hold tabs or newlines; skip any that would break the file.
        if !id.contains(['\t', '\n', '\r']) {
            text.push_str(&format!("{platform}\t{id}\n"));
        }
    }
    let path = path();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .and_then(|mut f| f.write_all(text.as_bytes()))
        .map_err(|e| {
            format!(
                "Known chatters: can't write {} ({e}). Check the folder's permissions.",
                path.display()
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_what_it_writes_and_skips_odd_lines() {
        let text = "since\ttwitch\t1700\ntwitch\t42\nkick\tname:bob\nnonsense\nsince\tkick\tx\n";
        let k = parse(text);
        assert_eq!(k.since, [("twitch".to_string(), 1700)]);
        assert_eq!(
            k.ids,
            [
                ("twitch".to_string(), "42".to_string()),
                ("kick".to_string(), "name:bob".to_string())
            ]
        );
    }
}
