//! Rill's Twitch adapter: device-code sign-in, EventSub chat over WebSocket, Helix actions.

pub mod auth;
pub mod eventsub;
pub mod helix;

use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use rill_core::backoff::Backoff;
use rill_core::{
    Account, Action, Adapter, AdapterIo, Capabilities, Command, ConnectionStatus, Event, EventSink,
    Level, Platform, Request, SignInPrompt, UserRef, adapter::BoxFuture, now_ms,
};
use tokio::sync::mpsc;
use tokio::time::{Instant, sleep, sleep_until, timeout};
use tokio_tungstenite::tungstenite::Message;

use crate::auth::{AuthError, Token};
use crate::eventsub::Frame;
use crate::helix::{Badges, Helix, HelixError, User};

/// The client ID built into release builds; settings.json can override it.
const BUILTIN_CLIENT_ID: Option<&str> = option_env!("RILL_TWITCH_CLIENT_ID");
const VALIDATE_EVERY: Duration = Duration::from_secs(3600);
const NOT_CONNECTED: &str = "Twitch isn't connected. Click the Twitch icon to sign in.";

const BROADCASTER: Capabilities = Capabilities::of(&[
    Action::Send,
    Action::Reply,
    Action::Delete,
    Action::Timeout,
    Action::Ban,
    Action::Unban,
    Action::Poll,
]);
const MODERATOR: Capabilities = Capabilities::of(&[
    Action::Send,
    Action::Reply,
    Action::Delete,
    Action::Timeout,
    Action::Ban,
    Action::Unban,
]);
const VIEWER: Capabilities = Capabilities::of(&[Action::Send, Action::Reply]);

/// The Twitch connection.
pub struct TwitchAdapter {
    client_id: Option<String>,
    channel: Option<String>,
}

impl TwitchAdapter {
    /// Uses settings.json's client ID and channel, falling back to the built-in client ID.
    pub fn new(settings: &rill_os::settings::TwitchSettings) -> TwitchAdapter {
        TwitchAdapter {
            client_id: settings
                .client_id
                .clone()
                .filter(|s| !s.trim().is_empty())
                .or_else(|| BUILTIN_CLIENT_ID.map(str::to_string)),
            channel: settings
                .channel
                .clone()
                .map(|c| c.trim().trim_start_matches('#').to_lowercase())
                .filter(|c| !c.is_empty()),
        }
    }
}

impl Adapter for TwitchAdapter {
    fn platform(&self) -> Platform {
        Platform::Twitch
    }

    fn run(self: Box<Self>, io: AdapterIo) -> BoxFuture {
        Box::pin(run(*self, io))
    }
}

struct Ctx {
    events: EventSink,
    requests: mpsc::Receiver<Request>,
    http: reqwest::Client,
    client_id: String,
    channel: Option<String>,
}

/// Why a connected session ended.
enum Outcome {
    /// The core is gone.
    Stop,
    SignedOut,
    /// Needs the user (sign in again, fix settings).
    Failed(String),
}

/// The signed-in user, the channel being read, and what the user may do there.
struct Identity {
    me: User,
    channel: User,
    caps: Capabilities,
    badges: Badges,
}

