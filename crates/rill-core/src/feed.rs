//! The unified chat feed: a bounded, filtered, chronological buffer.

use std::collections::{BTreeSet, HashMap, VecDeque};

use crate::{ChatMessage, EventKind, Fragment, Level, Platform};

/// The default number of entries kept.
pub const DEFAULT_CAPACITY: usize = 5_000;

/// One line in the feed.
#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    /// Increases by one for every entry ever pushed.
    pub seq: u64,
    pub item: Item,
    /// When Rill received it, in Unix milliseconds.
    pub received_ms: i64,
    /// Mentions the signed-in user.
    pub highlighted: bool,
    /// The author's first message in this channel, as far as Rill knows.
    pub first: bool,
    pub deleted: bool,
}

impl Entry {
    /// The sort of channel event this line is, if it is one. Event notices are ad breaks.
    pub fn event(&self) -> Option<EventKind> {
        match &self.item {
            Item::Chat(m) => match m.kind {
                crate::MessageKind::Event(k) => Some(k),
                _ => None,
            },
            Item::Notice(n) => (n.level == Level::Event).then_some(EventKind::AdBreak),
            Item::Sent(_) => None,
        }
    }

    /// A channel event: a subscription, gift, raid, Rant, redemption or ad break.
    pub fn is_event(&self) -> bool {
        self.event().is_some()
    }
}

/// A line marked on the scroll bar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Mark {
    /// Its place among the visible lines.
    pub ix: usize,
    /// Mentions you.
    pub mention: bool,
    /// The author's first message.
    pub first: bool,
    pub event: Option<EventKind>,
}

/// What a feed line holds.
#[derive(Debug, Clone, PartialEq)]
pub enum Item {
    Chat(ChatMessage),
    Sent(SentMessage),
    Notice(Notice),
}

/// A message the user sent, with its delivery on each platform.
#[derive(Debug, Clone, PartialEq)]
pub struct SentMessage {
    pub text: String,
    /// The text with emotes picked out, from the emote lists, then from a platform's echo.
    pub fragments: Vec<Fragment>,
    pub deliveries: Vec<Delivery>,
}

/// A sent message's state on one platform.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Delivery {
    pub platform: Platform,
    pub state: DeliveryState,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeliveryState {
    Pending,
    Sent,
    Failed(String),
}

/// A line from Rill or a platform that isn't chat.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notice {
    pub platform: Option<Platform>,
    pub level: Level,
    pub text: String,
}

/// Which entries the feed shows.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Filter {
    pub hidden: BTreeSet<Platform>,
    pub mentions_only: bool,
    /// Only messages containing this (or written by a matching name). Lowercase; see [`Filter::search_for`].
    pub search: Option<String>,
}

impl Filter {
    /// Whether an entry passes this filter.
    pub fn shows(&self, entry: &Entry) -> bool {
        let platform_ok = match &entry.item {
            Item::Chat(m) => !self.hidden.contains(&m.platform),
            Item::Sent(s) => s
                .deliveries
                .iter()
                .any(|d| !self.hidden.contains(&d.platform)),
            Item::Notice(n) => n.platform.is_none_or(|p| !self.hidden.contains(&p)),
        };
        let mention_ok = !self.mentions_only
            || entry.highlighted
            || matches!(
                entry.item,
                Item::Notice(Notice {
                    level: Level::Error,
                    ..
                })
            );
        platform_ok && mention_ok && self.found(entry)
    }

    /// Sets the search text: case-insensitive, and empty means no search.
    pub fn search_for(&mut self, text: &str) {
        let q = text.trim().to_lowercase();
        self.search = (!q.is_empty()).then_some(q);
    }

    fn found(&self, entry: &Entry) -> bool {
        let Some(q) = &self.search else {
            return true;
        };
        let has = |s: &str| s.to_lowercase().contains(q.as_str());
        match &entry.item {
            Item::Chat(m) => {
                has(&m.plain_text()) || has(&m.author.display_name) || has(&m.author.login)
            }
            Item::Sent(s) => has(&s.text),
            Item::Notice(_) => false,
        }
    }
}

