//! Rill's YouTube adapter: Google desktop sign-in, and the live chat of your active broadcast.

pub mod api;
pub mod auth;
pub mod chat;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use rill_core::backoff::Backoff;
use rill_core::{
    Account, Action, Adapter, AdapterIo, Capabilities, Command, ConnectionStatus, Event, EventSink,
    Level, Platform, Request, SignInPrompt, UserRef, adapter::BoxFuture, now_ms,
};
use tokio::sync::{Mutex, mpsc};
use tokio::time::{Instant, sleep_until};

use crate::api::{Api, ApiError};
use crate::auth::{AuthError, Client, Token};

const BUILTIN_CLIENT_ID: Option<&str> = option_env!("RILL_YOUTUBE_CLIENT_ID");
const BUILTIN_CLIENT_SECRET: Option<&str> = option_env!("RILL_YOUTUBE_CLIENT_SECRET");
const NOT_CONNECTED: &str = "YouTube isn't connected. Open Accounts to connect it.";
const NOT_LIVE: &str = "YouTube: you aren't live, so there's no chat to act on yet.";
/// How often to look for a live broadcast while not live.
const FIND_EVERY: Duration = Duration::from_secs(60);
/// After the daily quota runs out, how long before trying again.
const QUOTA_WAIT: Duration = Duration::from_secs(15 * 60);
/// Units of daily quota after which Rill warns once.
const QUOTA_WARN: u64 = 8_000;
/// Authors remembered for `/ban name`.
const AUTHOR_LIMIT: usize = 5_000;

const OWNER: Capabilities = Capabilities::of(&[
    Action::Send,
    Action::Delete,
    Action::Timeout,
    Action::Ban,
    Action::Unban,
]);

/// The YouTube connection.
pub struct YouTubeAdapter {
    client: Option<Client>,
    min_poll: Duration,
}

impl YouTubeAdapter {
    pub fn new(settings: &rill_os::settings::YouTubeSettings) -> YouTubeAdapter {
        let pick = |s: &Option<String>, built_in: Option<&str>| {
            s.clone()
                .filter(|v| !v.trim().is_empty())
                .or_else(|| built_in.map(str::to_string))
        };
        let client = match (
            pick(&settings.client_id, BUILTIN_CLIENT_ID),
            pick(&settings.client_secret, BUILTIN_CLIENT_SECRET),
        ) {
            (Some(id), Some(secret)) => Some(Client { id, secret }),
            _ => None,
        };
        YouTubeAdapter {
            client,
            min_poll: Duration::from_millis(settings.min_poll_ms.unwrap_or(5_000).max(1_000)),
        }
    }
}

impl Adapter for YouTubeAdapter {
    fn platform(&self) -> Platform {
        Platform::YouTube
    }

    fn run(self: Box<Self>, io: AdapterIo) -> BoxFuture {
        Box::pin(run(*self, io))
    }
}

struct Ctx {
    events: EventSink,
    requests: mpsc::Receiver<Request>,
    http: reqwest::Client,
    client: Client,
    min_poll: Duration,
    /// The Data API's address; tests use a local server.
    api_base: String,
    /// Where to load 7TV's global emotes from; tests leave it out.
    seven_tv: Option<rill_emotes::Config>,
}

enum Outcome {
    Stop,
    SignedOut,
    Failed(String),
}

/// The live chat being followed, shared with request tasks.
#[derive(Clone, Default)]
struct Live {
    chat: Arc<Mutex<Option<String>>>,
    /// Lowercased name → channel ID, for `/ban name`.
    authors: Arc<Mutex<HashMap<String, String>>>,
    /// Channel ID → ban ID, to unban.
    bans: Arc<Mutex<HashMap<String, String>>>,
}

