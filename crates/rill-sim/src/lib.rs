//! Simulated chat traffic: an adapter that behaves like a busy platform, for prototyping and benchmarks.

use std::collections::VecDeque;
use std::time::Duration;

use rill_core::backoff::SmallRng;
use rill_core::{
    Account, Action, Adapter, AdapterIo, Author, Badge, Capabilities, ChatMessage, Command,
    ConnectionStatus, Event, EventSink, Fragment, Level, MessageKind, Platform, Request, Roles,
    adapter::BoxFuture, merge_text, now_ms,
};
use tokio::time::{MissedTickBehavior, interval, sleep};

/// The simulated signed-in user.
pub const ME: &str = "rill_dev";

/// How busy and how unreliable the simulated chat is.
#[derive(Debug, Clone, PartialEq)]
pub struct Profile {
    /// Messages per second.
    pub rate: f64,
    /// Every 20 s, ten times the rate for 3 s.
    pub bursts: bool,
    /// Most words are emotes.
    pub emote_heavy: bool,
    /// Share of sends and actions that fail, 0 to 1.
    pub fail_rate: f64,
    /// Drops the connection now and then.
    pub flaky: bool,
    pub seed: u64,
}

impl Default for Profile {
    fn default() -> Self {
        Profile {
            rate: 4.0,
            bursts: false,
            emote_heavy: false,
            fail_rate: 0.05,
            flaky: false,
            seed: 7,
        }
    }
}

/// A simulated platform connection.
pub struct SimAdapter {
    platform: Platform,
    profile: Profile,
}

impl SimAdapter {
    pub fn new(platform: Platform, profile: Profile) -> SimAdapter {
        SimAdapter { platform, profile }
    }

    /// What the simulated platform supports, mirroring the real ones.
    pub fn capabilities(platform: Platform) -> Capabilities {
        use Action::*;
        match platform {
            Platform::Twitch => Capabilities::of(&[Send, Reply, Delete, Timeout, Ban, Unban, Poll]),
            Platform::YouTube | Platform::Kick => {
                Capabilities::of(&[Send, Delete, Timeout, Ban, Unban])
            }
        }
    }
}

impl Adapter for SimAdapter {
    fn platform(&self) -> Platform {
        self.platform
    }

    fn run(self: Box<Self>, io: AdapterIo) -> BoxFuture {
        Box::pin(run(*self, io))
    }
}

/// Global emotes with their Twitch CDN images; names only when offline.
const EMOTES: &[(&str, &str)] = &[
    ("Kappa", "25"),
    ("PogChamp", "305954156"),
    ("LUL", "425618"),
    ("BibleThump", "86"),
    ("Kreygasm", "41"),
    ("ResidentSleeper", "245"),
    ("4Head", "354"),
    ("SeemsGood", "64138"),
    ("NotLikeThis", "58765"),
    ("HeyGuys", "30259"),
    ("VoHiYo", "81274"),
    ("DansGame", "33"),
];

const WORDS: &[&str] = &[
    "the",
    "stream",
    "is",
    "so",
    "good",
    "today",
    "what",
    "game",
    "next",
    "lol",
    "that",
    "was",
    "clean",
    "insane",
    "play",
    "chat",
    "hello",
    "from",
    "brazil",
    "germany",
    "first",
    "time",
    "here",
    "love",
    "this",
    "song",
    "why",
    "did",
    "he",
    "do",
    "that",
    "gg",
    "wp",
    "no",
    "way",
    "clip",
    "it",
    "how",
    "long",
    "have",
    "you",
    "been",
    "streaming",
    "boss",
    "fight",
    "again",
    "almost",
    "had",
    "it",
    "nice",
    "build",
    "check",
    "the",
    "map",
    "behind",
    "you",
    "run",
];

const SYLLABLES: &[&str] = &[
    "ka", "ro", "mi", "zen", "tor", "ix", "va", "lu", "ne", "shi", "pa", "dor", "qu", "el", "fy",
    "ga", "mo", "rin", "tek", "sol",
];

const COLORS: &[u32] = &[
    0xff7f50, 0x1e90ff, 0x9acd32, 0xff69b4, 0x00ced1, 0xdaa520, 0xb22222, 0x8a2be2, 0x2e8b57,
    0xd2691e,
];

struct Sim {
    platform: Platform,
    profile: Profile,
    rng: SmallRng,
    users: Vec<Author>,
    recent: VecDeque<String>,
    next_id: u64,
    me: Account,
}

impl Sim {
    fn new(platform: Platform, profile: Profile) -> Sim {
        let mut rng = SmallRng::seeded(profile.seed ^ (platform as u64 + 1) * 0x9E37);
        let users = (0..300).map(|i| make_user(&mut rng, platform, i)).collect();
        Sim {
            platform,
            profile,
            rng,
            users,
            recent: VecDeque::new(),
            next_id: 0,
            me: Account {
                id: "sim-me".into(),
                login: ME.into(),
                display_name: "Rill_Dev".into(),
            },
        }
    }