/// How the visible list changed since the last [`Feed::take_changes`].
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Changes {
    /// Rebuild the whole list (the filter changed).
    pub reset: bool,
    /// Visible entries removed from the front.
    pub evicted: usize,
    /// Visible entries added at the end.
    pub appended: usize,
    /// Entries changed in place (by seq), which may need measuring again.
    pub updated: Vec<u64>,
}

/// The bounded chat buffer and its filtered view.
#[derive(Debug)]
pub struct Feed {
    entries: VecDeque<Entry>,
    capacity: usize,
    next_seq: u64,
    filter: Filter,
    /// Seqs of visible entries, ascending.
    visible: VecDeque<u64>,
    /// Lowercase names that count as a mention, per platform.
    own_names: HashMap<Platform, String>,
    changes: Changes,
    /// Visible entries the UI knew about at the last take, not yet evicted.
    known: usize,
    /// Seqs of entries that mention you or are events, oldest first, for scroll bar marks.
    flagged: VecDeque<u64>,
}

impl Default for Feed {
    fn default() -> Self {
        Feed::new(DEFAULT_CAPACITY)
    }
}

impl Feed {
    /// An empty feed keeping at most `capacity` entries.
    pub fn new(capacity: usize) -> Feed {
        Feed {
            entries: VecDeque::with_capacity(capacity.min(1024)),
            capacity: capacity.max(1),
            next_seq: 0,
            filter: Filter::default(),
            visible: VecDeque::new(),
            own_names: HashMap::new(),
            changes: Changes::default(),
            known: 0,
            flagged: VecDeque::new(),
        }
    }

    /// Sets the name that counts as a mention on a platform.
    pub fn set_own_name(&mut self, platform: Platform, name: &str) {
        self.own_names.insert(platform, name.to_lowercase());
    }

    /// Adds an entry at the end, evicting the oldest past capacity. Returns its seq.
    pub fn push(&mut self, item: Item, received_ms: i64) -> u64 {
        self.push_line(item, received_ms, false)
    }

    /// Adds an entry, noting whether it's the author's first message. Returns its seq.
    pub fn push_line(&mut self, item: Item, received_ms: i64, first: bool) -> u64 {
        let seq = self.next_seq;
        self.next_seq += 1;
        let highlighted = self.mentions_me(&item);
        let entry = Entry {
            seq,
            item,
            received_ms,
            highlighted,
            first,
            deleted: false,
        };
        if self.filter.shows(&entry) {
            self.visible.push_back(seq);
            self.changes.appended += 1;
        }
        if entry.highlighted || entry.first || entry.is_event() {
            self.flagged.push_back(seq);
        }
        self.entries.push_back(entry);
        self.evict();
        seq
    }

    /// How many entries the feed keeps.
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Keeps at most `capacity` entries from now on, dropping the oldest at once if needed.
    pub fn set_capacity(&mut self, capacity: usize) {
        self.capacity = capacity.max(1);
        self.evict();
    }

    /// Visible lines that mention you or are events, oldest first.
    pub fn marks(&self) -> Vec<Mark> {
        self.flagged
            .iter()
            .filter_map(|&seq| {
                let ix = self.visible_index(seq)?;
                let e = self.get(seq)?;
                Some(Mark {
                    ix,
                    mention: e.highlighted,
                    first: e.first,
                    event: e.event(),
                })
            })
            .collect()
    }

    /// Drops the oldest entries past capacity.
    fn evict(&mut self) {
        while self.entries.len() > self.capacity {
            if let Some(front) = self.entries.front().map(|e| e.seq) {
                while self.flagged.front().is_some_and(|&s| s <= front) {
                    self.flagged.pop_front();
                }
            }
            let old = self.entries.pop_front().map(|e| e.seq);
            if old.is_some() && self.visible.front().copied() == old {
                self.visible.pop_front();
                if self.known > 0 {
                    self.known -= 1;
                    self.changes.evicted += 1;
                } else {
                    // After a reset the counts are unused; saturate rather than underflow.
                    self.changes.appended = self.changes.appended.saturating_sub(1);
                }
            }
        }
    }

    /// The number of visible entries.
    pub fn visible_len(&self) -> usize {
        self.visible.len()
    }

