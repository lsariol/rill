//! The chat core: applies adapter events to the feed and turns composer input into requests.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};

use crate::command::{self, Invocation, Parsed};
use crate::event::SignInPrompt;
use crate::feed::{Delivery, DeliveryState, Feed, Item, Notice, SentMessage};
use crate::{
    Account, Action, Capabilities, Command, ConnectionStatus, Destination, EmoteOption,
    EmoteSource, Event, Level, Platform, Request, RequestId, UserRef,
};

/// How many sent message IDs are remembered to hide the platform's echo of them.
const SUPPRESS_LIMIT: usize = 1_000;

/// One platform's connection as the core sees it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Link {
    pub status: ConnectionStatus,
    pub account: Option<Account>,
    pub capabilities: Capabilities,
    /// Shown while the user still has to finish signing in.
    pub sign_in: Option<SignInPrompt>,
}

/// A request to hand to one platform's adapter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outgoing {
    pub platform: Platform,
    pub request: Request,
}

/// A platform command waiting for the user to pick where it runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Plan {
    Run { command: Command, label: String },
    PollDialog,
}

/// What submitting the composer led to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Submitted {
    /// Nothing to do (empty input).
    Nothing,
    /// Requests to hand to adapters.
    Send(Vec<Outgoing>),
    /// Handled inside Rill.
    Local,
    /// Ask which platform to run on.
    NeedsTarget {
        plan: Plan,
        prompt: String,
        choices: Vec<Platform>,
    },
    /// Open the poll dialog for this platform.
    PollDialog(Platform),
    /// Not done; the message says why and how to fix it. Keep the composer text.
    Rejected(String),
    /// Ask before running a batch (such as a nuke).
    Confirm { prompt: String, batch: Batch },
    /// Open the search bar with this text.
    Search(String),
}

/// Many requests that run after one confirmation and report back as one notice.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Batch {
    /// What was done, such as `Timed out 23 users for 48h`.
    pub label: String,
    pub items: Vec<(Platform, Command)>,
    /// Remember the users, so `/unnuke` can lift these bans and timeouts.
    pub undoable: bool,
}

#[derive(Debug, Clone)]
struct BatchState {
    label: String,
    remaining: usize,
    failed: usize,
    first_error: Option<String>,
}

#[derive(Debug, Clone)]
enum Pending {
    Send { seq: u64 },
    Action { label: String },
    Batch { id: u64 },
}

/// The chat core's state.
#[derive(Debug, Default)]
pub struct Session {
    pub feed: Feed,
    links: BTreeMap<Platform, Link>,
    next_request: u64,
    pending: HashMap<(Platform, RequestId), Pending>,
    suppress: HashSet<(Platform, String)>,
    suppress_order: VecDeque<(Platform, String)>,
    batches: HashMap<u64, BatchState>,
    next_batch: u64,
    /// The users the last nuke hit, for `/unnuke`.
    pub(crate) last_nuke: Vec<(Platform, UserRef)>,
    /// Emotes the user can use, by platform and source.
    emotes: BTreeMap<(Platform, EmoteSource), Vec<EmoteOption>>,
}

impl Session {
    /// A session with a feed of the given capacity.
    pub fn new(capacity: usize) -> Session {
        Session {
            feed: Feed::new(capacity),
            ..Session::default()
        }
    }

    /// A platform's connection, if it has reported one.
    pub fn link(&self, platform: Platform) -> Option<&Link> {
        self.links.get(&platform)
    }

    /// Connected platforms that can send messages: the composer's destinations.
    pub fn sendable(&self) -> Vec<Platform> {
        self.connected()
            .into_iter()
            .filter(|p| self.can(*p, Action::Send))
            .collect()
    }

    /// Platforms that are connected now, in cycle order.
    pub fn connected(&self) -> Vec<Platform> {
        self.links
            .iter()
            .filter(|(_, l)| l.status == ConnectionStatus::Connected)
            .map(|(p, _)| *p)
            .collect()
    }

    /// Whether a connected platform supports an action.
    pub fn can(&self, platform: Platform, action: Action) -> bool {
        self.links
            .get(&platform)
            .is_some_and(|l| l.status == ConnectionStatus::Connected && l.capabilities.has(action))
    }