    fn id(&mut self) -> String {
        self.next_id += 1;
        format!(
            "sim-{}-{}",
            self.platform.name().to_lowercase(),
            self.next_id
        )
    }

    fn message(&mut self) -> ChatMessage {
        let author = self.rng.pick(&self.users).clone();
        let long = self.rng.unit() < 0.05;
        let words = if long {
            30 + self.rng.below(30)
        } else {
            1 + self.rng.below(14)
        };
        let emote_share = if self.profile.emote_heavy { 0.6 } else { 0.12 };
        let mut fragments = Vec::new();
        for i in 0..words {
            if i > 0 {
                fragments.push(Fragment::Text(" ".into()));
            }
            let roll = self.rng.unit();
            if roll < emote_share {
                let (name, id) = *self.rng.pick(EMOTES);
                fragments.push(Fragment::Emote {
                    name: name.into(),
                    url: Some(format!(
                        "https://static-cdn.jtvnw.net/emoticons/v2/{id}/default/dark/1.0"
                    )),
                });
            } else if roll < emote_share + 0.004 {
                fragments.push(Fragment::Mention(ME.into()));
            } else if roll < emote_share + 0.006 {
                fragments.push(Fragment::Link("https://example.com/clip".into()));
            } else {
                fragments.push(Fragment::Text((*self.rng.pick(WORDS)).into()));
            }
        }
        let id = self.id();
        self.recent.push_back(id.clone());
        if self.recent.len() > 50 {
            self.recent.pop_front();
        }
        let kind = if self.rng.unit() < 0.01 {
            MessageKind::Action
        } else {
            MessageKind::Chat
        };
        ChatMessage {
            platform: self.platform,
            id,
            author,
            fragments: merge_text(fragments),
            kind,
            sent_at_ms: now_ms(),
            reply_to: None,
        }
    }

    fn fails(&mut self) -> bool {
        self.rng.unit() < self.profile.fail_rate
    }

    fn user_id(&self, login: &str) -> Option<String> {
        self.users
            .iter()
            .find(|u| u.login.eq_ignore_ascii_case(login))
            .map(|u| u.id.clone())
    }
}

fn make_user(rng: &mut SmallRng, platform: Platform, i: usize) -> Author {
    let mut name = String::new();
    for _ in 0..2 + rng.below(2) {
        name.push_str(rng.pick(SYLLABLES));
    }
    if rng.unit() < 0.4 {
        name.push_str(&rng.below(100).to_string());
    }
    let mut display: String = name.clone();
    if let Some(first) = display.get_mut(0..1) {
        first.make_ascii_uppercase();
    }
    let roles = Roles {
        broadcaster: false,
        moderator: rng.unit() < 0.04,
        vip: rng.unit() < 0.05,
        subscriber: rng.unit() < 0.3,
    };
    let mut badges = Vec::new();
    for (has, id, title) in [
        (roles.moderator, "moderator/1", "Moderator"),
        (roles.vip, "vip/1", "VIP"),
        (roles.subscriber, "subscriber/0", "Subscriber"),
    ] {
        if has {
            badges.push(Badge {
                id: id.into(),
                title: title.into(),
                image_url: None,
            });
        }
    }
    Author {
        id: format!("sim-{}-u{i}", platform.name().to_lowercase()),
        login: name,
        display_name: display,
        color: Some(*rng.pick(COLORS)),
        badges,
        roles,
    }
}

async fn run(adapter: SimAdapter, io: AdapterIo) {
    let AdapterIo {
        events,
        mut requests,
    } = io;
    let caps = SimAdapter::capabilities(adapter.platform);
    let mut sim = Sim::new(adapter.platform, adapter.profile);
    let account = Some(sim.me.clone());
    events
        .status(ConnectionStatus::Connecting, None, caps)
        .await;
    sleep(Duration::from_millis(300)).await;
    events
        .status(ConnectionStatus::Connected, account.clone(), caps)
        .await;

    let step = Duration::from_millis(20);
    let mut tick = interval(step);
    tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let active = sim.profile.rate > 0.0;
    let mut owed = 0.0;
    let mut elapsed = Duration::ZERO;
    let mut next_drop = Duration::from_secs(60 + sim.rng.below(60));
    loop {
        tokio::select! {
            req = requests.recv() => match req {
                Some(req) => handle(&mut sim, &events, req),
                None => return,
            },
            _ = tick.tick(), if active => {
                elapsed += step;
                let burst = sim.profile.bursts && elapsed.as_secs() % 20 < 3;
                owed += sim.profile.rate * if burst { 10.0 } else { 1.0 } * step.as_secs_f64();
                while owed >= 1.0 {
                    owed -= 1.0;
                    let m = sim.message();
                    if !events.message(m).await {
                        return;
                    }
                }
                if sim.rng.unit() < step.as_secs_f64() / 45.0
                    && let Some(id) = sim.recent.pop_front()
                {
                    events.send(Event::Deleted { platform: sim.platform, message_id: id }).await;
                }
                if sim.profile.flaky && elapsed >= next_drop {
                    next_drop = elapsed + Duration::from_secs(60 + sim.rng.below(60));
                    events.status(ConnectionStatus::Reconnecting { attempt: 1 }, None, caps).await;
                    sleep(Duration::from_secs(2)).await;
                    events.status(ConnectionStatus::Connected, account.clone(), caps).await;
                }
            }
        }
    }
}

