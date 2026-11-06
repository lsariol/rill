//! Chat messages, normalized from every platform into one shape.

use crate::Platform;

/// A chat message as received from a platform.
#[derive(Debug, Clone, PartialEq)]
pub struct ChatMessage {
    pub platform: Platform,
    /// The platform's own message ID.
    pub id: String,
    pub author: Author,
    pub fragments: Vec<Fragment>,
    pub kind: MessageKind,
    /// When the platform says it was sent, in Unix milliseconds.
    pub sent_at_ms: i64,
    pub reply_to: Option<ReplyRef>,
}

/// Who wrote a message.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Author {
    /// The platform's user ID.
    pub id: String,
    /// The login or handle, used for commands.
    pub login: String,
    /// The name shown in chat.
    pub display_name: String,
    /// The user's chosen name color, as 0xRRGGBB.
    pub color: Option<u32>,
    pub badges: Vec<Badge>,
    pub roles: Roles,
}

/// The roles that matter for moderation and display.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Roles {
    pub broadcaster: bool,
    pub moderator: bool,
    pub vip: bool,
    pub subscriber: bool,
}

/// A badge shown next to a name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Badge {
    /// A stable key, such as `moderator/1`.
    pub id: String,
    pub title: String,
    pub image_url: Option<String>,
}

/// How long someone has subscribed, in words: "8 months", "1 year", "2 years, 3 months".
pub fn tenure(months: u64) -> String {
    let plural = |n: u64, one: &str| {
        if n == 1 {
            format!("1 {one}")
        } else {
            format!("{n} {one}s")
        }
    };
    match (months / 12, months % 12) {
        (0, m) => plural(m, "month"),
        (y, 0) => plural(y, "year"),
        (y, m) => format!("{}, {}", plural(y, "year"), plural(m, "month")),
    }
}

/// One piece of a message's text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fragment {
    Text(String),
    Emote {
        name: String,
        url: Option<String>,
        /// The image's size in pixels at 1x, when the provider says; `None` means square.
        size: Option<(u16, u16)>,
        /// A zero-width emote, drawn over the emote before it.
        overlay: bool,
    },
    /// An `@name` mention, without the `@`.
    Mention(String),
    Link(String),
}

/// What sort of chat line a message is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MessageKind {
    #[default]
    Chat,
    /// A `/me` action.
    Action,
    /// A platform event shown in chat, such as a subscription.
    Event,
}

/// The message a reply answers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplyRef {
    pub message_id: String,
    pub author: String,
    pub text: String,
}

impl ChatMessage {
    /// The message as plain text, with emotes by name.
    pub fn plain_text(&self) -> String {
        let mut out = String::new();
        for f in &self.fragments {
            match f {
                Fragment::Text(t) | Fragment::Link(t) => out.push_str(t),
                Fragment::Emote { name, .. } => out.push_str(name),
                Fragment::Mention(m) => {
                    out.push('@');
                    out.push_str(m);
                }
            }
        }
        out
    }

    /// Whether any fragment is an emote with an image.
    pub fn has_images(&self) -> bool {
        self.fragments
            .iter()
            .any(|f| matches!(f, Fragment::Emote { url: Some(_), .. }))
    }
}

impl Fragment {
    /// A square platform emote.
    pub fn emote(name: impl Into<String>, url: Option<String>) -> Fragment {
        Fragment::Emote {
            name: name.into(),
            url,
            size: None,
            overlay: false,
        }
    }
}

/// Replaces whole words in text fragments with the emotes `find` knows; spaces stay as text.
pub fn replace_words(
    fragments: Vec<Fragment>,
    find: impl Fn(&str) -> Option<Fragment>,
) -> Vec<Fragment> {
    let mut out = Vec::with_capacity(fragments.len());
    for f in fragments {
        let Fragment::Text(text) = f else {
            out.push(f);
            continue;
        };
        let mut plain = String::new();
        for (i, word) in text.split(' ').enumerate() {
            if i > 0 {
                plain.push(' ');
            }
            match (!word.is_empty()).then(|| find(word)).flatten() {
                Some(emote) => {
                    if !plain.is_empty() {
                        out.push(Fragment::Text(std::mem::take(&mut plain)));
                    }
                    out.push(emote);
                }
                None => plain.push_str(word),
            }
        }
        if !plain.is_empty() {
            out.push(Fragment::Text(plain));
        }
    }
    out
}