    /// Applies one adapter event.
    pub fn apply(&mut self, event: Event, now_ms: i64) {
        match event {
            Event::Status {
                platform,
                status,
                account,
                capabilities,
            } => self.apply_status(platform, status, account, capabilities, now_ms),
            Event::SignIn { platform, prompt } => {
                self.links.entry(platform).or_default().sign_in = Some(prompt);
            }
            Event::Message(m) => {
                let mine = self
                    .links
                    .get(&m.platform)
                    .and_then(|l| l.account.as_ref())
                    .is_some_and(|a| a.id == m.author.id);
                if mine && self.is_own_echo(m.platform, &m.id, &m.plain_text()) {
                    return;
                }
                self.feed.push(Item::Chat(m), now_ms);
            }
            Event::Deleted {
                platform,
                message_id,
            } => {
                self.feed.mark_deleted(platform, &message_id);
            }
            Event::UserCleared { platform, user_id } => {
                self.feed.mark_user_deleted(platform, &user_id);
            }
            Event::Cleared { platform } => {
                self.feed.mark_platform_deleted(platform);
                self.notice(Some(platform), Level::Info, "Chat was cleared.", now_ms);
            }
            Event::Notice {
                platform,
                level,
                text,
            } => self.notice(Some(platform), level, &text, now_ms),
            Event::Done {
                platform,
                request,
                result,
            } => self.apply_done(platform, request, result, now_ms),
            Event::Emotes {
                platform,
                source,
                emotes,
            } => {
                self.emotes.insert((platform, source), emotes);
            }
        }
    }

    fn apply_status(
        &mut self,
        platform: Platform,
        status: ConnectionStatus,
        account: Option<Account>,
        capabilities: Capabilities,
        now_ms: i64,
    ) {
        let link = self.links.entry(platform).or_default();
        let before = std::mem::replace(&mut link.status, status.clone());
        if matches!(
            status,
            ConnectionStatus::Connected
                | ConnectionStatus::Failed(_)
                | ConnectionStatus::Disconnected
        ) {
            link.sign_in = None;
        }
        link.capabilities = capabilities;
        if let Some(a) = account {
            self.feed.set_own_name(platform, &a.login);
            link.account = Some(a);
        }
        let note = match (&before, &status) {
            (ConnectionStatus::Reconnecting { .. }, ConnectionStatus::Connected) => {
                Some((Level::Info, "Reconnected.".to_string()))
            }
            (ConnectionStatus::Connected, ConnectionStatus::Reconnecting { .. }) => {
                Some((Level::Warn, "Connection lost; reconnecting.".to_string()))
            }
            (_, ConnectionStatus::Failed(why)) if before != status => {
                Some((Level::Error, why.clone()))
            }
            _ => None,
        };
        if let Some((level, text)) = note {
            self.notice(Some(platform), level, &text, now_ms);
        }
    }

    fn apply_done(
        &mut self,
        platform: Platform,
        request: RequestId,
        result: Result<Option<String>, String>,
        now_ms: i64,
    ) {
        match self.pending.remove(&(platform, request)) {
            Some(Pending::Send { seq }) => {
                let state = match &result {
                    Ok(_) => DeliveryState::Sent,
                    Err(e) => DeliveryState::Failed(e.clone()),
                };
                if let Ok(Some(id)) = result {
                    self.remember_sent(platform, id);
                }
                self.feed.update(seq, |e| {
                    if let Item::Sent(s) = &mut e.item
                        && let Some(d) = s.deliveries.iter_mut().find(|d| d.platform == platform)
                        && d.state == DeliveryState::Pending
                    {
                        d.state = state;
                    }
                });
            }
            Some(Pending::Action { label }) => match result {
                Ok(_) => self.notice(Some(platform), Level::Info, &format!("✓ {label}"), now_ms),
                Err(e) => self.notice(
                    Some(platform),
                    Level::Error,
                    &format!("✗ {label}: {e}"),
                    now_ms,
                ),
            },
            Some(Pending::Batch { id }) => self.batch_done(id, result.map(|_| ()), now_ms),
            None => {}
        }
    }

    /// Whether an incoming message of ours echoes a send; marks the send delivered if so.
    fn is_own_echo(&mut self, platform: Platform, id: &str, text: &str) -> bool {
        if self.suppress.contains(&(platform, id.to_string())) {
            return true;
        }
        let text = text.trim();
        let seq = self.pending.iter().find_map(|(key, p)| match p {
            Pending::Send { seq } if key.0 == platform => {
                let entry = self.feed.get(*seq)?;
                match &entry.item {
                    Item::Sent(s) if s.text.trim() == text => Some(*seq),
                    _ => None,
                }
            }
            _ => None,
        });
        let Some(seq) = seq else {
            return false;
        };
        self.feed.update(seq, |e| {
            if let Item::Sent(s) = &mut e.item
                && let Some(d) = s.deliveries.iter_mut().find(|d| d.platform == platform)
            {
                d.state = DeliveryState::Sent;
            }
        });
        self.remember_sent(platform, id.to_string());
        true
    }