/// Answers a request after a short, realistic delay, without holding up chat.
fn handle(sim: &mut Sim, events: &EventSink, req: Request) {
    let delay = Duration::from_millis(60 + sim.rng.below(140));
    let fail = sim.fails();
    let platform = sim.platform;
    let mut follow_up: Vec<Event> = Vec::new();
    let result: Result<Option<String>, String> = if fail {
        Err(format!(
            "{platform}: rate limited (simulated). Wait a few seconds and try again."
        ))
    } else {
        match &req.command {
            Command::Send { text, .. } => {
                let id = sim.id();
                let mut author = Author {
                    id: sim.me.id.clone(),
                    login: sim.me.login.clone(),
                    display_name: sim.me.display_name.clone(),
                    color: Some(0x53fc18),
                    ..Author::default()
                };
                author.roles.broadcaster = true;
                follow_up.push(Event::Message(ChatMessage {
                    platform,
                    id: id.clone(),
                    author,
                    fragments: rill_core::split_text(text),
                    kind: MessageKind::Chat,
                    sent_at_ms: now_ms(),
                    reply_to: None,
                }));
                Ok(Some(id))
            }
            Command::Delete { message_id } => {
                follow_up.push(Event::Deleted {
                    platform,
                    message_id: message_id.clone(),
                });
                Ok(None)
            }
            Command::Timeout { user, .. } | Command::Ban { user, .. } => {
                match user.id.clone().or_else(|| sim.user_id(&user.login)) {
                    Some(user_id) => {
                        follow_up.push(Event::UserCleared { platform, user_id });
                        Ok(None)
                    }
                    None => Err(format!("{platform}: no user named \"{}\".", user.login)),
                }
            }
            Command::Unban { .. } => Ok(None),
            Command::Poll { title, .. } => {
                follow_up.push(Event::Notice {
                    platform,
                    level: Level::Info,
                    text: format!("Poll started: {title}"),
                });
                Ok(None)
            }
        }
    };
    let echo_first = sim.rng.unit() < 0.5;
    let events = events.clone();
    tokio::spawn(async move {
        sleep(delay).await;
        if echo_first {
            for e in follow_up.drain(..) {
                events.send(e).await;
            }
        }
        events.done(req.id, result).await;
        for e in follow_up {
            events.send(e).await;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use rill_core::{Request, RequestId, UserRef};
    use tokio::sync::mpsc;

    async fn start(profile: Profile) -> (mpsc::Sender<Request>, mpsc::Receiver<Event>) {
        let (sink, rx) = EventSink::channel(Platform::Twitch, 10_000);
        let (req_tx, req_rx) = mpsc::channel(16);
        let adapter = Box::new(SimAdapter::new(Platform::Twitch, profile));
        tokio::spawn(adapter.run(AdapterIo {
            events: sink,
            requests: req_rx,
        }));
        (req_tx, rx)
    }

    #[tokio::test(start_paused = true)]
    async fn produces_messages_at_the_rate() {
        let (_tx, mut rx) = start(Profile {
            rate: 50.0,
            ..Profile::default()
        })
        .await;
        sleep(Duration::from_millis(2_300)).await;
        let mut messages = 0;
        while let Ok(e) = rx.try_recv() {
            if matches!(e, Event::Message(_)) {
                messages += 1;
            }
        }
        assert!((90..=110).contains(&messages), "got {messages}");
    }

    #[tokio::test(start_paused = true)]
    async fn answers_sends_and_bans() {
        let (tx, mut rx) = start(Profile {
            rate: 0.0,
            fail_rate: 0.0,
            ..Profile::default()
        })
        .await;
        sleep(Duration::from_millis(400)).await;
        let send = Command::Send {
            text: "hi".into(),
            reply_to: None,
        };
        tx.send(Request {
            id: RequestId(1),
            command: send,
        })
        .await
        .unwrap();
        tx.send(Request {
            id: RequestId(2),
            command: Command::Ban {
                user: UserRef {
                    id: Some("x".into()),
                    login: "x".into(),
                },
                reason: None,
            },
        })
        .await
        .unwrap();
        sleep(Duration::from_millis(500)).await;
        let mut done = Vec::new();
        let mut echoed = false;
        let mut cleared = false;
        while let Ok(e) = rx.try_recv() {
            match e {
                Event::Done {
                    request, result, ..
                } => done.push((request, result.is_ok())),
                Event::Message(m) if m.author.id == "sim-me" => echoed = true,
                Event::UserCleared { user_id, .. } => cleared = user_id == "x",
                _ => {}
            }
        }
        done.sort();
        assert_eq!(done, vec![(RequestId(1), true), (RequestId(2), true)]);
        assert!(echoed && cleared);
    }
}
