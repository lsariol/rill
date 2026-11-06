//! Rill's Rumble adapter. Reads live chat and Rants by polling Rumble's Live Stream API with the
//! private link from the user's Rumble account; read-only, since the API can't send.

pub mod data;

use std::collections::{HashSet, VecDeque};
use std::time::Duration;

use rill_core::backoff::Backoff;
use rill_core::{
    Account, Adapter, AdapterIo, Capabilities, Command, ConnectionStatus, EventSink, Level,
    Platform, Request, adapter::BoxFuture,
};
use serde_json::Value;
use tokio::sync::mpsc;
use tokio::time::{Instant, sleep_until};

/// The credential store key for the Live Stream API link, which carries a private key.
pub const CREDENTIAL_KEY: &str = "rumble";
/// Rumble asks API clients to poll no faster than this.
const POLL: Duration = Duration::from_secs(10);
/// How many message identities are remembered to skip repeats.
const SEEN_MAX: usize = 2_000;
const READ_ONLY: &str =
    "Rumble is read-only: its Live Stream API can't send or moderate. Use rumble.com for that.";
const WHERE: &str = "Copy it from rumble.com/account/livestream-api.";

/// The Rumble connection.
pub struct RumbleAdapter {
    /// Polls at this interval instead of [`POLL`]; tests go faster.
    poll: Duration,
    /// Keeps the link in memory only; tests don't touch the credential store.
    persist: bool,
    link: Option<String>,
}

impl RumbleAdapter {
    /// Uses the link saved in the credential store, if any.
    pub fn new() -> RumbleAdapter {
        let link = match rill_os::credentials::load(CREDENTIAL_KEY) {
            Ok(link) => link.and_then(|l| clean_link(&l)),
            Err(e) => {
                tracing::warn!(err = %e, "Rumble link not loaded");
                None
            }
        };
        RumbleAdapter {
            poll: POLL,
            persist: true,
            link,
        }
    }
}

impl Default for RumbleAdapter {
    fn default() -> Self {
        RumbleAdapter::new()
    }
}

/// Checks a pasted Live Stream API link. Its key is private, so it's never logged.
pub fn clean_link(input: &str) -> Option<String> {
    let url = reqwest::Url::parse(input.trim()).ok()?;
    let host_ok = matches!(url.host_str(), Some("rumble.com" | "www.rumble.com"));
    let path_ok = url.path().starts_with("/-livestream-api/get-data");
    let key_ok = url.query_pairs().any(|(k, v)| k == "key" && !v.is_empty());
    (url.scheme() == "https" && host_ok && path_ok && key_ok).then(|| url.to_string())
}

impl Adapter for RumbleAdapter {
    fn platform(&self) -> Platform {
        Platform::Rumble
    }

    fn run(self: Box<Self>, io: AdapterIo) -> BoxFuture {
        Box::pin(run(*self, io))
    }
}

struct Ctx {
    events: EventSink,
    requests: mpsc::Receiver<Request>,
    http: reqwest::Client,
    poll: Duration,
    persist: bool,
}

/// What made a polling session stop.
enum End {
    Stop,
    /// Forget the link (sign out).
    Forget,
    /// Use this link instead.
    Replace(String),
}

async fn run(adapter: RumbleAdapter, io: AdapterIo) {
    let AdapterIo { events, requests } = io;
    let http = match reqwest::Client::builder()
        .user_agent(concat!("Rill/", env!("CARGO_PKG_VERSION")))
        .timeout(Duration::from_secs(20))
        .build()
    {
        Ok(h) => h,
        Err(e) => {
            let why = format!("Rumble: HTTP setup failed ({e}). Restart Rill.");
            events
                .status(ConnectionStatus::Failed(why), None, Capabilities::NONE)
                .await;
            return;
        }
    };
    let mut ctx = Ctx {
        events,
        requests,
        http,
        poll: adapter.poll,
        persist: adapter.persist,
    };
    // 7TV's global emotes; Rumble has no channel sets.
    let emotes = rill_emotes::follow(ctx.http.clone(), rill_emotes::Config::default(), None);
    ctx.events.set_enrich(Some(emotes.enrich()));
    let mut link = adapter.link;
    loop {
        let end = match &link {
            None => {
                ctx.events
                    .status(ConnectionStatus::Disconnected, None, Capabilities::NONE)
                    .await;
                wait(&mut ctx, None).await
            }
            Some(l) => session(&mut ctx, l).await,
        };
        match end {
            Some(End::Stop) => return,
            Some(End::Forget) => {
                link = None;
                if ctx.persist
                    && let Err(e) = rill_os::credentials::delete(CREDENTIAL_KEY)
                {
                    tracing::warn!(err = %e, "Rumble link not deleted");
                }
            }
            Some(End::Replace(l)) => {
                if ctx.persist
                    && let Err(e) = rill_os::credentials::save(CREDENTIAL_KEY, &l)
                {
                    tracing::warn!(err = %e, "Rumble link not saved");
                }
                link = Some(l);
            }
            None => {}
        }
    }
}