    fn remember_sent(&mut self, platform: Platform, id: String) {
        if self.suppress.insert((platform, id.clone())) {
            self.suppress_order.push_back((platform, id));
            if self.suppress_order.len() > SUPPRESS_LIMIT
                && let Some(old) = self.suppress_order.pop_front()
            {
                self.suppress.remove(&old);
            }
        }
    }

    /// Adds a notice line to the feed.
    pub fn notice(&mut self, platform: Option<Platform>, level: Level, text: &str, now_ms: i64) {
        self.feed.push(
            Item::Notice(Notice {
                platform,
                level,
                text: text.to_string(),
            }),
            now_ms,
        );
    }

    /// Handles what the user typed in the composer.
    pub fn submit(&mut self, input: &str, destination: Destination, now_ms: i64) -> Submitted {
        match command::parse(input) {
            Parsed::Empty => Submitted::Nothing,
            Parsed::Text(text) => self.send_text(text, destination, now_ms),
            Parsed::Unknown(name) => Submitted::Rejected(format!(
                "Unknown command \"/{name}\". Type /help for the list, or start with // to send a slash."
            )),
            Parsed::Command { spec, args } => (spec.run)(
                self,
                &Invocation {
                    spec,
                    args,
                    destination,
                    now_ms,
                },
            ),
        }
    }

    /// Handles the composer while replying to a message: text goes to that platform as a reply;
    /// commands run as usual there.
    pub fn submit_reply(
        &mut self,
        input: &str,
        platform: Platform,
        message_id: &str,
        now_ms: i64,
    ) -> Submitted {
        match command::parse(input) {
            Parsed::Text(text) => {
                if !self.connected().contains(&platform) {
                    return Submitted::Rejected(format!("{platform} isn't connected."));
                }
                if !self.can(platform, Action::Reply) {
                    return Submitted::Rejected(format!(
                        "{platform} can't take replies from this account. Press Esc to send a normal message."
                    ));
                }
                self.send(
                    text,
                    Destination::One(platform),
                    Some(message_id.to_string()),
                    now_ms,
                )
            }
            _ => self.submit(input, Destination::One(platform), now_ms),
        }
    }

    fn send_text(&mut self, text: String, destination: Destination, now_ms: i64) -> Submitted {
        self.send(text, destination, None, now_ms)
    }

    fn send(
        &mut self,
        text: String,
        destination: Destination,
        reply_to: Option<String>,
        now_ms: i64,
    ) -> Submitted {
        let connected = self.connected();
        let action = if reply_to.is_some() {
            Action::Reply
        } else {
            Action::Send
        };
        let targets: Vec<Platform> = destination
            .targets(&connected)
            .into_iter()
            .filter(|p| self.can(*p, action))
            .collect();
        // Refuse before anything is sent, rather than reaching some platforms and not others.
        let chars = text.chars().count();
        if let Some((p, max)) = targets
            .iter()
            .filter_map(|p| p.max_message_chars().map(|max| (*p, max)))
            .filter(|(_, max)| chars > *max)
            .min_by_key(|(_, max)| *max)
        {
            return Submitted::Rejected(format!(
                "{p} takes messages of up to {max} characters; this one has {chars}. Shorten it, or press Tab to send it elsewhere."
            ));
        }
        if targets.is_empty() {
            return Submitted::Rejected(match destination {
                Destination::One(p) if !connected.contains(&p) => {
                    format!("{p} isn't connected. Press Tab to pick another destination.")
                }
                Destination::One(p) => format!("{p} doesn't let this account send messages."),
                Destination::All => "No connected platform can send messages.".into(),
            });
        }
        let seq = self.feed.push(
            Item::Sent(SentMessage {
                text: text.clone(),
                deliveries: targets
                    .iter()
                    .map(|p| Delivery {
                        platform: *p,
                        state: DeliveryState::Pending,
                    })
                    .collect(),
            }),
            now_ms,
        );
        let out = targets
            .into_iter()
            .map(|platform| {
                let id = self.request_id();
                self.pending.insert((platform, id), Pending::Send { seq });
                Outgoing {
                    platform,
                    request: Request {
                        id,
                        command: Command::Send {
                            text: text.clone(),
                            reply_to: reply_to.clone(),
                        },
                    },
                }
            })
            .collect();
        Submitted::Send(out)
    }

