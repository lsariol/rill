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

impl Roles {
    /// The highest role, which colors the name in Simple view.
    pub fn rank(self) -> Rank {
        if self.broadcaster {
            Rank::Owner
        } else if self.moderator {
            Rank::Moderator
        } else if self.vip {
            Rank::Vip
        } else if self.subscriber {
            Rank::Subscriber
        } else {
            Rank::Viewer
        }
    }
}

/// Someone's standing in a chat, highest first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rank {
    Owner,
    Moderator,
    Vip,
    Subscriber,
    Viewer,
}

impl Rank {
    pub const ALL: [Rank; 5] = [
        Rank::Owner,
        Rank::Moderator,
        Rank::Vip,
        Rank::Subscriber,
        Rank::Viewer,
    ];

    /// Its name in settings.
    pub fn key(self) -> &'static str {
        match self {
            Rank::Owner => "owner",
            Rank::Moderator => "moderator",
            Rank::Vip => "vip",
            Rank::Subscriber => "subscriber",
            Rank::Viewer => "viewer",
        }
    }

    /// The color behind its badge icon, where the platform sends no image.
    pub fn badge_color(self, platform: crate::Platform) -> u32 {
        match (platform, self) {
            (crate::Platform::YouTube, Rank::Owner) => 0xd9a400,
            (crate::Platform::YouTube, Rank::Moderator) => 0x5e84f1,
            (crate::Platform::YouTube, _) => 0x2ba640,
            (_, Rank::Owner) => 0xe91916,
            (_, Rank::Moderator) => 0x00ad03,
            (_, Rank::Vip) => 0xe005b9,
            (_, _) => 0x8205b4,
        }
    }

    /// Its badge icon's name, for platforms that send no image.
    pub fn badge_icon(self) -> &'static str {
        match self {
            Rank::Owner => "owner",
            Rank::Moderator => "moderator",
            Rank::Vip => "vip",
            Rank::Subscriber | Rank::Viewer => "subscriber",
        }
    }

    /// Its name in Settings.
    pub fn label(self) -> &'static str {
        match self {
            Rank::Owner => "Channel owner",
            Rank::Moderator => "Moderators",
            Rank::Vip => "VIPs",
            Rank::Subscriber => "Subscribers and members",
            Rank::Viewer => "Everyone else",
        }
    }
}

/// A badge shown next to a name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Badge {
    /// A stable key, such as `moderator/1`.
    pub id: String,
    pub title: String,
    pub image_url: Option<String>,
}

impl Badge {
    /// Its kind: the key before the `/`, such as `moderator`.
    pub fn kind(&self) -> &str {
        self.id.split('/').next().unwrap_or_default()
    }

    /// The role it stands for, for badges Badge view keeps: owner, moderator, VIP and subscriber.
    pub fn role(&self) -> Option<Rank> {
        Some(match self.kind() {
            "broadcaster" => Rank::Owner,
            "moderator" | "lead_moderator" => Rank::Moderator,
            "vip" => Rank::Vip,
            "subscriber" | "founder" => Rank::Subscriber,
            _ => return None,
        })
    }
}

/// The largest size the image's service has, for hover cards and the overlay; other images as they are.
pub fn bigger_image(url: &str) -> String {
    let swap = |from: &str, to: &str| url.strip_suffix(from).map(|base| format!("{base}{to}"));
    if url.contains("static-cdn.jtvnw.net/emoticons/") {
        swap("/1.0", "/3.0").or_else(|| swap("/2.0", "/3.0"))
    } else if url.contains("static-cdn.jtvnw.net/badges/") {
        swap("/2", "/3").or_else(|| swap("/1", "/3"))
    } else if url.contains("7tv.app") {
        swap("/1x.webp", "/3x.webp").or_else(|| swap("/1x.avif", "/3x.avif"))
    } else if url.contains("betterttv.net") {
        swap("/1x.webp", "/3x.webp").or_else(|| swap("/1x", "/3x"))
    } else if url.contains("frankerfacez.com/emote/") {
        swap("/1", "/4")
    } else {
        None
    }
    .unwrap_or_else(|| url.to_string())
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
        /// Whose emote it is.
        scope: EmoteScope,
    },
    /// An `@name` mention, without the `@`.
    Mention(String),
    Link(String),
}

/// Whose emote it is, for its hover card.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum EmoteScope {
    /// The platform didn't say.
    #[default]
    Unknown,
    /// The channel's own.
    Channel,
    /// Everyone's.
    Global,
    /// Another channel's, usable here through a subscription.
    OtherChannel,
}

/// What sort of chat line a message is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MessageKind {
    #[default]
    Chat,
    /// A `/me` action.
    Action,
    /// A platform event shown in chat, such as a subscription.
    Event(EventKind),
}

impl MessageKind {
    pub fn is_event(self) -> bool {
        matches!(self, MessageKind::Event(_))
    }
}

/// What sort of event a line is, so each can have its own highlight.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
pub enum EventKind {
    /// Subscriptions, resubs and memberships.
    Subscription,
    /// Gifted subs and memberships.
    Gift,
    /// Raids and hosts.
    Raid,
    /// Money: Bits, Rants, Super Chats.
    Money,
    /// Channel point redemptions.
    Redemption,
    Follow,
    Announcement,
    /// An ad break starting or ending (a notice line).
    AdBreak,
    #[default]
    Other,
}

impl EventKind {
    pub const ALL: [EventKind; 9] = [
        EventKind::Subscription,
        EventKind::Gift,
        EventKind::Raid,
        EventKind::Money,
        EventKind::Redemption,
        EventKind::Follow,
        EventKind::Announcement,
        EventKind::AdBreak,
        EventKind::Other,
    ];

    /// Its name in `settings.json`.
    pub fn key(self) -> &'static str {
        match self {
            EventKind::Subscription => "subscription",
            EventKind::Gift => "gift",
            EventKind::Raid => "raid",
            EventKind::Money => "money",
            EventKind::Redemption => "redemption",
            EventKind::Follow => "follow",
            EventKind::Announcement => "announcement",
            EventKind::AdBreak => "ad_break",
            EventKind::Other => "other",
        }
    }

    /// Its name in Settings.
    pub fn label(self) -> &'static str {
        match self {
            EventKind::Subscription => "Subscriptions and memberships",
            EventKind::Gift => "Gifted subs",
            EventKind::Raid => "Raids and hosts",
            EventKind::Money => "Bits, Rants and Super Chats",
            EventKind::Redemption => "Channel point redemptions",
            EventKind::Follow => "Follows",
            EventKind::Announcement => "Announcements",
            EventKind::AdBreak => "Ad breaks",
            EventKind::Other => "Other events",
        }
    }
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
            scope: EmoteScope::Unknown,
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