async fn run(adapter: YouTubeAdapter, io: AdapterIo) {
    let AdapterIo {
        events,
        mut requests,
    } = io;
    let http = reqwest::Client::builder()
        .user_agent(concat!("Rill/", env!("CARGO_PKG_VERSION")))
        .timeout(Duration::from_secs(20))
        .build();
    let (http, client) = match (http, adapter.client) {
        (Ok(h), Some(c)) => (h, c),
        (Err(e), _) => {
            fail_forever(
                &events,
                &mut requests,
                format!("YouTube: HTTP setup failed ({e}). Restart Rill."),
            )
            .await;
            return;
        }
        (_, None) => {
            let why = format!(
                "YouTube: no Google OAuth client is set. Add \"youtube\": {{\"client_id\": \"…\", \"client_secret\": \"…\"}} to {}.",
                rill_os::settings::path().display()
            );
            fail_forever(&events, &mut requests, why).await;
            return;
        }
    };
    let mut ctx = Ctx {
        events,
        requests,
        http,
        client,
        min_poll: adapter.min_poll,
        api_base: api::API.into(),
        seven_tv: Some(rill_emotes::Config::default()),
    };
    let mut failure: Option<String> = None;
    loop {
        let stored = match auth::load_token() {
            Ok(t) => t,
            Err(e) => {
                ctx.events.notice(Level::Error, e).await;
                None
            }
        };
        let token = match stored {
            Some(t) => t,
            None => {
                let status = failure
                    .take()
                    .map_or(ConnectionStatus::Disconnected, ConnectionStatus::Failed);
                ctx.events.status(status, None, Capabilities::NONE).await;
                match wait_for_sign_in(&mut ctx).await {
                    Some(t) => t,
                    None => return,
                }
            }
        };
        match session(&mut ctx, token).await {
            Outcome::Stop => return,
            Outcome::SignedOut => {}
            Outcome::Failed(why) => failure = Some(why),
        }
    }
}

async fn fail_forever(events: &EventSink, requests: &mut mpsc::Receiver<Request>, why: String) {
    tracing::error!(err = %why, "YouTube adapter can't start");
    events
        .status(
            ConnectionStatus::Failed(why.clone()),
            None,
            Capabilities::NONE,
        )
        .await;
    while let Some(req) = requests.recv().await {
        events.done(req.id, Err(why.clone())).await;
    }
}

async fn wait_for_sign_in(ctx: &mut Ctx) -> Option<Token> {
    loop {
        let req = ctx.requests.recv().await?;
        match req.command {
            Command::SignIn => match browser_flow(ctx).await {
                Ok(Some(token)) => {
                    ctx.events.done(req.id, Ok(None)).await;
                    return Some(token);
                }
                Ok(None) => return None,
                Err(why) => {
                    ctx.events.done(req.id, Err(why.clone())).await;
                    ctx.events
                        .status(ConnectionStatus::Failed(why), None, Capabilities::NONE)
                        .await;
                }
            },
            Command::SignOut => {
                ctx.events.done(req.id, Ok(None)).await;
            }
            _ => {
                ctx.events.done(req.id, Err(NOT_CONNECTED.into())).await;
            }
        }
    }
}

/// Shows Google's consent page link and waits for the redirect.
async fn browser_flow(ctx: &mut Ctx) -> Result<Option<Token>, String> {
    ctx.events
        .status(ConnectionStatus::Connecting, None, Capabilities::NONE)
        .await;
    let pending = auth::start(&ctx.client).await.map_err(|e| e.message())?;
    ctx.events
        .send(Event::SignIn {
            platform: Platform::YouTube,
            prompt: SignInPrompt {
                url: pending.url.clone(),
                code: None,
                expires_at_ms: now_ms() + 10 * 60_000,
            },
        })
        .await;
    let finish = auth::finish(&ctx.http, &ctx.client, pending);
    tokio::pin!(finish);
    let deadline = Instant::now() + Duration::from_secs(10 * 60);
    loop {
        tokio::select! {
            result = &mut finish => {
                let token = result.map_err(|e| e.message())?;
                if let Err(e) = auth::save_token(&token) {
                    ctx.events.notice(Level::Warn, format!("{e} You'll need to sign in again next time.")).await;
                }
                return Ok(Some(token));
            }
            _ = sleep_until(deadline) => return Err("YouTube: sign-in timed out. Open Accounts and try again.".into()),
            req = ctx.requests.recv() => {
                let Some(req) = req else { return Ok(None) };
                match req.command {
                    Command::SignOut => {
                        ctx.events.done(req.id, Ok(None)).await;
                        return Err("YouTube: sign-in cancelled.".into());
                    }
                    _ => {
                ctx.events.done(req.id, Err(NOT_CONNECTED.into())).await;
            }
                }
            }
        }
    }
}