    /// Runs a plan on the destination platform, or asks which platform when sending to all.
    pub(crate) fn target(
        &mut self,
        plan: Plan,
        action: Action,
        destination: Destination,
    ) -> Submitted {
        match destination {
            Destination::One(p) => self.execute(p, plan),
            Destination::All => {
                let choices: Vec<Platform> = self
                    .connected()
                    .into_iter()
                    .filter(|p| self.can(*p, action))
                    .collect();
                if choices.is_empty() {
                    return Submitted::Rejected(format!("No connected platform can {action}."));
                }
                // Nothing to choose: run it where it can run.
                if let [only] = choices[..] {
                    return self.execute(only, plan);
                }
                let prompt = match &plan {
                    Plan::Run { label, .. } => format!("{label} on which platform?"),
                    Plan::PollDialog => "Start a poll on which platform?".into(),
                };
                Submitted::NeedsTarget {
                    plan,
                    prompt,
                    choices,
                }
            }
        }
    }

    /// Runs a planned command on one platform.
    pub fn execute(&mut self, platform: Platform, plan: Plan) -> Submitted {
        let action = match &plan {
            Plan::Run { command, .. } => command.action(),
            Plan::PollDialog => Some(Action::Poll),
        };
        if let Some(action) = action {
            if !self.connected().contains(&platform) {
                return Submitted::Rejected(format!("{platform} isn't connected."));
            }
            if !self.can(platform, action) {
                return Submitted::Rejected(format!(
                    "{platform} can't {action} with this account."
                ));
            }
        }
        match plan {
            Plan::PollDialog => Submitted::PollDialog(platform),
            Plan::Run { command, label } => {
                let id = self.request_id();
                self.pending
                    .insert((platform, id), Pending::Action { label });
                Submitted::Send(vec![Outgoing {
                    platform,
                    request: Request { id, command },
                }])
            }
        }
    }

    /// Emotes whose names start with `prefix` (any case) on the destination's platforms: every
    /// platform you can send to for All. A platform's own emotes come before 7TV's and the like,
    /// then shorter names; each name once, with every platform it works on.
    pub fn emote_choices(
        &self,
        destination: Destination,
        prefix: &str,
        limit: usize,
    ) -> Vec<(Vec<Platform>, EmoteOption)> {
        let prefix = prefix.to_lowercase();
        let platforms = destination.targets(&self.sendable());
        let mut found: Vec<(EmoteSource, Platform, &EmoteOption)> = self
            .emotes
            .iter()
            .filter(|((p, _), _)| platforms.contains(p))
            .flat_map(|((p, source), list)| list.iter().map(move |e| (*source, *p, e)))
            .filter(|(_, _, e)| e.name.to_lowercase().starts_with(&prefix))
            .collect();
        found.sort_by(|a, b| {
            (a.0, a.2.name.len(), &a.2.name).cmp(&(b.0, b.2.name.len(), &b.2.name))
        });
        let mut out: Vec<(Vec<Platform>, EmoteOption)> = Vec::new();
        for (_, p, e) in found {
            match out.iter().position(|(_, o)| o.name == e.name) {
                Some(i) if !out[i].0.contains(&p) => out[i].0.push(p),
                Some(_) => {}
                None if out.len() < limit => out.push((vec![p], e.clone())),
                None => {}
            }
        }
        for (ps, _) in &mut out {
            ps.sort();
        }
        out
    }

    /// Starts a confirmed batch.
    pub fn run_batch(&mut self, batch: Batch, now_ms: i64) -> Submitted {
        if batch.items.is_empty() {
            return Submitted::Nothing;
        }
        self.next_batch += 1;
        let id = self.next_batch;
        self.batches.insert(
            id,
            BatchState {
                label: batch.label.clone(),
                remaining: batch.items.len(),
                failed: 0,
                first_error: None,
            },
        );
        if batch.undoable {
            self.last_nuke = batch
                .items
                .iter()
                .filter_map(|(p, c)| match c {
                    Command::Timeout { user, .. } | Command::Ban { user, .. } => {
                        Some((*p, user.clone()))
                    }
                    _ => None,
                })
                .collect();
        }
        self.notice(None, Level::Info, &format!("{}…", batch.label), now_ms);
        let out = batch
            .items
            .into_iter()
            .map(|(platform, command)| {
                let rid = self.request_id();
                self.pending.insert((platform, rid), Pending::Batch { id });
                Outgoing {
                    platform,
                    request: Request { id: rid, command },
                }
            })
            .collect();
        Submitted::Send(out)
    }

    fn batch_done(&mut self, id: u64, result: Result<(), String>, now_ms: i64) {
        let Some(b) = self.batches.get_mut(&id) else {
            return;
        };
        b.remaining -= 1;
        if let Err(e) = result {
            b.failed += 1;
            b.first_error.get_or_insert(e);
        }
        if b.remaining > 0 {
            return;
        }
        let Some(b) = self.batches.remove(&id) else {
            return;
        };
        match b.first_error {
            None => self.notice(None, Level::Info, &format!("✓ {}", b.label), now_ms),
            Some(e) => self.notice(
                None,
                Level::Warn,
                &format!("! {}: {} failed. First error: {e}", b.label, b.failed),
                now_ms,
            ),
        }
    }

