//! YouTube's own emoji (`:hand-pink-waving:`), sent as text by its API: their images by name.

use std::collections::HashMap;
use std::sync::LazyLock;

use rill_core::{EmoteScope, Fragment};

/// Each emoji's name and its image on YouTube's image server, one `name<TAB>address` per line.
const LIST: &str = include_str!("emoji.tsv");

/// The size Rill asks YouTube's image server for, in pixels.
const SIZE: &str = "=w48-h48-c-k-nd";

static EMOJI: LazyLock<HashMap<&'static str, &'static str>> = LazyLock::new(|| {
    LIST.lines()
        .filter_map(|line| line.split_once('\t'))
        .map(|(name, url)| (name.trim(), url.trim()))
        .filter(|(name, url)| !name.is_empty() && url.starts_with("https://"))
        .collect()
});

/// The image for a YouTube emoji, by its name without colons, such as `hand-pink-waving`.
pub fn url(name: &str) -> Option<String> {
    EMOJI.get(name).map(|base| format!("{base}{SIZE}"))
}

/// Turns the `:name:` of every YouTube emoji Rill knows into its image; others stay text.
pub fn emojify(fragments: Vec<Fragment>) -> Vec<Fragment> {
    let mut out = Vec::with_capacity(fragments.len());
    for f in fragments {
        match f {
            Fragment::Text(text) => split(&text, &mut out),
            other => out.push(other),
        }
    }
    out
}

/// Splits one text into text and emoji.
fn split(text: &str, out: &mut Vec<Fragment>) {
    let mut plain = 0;
    let mut at = 0;
    while let Some(open) = text[at..].find(':').map(|i| at + i) {
        let Some(close) = text[open + 1..].find(':').map(|i| open + 1 + i) else {
            break;
        };
        let name = &text[open + 1..close];
        let known = name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
            && !name.is_empty();
        match known.then(|| url(name)).flatten() {
            Some(url) => {
                if plain < open {
                    out.push(Fragment::Text(text[plain..open].to_string()));
                }
                out.push(Fragment::Emote {
                    name: format!(":{name}:"),
                    url: Some(url),
                    size: None,
                    overlay: false,
                    scope: EmoteScope::Global,
                });
                plain = close + 1;
                at = close + 1;
            }
            // The closing colon may open the next emoji (`hi :x::face-blue-smiling:`).
            None => at = close,
        }
    }
    if plain < text.len() {
        out.push(Fragment::Text(text[plain..].to_string()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(fragments: &[Fragment]) -> Vec<String> {
        fragments
            .iter()
            .map(|f| match f {
                Fragment::Text(t) => format!("text {t}"),
                Fragment::Emote { name, .. } => format!("emoji {name}"),
                _ => "other".into(),
            })
            .collect()
    }

    #[test]
    fn known_emoji_become_images_and_the_rest_stays_text() {
        let out = emojify(vec![Fragment::Text(
            "hi :face-green-smiling::face-blue-smiling: at 10:30 :not-one: :_member: :elbowcough: \
             :wormRedBlue: :levels_cat_crying:"
                .into(),
        )]);
        assert_eq!(
            names(&out),
            [
                "text hi ",
                "emoji :face-green-smiling:",
                "emoji :face-blue-smiling:",
                "text  at 10:30 :not-one: :_member: ",
                "emoji :elbowcough:",
                "text  ",
                "emoji :wormRedBlue:",
                "text  ",
                "emoji :levels_cat_crying:",
            ]
        );
        let Fragment::Emote { url: Some(url), .. } = &out[1] else {
            panic!("expected an image");
        };
        assert!(url.starts_with("https://yt3.ggpht.com/") && url.ends_with(SIZE));
    }

    #[test]
    fn every_line_of_the_list_is_read() {
        let lines = LIST.lines().filter(|l| !l.trim().is_empty()).count();
        assert_eq!(
            EMOJI.len(),
            lines,
            "a line without a tab or an https address"
        );
    }
}