fn expired() -> Outcome {
    if let Err(e) = auth::delete_token() {
        tracing::error!(err = %e, "YouTube token not deleted");
    }
    Outcome::Failed(AuthError::Invalid.message())
}

/// A signed-in session: find the live chat, then poll it.
async fn session(ctx: &mut Ctx, token: Token) -> Outcome {
    let api = Api::new(ctx.http.clone(), ctx.client.clone(), token, &ctx.api_base);
    let live = Live::default();
    let mut backoff = Backoff::chat();
    ctx.events
        .status(ConnectionStatus::Connecting, None, Capabilities::NONE)
        .await;
    let account = loop {
        match api.my_channel().await {
            Ok((id, title)) => {
                break Account {
                    id,
                    login: title.clone(),
                    display_name: title,
                };
            }
            Err(ApiError::Unauthorized) => return expired(),
            Err(e @ ApiError::Api { status: 404, .. }) => return Outcome::Failed(e.message()),
            Err(e) => {
                tracing::warn!(err = %e.message(), "YouTube channel lookup failed");
                let attempt = backoff.attempt() + 1;
                ctx.events
                    .status(
                        ConnectionStatus::Reconnecting { attempt },
                        None,
                        Capabilities::NONE,
                    )
                    .await;
                if let Some(end) =
                    pause(ctx, &api, &live, Instant::now() + backoff.next_delay()).await
                {
                    return end;
                }
            }
        }
    };
    tracing::info!("YouTube signed in");
    // 7TV has no YouTube channel sets; its global emotes still apply.
    let _emotes = ctx.seven_tv.clone().map(|cfg| {
        let f = rill_emotes::follow(ctx.http.clone(), cfg, None);
        ctx.events.set_enrich(Some(f.enrich()));
        f
    });
    let mut told_not_live = false;
    let mut told_not_enabled = false;
    let mut warned_quota = false;
    loop {
        let chat = match api.active_chat().await {
            Ok(Some(chat)) => chat,
            Ok(None) => {
                ctx.events
                    .status(
                        ConnectionStatus::Connected,
                        Some(account.clone()),
                        Capabilities::NONE,
                    )
                    .await;
                if !told_not_live {
                    told_not_live = true;
                    ctx.events
                        .notice(
                            Level::Info,
                            "YouTube: signed in. Chat appears here once you go live.",
                        )
                        .await;
                }
                if let Some(end) = pause(ctx, &api, &live, Instant::now() + FIND_EVERY).await {
                    return end;
                }
                continue;
            }
            Err(ApiError::Unauthorized) => return expired(),
            Err(ApiError::NotEnabled) => {
                // Signed in fine; YouTube just won't list broadcasts until streaming is turned on.
                ctx.events
                    .status(
                        ConnectionStatus::Connected,
                        Some(account.clone()),
                        Capabilities::NONE,
                    )
                    .await;
                if !told_not_enabled {
                    told_not_enabled = true;
                    tracing::warn!("YouTube live streaming is off for this channel");
                    ctx.events
                        .notice(Level::Warn, ApiError::NotEnabled.message())
                        .await;
                }
                if let Some(end) = pause(ctx, &api, &live, Instant::now() + FIND_EVERY).await {
                    return end;
                }
                continue;
            }
            Err(ApiError::Quota) => {
                if let Some(end) = quota_pause(ctx, &api, &live, &account).await {
                    return end;
                }
                continue;
            }
            Err(e) => {
                tracing::warn!(err = %e.message(), "YouTube broadcast lookup failed");
                if let Some(end) =
                    pause(ctx, &api, &live, Instant::now() + backoff.next_delay()).await
                {
                    return end;
                }
                continue;
            }
        };
        told_not_live = false;
        told_not_enabled = false;
        *live.chat.lock().await = Some(chat.clone());
        ctx.events
            .status(ConnectionStatus::Connected, Some(account.clone()), OWNER)
            .await;
        let mut page_token: Option<String> = None;
        loop {
            match api.messages(&chat, page_token.as_deref()).await {
                Ok(v) => {
                    backoff.reset();
                    let page = chat::page(&v);
                    remember_authors(&live, &page.events).await;
                    for e in page.events {
                        if !ctx.events.send(e).await {
                            return Outcome::Stop;
                        }
                    }
                    if page.offline {
                        ctx.events
                            .notice(Level::Info, "YouTube: the stream's chat went offline.")
                            .await;
                        break;
                    }
                    page_token = page.next_page_token;
                    if !warned_quota && api.units() > QUOTA_WARN {
                        warned_quota = true;
                        ctx.events.notice(Level::Warn, format!(
                            "YouTube: Rill has used about {} of the usual 10,000 daily quota units. Set youtube.min_poll_ms higher in settings.json to stretch it.",
                            api.units()
                        )).await;
                    }
                    let wait = Duration::from_millis(page.wait_ms).max(ctx.min_poll);
                    if let Some(end) = pause(ctx, &api, &live, Instant::now() + wait).await {
                        return end;
                    }
                }
                Err(ApiError::Unauthorized) => return expired(),
                Err(ApiError::ChatEnded) => {
                    ctx.events
                        .notice(Level::Info, ApiError::ChatEnded.message())
                        .await;
                    break;
                }
                Err(ApiError::Quota) => {
                    if let Some(end) = quota_pause(ctx, &api, &live, &account).await {
                        return end;
                    }
                }
                Err(e) => {
                    tracing::warn!(err = %e.message(), "YouTube chat poll failed");
                    let attempt = backoff.attempt() + 1;
                    ctx.events
                        .status(
                            ConnectionStatus::Reconnecting { attempt },
                            Some(account.clone()),
                            OWNER,
                        )
                        .await;
                    if let Some(end) =
                        pause(ctx, &api, &live, Instant::now() + backoff.next_delay()).await
                    {
                        return end;
                    }
                    ctx.events
                        .status(ConnectionStatus::Connected, Some(account.clone()), OWNER)
                        .await;
                }
            }
        }
        *live.chat.lock().await = None;
    }
}