    fn request_id(&mut self) -> RequestId {
        self.next_request += 1;
        RequestId(self.next_request)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Author, ChatMessage, MessageKind, split_text};

    const ALL_CAPS: Capabilities = Capabilities::of(&[
        Action::Send,
        Action::Delete,
        Action::Timeout,
        Action::Ban,
        Action::Unban,
        Action::Poll,
    ]);

    fn connect(s: &mut Session, p: Platform, caps: Capabilities) {
        s.apply(
            Event::Status {
                platform: p,
                status: ConnectionStatus::Connected,
                account: Some(Account {
                    id: format!("me-{p}"),
                    login: "rill_dev".into(),
                    display_name: "Rill_Dev".into(),
                }),
                capabilities: caps,
            },
            0,
        );
    }

    fn msg(p: Platform, id: &str, author_id: &str, text: &str) -> Event {
        Event::Message(ChatMessage {
            platform: p,
            id: id.into(),
            author: Author {
                id: author_id.into(),
                ..Author::default()
            },
            fragments: split_text(text),
            kind: MessageKind::Chat,
            sent_at_ms: 0,
            reply_to: None,
        })
    }

    fn done(s: &mut Session, out: &[Outgoing], fail: usize, now: i64) {
        for (i, o) in out.iter().enumerate() {
            let result = if i < fail {
                Err("rate limited".into())
            } else {
                Ok(None)
            };
            s.apply(
                Event::Done {
                    platform: o.platform,
                    request: o.request.id,
                    result,
                },
                now,
            );
        }
    }

    fn last_notice(s: &Session) -> (Level, String) {
        s.feed
            .entries()
            .rev()
            .find_map(|e| match &e.item {
                Item::Notice(n) => Some((n.level, n.text.clone())),
                _ => None,
            })
            .unwrap()
    }

    #[test]
    fn nuke_confirms_runs_reports_and_undoes() {
        let mut s = Session::new(100);
        connect(&mut s, Platform::Twitch, ALL_CAPS);
        connect(&mut s, Platform::YouTube, ALL_CAPS);
        connect(&mut s, Platform::Kick, Capabilities::NONE);
        let minute = 60_000;
        s.apply(msg(Platform::Twitch, "1", "old", "HAHA"), 0);
        s.apply(msg(Platform::Twitch, "2", "a", "haha lol"), 10 * minute);
        s.apply(msg(Platform::Twitch, "3", "a", "HaHa again"), 10 * minute);
        s.apply(msg(Platform::YouTube, "4", "b", "so haha"), 10 * minute);
        s.apply(msg(Platform::Kick, "5", "c", "haha"), 10 * minute);
        s.apply(msg(Platform::Twitch, "6", "me-Twitch", "haha"), 10 * minute);
        s.apply(msg(Platform::Twitch, "7", "d", "nothing here"), 10 * minute);
        let mut m = msg(Platform::Twitch, "8", "mod", "haha");
        if let Event::Message(m) = &mut m {
            m.author.roles.moderator = true;
        }
        s.apply(m, 10 * minute);

        let now = 12 * minute;
        let Submitted::Confirm { prompt, batch } =
            s.submit("/nuke haha 48h", Destination::All, now)
        else {
            panic!("expected a confirmation");
        };
        assert_eq!(
            prompt,
            "Time out 2 users for 2d who said \"haha\" in the last 5m? Twitch 1, YouTube 1. Skipped Kick: no permission."
        );
        assert!(batch.items.iter().all(|(_, c)| matches!(
            c,
            Command::Timeout { seconds: 172_800, user, .. } if user.id.is_some()
        )));
        let Submitted::Send(out) = s.run_batch(batch, now) else {
            panic!("expected requests");
        };
        assert_eq!(out.len(), 2);
        done(&mut s, &out, 1, now);
        let (level, text) = last_notice(&s);
        assert_eq!(level, Level::Warn);
        assert!(
            text.contains("1 failed") && text.contains("rate limited"),
            "{text}"
        );

        // Only Twitch, and further back.
        let Submitted::Confirm { batch, .. } =
            s.submit("/meganuke twitch haha 15m", Destination::All, now)
        else {
            panic!("expected a confirmation");
        };
        let logins: Vec<_> = batch
            .items
            .iter()
            .map(|(p, c)| match c {
                Command::Ban { user, .. } => (*p, user.id.clone().unwrap()),
                c => panic!("not a ban: {c:?}"),
            })
            .collect();
        assert_eq!(
            logins,
            [
                (Platform::Twitch, "old".to_string()),
                (Platform::Twitch, "a".to_string())
            ]
        );
        let Submitted::Send(out) = s.run_batch(batch, now) else {
            panic!("expected requests");
        };
        done(&mut s, &out, 0, now);
        assert_eq!(
            last_notice(&s),
            (
                Level::Info,
                "✓ Banned 2 users who said \"haha\"".to_string()
            )
        );

        let Submitted::Confirm { batch, .. } = s.submit("/unnuke", Destination::All, now) else {
            panic!("expected a confirmation");
        };
        assert_eq!(batch.items.len(), 2, "the last nuke, not the one before");
        assert!(
            batch
                .items
                .iter()
                .all(|(_, c)| matches!(c, Command::Unban { .. }))
        );

        assert!(matches!(
            s.submit("/nuke nobody said this", Destination::All, now),
            Submitted::Rejected(why) if why.starts_with("No one said")
        ));
        assert!(matches!(
            s.submit("/nuke kick haha", Destination::All, now),
            Submitted::Rejected(why) if why.contains("can't")
        ));
        assert!(matches!(
            s.submit("/nuke", Destination::All, now),
            Submitted::Rejected(why) if why.starts_with("Usage")
        ));
    }

