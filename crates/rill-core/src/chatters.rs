//! Names seen in chat this session, for `@` mentions and Tab completion.

use std::collections::HashMap;

use crate::{Author, Platform};

/// The most names kept in all; the ones seen longest ago go first.
const CAP: usize = 20_000;

/// Everyone who has chatted, per platform, by the name a mention uses.
#[derive(Debug, Default)]
pub struct Chatters {
    /// By platform and lowercase name: the name as written, and when it was last seen.
    names: HashMap<(Platform, String), (String, u64)>,
    tick: u64,
}

impl Chatters {
    /// Notes that the author chatted on the platform.
    pub fn seen(&mut self, platform: Platform, author: &Author) {
        let name = mention_name(author);
        if name.is_empty() {
            return;
        }
        self.tick += 1;
        self.names.insert(
            (platform, name.to_lowercase()),
            (name.to_string(), self.tick),
        );
        if self.names.len() > CAP + CAP / 10 {
            self.trim();
        }
    }

    /// Drops a platform's names, when it's turned off.
    pub fn forget(&mut self, platform: Platform) {
        self.names.retain(|(p, _), _| *p != platform);
    }

    /// Names starting with `prefix`, any case, alphabetically, with their platforms.
    pub fn starting_with(
        &self,
        platforms: &[Platform],
        prefix: &str,
        limit: usize,
    ) -> Vec<(Vec<Platform>, String)> {
        let prefix = prefix.to_lowercase();
        let mut found: Vec<(&str, Platform, &str)> = self
            .names
            .iter()
            .filter(|((p, lower), _)| platforms.contains(p) && lower.starts_with(&prefix))
            .map(|((p, lower), (name, _))| (lower.as_str(), *p, name.as_str()))
            .collect();
        found.sort_by(|a, b| (a.0, a.2, a.1).cmp(&(b.0, b.2, b.1)));
        let mut out: Vec<(Vec<Platform>, String)> = Vec::new();
        let mut last = "";
        for (lower, p, name) in found {
            if lower == last
                && let Some((ps, _)) = out.last_mut()
            {
                ps.push(p);
                continue;
            }
            if out.len() >= limit {
                break;
            }
            last = lower;
            out.push((vec![p], name.to_string()));
        }
        for (ps, _) in &mut out {
            ps.sort();
        }
        out
    }

    /// Keeps the `CAP` names seen most recently.
    fn trim(&mut self) {
        let mut ticks: Vec<u64> = self.names.values().map(|(_, t)| *t).collect();
        let cut = ticks.len() - CAP;
        let (_, oldest_kept, _) = ticks.select_nth_unstable(cut);
        let oldest_kept = *oldest_kept;
        self.names.retain(|_, (_, t)| *t >= oldest_kept);
    }
}

/// The name a mention uses: the display name if it's the login in other capitals, else the login.
fn mention_name(a: &Author) -> &str {
    if a.display_name.to_lowercase() == a.login.to_lowercase() || a.login.is_empty() {
        a.display_name.trim()
    } else {
        a.login.trim()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn author(login: &str, display: &str) -> Author {
        Author {
            id: login.into(),
            login: login.into(),
            display_name: display.into(),
            ..Author::default()
        }
    }

    #[test]
    fn names_are_alphabetical_once_each_with_their_platforms() {
        let mut c = Chatters::default();
        c.seen(Platform::Kick, &author("mobasity", "mobasity"));
        c.seen(Platform::Twitch, &author("mobasity", "Mobasity"));
        c.seen(Platform::Twitch, &author("bob", "Bob"));
        c.seen(Platform::Twitch, &author("moe", "Moe"));
        let all = [Platform::Twitch, Platform::Kick];
        let found = c.starting_with(&all, "MO", 10);
        let names: Vec<_> = found.iter().map(|(_, n)| n.as_str()).collect();
        assert_eq!(names, ["Mobasity", "Moe"]);
        assert_eq!(found[0].0, [Platform::Twitch, Platform::Kick]);
        assert_eq!(c.starting_with(&[Platform::Kick], "b", 10).len(), 0);
        assert_eq!(c.starting_with(&all, "", 1).len(), 1, "the limit");
        c.forget(Platform::Twitch);
        assert_eq!(c.starting_with(&all, "", 10).len(), 1);
    }

    #[test]
    fn a_display_name_in_another_script_uses_the_login() {
        let mut c = Chatters::default();
        c.seen(Platform::Twitch, &author("hanako", "花子"));
        let found = c.starting_with(&[Platform::Twitch], "ha", 10);
        assert_eq!(found[0].1, "hanako");
    }

    #[test]
    fn the_oldest_names_go_once_the_list_is_full() {
        let mut c = Chatters::default();
        for i in 0..CAP + CAP / 10 + 1 {
            c.seen(
                Platform::Twitch,
                &author(&format!("u{i}"), &format!("u{i}")),
            );
        }
        assert_eq!(c.names.len(), CAP);
        assert!(c.starting_with(&[Platform::Twitch], "u0", 10).is_empty());
        let newest = format!("u{}", CAP + CAP / 10);
        assert_eq!(c.starting_with(&[Platform::Twitch], &newest, 10).len(), 1);
    }
}
