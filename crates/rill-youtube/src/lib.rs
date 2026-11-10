//! Rill's YouTube adapter: Google desktop sign-in, and the live chat of your active broadcast.

pub mod api;
pub mod auth;
pub mod chat;
pub mod stream;

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

/// Google hasn't verified Rill yet, so YouTube is limited to test users and 7-day sign-ins.
pub const LIMITED: bool = true;

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
/// The shortest wait before reopening a chat stream that closed normally (each opening costs quota).
const REOPEN: Duration = Duration::from_secs(5);
/// The first wait after a failed chat stream; it doubles each time, up to 60 times this.
const RETRY: Duration = Duration::from_secs(5);
/// A stream open this long, or one that carried chat, was working.
const HEALTHY: Duration = Duration::from_secs(60);
/// Failed attempts in a row before the user is told.
const TELL_AFTER: u32 = 3;
/// Authors remembered for `/ban name`.
const AUTHOR_LIMIT: usize = 5_000;

const OWNER: Capabilities = Capabilities::of(&[
    Action::Ads,
    Action::Title,
    Action::Send,
    Action::Delete,
    Action::Timeout,
    Action::Ban,
    Action::Unban,
]);

/// The YouTube connection.
pub struct YouTubeAdapter {
    client: Option<Client>,
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
        YouTubeAdapter { client }
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
    /// The shortest wait before reopening a stream that closed normally.
    reopen: Duration,
    /// The first wait after a failed stream.
    retry: Duration,
    /// The Data API's address; tests use a local server.
    api_base: String,
    /// Where to load 7TV's global emotes from; tests leave it out.
    seven_tv: Option<rill_emotes::Config>,
    /// The gRPC chat stream's address; tests use a local server.
    stream: String,
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
        reopen: REOPEN,
        retry: RETRY,
        api_base: api::API.into(),
        seven_tv: Some(rill_emotes::Config::default()),
        stream: stream::ENDPOINT.into(),
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

/// A signed-in session: find the live chat, then stream it.
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
        let f = rill_emotes::follow(ctx.http.clone(), cfg, None, Some(ctx.events.clone()));
        ctx.events.set_enrich(Some(f.enrich()));
        f
    });
    let mut told_not_live = false;
    let mut told_not_enabled = false;
    loop {
        let chat = match api.active_chat().await {
            Ok(Some(chat)) => chat,
            Ok(None) => {
                ctx.events
                    .status(
                        ConnectionStatus::Waiting(
                            "Signed in. Chat appears here once you go live.".into(),
                        ),
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
                        ConnectionStatus::Waiting(ApiError::NotEnabled.message()),
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
        let mut f = Follow {
            chat,
            account: account.clone(),
            page_token: None,
            down: false,
            told_down: false,
            warned_quota: false,
        };
        let mut retry = Backoff::new(ctx.retry, ctx.retry * 60);
        let mut refreshed = false;
        loop {
            let (error, healthy) = match stream_chat(ctx, &api, &live, &mut f).await {
                StreamEnd::Stop(end) => return end,
                StreamEnd::ChatOver => {
                    ctx.events
                        .notice(Level::Info, "YouTube: the stream's chat went offline.")
                        .await;
                    break;
                }
                StreamEnd::Ended { error, healthy } => (error, healthy),
            };
            if healthy {
                retry.reset();
                refreshed = false;
            }
            let why = match error {
                None => {
                    // Each opening costs quota, so a stream that keeps closing early waits longer.
                    let wait = if healthy {
                        ctx.reopen
                    } else {
                        retry.next_delay()
                    };
                    tracing::debug!(healthy, "YouTube chat stream closed");
                    if let Some(end) = pause(ctx, &api, &live, Instant::now() + wait).await {
                        return end;
                    }
                    continue;
                }
                Some(stream::StreamError::ChatEnded) => {
                    ctx.events
                        .notice(Level::Info, ApiError::ChatEnded.message())
                        .await;
                    break;
                }
                Some(stream::StreamError::Quota) => {
                    if let Some(end) = quota_pause(ctx, &api, &live, &account).await {
                        return end;
                    }
                    continue;
                }
                Some(stream::StreamError::Unauthorized) if !refreshed => {
                    refreshed = true;
                    if let Err(ApiError::Unauthorized) = api.bearer(true).await {
                        return expired();
                    }
                    continue;
                }
                Some(stream::StreamError::Unauthorized) => {
                    "YouTube: the chat stream refuses the sign-in.".to_string()
                }
                Some(stream::StreamError::Unsupported(why) | stream::StreamError::Lost(why)) => why,
            };
            // Never poll instead: polling uses up the quota every user shares far faster.
            let wait = retry.next_delay();
            let attempt = retry.attempt();
            tracing::warn!(err = %why, attempt, "YouTube chat stream failed");
            f.down = true;
            ctx.events
                .status(
                    ConnectionStatus::Reconnecting { attempt },
                    Some(account.clone()),
                    OWNER,
                )
                .await;
            if attempt >= TELL_AFTER && !f.told_down {
                f.told_down = true;
                ctx.events
                    .notice(
                        Level::Warn,
                        "YouTube: Rill can't read chat right now and keeps trying, less often as time goes on. Other platforms aren't affected. If chat doesn't come back, send your logs from Settings → About.",
                    )
                    .await;
            }
            if let Some(end) = pause(ctx, &api, &live, Instant::now() + wait).await {
                return end;
            }
        }
        *live.chat.lock().await = None;
    }
}

/// The chat being read, and how its stream is doing.
struct Follow {
    chat: String,
    account: Account,
    /// Where to resume after the stream closes.
    page_token: Option<String>,
    /// The stream failed and the header shows it reconnecting.
    down: bool,
    /// The user was told chat can't be read.
    told_down: bool,
    warned_quota: bool,
}

/// How a chat stream ended.
enum StreamEnd {
    Stop(Outcome),
    /// The chat went offline or ended.
    ChatOver,
    /// The stream closed (`error` is `None`) or failed; `healthy` if it had worked a while.
    Ended {
        error: Option<stream::StreamError>,
        healthy: bool,
    },
}

fn failed(e: stream::StreamError) -> StreamEnd {
    StreamEnd::Ended {
        error: Some(e),
        healthy: false,
    }
}

/// Reads the chat over the gRPC stream until it ends, answering requests meanwhile.
async fn stream_chat(ctx: &mut Ctx, api: &Api, live: &Live, f: &mut Follow) -> StreamEnd {
    let access = match api.bearer(false).await {
        Ok(a) => a,
        Err(ApiError::Unauthorized) => return failed(stream::StreamError::Unauthorized),
        Err(e) => return failed(stream::StreamError::Lost(e.message())),
    };
    let channel = match stream::channel(&ctx.stream).await {
        Ok(c) => c,
        Err(e) => return failed(e),
    };
    api.spend(api::cost::OPEN_STREAM);
    let opened = Instant::now();
    let mut messages = match stream::open(channel, &access, &f.chat, f.page_token.as_deref()).await
    {
        Ok(s) => s,
        Err(e) => return failed(e),
    };
    tracing::debug!("YouTube chat stream open");
    let mut got_chat = false;
    loop {
        tokio::select! {
            next = messages.message() => match next {
                Ok(Some(reply)) => {
                    if f.down {
                        f.down = false;
                        ctx.events.status(ConnectionStatus::Connected, Some(f.account.clone()), OWNER).await;
                        if f.told_down {
                            f.told_down = false;
                            ctx.events.notice(Level::Info, "YouTube: chat is back.").await;
                        }
                    }
                    let page = chat::page(&stream::to_json(&reply));
                    got_chat |= !page.events.is_empty();
                    remember_authors(live, &page.events).await;
                    for e in page.events {
                        if !ctx.events.send(e).await {
                            return StreamEnd::Stop(Outcome::Stop);
                        }
                    }
                    if page.next_page_token.is_some() {
                        f.page_token = page.next_page_token;
                    }
                    if page.offline {
                        return StreamEnd::ChatOver;
                    }
                    if !f.warned_quota && api.units() > QUOTA_WARN {
                        f.warned_quota = true;
                        ctx.events.notice(Level::Warn, format!(
                            "YouTube: Rill has used about {} of YouTube's usual 10,000 daily quota units this session. Each message sent costs 50; if the quota runs out, YouTube chat stops until midnight Pacific time.",
                            api.units()
                        )).await;
                    }
                }
                Ok(None) => {
                    return StreamEnd::Ended { error: None, healthy: got_chat || opened.elapsed() >= HEALTHY };
                }
                Err(status) => {
                    return StreamEnd::Ended {
                        error: Some(stream::status_error(status)),
                        healthy: got_chat || opened.elapsed() >= HEALTHY,
                    };
                }
            },
            req = ctx.requests.recv() => {
                if let Some(end) = handle(ctx, api, live, req).await {
                    return StreamEnd::Stop(end);
                }
            }
        }
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
                if let Some(end) = handle(ctx, api, live, req).await {
                    return Some(end);
                }
            }
        }
    }
}