    fn deliveries(s: &Session) -> Vec<(Platform, DeliveryState)> {
        s.feed
            .entries()
            .find_map(|e| match &e.item {
                Item::Sent(m) => Some(
                    m.deliveries
                        .iter()
                        .map(|d| (d.platform, d.state.clone()))
                        .collect(),
                ),
                _ => None,
            })
            .unwrap_or_default()
    }

    #[test]
    fn send_to_all_fans_out_and_tracks_each_platform() {
        let mut s = Session::new(100);
        connect(&mut s, Platform::Twitch, ALL_CAPS);
        connect(&mut s, Platform::Kick, ALL_CAPS);
        let Submitted::Send(out) = s.submit("hello", Destination::All, 0) else {
            panic!("expected sends");
        };
        assert_eq!(out.len(), 2);
        let (tw, kick) = (out[0].request.id, out[1].request.id);
        s.apply(
            Event::Done {
                platform: Platform::Twitch,
                request: tw,
                result: Ok(Some("t1".into())),
            },
            0,
        );
        s.apply(
            Event::Done {
                platform: Platform::Kick,
                request: kick,
                result: Err("rate limited".into()),
            },
            0,
        );
        assert_eq!(
            deliveries(&s),
            vec![
                (Platform::Twitch, DeliveryState::Sent),
                (Platform::Kick, DeliveryState::Failed("rate limited".into())),
            ]
        );
        // The platform's echo of a sent message is hidden.
        s.apply(msg(Platform::Twitch, "t1", "me-Twitch", "hello"), 0);
        assert_eq!(s.feed.len(), 1);
    }

    #[test]
    fn echo_before_result_counts_as_delivered() {
        let mut s = Session::new(100);
        connect(&mut s, Platform::Twitch, ALL_CAPS);
        let Submitted::Send(out) = s.submit("hi there", Destination::All, 0) else {
            panic!("expected sends");
        };
        s.apply(msg(Platform::Twitch, "t9", "me-Twitch", "hi there"), 0);
        assert_eq!(
            deliveries(&s),
            vec![(Platform::Twitch, DeliveryState::Sent)]
        );
        s.apply(
            Event::Done {
                platform: Platform::Twitch,
                request: out[0].request.id,
                result: Ok(Some("t9".into())),
            },
            0,
        );
        assert_eq!(s.feed.len(), 1);
        // Someone else saying the same thing is shown.
        s.apply(msg(Platform::Twitch, "t10", "other", "hi there"), 0);
        assert_eq!(s.feed.len(), 2);
    }