/// Splits plain text into text, `@mention` and link fragments.
pub fn split_text(text: &str) -> Vec<Fragment> {
    let mut out: Vec<Fragment> = Vec::new();
    let mut plain = String::new();
    for (i, word) in text.split(' ').enumerate() {
        if i > 0 {
            plain.push(' ');
        }
        let special = if let Some(name) = word.strip_prefix('@') {
            let name = name.trim_end_matches(|c: char| !(c.is_alphanumeric() || c == '_'));
            (!name.is_empty()).then(|| (Fragment::Mention(name.to_string()), 1 + name.len()))
        } else if word.starts_with("https://") || word.starts_with("http://") {
            Some((Fragment::Link(word.to_string()), word.len()))
        } else {
            None
        };
        match special {
            Some((fragment, used)) => {
                if !plain.is_empty() {
                    out.push(Fragment::Text(std::mem::take(&mut plain)));
                }
                out.push(fragment);
                plain.push_str(&word[used..]);
            }
            None => plain.push_str(word),
        }
    }
    if !plain.is_empty() {
        out.push(Fragment::Text(plain));
    }
    out
}

/// Joins adjacent text fragments, so renderers see as few pieces as possible.
pub fn merge_text(fragments: Vec<Fragment>) -> Vec<Fragment> {
    let mut out: Vec<Fragment> = Vec::with_capacity(fragments.len());
    for f in fragments {
        match (out.last_mut(), f) {
            (Some(Fragment::Text(prev)), Fragment::Text(t)) => prev.push_str(&t),
            (_, f) => out.push(f),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tenure_reads_naturally() {
        assert_eq!(tenure(1), "1 month");
        assert_eq!(tenure(8), "8 months");
        assert_eq!(tenure(12), "1 year");
        assert_eq!(tenure(18), "1 year, 6 months");
        assert_eq!(tenure(25), "2 years, 1 month");
    }

    #[test]
    fn replace_words_swaps_whole_words_only() {
        let find = |w: &str| (w == "KEKW").then(|| Fragment::emote("KEKW", Some("u".into())));
        let out = replace_words(
            vec![
                Fragment::Text("lol KEKW KEKWx KEKW".into()),
                Fragment::Mention("KEKW".into()),
                Fragment::Text(" KEKW".into()),
            ],
            find,
        );
        let kekw = Fragment::emote("KEKW", Some("u".into()));
        assert_eq!(
            out,
            vec![
                Fragment::Text("lol ".into()),
                kekw.clone(),
                Fragment::Text(" KEKWx ".into()),
                kekw.clone(),
                Fragment::Mention("KEKW".into()),
                Fragment::Text(" ".into()),
                kekw,
            ]
        );
        let none = replace_words(vec![Fragment::Text("a  b ".into())], find);
        assert_eq!(none, vec![Fragment::Text("a  b ".into())]);
    }

    #[test]
    fn split_finds_mentions_and_links() {
        assert_eq!(
            split_text("hi @Rill_Dev, see https://x.io now"),
            vec![
                Fragment::Text("hi ".into()),
                Fragment::Mention("Rill_Dev".into()),
                Fragment::Text(", see ".into()),
                Fragment::Link("https://x.io".into()),
                Fragment::Text(" now".into()),
            ]
        );
    }

    #[test]
    fn split_keeps_lone_at_sign_as_text() {
        assert_eq!(split_text("a @ b"), vec![Fragment::Text("a @ b".into())]);
    }

    #[test]
    fn plain_text_round_trips() {
        let text = "hey @bob look https://a.b ok";
        let m = ChatMessage {
            platform: Platform::Twitch,
            id: "1".into(),
            author: Author::default(),
            fragments: split_text(text),
            kind: MessageKind::Chat,
            sent_at_ms: 0,
            reply_to: None,
        };
        assert_eq!(m.plain_text(), text);
    }

    #[test]
    fn merge_joins_text() {
        let merged = merge_text(vec![
            Fragment::Text("a".into()),
            Fragment::Text("b".into()),
            Fragment::Mention("c".into()),
        ]);
        assert_eq!(
            merged,
            vec![Fragment::Text("ab".into()), Fragment::Mention("c".into())]
        );
    }
}