async fn quota_pause(ctx: &mut Ctx, api: &Api, live: &Live, account: &Account) -> Option<Outcome> {
    ctx.events
        .status(
            ConnectionStatus::Reconnecting { attempt: 1 },
            Some(account.clone()),
            Capabilities::NONE,
        )
        .await;
    ctx.events
        .notice(Level::Error, ApiError::Quota.message())
        .await;
    pause(ctx, api, live, Instant::now() + QUOTA_WAIT).await
}

async fn remember_authors(live: &Live, events: &[Event]) {
    let mut authors = live.authors.lock().await;
    if authors.len() > AUTHOR_LIMIT {
        authors.clear();
    }
    for e in events {
        if let Event::Message(m) = e {
            authors.insert(m.author.login.to_lowercase(), m.author.id.clone());
        }
    }
}

/// Waits until `until`, answering requests. Returns an outcome for sign-out or shutdown.
async fn pause(ctx: &mut Ctx, api: &Api, live: &Live, until: Instant) -> Option<Outcome> {
    loop {
        tokio::select! {
            _ = sleep_until(until) => return None,
            req = ctx.requests.recv() => {
                let Some(req) = req else { return Some(Outcome::Stop) };
                match req.command {
                    Command::SignOut => {
                        auth::revoke(&ctx.http, &api.token().await).await;
                        let result = auth::delete_token().map(|_| None);
                        ctx.events.done(req.id, result).await;
                        return Some(Outcome::SignedOut);
                    }
                    Command::SignIn => {
                ctx.events.done(req.id, Err("YouTube: already signed in.".into())).await;
            }
                    Command::Join { .. } => {
                ctx.events.done(req.id, Err("YouTube follows your own live stream; there's no channel to pick.".into())).await;
            }
                    command => {
                        let (events, api, live) = (ctx.events.clone(), api.clone(), live.clone());
                        tokio::spawn(async move {
                            let result = run_request(&api, &live, command).await;
                            if let Err(e) = &result {
                                tracing::warn!(err = %e, "YouTube request failed");
                            }
                            events.done(req.id, result).await;
                        });
                    }
                }
            }
        }
    }
}