    #[test]
    fn emote_choices_follow_the_destination() {
        let mut s = Session::new(10);
        connect(&mut s, Platform::Twitch, ALL_CAPS);
        connect(&mut s, Platform::Kick, ALL_CAPS);
        let e = |n: &str| EmoteOption {
            name: n.into(),
            url: None,
            size: None,
        };
        let list = |platform, source, names: &[&str]| Event::Emotes {
            platform,
            source,
            emotes: names.iter().map(|n| e(n)).collect(),
        };
        s.apply(
            list(
                Platform::Twitch,
                EmoteSource::ThirdParty,
                &["Smoked", "Smadge", "KEKW"],
            ),
            0,
        );
        s.apply(list(Platform::Twitch, EmoteSource::Platform, &["Smile"]), 0);
        s.apply(
            list(
                Platform::Kick,
                EmoteSource::Platform,
                &["SmileKick", "Smile"],
            ),
            0,
        );
        fn names(s: &Session, d: Destination, p: &str) -> Vec<String> {
            s.emote_choices(d, p, 10)
                .into_iter()
                .map(|(_, e)| e.name)
                .collect()
        }
        assert_eq!(
            names(&s, Destination::All, "s"),
            ["Smile", "SmileKick", "Smadge", "Smoked"],
            "own emotes first, shorter first, each name once"
        );
        assert_eq!(names(&s, Destination::All, "SMI"), ["Smile", "SmileKick"]);
        assert_eq!(
            names(&s, Destination::One(Platform::Twitch), "sm"),
            ["Smile", "Smadge", "Smoked"]
        );
        assert_eq!(
            names(&s, Destination::One(Platform::Kick), "sm"),
            ["Smile", "SmileKick"]
        );
        assert!(names(&s, Destination::All, "zzz").is_empty());
        let smile = s.emote_choices(Destination::All, "smile", 10);
        assert_eq!(
            smile[0].0,
            [Platform::Twitch, Platform::Kick],
            "every platform it works on"
        );
        // A new list from the same source replaces the old one.
        s.apply(
            list(Platform::Twitch, EmoteSource::ThirdParty, &["Smug"]),
            0,
        );
        assert_eq!(
            names(&s, Destination::One(Platform::Twitch), "sm"),
            ["Smile", "Smug"]
        );
    }

    #[test]
    fn replies_go_to_the_messages_platform_only() {
        let mut s = Session::new(10);
        connect(
            &mut s,
            Platform::Twitch,
            Capabilities::of(&[Action::Send, Action::Reply]),
        );
        connect(
            &mut s,
            Platform::YouTube,
            Capabilities::of(&[Action::Send, Action::Timeout]),
        );
        let Submitted::Send(out) = s.submit_reply("hi back", Platform::Twitch, "m1", 0) else {
            panic!("expected a send");
        };
        assert_eq!(out.len(), 1);
        assert_eq!(
            out[0].request.command,
            Command::Send {
                text: "hi back".into(),
                reply_to: Some("m1".into())
            }
        );
        assert!(matches!(
            s.submit_reply("hi", Platform::YouTube, "m2", 0),
            Submitted::Rejected(why) if why.contains("replies")
        ));
        // Commands still work while replying, on that platform.
        assert!(matches!(
            s.submit_reply("/timeout bob", Platform::YouTube, "m2", 0),
            Submitted::Send(out) if out[0].platform == Platform::YouTube
        ));
    }

    #[test]
    fn a_message_too_long_for_one_target_is_refused_before_sending() {
        let mut s = Session::new(10);
        connect(&mut s, Platform::Twitch, ALL_CAPS);
        connect(&mut s, Platform::YouTube, ALL_CAPS);
        let long = "x".repeat(250);
        assert!(matches!(
            s.submit(&long, Destination::All, 0),
            Submitted::Rejected(why) if why.starts_with("YouTube takes messages of up to 200")
        ));
        assert!(matches!(
            s.submit(&long, Destination::One(Platform::Twitch), 0),
            Submitted::Send(out) if out.len() == 1
        ));
    }

    #[test]
    fn rejects_sending_to_a_disconnected_platform() {
        let mut s = Session::new(100);
        connect(&mut s, Platform::Twitch, ALL_CAPS);
        let r = s.submit("x", Destination::One(Platform::Kick), 0);
        assert!(matches!(r, Submitted::Rejected(m) if m.contains("Kick isn't connected")));
        assert!(s.feed.is_empty());
    }

    #[test]
    fn moderation_on_all_asks_for_a_target() {
        let mut s = Session::new(100);
        connect(&mut s, Platform::Twitch, ALL_CAPS);
        connect(
            &mut s,
            Platform::YouTube,
            Capabilities::of(&[Action::Send, Action::Ban]),
        );
        connect(&mut s, Platform::Kick, Capabilities::of(&[Action::Send]));
        let Submitted::NeedsTarget { plan, choices, .. } =
            s.submit("/ban @bob being rude", Destination::All, 0)
        else {
            panic!("expected a target prompt");
        };
        assert_eq!(choices, vec![Platform::Twitch, Platform::YouTube]);
        let Submitted::Send(out) = s.execute(Platform::YouTube, plan) else {
            panic!("expected a request");
        };
        assert_eq!(
            out[0].request.command,
            Command::Ban {
                user: UserRef {
                    id: None,
                    login: "bob".into()
                },
                reason: Some("being rude".into())
            }
        );
    }