/// Answers requests until `deadline` (or forever). `Some` when the session must change.
async fn wait(ctx: &mut Ctx, deadline: Option<Instant>) -> Option<End> {
    loop {
        let sleeper = async {
            match deadline {
                Some(d) => sleep_until(d).await,
                None => std::future::pending().await,
            }
        };
        tokio::select! {
            _ = sleeper => return None,
            req = ctx.requests.recv() => {
                let Some(req) = req else { return Some(End::Stop) };
                if let Some(end) = answer(ctx, req).await {
                    return Some(end);
                }
            }
        }
    }
}

async fn answer(ctx: &mut Ctx, req: Request) -> Option<End> {
    let (result, end) = match req.command {
        Command::Join { channel } => match clean_link(&channel) {
            Some(link) => (Ok(None), Some(End::Replace(link))),
            None => (
                Err(format!(
                    "Rumble: that isn't a Live Stream API link (https://rumble.com/-livestream-api/get-data?key=…). {WHERE}"
                )),
                None,
            ),
        },
        Command::SignOut => (Ok(None), Some(End::Forget)),
        Command::SignIn => (
            Err(format!(
                "Rumble: paste your Live Stream API link in Accounts. {WHERE}"
            )),
            None,
        ),
        _ => (Err(READ_ONLY.to_string()), None),
    };
    ctx.events.done(req.id, result).await;
    end
}

/// Why a poll failed.
enum Fetch {
    /// Rumble refused the link; polling again won't help.
    Refused(String),
    Retry(String),
}

async fn fetch(http: &reqwest::Client, link: &str) -> Result<data::Snapshot, Fetch> {
    let res = http
        .get(link)
        .header("Accept", "application/json")
        .send()
        .await
        .map_err(|e| {
            // Errors can quote the URL, and the URL holds the key.
            let e = e.without_url();
            Fetch::Retry(format!(
                "Rumble: can't reach rumble.com ({e}). Check the internet connection."
            ))
        })?;
    match res.status().as_u16() {
        200 => {}
        401 | 403 | 404 => {
            return Err(Fetch::Refused(format!(
                "Rumble refused the Live Stream API link ({}). It may have been regenerated: paste the current one in Accounts. {WHERE}",
                res.status().as_u16()
            )));
        }
        429 => return Err(Fetch::Retry("Rumble asked Rill to slow down.".into())),
        s => {
            return Err(Fetch::Retry(format!(
                "Rumble: the API answered {s}; Rill will try again."
            )));
        }
    }
    let v: Value = res
        .json()
        .await
        .map_err(|e| Fetch::Retry(format!("Rumble: unexpected reply ({}).", e.without_url())))?;
    data::parse(&v).ok_or_else(|| {
        Fetch::Refused(format!(
            "Rumble: the link answered, but not with Live Stream API data. {WHERE}"
        ))
    })
}

/// Remembers which messages were shown, so each poll's repeats are skipped.
#[derive(Default)]
struct Seen {
    keys: HashSet<String>,
    order: VecDeque<String>,
}

impl Seen {
    /// True the first time a key is seen.
    fn insert(&mut self, key: String) -> bool {
        if !self.keys.insert(key.clone()) {
            return false;
        }
        self.order.push_back(key);
        if self.order.len() > SEEN_MAX
            && let Some(old) = self.order.pop_front()
        {
            self.keys.remove(&old);
        }
        true
    }

    fn clear(&mut self) {
        self.keys.clear();
        self.order.clear();
    }
}