async fn run(adapter: TwitchAdapter, io: AdapterIo) {
    let AdapterIo { events, requests } = io;
    let http = match reqwest::Client::builder()
        .user_agent(concat!("Rill/", env!("CARGO_PKG_VERSION")))
        .timeout(Duration::from_secs(20))
        .build()
    {
        Ok(h) => h,
        Err(e) => {
            fail_forever(
                events,
                requests,
                format!("Twitch: HTTP setup failed ({e}). Restart Rill."),
            )
            .await;
            return;
        }
    };
    let Some(client_id) = adapter.client_id else {
        let why = format!(
            "Twitch: no app client ID is set. Add \"twitch\": {{\"client_id\": \"…\"}} to {}.",
            rill_os::settings::path().display()
        );
        fail_forever(events, requests, why).await;
        return;
    };
    let mut ctx = Ctx {
        events,
        requests,
        http,
        client_id,
        channel: adapter.channel,
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
                let status = match failure.take() {
                    Some(why) => ConnectionStatus::Failed(why),
                    None => ConnectionStatus::Disconnected,
                };
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

/// Reports a permanent failure and answers every request with it until the core stops.
async fn fail_forever(events: EventSink, mut requests: mpsc::Receiver<Request>, why: String) {
    tracing::error!(err = %why, "Twitch adapter can't start");
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

/// Waits while signed out; runs the device flow when asked. `None` once the core is gone.
async fn wait_for_sign_in(ctx: &mut Ctx) -> Option<Token> {
    loop {
        let req = ctx.requests.recv().await?;
        match req.command {
            Command::SignIn => match device_flow(ctx).await {
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

/// Shows a code, waits for the user to approve it, and stores the token.
async fn device_flow(ctx: &mut Ctx) -> Result<Option<Token>, String> {
    ctx.events
        .status(ConnectionStatus::Connecting, None, Capabilities::NONE)
        .await;
    let device = auth::start_device(&ctx.http, &ctx.client_id)
        .await
        .map_err(|e| e.message())?;
    let deadline = Instant::now() + Duration::from_secs(device.expires_in);
    let code = format_code(&device.user_code);
    ctx.events
        .send(Event::SignIn {
            platform: Platform::Twitch,
            prompt: SignInPrompt {
                url: device.verification_uri.clone(),
                code: Some(code),
                expires_at_ms: now_ms() + device.expires_in as i64 * 1000,
            },
        })
        .await;
    loop {
        tokio::select! {
            _ = sleep(auth::poll_interval(&device)) => {
                match auth::poll_device(&ctx.http, &ctx.client_id, &device).await {
                    Ok(Some(token)) => {
                        if let Err(e) = auth::save_token(&token) {
                            ctx.events.notice(Level::Warn, format!("{e} You'll need to sign in again next time.")).await;
                        }
                        return Ok(Some(token));
                    }
                    Ok(None) if Instant::now() < deadline => {}
                    Ok(None) => return Err("Twitch: the sign-in code expired. Click the Twitch icon to get a new one.".into()),
                    Err(AuthError::Temporary(m)) => tracing::warn!(err = %m, "Twitch device poll failed"),
                    Err(e) => return Err(e.message()),
                }
            }
            req = ctx.requests.recv() => {
                let Some(req) = req else { return Ok(None) };
                match req.command {
                    Command::SignOut => {
                        ctx.events.done(req.id, Ok(None)).await;
                        return Err("Twitch: sign-in cancelled.".into());
                    }
                    Command::SignIn => {
                        ctx.events.done(req.id, Err("Twitch: already signing in; enter the code shown.".into())).await;
                    }
                    _ => {
                        ctx.events.done(req.id, Err(NOT_CONNECTED.into())).await;
                    }
                }
            }
        }
    }
}

/// Shows an 8-letter device code as `ABCD-EFGH`.
fn format_code(code: &str) -> String {
    if code.len() == 8 && code.is_ascii() {
        format!("{}-{}", &code[..4], &code[4..])
    } else {
        code.to_string()
    }
}

/// A signed-in session: identify, then keep an EventSub connection up.
async fn session(ctx: &mut Ctx, token: Token) -> Outcome {
    let helix = Helix::new(ctx.http.clone(), ctx.client_id.clone(), token);
    let mut backoff = Backoff::chat();
    ctx.events
        .status(ConnectionStatus::Connecting, None, Capabilities::NONE)
        .await;
    let id = loop {
        match identify(&helix, &ctx.http, ctx.channel.as_deref()).await {
            Ok(id) => break id,
            Err(Ident::Expired) => return expired(),
            Err(Ident::Fatal(why)) => return Outcome::Failed(why),
            Err(Ident::Retry(why)) => {
                tracing::warn!(err = %why, "Twitch identify failed");
                let attempt = backoff.attempt() + 1;
                ctx.events
                    .status(
                        ConnectionStatus::Reconnecting { attempt },
                        None,
                        Capabilities::NONE,
                    )
                    .await;
                if let Some(end) = pause(ctx, None, backoff.next_delay()).await {
                    return end;
                }
            }
        }
    };
    let account = Account {
        id: id.me.id.clone(),
        login: id.me.login.clone(),
        display_name: id.me.display_name.clone(),
    };
    tracing::info!(channel = %id.channel.login, "Twitch signed in");
    // The channel's 7TV emotes, kept current for as long as this session runs.
    let emotes = rill_emotes::follow(
        ctx.http.clone(),
        rill_emotes::Config::default(),
        Some(rill_emotes::Channel {
            platform: "twitch",
            ids: vec![id.channel.id.clone()],
        }),
    );
    ctx.events.set_enrich(Some(emotes.enrich()));

    let mut url = eventsub::URL.to_string();
    let mut fresh = true;
    loop {
        match connection(ctx, &helix, &id, &account, &mut url, fresh).await {
            Ws::Stop => return Outcome::Stop,
            Ws::SignedOut => return Outcome::SignedOut,
            Ws::Expired => return expired(),
            Ws::Moved => fresh = false,
            Ws::Lost(why) => {
                tracing::warn!(err = %why, "Twitch EventSub connection lost");
                fresh = true;
                url = eventsub::URL.to_string();
                let attempt = backoff.attempt() + 1;
                ctx.events
                    .status(
                        ConnectionStatus::Reconnecting { attempt },
                        Some(account.clone()),
                        id.caps,
                    )
                    .await;
                if let Some(end) = pause(ctx, Some((&helix, &id)), backoff.next_delay()).await {
                    return end;
                }
            }
            Ws::Healthy => {
                backoff.reset();
                fresh = true;
                url = eventsub::URL.to_string();
            }
        }
    }
}

fn expired() -> Outcome {
    if let Err(e) = auth::delete_token() {
        tracing::error!(err = %e, "Twitch token not deleted");
    }
    Outcome::Failed(AuthError::Invalid.message())
}

enum Ident {
    Expired,
    Fatal(String),
    Retry(String),
}

async fn identify(
    helix: &Helix,
    http: &reqwest::Client,
    channel: Option<&str>,
) -> Result<Identity, Ident> {
    let map = |e: HelixError| match e {
        HelixError::Unauthorized => Ident::Expired,
        HelixError::Network(m) => Ident::Retry(m),
        e => Ident::Retry(e.message()),
    };
    let access = helix.access().await;
    let validated = match auth::validate(http, &access).await {
        Ok(v) => v,
        Err(AuthError::Invalid) => {
            helix.refresh(&access).await.map_err(map)?;
            auth::validate(http, &helix.access().await)
                .await
                .map_err(|e| match e {
                    AuthError::Invalid => Ident::Expired,
                    e => Ident::Retry(e.message()),
                })?
        }
        Err(e) => return Err(Ident::Retry(e.message())),
    };
    let me = helix
        .user_by_id(&validated.user_id)
        .await
        .map_err(map)?
        .unwrap_or(User {
            id: validated.user_id.clone(),
            login: validated.login.clone(),
            display_name: validated.login.clone(),
        });
    let channel = match channel {
        Some(login) if !login.eq_ignore_ascii_case(&me.login) => helix
            .user_by_login(login)
            .await
            .map_err(map)?
            .ok_or_else(|| {
                Ident::Fatal(format!(
                    "Twitch: there's no channel named \"{login}\". Fix twitch.channel in {}.",
                    rill_os::settings::path().display()
                ))
            })?,
        _ => me.clone(),
    };
    let caps = if channel.id == me.id {
        BROADCASTER
    } else {
        match helix.moderates(&me.id, &channel.id).await {
            Ok(true) => MODERATOR,
            Ok(false) => VIEWER,
            Err(e) => {
                tracing::warn!(err = %e.message(), "Twitch moderator check failed");
                VIEWER
            }
        }
    };
    let badges = helix.badges(&channel.id).await.unwrap_or_else(|e| {
        tracing::warn!(err = %e.message(), "Twitch badges not loaded");
        Badges::new()
    });
    Ok(Identity {
        me,
        channel,
        caps,
        badges,
    })
}

/// Waits out a reconnect delay while still answering requests.
async fn pause(
    ctx: &mut Ctx,
    live: Option<(&Helix, &Identity)>,
    delay: Duration,
) -> Option<Outcome> {
    let until = Instant::now() + delay;
    loop {
        tokio::select! {
            _ = sleep_until(until) => return None,
            req = ctx.requests.recv() => {
                let Some(req) = req else { return Some(Outcome::Stop) };
                if let Some(end) = answer(ctx, live, req).await {
                    return Some(end);
                }
            }
        }
    }
}

/// Handles a request while signed in. Returns an outcome only for sign-out.
async fn answer(ctx: &mut Ctx, live: Option<(&Helix, &Identity)>, req: Request) -> Option<Outcome> {
    match (&req.command, live) {
        (Command::SignOut, _) => {
            if let Some((helix, _)) = live {
                auth::revoke(&ctx.http, &ctx.client_id, &helix.access().await).await;
            }
            let result = auth::delete_token().map(|_| None);
            ctx.events.done(req.id, result).await;
            Some(Outcome::SignedOut)
        }
        (Command::SignIn, _) => {
            ctx.events
                .done(req.id, Err("Twitch: already signed in.".into()))
                .await;
            None
        }
        (_, Some((helix, id))) => {
            spawn_request(
                ctx.events.clone(),
                helix.clone(),
                id.channel.id.clone(),
                id.me.id.clone(),
                req,
            );
            None
        }
        (_, None) => {
            ctx.events
                .done(
                    req.id,
                    Err("Twitch is reconnecting; try again in a moment.".into()),
                )
                .await;
            None
        }
    }
}

/// Runs a Helix request in its own task so chat keeps flowing.
fn spawn_request(events: EventSink, helix: Helix, channel: String, me: String, req: Request) {
    tokio::spawn(async move {
        let result = run_request(&helix, &channel, &me, req.command).await;
        if let Err(e) = &result {
            tracing::warn!(err = %e, "Twitch request failed");
        }
        events.done(req.id, result).await;
    });
}

async fn run_request(
    helix: &Helix,
    channel: &str,
    me: &str,
    command: Command,
) -> Result<Option<String>, String> {
    let r = async {
        Ok(match command {
            Command::Send { text, reply_to } => Some(
                helix
                    .send_message(channel, me, &text, reply_to.as_deref())
                    .await?,
            ),
            Command::Delete { message_id } => {
                helix.delete_message(channel, me, &message_id).await?;
                None
            }
            Command::Timeout {
                user,
                seconds,
                reason,
            } => {
                let id = resolve(helix, &user).await?;
                helix
                    .ban(channel, me, &id, Some(seconds), reason.as_deref())
                    .await?;
                None
            }
            Command::Ban { user, reason } => {
                let id = resolve(helix, &user).await?;
                helix.ban(channel, me, &id, None, reason.as_deref()).await?;
                None
            }
            Command::Unban { user } => {
                let id = resolve(helix, &user).await?;
                helix.unban(channel, me, &id).await?;
                None
            }
            Command::Poll {
                title,
                choices,
                seconds,
            } => {
                helix
                    .create_poll(channel, &title, &choices, seconds)
                    .await?;
                None
            }
            Command::SignIn | Command::SignOut | Command::Join { .. } => None,
        })
    };
    r.await.map_err(|e: HelixError| e.message())
}

async fn resolve(helix: &Helix, user: &UserRef) -> Result<String, HelixError> {
    if let Some(id) = &user.id {
        return Ok(id.clone());
    }
    helix
        .user_by_login(&user.login)
        .await?
        .map(|u| u.id)
        .ok_or_else(|| HelixError::Api {
            status: 404,
            message: format!("there's no user named \"{}\"", user.login),
        })
}

/// How one EventSub connection ended.
enum Ws {
    Stop,
    SignedOut,
    Expired,
    /// Twitch moved the session to another URL; reconnect at once, keeping subscriptions.
    Moved,
    /// The connection stayed up a while before ending; start backoff over.
    Healthy,
    Lost(String),
}

async fn connection(
    ctx: &mut Ctx,
    helix: &Helix,
    id: &Identity,
    account: &Account,
    url: &mut String,
    fresh: bool,
) -> Ws {
    let (mut ws, _) = match timeout(
        Duration::from_secs(15),
        tokio_tungstenite::connect_async(url.as_str()),
    )
    .await
    {
        Ok(Ok(c)) => c,
        Ok(Err(e)) => return Ws::Lost(format!("Twitch: can't reach EventSub ({e}).")),
        Err(_) => return Ws::Lost("Twitch: EventSub didn't answer in 15 s.".into()),
    };
    let welcome = timeout(Duration::from_secs(15), async {
        while let Some(msg) = ws.next().await {
            if let Ok(Message::Text(t)) = msg
                && let Ok(Frame::Welcome {
                    session_id,
                    keepalive_secs,
                }) = eventsub::parse(&t)
            {
                return Some((session_id, keepalive_secs));
            }
        }
        None
    })
    .await;
    let Ok(Some((session_id, keepalive_secs))) = welcome else {
        return Ws::Lost("Twitch: EventSub sent no welcome.".into());
    };
    if fresh {
        for kind in eventsub::SUBSCRIPTIONS {
            match helix
                .subscribe(&session_id, kind, &id.channel.id, &id.me.id)
                .await
            {
                Ok(()) => {}
                Err(HelixError::Unauthorized) => return Ws::Expired,
                Err(e) if kind == "channel.chat.message" => return Ws::Lost(e.message()),
                Err(e) => tracing::warn!(kind, err = %e.message(), "Twitch subscription failed"),
            }
        }
    }
    ctx.events
        .status(ConnectionStatus::Connected, Some(account.clone()), id.caps)
        .await;

    let started = Instant::now();
    let keepalive = Duration::from_secs(keepalive_secs + 5);
    let mut last = Instant::now();
    let mut validate_at = Instant::now() + VALIDATE_EVERY;
    let end = loop {
        tokio::select! {
            msg = ws.next() => match msg {
                Some(Ok(Message::Text(t))) => {
                    last = Instant::now();
                    match eventsub::parse(&t) {
                        Ok(Frame::Notification { kind, timestamp, event }) => {
                            if let Some(e) = eventsub::to_event(&kind, &timestamp, &event, &id.badges)
                                && !ctx.events.send(e).await
                            {
                                break Ws::Stop;
                            }
                        }
                        Ok(Frame::Reconnect { url: new }) => {
                            *url = new;
                            break Ws::Moved;
                        }
                        Ok(Frame::Revocation { kind, status }) => {
                            ctx.events.notice(Level::Warn, format!("Twitch stopped sending {kind} ({status}).")).await;
                            if kind == "channel.chat.message" {
                                break Ws::Lost(format!("Twitch revoked chat access ({status})."));
                            }
                        }
                        Ok(_) => {}
                        Err(e) => tracing::warn!(err = %e, len = t.len(), "Twitch EventSub message skipped"),
                    }
                }
                Some(Ok(Message::Ping(p))) => {
                    last = Instant::now();
                    if ws.send(Message::Pong(p)).await.is_err() {
                        break Ws::Lost("Twitch: EventSub ping failed.".into());
                    }
                }
                Some(Ok(Message::Close(frame))) => {
                    break Ws::Lost(format!("Twitch closed EventSub ({}).", frame.map(|f| f.reason.to_string()).unwrap_or_default()));
                }
                Some(Ok(_)) => last = Instant::now(),
                Some(Err(e)) => break Ws::Lost(format!("Twitch: EventSub error ({e}).")),
                None => break Ws::Lost("Twitch: EventSub closed.".into()),
            },
            _ = sleep_until(last + keepalive) => break Ws::Lost("Twitch: EventSub went quiet.".into()),
            _ = sleep_until(validate_at) => {
                validate_at = Instant::now() + VALIDATE_EVERY;
                let access = helix.access().await;
                if let Err(AuthError::Invalid) = auth::validate(&ctx.http, &access).await
                    && let Err(HelixError::Unauthorized) = helix.refresh(&access).await
                {
                    break Ws::Expired;
                }
            }
            req = ctx.requests.recv() => {
                let Some(req) = req else { break Ws::Stop };
                if let Some(Outcome::SignedOut) = answer(ctx, Some((helix, id)), req).await {
                    break Ws::SignedOut;
                }
            }
        }
    };
    let _ = ws.close(None).await;
    match end {
        Ws::Lost(why) if started.elapsed() > Duration::from_secs(60) => {
            tracing::info!(err = %why, "Twitch EventSub dropped after a healthy run");
            Ws::Healthy
        }
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_codes_are_split_for_reading() {
        assert_eq!(format_code("ABCDEFGH"), "ABCD-EFGH");
        assert_eq!(format_code("ABC"), "ABC");
    }

    #[test]
    fn settings_override_and_clean_the_channel() {
        let a = TwitchAdapter::new(&rill_os::settings::TwitchSettings {
            client_id: Some("abc".into()),
            channel: Some(" #SomeChannel ".into()),
        });
        assert_eq!(a.client_id.as_deref(), Some("abc"));
        assert_eq!(a.channel.as_deref(), Some("somechannel"));
    }
}

/// EventSub connections against local stand-ins for Twitch's servers.
#[cfg(test)]
mod sessions {
    use super::*;
    use rill_core::{Event, RequestId};
    use serde_json::{Value, json};
    use std::sync::{Arc, Mutex};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    const MESSAGE: &str = include_str!("../tests/fixtures/chat_message.json");

    /// Requests the fake Helix saw: method, path, JSON body.
    type Seen = Arc<Mutex<Vec<(String, String, Value)>>>;

    /// A fake Helix: accepts subscriptions and sends, and records every request.
    async fn helix_server() -> (String, Seen) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let seen: Seen = Arc::default();
        let log = seen.clone();
        tokio::spawn(async move {
            loop {
                let (mut s, _) = listener.accept().await.unwrap();
                let log = log.clone();
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
                    let path = first.next().unwrap().to_string();
                    let body: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
                    let reply = match path.as_str() {
                        "/eventsub/subscriptions" => json!({"data": [{"status": "enabled"}]}),
                        "/chat/messages" => {
                            json!({"data": [{"message_id": "sent-1", "is_sent": true}]})
                        }
                        _ => json!({"data": []}),
                    };
                    log.lock().unwrap().push((method, path, body));
                    let reply = reply.to_string();
                    let res = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}",
                        reply.len()
                    );
                    s.write_all(res.as_bytes()).await.unwrap();
                });
            }
        });
        (format!("http://{addr}"), seen)
    }

    type Server = tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>;

    /// A one-connection EventSub server; `script` plays Twitch's side.
    async fn socket_server<F, Fut>(script: F) -> String
    where
        F: FnOnce(Server) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = ()> + Send,
    {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (s, _) = listener.accept().await.unwrap();
            script(tokio_tungstenite::accept_async(s).await.unwrap()).await;
        });
        format!("ws://{addr}")
    }

    fn frame(kind: &str, payload: Value) -> Message {
        let meta = json!({"message_id": "x", "message_type": kind, "message_timestamp": "2026-11-05T12:00:00Z"});
        Message::text(json!({"metadata": meta, "payload": payload}).to_string())
    }

    fn welcome(session: &str, keepalive: u64) -> Message {
        frame(
            "session_welcome",
            json!({"session": {"id": session, "status": "connected", "keepalive_timeout_seconds": keepalive}}),
        )
    }

    fn user(id: &str, login: &str) -> User {
        User {
            id: id.into(),
            login: login.into(),
            display_name: login.into(),
        }
    }

    fn identity() -> Identity {
        Identity {
            me: user("100", "rill_dev"),
            channel: user("200", "somechannel"),
            caps: MODERATOR,
            badges: Badges::new(),
        }
    }

    fn account() -> Account {
        Account {
            id: "100".into(),
            login: "rill_dev".into(),
            display_name: "rill_dev".into(),
        }
    }

    fn setup(helix_url: &str) -> (Ctx, Helix, mpsc::Receiver<Event>, mpsc::Sender<Request>) {
        let (events, rx) = EventSink::channel(Platform::Twitch, 100);
        let (tx, requests) = mpsc::channel(4);
        let http = reqwest::Client::new();
        let token = Token {
            access: "access".into(),
            refresh: "refresh".into(),
        };
        let helix = Helix::new(http.clone(), "client".into(), token).with_base(helix_url);
        let ctx = Ctx {
            events,
            requests,
            http,
            client_id: "client".into(),
            channel: None,
        };
        (ctx, helix, rx, tx)
    }

    fn drained(rx: &mut mpsc::Receiver<Event>) -> Vec<Event> {
        std::iter::from_fn(|| rx.try_recv().ok()).collect()
    }

    fn subscriptions(seen: &Seen) -> Vec<(String, String)> {
        seen.lock()
            .unwrap()
            .iter()
            .filter(|(_, path, _)| path == "/eventsub/subscriptions")
            .map(|(_, _, b)| {
                (
                    b["type"].as_str().unwrap().to_string(),
                    b["transport"]["session_id"].as_str().unwrap().to_string(),
                )
            })
            .collect()
    }

    #[tokio::test]
    async fn subscribes_reads_and_follows_a_reconnect_without_resubscribing() {
        let (helix_url, seen) = helix_server().await;
        let watch = seen.clone();
        let second = socket_server(|mut ws| async move {
            ws.send(welcome("s2", 10)).await.unwrap();
            ws.send(Message::text(MESSAGE)).await.unwrap();
            // Stay up until the send reaches Helix, then drop.
            for _ in 0..250 {
                if watch
                    .lock()
                    .unwrap()
                    .iter()
                    .any(|(_, p, _)| p == "/chat/messages")
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            ws.close(None).await.unwrap();
        })
        .await;
        let moved_to = second.clone();
        let first = socket_server(|mut ws| async move {
            ws.send(welcome("s1", 10)).await.unwrap();
            ws.send(Message::text(MESSAGE)).await.unwrap();
            let reconnect = json!({"session": {"id": "s1", "status": "reconnecting", "reconnect_url": moved_to}});
            ws.send(frame("session_reconnect", reconnect)).await.unwrap();
            let _ = ws.next().await;
        })
        .await;

        let (mut ctx, helix, mut rx, tx) = setup(&helix_url);
        let (id, account) = (identity(), account());
        let mut url = first;
        let end = connection(&mut ctx, &helix, &id, &account, &mut url, true).await;
        assert!(matches!(end, Ws::Moved));
        assert_eq!(url, second);
        let subs = subscriptions(&seen);
        assert_eq!(
            subs.iter().map(|(k, _)| k.as_str()).collect::<Vec<_>>(),
            eventsub::SUBSCRIPTIONS
        );
        assert!(subs.iter().all(|(_, s)| s == "s1"));
        let events = drained(&mut rx);
        assert!(events.iter().any(|e| matches!(
            e,
            Event::Status {
                status: ConnectionStatus::Connected,
                ..
            }
        )));
        assert!(
            events
                .iter()
                .any(|e| matches!(e, Event::Message(m) if m.plain_text().starts_with("Hi chat")))
        );

        let command = Command::Send {
            text: "hello".into(),
            reply_to: None,
        };
        tx.send(Request {
            id: RequestId(7),
            command,
        })
        .await
        .unwrap();
        let end = timeout(
            Duration::from_secs(10),
            connection(&mut ctx, &helix, &id, &account, &mut url, false),
        )
        .await
        .expect("connection hung");
        assert!(matches!(end, Ws::Lost(_)));
        assert_eq!(
            subscriptions(&seen).len(),
            4,
            "a moved session keeps its subscriptions"
        );
        let sent: Vec<Value> = seen
            .lock()
            .unwrap()
            .iter()
            .filter(|(m, p, _)| m == "POST" && p == "/chat/messages")
            .map(|(_, _, b)| b.clone())
            .collect();
        assert_eq!(
            sent,
            [json!({"broadcaster_id": "200", "sender_id": "100", "message": "hello"})]
        );
        let events = drained(&mut rx);
        assert!(events.iter().any(|e| matches!(e, Event::Message(_))));
        assert!(events.iter().any(|e| matches!(
            e,
            Event::Done { request: RequestId(7), result: Ok(Some(id)), .. } if id == "sent-1"
        )));
    }

    #[tokio::test]
    async fn revoked_chat_access_ends_the_connection_with_a_notice() {
        let (helix_url, _) = helix_server().await;
        let url = socket_server(|mut ws| async move {
            ws.send(welcome("s1", 10)).await.unwrap();
            let sub = json!({"subscription": {"type": "channel.chat.message", "status": "authorization_revoked"}});
            ws.send(frame("revocation", sub)).await.unwrap();
            let _ = ws.next().await;
        })
        .await;
        let (mut ctx, helix, mut rx, _tx) = setup(&helix_url);
        let mut url = url;
        let end = connection(&mut ctx, &helix, &identity(), &account(), &mut url, true).await;
        assert!(matches!(end, Ws::Lost(why) if why.contains("revoked")));
        assert!(drained(&mut rx).iter().any(|e| matches!(
            e,
            Event::Notice { text, .. } if text.contains("authorization_revoked")
        )));
    }

    #[tokio::test]
    async fn a_silent_connection_counts_as_lost() {
        let (helix_url, _) = helix_server().await;
        let url = socket_server(|mut ws| async move {
            ws.send(welcome("s1", 1)).await.unwrap();
            tokio::time::sleep(Duration::from_secs(30)).await;
            drop(ws);
        })
        .await;
        let (mut ctx, helix, _rx, _tx) = setup(&helix_url);
        let mut url = url;
        let end = timeout(
            Duration::from_secs(15),
            connection(&mut ctx, &helix, &identity(), &account(), &mut url, true),
        )
        .await
        .expect("keepalive timeout never fired");
        assert!(matches!(end, Ws::Lost(why) if why.contains("quiet")));
    }
}