    #[test]
    fn timeout_parses_duration_and_reason() {
        let mut s = Session::new(100);
        connect(&mut s, Platform::Twitch, ALL_CAPS);
        let d = Destination::One(Platform::Twitch);
        let cmd = |s: &mut Session, input: &str| match s.submit(input, d, 0) {
            Submitted::Send(out) => out[0].request.command.clone(),
            other => panic!("unexpected {other:?}"),
        };
        assert!(matches!(
            cmd(&mut s, "/timeout bob"),
            Command::Timeout {
                seconds: 600,
                reason: None,
                ..
            }
        ));
        assert!(matches!(
            cmd(&mut s, "/to bob 1h spam links"),
            Command::Timeout { seconds: 3600, reason: Some(r), .. } if r == "spam links"
        ));
        assert!(matches!(
            cmd(&mut s, "/timeout bob too long"),
            Command::Timeout { seconds: 600, reason: Some(r), .. } if r == "too long"
        ));
        assert!(matches!(
            s.submit("/timeout bob 5x", d, 0),
            Submitted::Rejected(_)
        ));
        assert!(
            matches!(s.submit("/ban", d, 0), Submitted::Rejected(m) if m.starts_with("Usage:"))
        );
    }

    #[test]
    fn unsupported_action_is_rejected_on_one_platform() {
        let mut s = Session::new(100);
        connect(&mut s, Platform::Kick, Capabilities::of(&[Action::Send]));
        let r = s.submit("/poll", Destination::One(Platform::Kick), 0);
        assert!(matches!(r, Submitted::Rejected(m) if m.contains("can't run polls")));
    }

    #[test]
    fn action_results_become_notices() {
        let mut s = Session::new(100);
        connect(&mut s, Platform::Twitch, ALL_CAPS);
        let Submitted::Send(out) = s.submit("/ban bob", Destination::One(Platform::Twitch), 0)
        else {
            panic!("expected a request");
        };
        s.apply(
            Event::Done {
                platform: Platform::Twitch,
                request: out[0].request.id,
                result: Err("You aren't a moderator here.".into()),
            },
            0,
        );
        let last = s.feed.entries().last().unwrap();
        assert!(matches!(&last.item, Item::Notice(n)
            if n.level == Level::Error && n.text == "✗ Ban bob: You aren't a moderator here."));
    }

    #[test]
    fn local_commands_and_unknown() {
        let mut s = Session::new(100);
        assert_eq!(s.submit("/help", Destination::All, 0), Submitted::Local);
        assert_eq!(s.feed.len(), 1);
        assert_eq!(s.submit("/clear", Destination::All, 0), Submitted::Local);
        assert!(s.feed.is_empty());
        assert!(matches!(
            s.submit("/zap", Destination::All, 0),
            Submitted::Rejected(_)
        ));
        assert_eq!(s.submit("   ", Destination::All, 0), Submitted::Nothing);
    }

    #[test]
    fn sign_in_needs_no_connection_and_prompt_clears_on_connect() {
        let mut s = Session::new(100);
        let plan = Plan::Run {
            command: Command::SignIn,
            label: "Sign in to Twitch".into(),
        };
        assert!(matches!(
            s.execute(Platform::Twitch, plan),
            Submitted::Send(_)
        ));
        s.apply(
            Event::SignIn {
                platform: Platform::Twitch,
                prompt: SignInPrompt {
                    url: "https://www.twitch.tv/activate".into(),
                    code: Some("ABCD-EFGH".into()),
                    expires_at_ms: 0,
                },
            },
            0,
        );
        assert!(s.link(Platform::Twitch).unwrap().sign_in.is_some());
        connect(&mut s, Platform::Twitch, ALL_CAPS);
        assert!(s.link(Platform::Twitch).unwrap().sign_in.is_none());
    }

    #[test]
    fn status_changes_leave_notices() {
        let mut s = Session::new(100);
        connect(&mut s, Platform::Twitch, ALL_CAPS);
        let status = |status| Event::Status {
            platform: Platform::Twitch,
            status,
            account: None,
            capabilities: ALL_CAPS,
        };
        s.apply(status(ConnectionStatus::Reconnecting { attempt: 1 }), 0);
        s.apply(status(ConnectionStatus::Connected), 0);
        s.apply(
            status(ConnectionStatus::Failed("Twitch: token expired".into())),
            0,
        );
        let levels: Vec<Level> = s
            .feed
            .entries()
            .filter_map(|e| match &e.item {
                Item::Notice(n) => Some(n.level),
                _ => None,
            })
            .collect();
        assert_eq!(levels, vec![Level::Warn, Level::Info, Level::Error]);
        assert!(s.connected().is_empty());
    }
}