/// Polls one link until it's replaced, forgotten or refused.
async fn session(ctx: &mut Ctx, link: &str) -> Option<End> {
    ctx.events
        .status(ConnectionStatus::Connecting, None, Capabilities::NONE)
        .await;
    let mut backoff = Backoff::chat();
    let mut account: Option<Account> = None;
    let mut stream: Option<String> = None;
    let mut seen = Seen::default();
    let mut told_offline = false;
    loop {
        let delay = match fetch(&ctx.http, link).await {
            Ok(snap) => {
                backoff.reset();
                if account.as_ref() != Some(&snap.account) {
                    tracing::info!("Rumble connected");
                    account = Some(snap.account.clone());
                }
                ctx.events
                    .status(
                        ConnectionStatus::Connected,
                        account.clone(),
                        Capabilities::NONE,
                    )
                    .await;
                match snap.live {
                    None => {
                        stream = None;
                        if !told_offline {
                            told_offline = true;
                            ctx.events
                                .notice(
                                    Level::Info,
                                    "Rumble: connected. Chat appears here once you go live.",
                                )
                                .await;
                        }
                    }
                    Some(live) => {
                        told_offline = false;
                        // A new stream: what's already in its chat counts as seen.
                        let fresh = stream.as_deref() != Some(live.id.as_str());
                        if fresh {
                            seen.clear();
                            stream = Some(live.id.clone());
                        }
                        for m in &live.messages {
                            if seen.insert(m.key())
                                && !fresh
                                && !ctx.events.message(m.to_message()).await
                            {
                                return Some(End::Stop);
                            }
                        }
                    }
                }
                ctx.poll
            }
            Err(Fetch::Refused(why)) => {
                tracing::warn!("Rumble link refused");
                ctx.events
                    .status(
                        ConnectionStatus::Failed(why),
                        account.clone(),
                        Capabilities::NONE,
                    )
                    .await;
                return wait(ctx, None).await;
            }
            Err(Fetch::Retry(why)) => {
                let attempt = backoff.attempt() + 1;
                tracing::warn!(err = %why, "Rumble poll failed");
                ctx.events
                    .status(
                        ConnectionStatus::Reconnecting { attempt },
                        account.clone(),
                        Capabilities::NONE,
                    )
                    .await;
                if attempt == 1 {
                    ctx.events.notice(Level::Warn, why).await;
                }
                backoff.next_delay().max(ctx.poll)
            }
        };
        if let Some(end) = wait(ctx, Some(Instant::now() + delay)).await {
            return Some(end);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rill_core::{Event, RequestId};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    const DATA: &str = include_str!("../tests/fixtures/get_data.json");

    #[test]
    fn accepts_only_live_stream_api_links() {
        let ok = "https://rumble.com/-livestream-api/get-data?key=abc123";
        assert_eq!(clean_link(&format!("  {ok} ")).as_deref(), Some(ok));
        assert!(clean_link("https://www.rumble.com/-livestream-api/get-data?key=k").is_some());
        for bad in [
            "http://rumble.com/-livestream-api/get-data?key=abc",
            "https://rumble.com/-livestream-api/get-data",
            "https://rumble.com/-livestream-api/get-data?key=",
            "https://evil.example/-livestream-api/get-data?key=abc",
            "https://rumble.com/c/somechannel",
            "not a link",
        ] {
            assert_eq!(clean_link(bad), None, "{bad}");
        }
    }

    /// Serves the fixture; from the third poll on, one new message and one new Rant appear.
    async fn rumble_server(status: u16) -> (String, Arc<AtomicUsize>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let polls = Arc::new(AtomicUsize::new(0));
        let count = polls.clone();
        tokio::spawn(async move {
            loop {
                let (mut s, _) = listener.accept().await.unwrap();
                let n = count.fetch_add(1, Ordering::SeqCst);
                let mut buf = [0; 4096];
                let _ = s.read(&mut buf).await;
                let mut v: Value = serde_json::from_str(DATA).unwrap();
                if n >= 2 {
                    let chat = &mut v["livestreams"][1]["chat"];
                    chat["recent_messages"].as_array_mut().unwrap().push(serde_json::json!(
                        {"username": "late", "badges": [], "text": "just got here", "created_on": "2026-11-06T06:01:00+00:00"}
                    ));
                    chat["recent_rants"].as_array_mut().unwrap().push(serde_json::json!(
                        {"username": "rich", "badges": [], "text": "hi", "created_on": "2026-11-06T06:01:01+00:00", "amount_cents": 2000}
                    ));
                }
                let body = v.to_string();
                let res = format!(
                    "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = s.write_all(res.as_bytes()).await;
            }
        });
        (format!("http://{addr}"), polls)
    }

    fn start(
        link: &str,
    ) -> (
        mpsc::Receiver<Event>,
        mpsc::Sender<Request>,
        tokio::task::JoinHandle<()>,
    ) {
        let (events, rx) = EventSink::channel(Platform::Rumble, 1_000);
        let (tx, requests) = mpsc::channel(4);
        let link = link.to_string();
        let task = tokio::spawn(async move {
            let mut ctx = Ctx {
                events,
                requests,
                http: reqwest::Client::new(),
                poll: Duration::from_millis(50),
                persist: false,
            };
            session(&mut ctx, &link).await;
        });
        (rx, tx, task)
    }

    async fn until(rx: &mut mpsc::Receiver<Event>, want: impl Fn(&Event) -> bool) -> Vec<Event> {
        let mut seen = Vec::new();
        let found = tokio::time::timeout(Duration::from_secs(10), async {
            while let Some(e) = rx.recv().await {
                let hit = want(&e);
                seen.push(e);
                if hit {
                    return;
                }
            }
        })
        .await;
        assert!(found.is_ok(), "timed out; saw {seen:#?}");
        seen
    }

    #[tokio::test]
    async fn shows_only_new_messages_and_rants_once() {
        let (base, polls) = rumble_server(200).await;
        let (mut rx, tx, task) = start(&base);
        let seen = until(
            &mut rx,
            |e| matches!(e, Event::Message(m) if m.plain_text().starts_with("Rant $20.00")),
        )
        .await;
        let texts: Vec<String> = seen
            .iter()
            .filter_map(|e| match e {
                Event::Message(m) => Some(m.plain_text()),
                _ => None,
            })
            .collect();
        // What was in chat at connect isn't replayed; the new message and Rant appear once.
        assert_eq!(texts, ["just got here", "Rant $20.00: hi"]);
        assert!(seen.iter().any(|e| matches!(
            e,
            Event::Status { status: ConnectionStatus::Connected, account: Some(a), .. } if a.display_name == "Some Streamer"
        )));
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(polls.load(Ordering::SeqCst) >= 4);
        let repeats: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok())
            .filter(|e| matches!(e, Event::Message(_)))
            .collect();
        assert!(repeats.is_empty(), "repeated: {repeats:#?}");

        // Read-only, and a new link replaces the old one.
        tx.send(Request {
            id: RequestId(1),
            command: Command::Send {
                text: "hi".into(),
                reply_to: None,
            },
        })
        .await
        .unwrap();
        let seen = until(&mut rx, |e| matches!(e, Event::Done { .. })).await;
        assert!(
            matches!(seen.last(), Some(Event::Done { result: Err(why), .. }) if why.contains("read-only"))
        );
        drop(tx);
        task.await.unwrap();
    }

    #[tokio::test]
    async fn a_refused_link_says_how_to_fix_it_and_never_shows_the_key() {
        let (base, _) = rumble_server(403).await;
        let link = format!("{base}/-livestream-api/get-data?key=SECRETKEY");
        let (mut rx, tx, task) = start(&link);
        let seen = until(&mut rx, |e| {
            matches!(
                e,
                Event::Status {
                    status: ConnectionStatus::Failed(_),
                    ..
                }
            )
        })
        .await;
        let Some(Event::Status {
            status: ConnectionStatus::Failed(why),
            ..
        }) = seen.last()
        else {
            unreachable!()
        };
        assert!(
            why.contains("livestream-api") && why.contains("403"),
            "{why}"
        );
        assert!(!why.contains("SECRETKEY"));
        drop(tx);
        task.await.unwrap();
    }

    #[tokio::test]
    async fn network_errors_never_show_the_key() {
        let link = "http://127.0.0.1:9/-livestream-api/get-data?key=SECRETKEY";
        let (mut rx, tx, task) = start(link);
        let seen = until(&mut rx, |e| matches!(e, Event::Notice { .. })).await;
        let Some(Event::Notice { text, .. }) = seen.last() else {
            unreachable!()
        };
        assert!(!text.contains("SECRETKEY"), "{text}");
        drop(tx);
        task.await.unwrap();
    }
}
