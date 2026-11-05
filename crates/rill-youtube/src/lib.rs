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
    let api = Api::new(ctx.http.clone(), ctx.client.clone(), token);
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
    let mut told_not_live = false;
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
    result.map_err(|e| e.message())
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