/// Handles one request while signed in. Returns an outcome for sign-out or shutdown.
async fn handle(ctx: &mut Ctx, api: &Api, live: &Live, req: Option<Request>) -> Option<Outcome> {
    let Some(req) = req else {
        return Some(Outcome::Stop);
    };
    match req.command {
        Command::SignOut => {
            auth::revoke(&ctx.http, &api.token().await).await;
            let result = auth::delete_token().map(|_| None);
            ctx.events.done(req.id, result).await;
            return Some(Outcome::SignedOut);
        }
        Command::SignIn => {
            ctx.events
                .done(req.id, Err("YouTube: already signed in.".into()))
                .await;
        }
        Command::Join { .. } => {
            let why = "YouTube follows your own live stream; there's no channel to pick.";
            ctx.events.done(req.id, Err(why.into())).await;
        }
        command => {
            // Requests run on their own, so the chat keeps flowing meanwhile.
            let (events, api, live) = (ctx.events.clone(), api.clone(), live.clone());
            tokio::spawn(async move {
                let result = run_request(&api, &live, &events, command).await;
                if let Err(e) = &result {
                    tracing::warn!(err = %e, "YouTube request failed");
                }
                events.done(req.id, result).await;
            });
        }
    }
    None
}

async fn run_request(
    api: &Api,
    live: &Live,
    events: &EventSink,
    command: Command,
) -> Result<Option<String>, String> {
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
        Command::SetTitle { title } => api.set_title(&title).await.map(|_| None),
        Command::StartAds { seconds } => {
            api.insert_ad(seconds).await.map_err(|e| e.message())?;
            // YouTube reports nothing about ads, so the notices come from here.
            events.ads_started(seconds, "started by you").await;
            return Ok(None);
        }
        Command::SetCategory { .. } => {
            return Err(
                "YouTube: streams have no category to set here. Change it in YouTube Studio."
                    .into(),
            );
        }
        Command::SetMode { .. } | Command::Announce { .. } | Command::Shoutout { .. } => {
            return Err("YouTube: chat modes, announcements and shoutouts aren't in YouTube's API. Use YouTube Studio.".into());
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
        });
        if BUILTIN_CLIENT_SECRET.is_none() {
            assert!(none.client.is_none());
        }
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

    /// The fake Data API's usual answers. Chat itself only comes over the stream.
    fn base_route(r: &Seen) -> (u16, Value) {
        match (r.method.as_str(), r.path.as_str()) {
            ("GET", "/channels") => (
                200,
                json!({"items": [{"id": "UC1", "snippet": {"title": "Me"}}]}),
            ),
            ("GET", "/liveBroadcasts") => (
                200,
                json!({"items": [{"snippet": {"liveChatId": "chat1"}}]}),
            ),
            ("POST", "/liveChat/messages") => (200, json!({"id": "sent-1"})),
            ("POST", "/liveChat/bans") => (200, json!({"id": "ban-1"})),
            ("DELETE", "/liveChat/bans") => (204, Value::Null),
            ("PUT", "/liveBroadcasts") => (200, json!({"id": "b1"})),
            ("POST", "/liveBroadcasts/cuepoint") => (200, json!({"id": "cue1"})),
            _ => (404, json!({"error": {"message": "no route"}})),
        }
    }

    /// Whether the session ever asked the Data API for chat pages (polling).
    fn polled(log: &Log) -> bool {
        log.lock()
            .unwrap()
            .iter()
            .any(|r| r.method == "GET" && r.path == "/liveChat/messages")
    }

    fn api_error(status: u16, reason: &str) -> (u16, Value) {
        (
            status,
            json!({"error": {"code": status, "message": reason, "errors": [{"reason": reason}]}}),
        )
    }

    /// Starts a session streaming one message from `@viewer`: its events, requests and outcome.
    async fn start(
        base: &str,
    ) -> (
        mpsc::Receiver<Event>,
        mpsc::Sender<Request>,
        tokio::task::JoinHandle<bool>,
    ) {
        let stream = stream_server(FakeStream::sending(vec![hello()])).await;
        start_with(base, stream)
    }

    /// A session that reads chat over the gRPC stream at `stream`.
    fn start_with(
        base: &str,
        stream: String,
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
            reopen: Duration::from_millis(20),
            retry: Duration::from_millis(10),
            api_base: base.into(),
            seven_tv: None,
            stream,
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
    async fn sends_and_bans_by_name() {
        let (base, log) = api_server(base_route).await;
        let (mut rx, tx, task) = start(&base).await;
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

    use crate::stream::pb;
    use pb::v3_data_live_chat_message_service_server::{
        V3DataLiveChatMessageService, V3DataLiveChatMessageServiceServer,
    };

    /// A fake chat stream whose first `fails` calls fail with `fail`; then it sends `replies`.
    struct FakeStream {
        replies: Vec<pb::LiveChatMessageListResponse>,
        fail: Option<tonic::Code>,
        fails: usize,
        /// Each call's authorization header and chat ID.
        calls: Arc<StdMutex<Vec<(String, String)>>>,
    }

    impl FakeStream {
        fn sending(replies: Vec<pb::LiveChatMessageListResponse>) -> FakeStream {
            FakeStream {
                replies,
                fail: None,
                fails: 0,
                calls: Arc::default(),
            }
        }

        fn failing(code: tonic::Code, fails: usize) -> FakeStream {
            FakeStream {
                fail: Some(code),
                fails,
                ..FakeStream::sending(vec![hello()])
            }
        }
    }

    type Replies = std::pin::Pin<
        Box<
            dyn tokio_stream::Stream<Item = Result<pb::LiveChatMessageListResponse, tonic::Status>>
                + Send,
        >,
    >;

    #[tonic::async_trait]
    impl V3DataLiveChatMessageService for FakeStream {
        type StreamListStream = Replies;

        async fn stream_list(
            &self,
            req: tonic::Request<pb::LiveChatMessageListRequest>,
        ) -> Result<tonic::Response<Replies>, tonic::Status> {
            let auth = req
                .metadata()
                .get("authorization")
                .and_then(|v| v.to_str().ok())
                .unwrap_or_default()
                .to_string();
            let chat = req.get_ref().live_chat_id.clone().unwrap_or_default();
            let earlier = {
                let mut calls = self.calls.lock().unwrap();
                calls.push((auth, chat));
                calls.len() - 1
            };
            if let Some(code) = self.fail.filter(|_| earlier < self.fails) {
                return Err(tonic::Status::new(code, "not here"));
            }
            use tokio_stream::StreamExt as _;
            let replies = tokio_stream::iter(self.replies.clone().into_iter().map(Ok))
                .chain(tokio_stream::pending());
            Ok(tonic::Response::new(Box::pin(replies)))
        }
    }

    async fn stream_server(fake: FakeStream) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(
            tonic::transport::Server::builder()
                .add_service(V3DataLiveChatMessageServiceServer::new(fake))
                .serve_with_incoming(tokio_stream::wrappers::TcpListenerStream::new(listener)),
        );
        format!("http://{addr}")
    }

    fn streamed(id: &str, author: &str, text: &str) -> pb::LiveChatMessage {
        use pb::live_chat_message_snippet::DisplayedContent;
        use pb::live_chat_message_snippet::type_wrapper::Type;
        pb::LiveChatMessage {
            id: Some(id.into()),
            snippet: Some(pb::LiveChatMessageSnippet {
                r#type: Some(Type::TextMessageEvent as i32),
                author_channel_id: Some(format!("UC{author}")),
                published_at: Some("2026-11-06T12:00:00Z".into()),
                display_message: Some(text.into()),
                displayed_content: Some(DisplayedContent::TextMessageDetails(
                    pb::LiveChatTextMessageDetails {
                        message_text: Some(text.into()),
                    },
                )),
                ..Default::default()
            }),
            author_details: Some(pb::LiveChatMessageAuthorDetails {
                channel_id: Some(format!("UC{author}")),
                display_name: Some(author.into()),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    /// One reply with one message from `@viewer`.
    fn hello() -> pb::LiveChatMessageListResponse {
        pb::LiveChatMessageListResponse {
            items: vec![streamed("s0", "@viewer", "hello")],
            next_page_token: Some("t0".into()),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn reads_chat_over_the_stream_without_polling() {
        let (base, log) = api_server(base_route).await;
        let fake = FakeStream::sending(vec![
            pb::LiveChatMessageListResponse {
                items: vec![streamed("s1", "Ann", "first over the stream")],
                next_page_token: Some("t1".into()),
                ..Default::default()
            },
            pb::LiveChatMessageListResponse {
                items: vec![streamed("s2", "Bob", "second")],
                ..Default::default()
            },
        ]);
        let calls = fake.calls.clone();
        let stream = stream_server(fake).await;
        let (mut rx, tx, task) = start_with(&base, stream);
        let seen = until(
            &mut rx,
            |e| matches!(e, Event::Message(m) if m.plain_text() == "second"),
        )
        .await;
        let texts: Vec<_> = seen
            .iter()
            .filter_map(|e| match e {
                Event::Message(m) => Some(m.plain_text()),
                _ => None,
            })
            .collect();
        assert_eq!(texts, ["first over the stream", "second"]);
        // Sending and banning still go over the Data API while the stream is open.
        request(
            &tx,
            1,
            Command::Send {
                text: "hi".into(),
                reply_to: None,
            },
        )
        .await;
        let seen = until(&mut rx, |e| done(e, 1)).await;
        assert!(
            matches!(seen.last(), Some(Event::Done { result: Ok(Some(id)), .. }) if id == "sent-1")
        );
        drop(tx);
        assert!(task.await.unwrap());
        assert_eq!(
            calls.lock().unwrap().clone(),
            [("Bearer access".to_string(), "chat1".to_string())]
        );
        assert!(!polled(&log), "it polled while streaming");
    }

    #[tokio::test]
    async fn retries_a_failing_stream_and_never_polls() {
        let (base, log) = api_server(base_route).await;
        let fake = FakeStream::failing(tonic::Code::Unavailable, 4);
        let calls = fake.calls.clone();
        let stream = stream_server(fake).await;
        let (mut rx, tx, task) = start_with(&base, stream);
        let seen = until(&mut rx, |e| matches!(e, Event::Message(_))).await;
        assert!(seen.iter().any(|e| matches!(
            e,
            Event::Status {
                status: ConnectionStatus::Reconnecting { .. },
                ..
            }
        )));
        assert!(seen.iter().any(|e| matches!(
            e,
            Event::Notice { level: Level::Warn, text, .. } if text.contains("keeps trying")
        )));
        assert!(seen.iter().any(|e| matches!(
            e,
            Event::Notice { text, .. } if text == "YouTube: chat is back."
        )));
        drop(tx);
        assert!(task.await.unwrap());
        assert_eq!(calls.lock().unwrap().len(), 5);
        assert!(!polled(&log), "it polled instead of streaming");
    }

    #[tokio::test]
    async fn an_unavailable_stream_is_retried_ever_more_slowly() {
        let (base, log) = api_server(base_route).await;
        let fake = FakeStream::failing(tonic::Code::Unimplemented, usize::MAX);
        let calls = fake.calls.clone();
        let stream = stream_server(fake).await;
        let (mut rx, tx, task) = start_with(&base, stream);
        until(
            &mut rx,
            |e| matches!(e, Event::Notice { level: Level::Warn, text, .. } if text.contains("keeps trying")),
        )
        .await;
        // Waits of 10 ms doubling: about 8 tries in a second, where steady retries would be 100.
        tokio::time::sleep(Duration::from_secs(1)).await;
        let tries = calls.lock().unwrap().len();
        assert!((4..=12).contains(&tries), "{tries} tries");
        drop(tx);
        assert!(task.await.unwrap());
        assert!(!polled(&log), "it polled instead of streaming");
    }

    #[tokio::test]
    async fn renames_the_live_broadcast_keeping_its_snippet() {
        fn route(r: &Seen) -> (u16, Value) {
            match (r.method.as_str(), r.path.as_str()) {
                ("GET", "/liveBroadcasts") => (
                    200,
                    json!({"items": [{"id": "b1", "snippet": {
                        "liveChatId": "chat1", "title": "Old",
                        "scheduledStartTime": "2026-11-06T12:00:00Z", "description": "desc"}}]}),
                ),
                _ => base_route(r),
            }
        }
        let (base, log) = api_server(route).await;
        let (mut rx, tx, task) = start(&base).await;
        until(&mut rx, |e| matches!(e, Event::Message(_))).await;
        request(
            &tx,
            1,
            Command::SetTitle {
                title: "New title".into(),
            },
        )
        .await;
        let seen = until(&mut rx, |e| done(e, 1)).await;
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
        request(&tx, 2, Command::SetCategory { name: "x".into() }).await;
        let seen = until(&mut rx, |e| done(e, 2)).await;
        assert!(
            matches!(seen.last(), Some(Event::Done { result: Err(why), .. }) if why.contains("Studio"))
        );
        drop(tx);
        assert!(task.await.unwrap());
        let log = log.lock().unwrap().clone();
        let put = log.iter().find(|r| r.method == "PUT").unwrap();
        assert_eq!(put.query, "part=snippet");
        assert_eq!(
            put.body,
            json!({"id": "b1", "snippet": {"title": "New title",
                "scheduledStartTime": "2026-11-06T12:00:00Z", "description": "desc"}})
        );
    }

    #[tokio::test]
    async fn starts_an_ad_break_and_says_so() {
        fn route(r: &Seen) -> (u16, Value) {
            match (r.method.as_str(), r.path.as_str()) {
                ("GET", "/liveBroadcasts") => (
                    200,
                    json!({"items": [{"id": "b1", "snippet": {"liveChatId": "chat1"}}]}),
                ),
                _ => base_route(r),
            }
        }
        let (base, log) = api_server(route).await;
        let (mut rx, tx, task) = start(&base).await;
        until(&mut rx, |e| matches!(e, Event::Message(_))).await;
        request(&tx, 1, Command::StartAds { seconds: 60 }).await;
        let seen = until(&mut rx, |e| done(e, 1)).await;
        assert!(seen.iter().any(|e| matches!(
            e,
            Event::Notice { text, .. } if text == "Ads started on YouTube: 1m (started by you)"
        )));
        drop(tx);
        assert!(task.await.unwrap());
        let log = log.lock().unwrap().clone();
        let cue = log
            .iter()
            .find(|r| r.path == "/liveBroadcasts/cuepoint")
            .unwrap();
        assert_eq!(cue.query, "id=b1");
        assert_eq!(
            cue.body,
            json!({"cueType": "cueTypeAd", "durationSecs": 60})
        );
    }

    #[tokio::test]
    async fn warns_when_the_quota_runs_out() {
        let (base, _) = api_server(base_route).await;
        let stream = stream_server(FakeStream::failing(
            tonic::Code::ResourceExhausted,
            usize::MAX,
        ))
        .await;
        let (mut rx, tx, task) = start_with(&base, stream);
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
                _ => base_route(r),
            }
        }
        let (base, log) = api_server(route).await;
        let (mut rx, tx, task) = start(&base).await;
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
        // Signed in and waiting, with the reason for the header's tooltip.
        assert!(seen.iter().any(|e| matches!(
            e,
            Event::Status {
                status: ConnectionStatus::Waiting(why),
                ..
            } if why.contains("youtube.com/features")
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
        use pb::live_chat_message_snippet::type_wrapper::Type;
        let (base, log) = api_server(base_route).await;
        let mut end = streamed("s9", "@viewer", "");
        end.snippet.as_mut().unwrap().r#type = Some(Type::ChatEndedEvent as i32);
        let ended = pb::LiveChatMessageListResponse {
            items: vec![end],
            ..Default::default()
        };
        let stream = stream_server(FakeStream::sending(vec![hello(), ended])).await;
        let (mut rx, tx, task) = start_with(&base, stream);
        let seen = until(
            &mut rx,
            |e| matches!(e, Event::Notice { text, .. } if text.contains("offline")),
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
