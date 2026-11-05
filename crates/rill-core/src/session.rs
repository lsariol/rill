//! The chat core: applies adapter events to the feed and turns composer input into requests.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};

use crate::command::{self, CommandKind, Parsed};
use crate::event::SignInPrompt;
use crate::feed::{Delivery, DeliveryState, Feed, Item, Notice, SentMessage};
use crate::{
    Account, Action, Capabilities, Command, ConnectionStatus, Destination, Event, Level, Platform,
    Request, RequestId, UserRef,
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
}

#[derive(Debug, Clone)]
enum Pending {
    Send { seq: u64 },
    Action { label: String },
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
            Parsed::Command { spec, args } => match spec.kind {
                CommandKind::Local => {
                    match spec.name {
                        "clear" => self.feed.clear(),
                        _ => self.notice(None, Level::Info, &help_text(), now_ms),
                    }
                    Submitted::Local
                }
                CommandKind::Platform(action) => match plan_command(spec.name, args) {
                    Ok(plan) => self.target(plan, action, destination),
                    Err(usage) => Submitted::Rejected(format!("Usage: {usage}")),
                },
            },
        }
    }

    fn send_text(&mut self, text: String, destination: Destination, now_ms: i64) -> Submitted {
        let connected = self.connected();
        let targets: Vec<Platform> = destination
            .targets(&connected)
            .into_iter()
            .filter(|p| self.can(*p, Action::Send))
            .collect();
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
                            reply_to: None,
                        },
                    },
                }
            })
            .collect();
        Submitted::Send(out)
    }

    fn target(&mut self, plan: Plan, action: Action, destination: Destination) -> Submitted {
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

    fn request_id(&mut self) -> RequestId {
        self.next_request += 1;
        RequestId(self.next_request)
    }
}

/// Turns a platform command's arguments into a plan, or returns its usage.
fn plan_command(name: &str, args: &str) -> Result<Plan, &'static str> {
    let usage = command::CommandSpec::find(name).map_or("", |c| c.usage);
    let mut words = args.split_whitespace();
    let user = || -> Result<UserRef, &'static str> {
        let login = args
            .split_whitespace()
            .next()
            .map(|w| w.trim_start_matches('@'))
            .filter(|w| !w.is_empty())
            .ok_or(usage)?;
        Ok(UserRef {
            id: None,
            login: login.to_string(),
        })
    };
    let reason = |skip: usize| {
        let r = args
            .split_whitespace()
            .skip(skip)
            .collect::<Vec<_>>()
            .join(" ");
        (!r.is_empty()).then_some(r)
    };
    match name {
        "poll" => Ok(Plan::PollDialog),
        "ban" => {
            let user = user()?;
            Ok(Plan::Run {
                label: format!("Ban {}", user.login),
                command: Command::Ban {
                    user,
                    reason: reason(1),
                },
            })
        }
        "unban" => {
            let user = user()?;
            Ok(Plan::Run {
                label: format!("Unban {}", user.login),
                command: Command::Unban { user },
            })
        }
        "timeout" => {
            let user = user()?;
            words.next();
            let (seconds, skip) = match words.next().map(command::parse_duration) {
                Some(Some(s)) => (s, 2),
                Some(None) if args.split_whitespace().count() == 2 => return Err(usage),
                _ => (600, 1),
            };
            Ok(Plan::Run {
                label: format!(
                    "Time out {} for {}",
                    user.login,
                    command::format_duration(seconds)
                ),
                command: Command::Timeout {
                    user,
                    seconds,
                    reason: reason(skip),
                },
            })
        }
        _ => Err(usage),
    }
}

fn help_text() -> String {
    let mut out = String::from("Commands:");
    for c in command::COMMANDS {
        out.push_str(&format!("\n{}  {}", c.usage, c.summary));
    }
    out.push_str("\nStart a message with // to send a literal slash.");
    out
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