    /// The visible entry at `ix`.
    pub fn visible(&self, ix: usize) -> Option<&Entry> {
        self.visible.get(ix).and_then(|seq| self.get(*seq))
    }

    /// The position of a visible entry by seq.
    pub fn visible_index(&self, seq: u64) -> Option<usize> {
        let (a, b) = self.visible.as_slices();
        match a.binary_search(&seq) {
            Ok(i) => Some(i),
            Err(_) => b.binary_search(&seq).ok().map(|i| a.len() + i),
        }
    }

    /// An entry by seq, if still kept.
    pub fn get(&self, seq: u64) -> Option<&Entry> {
        let first = self.entries.front()?.seq;
        self.entries
            .get(usize::try_from(seq.checked_sub(first)?).ok()?)
    }

    fn get_mut(&mut self, seq: u64) -> Option<&mut Entry> {
        let first = self.entries.front()?.seq;
        self.entries
            .get_mut(usize::try_from(seq.checked_sub(first)?).ok()?)
    }

    /// Every kept entry, oldest first.
    pub fn entries(&self) -> impl DoubleEndedIterator<Item = &Entry> {
        self.entries.iter()
    }

    /// The number of kept entries, visible or not.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn filter(&self) -> &Filter {
        &self.filter
    }

    /// Replaces the filter and rebuilds the visible list.
    pub fn set_filter(&mut self, filter: Filter) {
        if filter == self.filter {
            return;
        }
        self.filter = filter;
        self.rebuild();
    }

    /// Removes every entry (the local `/clear`).
    pub fn clear(&mut self) {
        self.entries.clear();
        self.flagged.clear();
        self.rebuild();
    }

    /// Changes an entry in place and records the update.
    pub fn update(&mut self, seq: u64, f: impl FnOnce(&mut Entry)) -> bool {
        let Some(entry) = self.get_mut(seq) else {
            return false;
        };
        f(entry);
        self.changes.updated.push(seq);
        true
    }

    /// Marks a platform message deleted. Returns whether it was found.
    pub fn mark_deleted(&mut self, platform: Platform, message_id: &str) -> bool {
        let seq = self.entries.iter().rev().find_map(|e| match &e.item {
            Item::Chat(m) if m.platform == platform && m.id == message_id => Some(e.seq),
            _ => None,
        });
        seq.is_some_and(|seq| self.update(seq, |e| e.deleted = true))
    }

    /// Marks every message by a user deleted (timeout or ban). Returns how many.
    pub fn mark_user_deleted(&mut self, platform: Platform, user_id: &str) -> usize {
        self.mark_where(|m| m.platform == platform && m.author.id == user_id)
    }

    /// Marks every message from a platform deleted (chat cleared). Returns how many.
    pub fn mark_platform_deleted(&mut self, platform: Platform) -> usize {
        self.mark_where(|m| m.platform == platform)
    }

    fn mark_where(&mut self, pred: impl Fn(&ChatMessage) -> bool) -> usize {
        let mut seqs = Vec::new();
        for e in &mut self.entries {
            if let Item::Chat(m) = &e.item
                && !e.deleted
                && pred(m)
            {
                e.deleted = true;
                seqs.push(e.seq);
            }
        }
        let n = seqs.len();
        self.changes.updated.extend(seqs);
        n
    }

    /// Returns and resets what changed in the visible list.
    pub fn take_changes(&mut self) -> Changes {
        self.known = self.visible.len();
        std::mem::take(&mut self.changes)
    }

    fn rebuild(&mut self) {
        self.visible = self
            .entries
            .iter()
            .filter(|e| self.filter.shows(e))
            .map(|e| e.seq)
            .collect();
        self.changes = Changes {
            reset: true,
            ..Changes::default()
        };
    }

    fn mentions_me(&self, item: &Item) -> bool {
        let Item::Chat(m) = item else {
            return false;
        };
        // Any of your names counts on any platform: viewers often use one name everywhere.
        let names: Vec<&String> = self.own_names.values().collect();
        m.fragments.iter().any(|f| match f {
            Fragment::Mention(name) => names.iter().any(|me| name.to_lowercase() == **me),
            Fragment::Text(t) => {
                let t = t.to_lowercase();
                names.iter().any(|me| contains_word(&t, me))
            }
            _ => false,
        })
    }
}