async fn run_request(api: &Api, live: &Live, command: Command) -> Result<Option<String>, String> {
    let Some(chat) = live.chat.lock().await.clone() else {
        return Err(NOT_LIVE.into());
    };
    let result = match command {
        Command::Send { text, .. } => api.send(&chat, &text).await.map(Some),
        Command::Delete { message_id } => api.delete(&message_id).await.map(|_| None),
        Command::Timeout { user, seconds, .. } => {
            let id = resolve(live, &user).await?;
            api.ban(&chat, &id, Some(seconds)).await.map(|_| None)
        }
        Command::Ban { user, .. } => {
            let id = resolve(live, &user).await?;
            let ban = api.ban(&chat, &id, None).await;
            if let Ok(ban_id) = &ban {
                live.bans.lock().await.insert(id, ban_id.clone());
            }
            ban.map(|_| None)
        }
        Command::Unban { user } => {
            let id = resolve(live, &user).await?;
            let Some(ban_id) = live.bans.lock().await.get(&id).cloned() else {
                return Err("YouTube: Rill can only unban users it banned this session. Unban others in YouTube Studio.".into());
            };
            let r = api.unban(&ban_id).await;
            if r.is_ok() {
                live.bans.lock().await.remove(&id);
            }
            r.map(|_| None)
        }
        Command::Poll { .. } => {
            return Err(
                "YouTube: polls can't be started through the API. Start them in YouTube Studio."
                    .into(),
            );
        }
        Command::SignIn | Command::SignOut | Command::Join { .. } => Ok(None),
    };
    result.map_err(|e| match e {
        ApiError::Api { status: 403, .. } => format!(
            "{} Moderator actions need the channel owner or a moderator.",
            e.message()
        ),
        e => e.message(),
    })
}