/// Whether `word` appears in `text` with no letter, digit or `_` on either side.
fn contains_word(text: &str, word: &str) -> bool {
    if word.is_empty() {
        return false;
    }
    let is_word = |c: char| c.is_alphanumeric() || c == '_';
    text.match_indices(word).any(|(i, _)| {
        let before = text[..i].chars().next_back();
        let after = text[i + word.len()..].chars().next();
        !before.is_some_and(is_word) && !after.is_some_and(is_word)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Author, MessageKind, split_text};

    fn chat(platform: Platform, id: &str, user: &str, text: &str) -> Item {
        Item::Chat(ChatMessage {
            platform,
            id: id.into(),
            author: Author {
                id: format!("u-{user}"),
                login: user.into(),
                display_name: user.into(),
                ..Author::default()
            },
            fragments: split_text(text),
            kind: MessageKind::Chat,
            sent_at_ms: 0,
            reply_to: None,
        })
    }

    #[test]
    fn marks_mentions_and_events_by_visible_place() {
        let mut f = Feed::new(4);
        f.set_own_name(Platform::Twitch, "me");
        f.push(chat(Platform::Twitch, "1", "a", "hi @me"), 0);
        f.push(chat(Platform::Twitch, "2", "a", "hi"), 0);
        let mut sub = chat(Platform::Kick, "3", "b", "subscribed");
        if let Item::Chat(m) = &mut sub {
            m.kind = crate::MessageKind::Event(EventKind::Subscription);
        }
        f.push(sub, 0);
        f.push(
            Item::Notice(Notice {
                platform: Some(Platform::Twitch),
                level: Level::Event,
                text: "Ads started".into(),
            }),
            0,
        );
        assert_eq!(
            f.marks(),
            [
                Mark {
                    ix: 0,
                    mention: true,
                    first: false,
                    event: None
                },
                Mark {
                    ix: 2,
                    mention: false,
                    first: false,
                    event: Some(EventKind::Subscription)
                },
                Mark {
                    ix: 3,
                    mention: false,
                    first: false,
                    event: Some(EventKind::AdBreak)
                },
            ]
        );
        // Hidden platforms' marks go with them; evicted lines' marks too.
        let mut filter = f.filter().clone();
        filter.hidden.insert(Platform::Kick);
        f.set_filter(filter);
        assert_eq!(f.marks().len(), 2);
        f.push(chat(Platform::Twitch, "5", "a", "hi"), 0);
        assert!(
            f.marks().iter().all(|m| !m.mention),
            "the mention was evicted"
        );
    }

    #[test]
    fn a_smaller_capacity_drops_the_oldest_at_once() {
        let mut f = Feed::new(10);
        for i in 0..10 {
            f.push(chat(Platform::Twitch, &i.to_string(), "a", "hi"), 0);
        }
        f.set_capacity(4);
        assert_eq!(f.capacity(), 4);
        assert_eq!(f.entries().count(), 4);
        assert_eq!(f.visible_len(), 4);
        assert_eq!(f.entries().next().map(|e| e.seq), Some(6));
        f.set_capacity(20);
        f.push(chat(Platform::Twitch, "more", "a", "hi"), 0);
        assert_eq!(f.entries().count(), 5, "a bigger limit keeps what's there");
    }

    #[test]
    fn evicts_oldest_past_capacity() {
        let mut f = Feed::new(3);
        for i in 0..5 {
            f.push(chat(Platform::Twitch, &i.to_string(), "a", "hi"), 0);
        }
        assert_eq!(f.len(), 3);
        assert_eq!(f.visible(0).unwrap().seq, 2);
        assert!(f.get(1).is_none());
        assert_eq!(f.get(4).unwrap().seq, 4);
    }

    #[test]
    fn changes_count_evictions_of_known_entries_only() {
        let mut f = Feed::new(3);
        for i in 0..3 {
            f.push(chat(Platform::Twitch, &i.to_string(), "a", "hi"), 0);
        }
        assert_eq!(f.take_changes().appended, 3);
        for i in 3..5 {
            f.push(chat(Platform::Twitch, &i.to_string(), "a", "hi"), 0);
        }
        let c = f.take_changes();
        assert_eq!((c.evicted, c.appended), (2, 2));
        // More new entries than capacity: the extras never reach the UI.
        for i in 5..10 {
            f.push(chat(Platform::Twitch, &i.to_string(), "a", "hi"), 0);
        }
        let c = f.take_changes();
        assert_eq!((c.evicted, c.appended), (3, 3));
        assert_eq!(f.visible_len(), 3);
    }

    #[test]
    fn filter_hides_platforms_and_rebuilds() {
        let mut f = Feed::new(10);
        f.push(chat(Platform::Twitch, "1", "a", "hi"), 0);
        f.push(chat(Platform::Kick, "2", "b", "yo"), 0);
        f.take_changes();
        let mut filter = Filter::default();
        filter.hidden.insert(Platform::Kick);
        f.set_filter(filter);
        assert!(f.take_changes().reset);
        assert_eq!(f.visible_len(), 1);
        f.push(chat(Platform::Kick, "3", "b", "yo"), 0);
        assert_eq!(f.take_changes().appended, 0);
    }

    #[test]
    fn mentions_highlight_and_filter() {
        let mut f = Feed::new(10);
        f.set_own_name(Platform::Twitch, "Rill_Dev");
        f.push(chat(Platform::Twitch, "1", "a", "hey @rill_dev"), 0);
        f.push(chat(Platform::Twitch, "2", "a", "rill_dev: hi"), 0);
        f.push(chat(Platform::Twitch, "3", "a", "rill_devs are fun"), 0);
        f.push(chat(Platform::Kick, "4", "a", "@rill_dev"), 0);
        f.push(chat(Platform::Kick, "5", "a", "@someone_else"), 0);
        let hl: Vec<bool> = f.entries().map(|e| e.highlighted).collect();
        // The Twitch name counts on Kick too.
        assert_eq!(hl, [true, true, false, true, false]);
        f.set_filter(Filter {
            mentions_only: true,
            ..Filter::default()
        });
        assert_eq!(f.visible_len(), 3);
    }

    #[test]
    fn search_matches_text_and_names_ignoring_case() {
        let mut f = Feed::new(10);
        f.push(chat(Platform::Twitch, "1", "alice", "Buy FOLLOWERS now"), 0);
        f.push(chat(Platform::Kick, "2", "bob", "nice stream"), 0);
        f.push(chat(Platform::YouTube, "3", "FollowerBot", "hi"), 0);
        f.push(
            Item::Notice(Notice {
                platform: None,
                level: Level::Info,
                text: "followers".into(),
            }),
            0,
        );
        let mut filter = Filter::default();
        filter.search_for("  followers ");
        f.set_filter(filter.clone());
        assert_eq!(f.visible_len(), 1);
        filter.search_for("follower");
        f.set_filter(filter.clone());
        assert_eq!(f.visible_len(), 2, "text and the author's name");
        filter.search_for("");
        assert_eq!(filter.search, None);
        f.set_filter(filter);
        assert_eq!(f.visible_len(), 4);
    }

    #[test]
    fn deletions_mark_and_report_updates() {
        let mut f = Feed::new(10);
        f.push(chat(Platform::Twitch, "1", "a", "x"), 0);
        f.push(chat(Platform::Twitch, "2", "b", "y"), 0);
        f.push(chat(Platform::Twitch, "3", "a", "z"), 0);
        f.take_changes();
        assert!(f.mark_deleted(Platform::Twitch, "2"));
        assert!(!f.mark_deleted(Platform::Kick, "2"));
        assert_eq!(f.mark_user_deleted(Platform::Twitch, "u-a"), 2);
        assert_eq!(f.take_changes().updated, vec![1, 0, 2]);
        assert!(f.entries().all(|e| e.deleted));
    }

    #[test]
    fn visible_index_finds_seq_across_wraparound() {
        let mut f = Feed::new(4);
        for i in 0..9 {
            f.push(chat(Platform::Twitch, &i.to_string(), "a", "hi"), 0);
        }
        assert_eq!(f.visible_index(8), Some(3));
        assert_eq!(f.visible_index(5), Some(0));
        assert_eq!(f.visible_index(4), None);
    }
}