/// A user's channel ID: given, or looked up among recent chatters by name.
async fn resolve(live: &Live, user: &UserRef) -> Result<String, String> {
    if let Some(id) = &user.id {
        return Ok(id.clone());
    }
    let key = user.login.trim_start_matches('@').to_lowercase();
    live.authors.lock().await.get(&key).cloned().ok_or_else(|| {
        format!(
            "YouTube: \"{}\" hasn't chatted since Rill connected, so Rill can't find them. Click their message instead.",
            user.login
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn needs_both_client_parts() {
        let none = YouTubeAdapter::new(&rill_os::settings::YouTubeSettings {
            client_id: Some("id".into()),
            client_secret: None,
            min_poll_ms: Some(200),
        });
        if BUILTIN_CLIENT_SECRET.is_none() {
            assert!(none.client.is_none());
        }
        assert_eq!(none.min_poll, Duration::from_millis(1_000));
    }

    #[tokio::test]
    async fn resolves_recent_chatters_by_name() {
        let live = Live::default();
        live.authors
            .lock()
            .await
            .insert("viewer".into(), "UCviewer".into());
        let user = UserRef {
            id: None,
            login: "@Viewer".into(),
        };
        assert_eq!(resolve(&live, &user).await, Ok("UCviewer".into()));
        let stranger = UserRef {
            id: None,
            login: "nobody".into(),
        };
        assert!(resolve(&live, &stranger).await.is_err());
    }
}

/// Signed-in sessions against a local stand-in for the YouTube Data API.
#[cfg(test)]
mod sessions {
    use super::*;
    use rill_core::{Event, RequestId, UserRef};
    use serde_json::{Value, json};
    use std::sync::Mutex as StdMutex;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    const MESSAGES: &str = include_str!("../tests/fixtures/messages.json");

    /// A request the fake API saw: method, path, query, JSON body.
    #[derive(Debug, Clone)]
    struct Seen {
        method: String,
        path: String,
        query: String,
        body: Value,
    }

    type Log = Arc<StdMutex<Vec<Seen>>>;
    type Route = fn(&Seen) -> (u16, Value);

    /// A fake Data API: answers with `route` and records every request.
    async fn api_server(route: Route) -> (String, Log) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let log: Log = Arc::default();
        let seen = log.clone();
        tokio::spawn(async move {
            loop {
                let (mut s, _) = listener.accept().await.unwrap();
                let seen = seen.clone();
                tokio::spawn(async move {
                    let mut buf = Vec::new();
                    let mut chunk = [0; 4096];
                    let (head, body) = loop {
                        let n = s.read(&mut chunk).await.unwrap();
                        assert!(n > 0, "request ended early");
                        buf.extend_from_slice(&chunk[..n]);
                        let text = String::from_utf8_lossy(&buf).to_string();
                        if let Some(end) = text.find("\r\n\r\n") {
                            let len = text[..end]
                                .lines()
                                .find_map(|l| {
                                    l.to_ascii_lowercase()
                                        .strip_prefix("content-length:")
                                        .map(|v| v.trim().parse::<usize>().unwrap())
                                })
                                .unwrap_or(0);
                            if buf.len() >= end + 4 + len {
                                break (text[..end].to_string(), text[end + 4..].to_string());
                            }
                        }
                    };
                    let mut first = head.lines().next().unwrap().split(' ');
                    let method = first.next().unwrap().to_string();
                    let target = first.next().unwrap();
                    let (path, query) = target.split_once('?').unwrap_or((target, ""));
                    let req = Seen {
                        method,
                        path: path.to_string(),
                        query: query.to_string(),
                        body: serde_json::from_str(&body).unwrap_or(Value::Null),
                    };
                    let (status, reply) = route(&req);
                    seen.lock().unwrap().push(req);
                    let reply = reply.to_string();
                    let res = format!(
                        "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}",
                        reply.len()
                    );
                    s.write_all(res.as_bytes()).await.unwrap();
                });
            }
        });
        (format!("http://{addr}"), log)
    }

    /// The first page is the fixture; later pages come from `later`.
    fn route_with(r: &Seen, later: fn() -> (u16, Value)) -> (u16, Value) {
        match (r.method.as_str(), r.path.as_str()) {
            ("GET", "/channels") => (
                200,
                json!({"items": [{"id": "UC1", "snippet": {"title": "Me"}}]}),
            ),
            ("GET", "/liveBroadcasts") => (
                200,
                json!({"items": [{"snippet": {"liveChatId": "chat1"}}]}),
            ),
            ("GET", "/liveChat/messages") if r.query.contains("pageToken=p2") => later(),
            ("GET", "/liveChat/messages") => {
                let mut v: Value = serde_json::from_str(MESSAGES).unwrap();
                v["nextPageToken"] = json!("p2");
                v["pollingIntervalMillis"] = json!(10);
                (200, v)
            }
            ("POST", "/liveChat/messages") => (200, json!({"id": "sent-1"})),
            ("POST", "/liveChat/bans") => (200, json!({"id": "ban-1"})),
            ("DELETE", "/liveChat/bans") => (204, Value::Null),
            _ => (404, json!({"error": {"message": "no route"}})),
        }
    }

    fn api_error(status: u16, reason: &str) -> (u16, Value) {
        (
            status,
            json!({"error": {"code": status, "message": reason, "errors": [{"reason": reason}]}}),
        )
    }

    /// Starts a session; returns its events, a request sender, and whether it stopped cleanly.
    fn start(
        base: &str,
    ) -> (
        mpsc::Receiver<Event>,
        mpsc::Sender<Request>,
        tokio::task::JoinHandle<bool>,
    ) {
        let (events, rx) = EventSink::channel(Platform::YouTube, 1_000);
        let (tx, requests) = mpsc::channel(4);
        let mut ctx = Ctx {
            events,
            requests,
            http: reqwest::Client::new(),
            client: Client {
                id: "client".into(),
                secret: "secret".into(),
            },
            min_poll: Duration::from_millis(20),
            api_base: base.into(),
            seven_tv: None,
        };
        let token = Token {
            access: "access".into(),
            refresh: "refresh".into(),
            expires_at_ms: now_ms() + 3_600_000,
        };
        let task =
            tokio::spawn(async move { matches!(session(&mut ctx, token).await, Outcome::Stop) });
        (rx, tx, task)
    }

    /// Waits for an event matching `want`, collecting everything seen on the way.
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
            panic!("the session ended early");
        })
        .await;
        assert!(found.is_ok(), "timed out; saw {seen:#?}");
        seen
    }

    async fn request(tx: &mpsc::Sender<Request>, id: u64, command: Command) {
        tx.send(Request {
            id: RequestId(id),
            command,
        })
        .await
        .unwrap();
    }

    fn done(e: &Event, id: u64) -> bool {
        matches!(e, Event::Done { request, .. } if *request == RequestId(id))
    }

    #[tokio::test]
    async fn polls_pages_sends_and_bans_by_name() {
        fn route(r: &Seen) -> (u16, Value) {
            route_with(r, || {
                (
                    200,
                    json!({"items": [], "nextPageToken": "p2", "pollingIntervalMillis": 10}),
                )
            })
        }
        let (base, log) = api_server(route).await;
        let (mut rx, tx, task) = start(&base);
        let seen = until(&mut rx, |e| matches!(e, Event::Message(_))).await;
        assert!(seen.iter().any(|e| matches!(
            e,
            Event::Status { status: ConnectionStatus::Connected, capabilities, .. } if *capabilities == OWNER
        )));

        request(
            &tx,
            1,
            Command::Send {
                text: "hello".into(),
                reply_to: None,
            },
        )
        .await;
        let seen = until(&mut rx, |e| done(e, 1)).await;
        assert!(
            matches!(seen.last(), Some(Event::Done { result: Ok(Some(id)), .. }) if id == "sent-1")
        );

        let viewer = UserRef {
            id: None,
            login: "@Viewer".into(),
        };
        request(
            &tx,
            2,
            Command::Ban {
                user: viewer.clone(),
                reason: None,
            },
        )
        .await;
        let seen = until(&mut rx, |e| done(e, 2)).await;
        assert!(
            matches!(
                seen.last(),
                Some(Event::Done {
                    result: Ok(None),
                    ..
                })
            ),
            "{seen:#?}"
        );
        request(&tx, 3, Command::Unban { user: viewer }).await;
        let seen = until(&mut rx, |e| done(e, 3)).await;
        assert!(
            matches!(
                seen.last(),
                Some(Event::Done {
                    result: Ok(None),
                    ..
                })
            ),
            "{seen:#?}"
        );

        let polled = || {
            log.lock()
                .unwrap()
                .iter()
                .any(|r| r.path == "/liveChat/messages" && r.query.contains("pageToken=p2"))
        };
        for _ in 0..250 {
            if polled() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(polled(), "the next page was never asked for");
        drop(tx);
        assert!(
            task.await.unwrap(),
            "the session didn't stop when Rill closed"
        );
        let log = log.lock().unwrap().clone();
        let sent = log
            .iter()
            .find(|r| r.method == "POST" && r.path == "/liveChat/messages")
            .unwrap();
        assert_eq!(sent.body["snippet"]["liveChatId"], "chat1");
        assert_eq!(
            sent.body["snippet"]["textMessageDetails"]["messageText"],
            "hello"
        );
        let ban = log
            .iter()
            .find(|r| r.path == "/liveChat/bans" && r.method == "POST")
            .unwrap();
        assert_eq!(ban.body["snippet"]["type"], "permanent");
        assert!(
            !ban.body["snippet"]["bannedUserDetails"]["channelId"]
                .as_str()
                .unwrap()
                .is_empty()
        );
        assert!(
            log.iter()
                .any(|r| r.method == "DELETE" && r.query == "id=ban-1")
        );
    }

    #[tokio::test]
    async fn warns_when_the_quota_runs_out() {
        fn route(r: &Seen) -> (u16, Value) {
            route_with(r, || api_error(403, "quotaExceeded"))
        }
        let (base, _) = api_server(route).await;
        let (mut rx, tx, task) = start(&base);
        let seen = until(&mut rx, |e| {
            matches!(
                e,
                Event::Notice {
                    level: Level::Error,
                    ..
                }
            )
        })
        .await;
        let Some(Event::Notice { text, .. }) = seen.last() else {
            unreachable!()
        };
        assert!(text.contains("quota"), "{text}");
        assert!(seen.iter().any(|e| matches!(
            e,
            Event::Status {
                status: ConnectionStatus::Reconnecting { .. },
                ..
            }
        )));
        // Requests still get answers during the long wait.
        request(
            &tx,
            1,
            Command::Poll {
                title: "q".into(),
                choices: vec![],
                seconds: 60,
            },
        )
        .await;
        let seen = until(&mut rx, |e| done(e, 1)).await;
        assert!(
            matches!(seen.last(), Some(Event::Done { result: Err(why), .. }) if why.contains("Studio"))
        );
        drop(tx);
        assert!(task.await.unwrap());
    }

    #[tokio::test]
    async fn explains_when_live_streaming_is_off() {
        fn route(r: &Seen) -> (u16, Value) {
            match r.path.as_str() {
                "/liveBroadcasts" => api_error(403, "liveStreamingNotEnabled"),
                _ => route_with(r, || (200, json!({"items": []}))),
            }
        }
        let (base, log) = api_server(route).await;
        let (mut rx, tx, task) = start(&base);
        let seen = until(&mut rx, |e| {
            matches!(
                e,
                Event::Notice {
                    level: Level::Warn,
                    ..
                }
            )
        })
        .await;
        let Some(Event::Notice { text, .. }) = seen.last() else {
            unreachable!()
        };
        assert!(text.contains("youtube.com/features"), "{text}");
        assert!(seen.iter().any(|e| matches!(
            e,
            Event::Status {
                status: ConnectionStatus::Connected,
                ..
            }
        )));
        // It waits a minute before asking again, rather than hammering the API.
        tokio::time::sleep(Duration::from_millis(500)).await;
        let lookups = log
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r.path == "/liveBroadcasts")
            .count();
        assert_eq!(lookups, 1);
        drop(tx);
        assert!(task.await.unwrap());
    }

    #[tokio::test]
    async fn looks_for_the_next_broadcast_when_chat_ends() {
        fn route(r: &Seen) -> (u16, Value) {
            route_with(r, || api_error(403, "liveChatEnded"))
        }
        let (base, log) = api_server(route).await;
        let (mut rx, tx, task) = start(&base);
        let seen = until(
            &mut rx,
            |e| matches!(e, Event::Notice { text, .. } if text.contains("ended")),
        )
        .await;
        assert!(seen.iter().any(|e| matches!(e, Event::Message(_))));
        // It goes straight back to finding the live broadcast.
        until(&mut rx, |e| matches!(e, Event::Message(_))).await;
        drop(tx);
        assert!(task.await.unwrap());
        let lookups = log
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r.path == "/liveBroadcasts")
            .count();
        assert!(lookups >= 2, "{lookups} broadcast lookups");
    }
}
