//! Rill's Kick adapter: reads chat from Kick's unofficial socket, sends through the official API.

pub mod api;
pub mod auth;
pub mod centrifugo;
pub mod events;
pub mod pusher;
pub mod realtime;

use std::collections::HashMap;
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use rill_core::backoff::Backoff;
use rill_core::{
    Account, Action, Adapter, AdapterIo, Capabilities, Command, ConnectionStatus, Event, EventSink,
    Level, Platform, Request, UserRef, adapter::BoxFuture, event::SignInPrompt,
};
use serde_json::Value;
use tokio::sync::mpsc;
use tokio::time::{Instant, sleep_until, timeout};
use tokio_tungstenite::tungstenite::{Error as WsError, Message};

use crate::api::{Api, ApiError};
use crate::auth::{AuthError, Exchange};
use crate::centrifugo::Reply;
use crate::events::SubBadges;
use crate::pusher::Frame;
use crate::realtime::Transport;

/// Prefix of errors where Kick's socket refused Rill outright; retrying won't help.
const REFUSED: &str = "Kick: Kick's chat refused Rill; Kick may have changed it. Reading Kick chat needs a Rill update.";
const NOT_SIGNED_IN: &str = "Kick: reading chat needs no account, but sending and moderating do. Open Accounts and sign in to Kick.";
const NO_APP: &str = "Kick: sign-in isn't set up in this build. Add kick.client_id and kick.client_secret (or kick.auth_helper) to settings.json.";

const BUILTIN_CLIENT_ID: Option<&str> = option_env!("RILL_KICK_CLIENT_ID");
const BUILTIN_AUTH_HELPER: Option<&str> = option_env!("RILL_KICK_AUTH_HELPER");

/// What a signed-in Kick account can do; Kick refuses moderation where it isn't a moderator.
const SIGNED_IN: Capabilities = Capabilities::of(&[
    Action::Ads,
    Action::Title,
    Action::Category,
    Action::Send,
    Action::Reply,
    Action::Delete,
    Action::Timeout,
    Action::Ban,
    Action::Unban,
]);

/// The Kick connection.
pub struct KickAdapter {
    channel: Option<String>,
    exchange: Option<Exchange>,
}

impl KickAdapter {
    pub fn new(settings: &rill_os::settings::KickSettings) -> KickAdapter {
        let given = |v: &Option<String>| v.clone().filter(|s| !s.trim().is_empty());
        let client_id = given(&settings.client_id).or(BUILTIN_CLIENT_ID.map(str::to_string));
        let exchange = client_id.and_then(|client_id| {
            if let Some(secret) = given(&settings.client_secret) {
                return Some(Exchange::Direct { client_id, secret });
            }
            given(&settings.auth_helper)
                .or(BUILTIN_AUTH_HELPER.map(str::to_string))
                .map(|url| Exchange::Helper { client_id, url })
        });
        KickAdapter {
            channel: settings.channel.as_deref().and_then(clean_channel),
            exchange,
        }
    }
}

/// A channel name from user input: trims, drops a kick.com URL prefix, lowercases.
pub fn clean_channel(input: &str) -> Option<String> {
    let s = input.trim().trim_end_matches('/');
    let s = s.rsplit('/').next().unwrap_or(s).trim_start_matches('@');
    let ok = !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    ok.then(|| s.to_lowercase())
}

impl Adapter for KickAdapter {
    fn platform(&self) -> Platform {
        Platform::Kick
    }

    fn run(self: Box<Self>, io: AdapterIo) -> BoxFuture {
        Box::pin(run(*self, io))
    }
}

/// What the channel page tells us.
struct Channel {
    slug: String,
    chatroom_id: u64,
    /// The owner's user ID: the `broadcaster_user_id` of Kick's official API.
    broadcaster_id: Option<u64>,
    /// The channel's own ID, which names its event channels on the socket.
    channel_id: Option<u64>,
    sub_badges: SubBadges,
    /// The IDs 7TV may know the channel by: the owner's user ID, then the channel ID.
    seven_tv_ids: Vec<String>,
}

/// What interrupted a wait or a connection.
#[derive(Debug)]
enum Interrupt {
    Stop,
    /// Switch to this channel, or forget the channel when `None`.
    Switch(Option<String>),
}

struct Ctx {
    /// A random ID for this run, like an anonymous viewer's.
    client_id: String,
    /// Where Centrifugo guest tokens come from; tests use a local server.
    token_url: String,
    events: EventSink,
    requests: mpsc::Receiver<Request>,
    http: reqwest::Client,
    shared: Shared,
    /// How Kick sign-in trades codes for tokens; `None` when this build can't sign in.
    exchange: Option<Exchange>,
    /// Sends and moderation, handled one at a time off the chat loop.
    work: mpsc::Sender<Request>,
    /// Interruptions from background tasks, such as reading your own channel after sign-in.
    wake: mpsc::Receiver<Interrupt>,
    wake_tx: mpsc::Sender<Interrupt>,
}

/// State the chat loop shares with the sign-in and API tasks.
#[derive(Clone, Default)]
struct Shared(Arc<StdMutex<State>>);

#[derive(Default)]
struct State {
    signed: Option<Signed>,
    /// The chat connection's status, reported again whenever the account changes.
    read: ConnectionStatus,
    slug: Option<String>,
    broadcaster: Option<u64>,
    /// Recent chatters' user IDs by login, for commands that name a user.
    logins: HashMap<String, u64>,
    /// The channel's Kick emotes by name, so sent messages show them as emotes.
    kick_emotes: HashMap<String, u64>,
    /// Ad breaks already announced, by Kick's ID.
    ads_seen: std::collections::HashSet<String>,
    /// Event names already logged as unknown.
    unknown_events: std::collections::HashSet<String>,
}

#[derive(Clone)]
struct Signed {
    api: Api,
    account: Account,
}

/// How many chatters' IDs are kept for commands by name.
const LOGINS_MAX: usize = 20_000;

impl Shared {
    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Reports the chat status with the current account and what it can do.
    async fn report(&self, events: &EventSink, status: ConnectionStatus) {
        let (account, caps) = {
            let mut s = self.lock();
            s.read = status.clone();
            match &s.signed {
                Some(signed) => (Some(signed.account.clone()), SIGNED_IN),
                None => (None, Capabilities::NONE),
            }
        };
        events.status(status, account, caps).await;
    }

    /// Reports the account change without a new chat status.
    async fn report_account(&self, events: &EventSink) {
        let status = self.lock().read.clone();
        self.report(events, status).await;
    }
}

async fn run(adapter: KickAdapter, io: AdapterIo) {
    let AdapterIo { events, requests } = io;
    let http = match reqwest::Client::builder()
        .user_agent(concat!("Rill/", env!("CARGO_PKG_VERSION")))
        .timeout(Duration::from_secs(20))
        .build()
    {
        Ok(h) => h,
        Err(e) => {
            let why = format!("Kick: HTTP setup failed ({e}). Restart Rill.");
            events
                .status(ConnectionStatus::Failed(why), None, Capabilities::NONE)
                .await;
            return;
        }
    };
    let shared = Shared::default();
    let (work, work_rx) = mpsc::channel(2_048);
    tokio::spawn(worker(shared.clone(), events.clone(), work_rx));
    tokio::spawn(watch_ads(shared.clone(), events.clone()));
    let (wake_tx, wake) = mpsc::channel(4);
    let mut ctx = Ctx {
        client_id: random_client_id(),
        token_url: realtime::TOKEN_URL.into(),
        events,
        requests,
        http,
        shared,
        exchange: adapter.exchange,
        work,
        wake,
        wake_tx,
    };
    restore_sign_in(&ctx, adapter.channel.is_none()).await;
    let mut channel = adapter.channel;
    loop {
        let Some(slug) = channel.clone() else {
            ctx.shared.lock().slug = None;
            ctx.shared
                .report(&ctx.events, ConnectionStatus::Disconnected)
                .await;
            match wait(&mut ctx, None).await {
                Some(Interrupt::Switch(next)) => {
                    channel = next;
                    remember(channel.as_deref());
                }
                _ => return,
            }
            continue;
        };
        match follow(&mut ctx, &slug).await {
            Interrupt::Stop => return,
            Interrupt::Switch(next) => {
                channel = next;
                remember(channel.as_deref());
            }
        }
    }
}

/// A random version-4 UUID, as Kick's web player makes for anonymous viewers.
fn random_client_id() -> String {
    random_uuid()
}

/// A random version-4 UUID.
pub(crate) fn random_uuid() -> String {
    let mut rng = rill_core::backoff::SmallRng::from_time();
    let (a, b) = (rng.next_u64(), rng.next_u64());
    format!(
        "{:08x}-{:04x}-4{:03x}-{:04x}-{:012x}",
        a >> 32,
        (a >> 16) & 0xffff,
        a & 0x0fff,
        0x8000 | ((b >> 48) & 0x3fff),
        b & 0xffff_ffff_ffff
    )
}

/// A user's ID: given, remembered from chat, or looked up by name.
async fn find_user(shared: &Shared, api: &Api, u: &UserRef) -> Result<u64, String> {
    if let Some(id) = u.id.as_deref().and_then(|id| id.parse().ok()) {
        return Ok(id);
    }
    let key = u.login.trim_start_matches('@').to_lowercase();
    if let Some(id) = shared.lock().logins.get(&key).copied() {
        return Ok(id);
    }
    match api.user_id(&key).await.map_err(|e| e.message())? {
        Some(id) => {
            shared.lock().logins.insert(key, id);
            Ok(id)
        }
        None => Err(format!("Kick: there's no user named \"{}\".", u.login)),
    }
}

/// Whether Kick refused an unban because the user isn't banned.
fn not_banned(message: &str) -> bool {
    let m = message.to_lowercase();
    m.contains("not banned") || m.contains("not currently banned") || m.contains("no active ban")
}

fn remember(channel: Option<&str>) {
    let result = rill_os::settings::update(|s| s.kick.channel = channel.map(str::to_string));
    if let Err(e) = result {
        tracing::error!(err = %e, "Kick channel not saved");
    }
}

fn account(id: u64, name: &str) -> Account {
    Account {
        id: id.to_string(),
        login: name.to_lowercase(),
        display_name: name.to_string(),
    }
}

/// Picks up a stored Kick sign-in at startup, checking it in the background; reads your own channel if none is set.
async fn restore_sign_in(ctx: &Ctx, read_own: bool) {
    let Some(exchange) = ctx.exchange.clone() else {
        return;
    };
    let token = match auth::load_token() {
        Ok(Some(t)) => t,
        Ok(None) => return,
        Err(e) => {
            tracing::warn!(err = %e, "Kick sign-in not loaded");
            return;
        }
    };
    let api = Api::new(ctx.http.clone(), exchange, token);
    let (events, shared, wake) = (ctx.events.clone(), ctx.shared.clone(), ctx.wake_tx.clone());
    tokio::spawn(async move {
        match api.me().await {
            Ok((id, name)) => {
                let own_slug = if read_own {
                    api.own_slug().await.ok().flatten()
                } else {
                    None
                };
                tracing::info!("Kick signed in");
                let reading = {
                    let mut s = shared.lock();
                    s.signed = Some(Signed {
                        api,
                        account: account(id, &name),
                    });
                    s.slug.is_some()
                };
                shared.report_account(&events).await;
                if !reading && let Some(slug) = own_slug {
                    let _ = wake.send(Interrupt::Switch(Some(slug))).await;
                }
            }
            Err(ApiError::Unauthorized) => {
                let _ = auth::delete_token();
                events
                    .notice(Level::Warn, AuthError::Invalid.message())
                    .await;
            }
            Err(e) => tracing::warn!(err = %e.message(), "Kick sign-in not checked"),
        }
    });
}

/// Runs a browser sign-in, then reports the account; reads your own channel if none is open.
async fn sign_in(
    http: reqwest::Client,
    exchange: Exchange,
    events: EventSink,
    shared: Shared,
    wake: mpsc::Sender<Interrupt>,
    request: rill_core::RequestId,
) {
    let result: Result<(), String> = async {
        let pending = auth::start(&exchange).await.map_err(|e| e.message())?;
        events
            .send(Event::SignIn {
                platform: Platform::Kick,
                prompt: SignInPrompt {
                    url: pending.url.clone(),
                    code: None,
                    expires_at_ms: rill_core::now_ms() + 10 * 60_000,
                },
            })
            .await;
        let token = timeout(
            Duration::from_secs(10 * 60),
            auth::finish(&http, &exchange, auth::TOKEN_URL, pending),
        )
        .await
        .map_err(|_| "Kick: sign-in timed out. Connect Kick again when you're ready.".to_string())?
        .map_err(|e| e.message())?;
        if let Err(e) = auth::save_token(&token) {
            tracing::warn!(err = %e, "Kick token not saved");
        }
        let api = Api::new(http.clone(), exchange.clone(), token);
        let (id, name) = api.me().await.map_err(|e| e.message())?;
        let own_slug = api.own_slug().await.ok().flatten();
        tracing::info!("Kick signed in");
        let reading = {
            let mut s = shared.lock();
            s.signed = Some(Signed {
                api,
                account: account(id, &name),
            });
            s.slug.is_some()
        };
        shared.report_account(&events).await;
        if !reading && let Some(slug) = own_slug {
            let _ = wake.send(Interrupt::Switch(Some(slug))).await;
        }
        Ok(())
    }
    .await;
    if result.is_err() {
        // Clears the sign-in prompt.
        shared.report_account(&events).await;
    }
    events.done(request, result.map(|_| None)).await;
}

/// Forgets the Kick account: revokes and deletes the stored sign-in.
async fn sign_out(ctx: &Ctx) {
    let signed = ctx.shared.lock().signed.take();
    if let Some(signed) = signed {
        auth::revoke(&ctx.http, &signed.api.token().await).await;
    }
    if let Err(e) = auth::delete_token() {
        tracing::warn!(err = %e, "Kick token not deleted");
    }
}

/// How often Kick's ad status is checked while signed in to your own channel.
const ADS_EVERY: Duration = Duration::from_secs(30);

/// Announces ad breaks that start without Rill (Kick's automatic ads), since Kick sends no event.
async fn watch_ads(shared: Shared, events: EventSink) {
    loop {
        tokio::time::sleep(ADS_EVERY).await;
        let (api, own) = {
            let s = shared.lock();
            let own = matches!((&s.signed, s.broadcaster), (Some(signed), Some(b)) if signed.account.id == b.to_string());
            (s.signed.as_ref().map(|x| x.api.clone()), own)
        };
        let (Some(api), true) = (api, own) else {
            continue;
        };
        let breaks = match api.ad_breaks().await {
            Ok(b) => b,
            Err(e) => {
                tracing::debug!(err = %e.message(), "Kick ad status not read");
                continue;
            }
        };
        let now = chrono::Utc::now();
        for b in breaks {
            let Some(remaining) = b.remaining(now) else {
                continue;
            };
            if shared.lock().ads_seen.insert(b.id.clone()) {
                let why = if b.source.contains("auto") {
                    "automatic"
                } else {
                    "started on Kick"
                };
                events.ads_started(remaining, why).await;
            }
        }
    }
}

/// Carries out sends and moderation one at a time, so the chat loop never waits on them.
async fn worker(shared: Shared, events: EventSink, mut requests: mpsc::Receiver<Request>) {
    while let Some(req) = requests.recv().await {
        let result = perform(&shared, &events, req.command).await;
        if !events.done(req.id, result).await {
            return;
        }
    }
}

async fn perform(
    shared: &Shared,
    events: &EventSink,
    command: Command,
) -> Result<Option<String>, String> {
    match command {
        Command::Poll { .. } => {
            return Err(
                "Kick: polls can't be started through Kick's API. Start them on kick.com.".into(),
            );
        }
        Command::SetMode { .. } | Command::Announce { .. } | Command::Shoutout { .. } => {
            return Err(
                "Kick: chat modes, announcements and shoutouts aren't in Kick's API yet. Use kick.com."
                    .into(),
            );
        }
        _ => {}
    }
    let (signed, broadcaster) = {
        let s = shared.lock();
        (s.signed.clone(), s.broadcaster)
    };
    let api = signed.ok_or(NOT_SIGNED_IN)?.api;
    let broadcaster = broadcaster
        .ok_or("Kick: Rill hasn't found this channel's owner yet. Try again in a moment.")?;
    let result = match command {
        Command::Send { text, reply_to } => {
            let text = events::encode_emotes(&text, &shared.lock().kick_emotes);
            api.send(broadcaster, &text, reply_to.as_deref())
                .await
                .map(Some)
        }
        Command::Delete { message_id } => api.delete(&message_id).await.map(|_| None),
        Command::Timeout {
            user,
            seconds,
            reason,
        } => {
            let id = find_user(shared, &api, &user).await?;
            api.ban(
                broadcaster,
                id,
                Some(api::minutes(seconds)),
                reason.as_deref(),
            )
            .await
            .map(|_| None)
        }
        Command::Ban { user, reason } => {
            let id = find_user(shared, &api, &user).await?;
            api.ban(broadcaster, id, None, reason.as_deref())
                .await
                .map(|_| None)
        }
        Command::Unban { user } => {
            let id = find_user(shared, &api, &user).await?;
            match api.unban(broadcaster, id).await {
                Err(ApiError::Api { message, .. }) if not_banned(&message) => Ok(Some(format!(
                    "{} is not currently banned.",
                    user.login.trim_start_matches('@')
                ))),
                r => r.map(|_| None),
            }
        }
        Command::StartAds { seconds } => {
            let seconds = seconds.clamp(7, api::MAX_AD_SECONDS);
            let id = api.start_ad(seconds).await.map_err(|e| e.message())?;
            shared.lock().ads_seen.insert(id);
            events.ads_started(seconds, "started by you").await;
            Ok(None)
        }
        Command::SetTitle { title } => api
            .set_channel(serde_json::json!({ "stream_title": title }))
            .await
            .map(|_| None),
        Command::SetCategory { name } => {
            let (id, found) = api.find_category(&name).await.map_err(|e| e.message())?;
            api.set_channel(serde_json::json!({ "category_id": id }))
                .await
                .map(|_| {
                    (!found.eq_ignore_ascii_case(&name)).then(|| format!("matched \"{found}\""))
                })
        }
        _ => Ok(None),
    };
    result.map_err(|e| {
        if e == ApiError::Unauthorized {
            shared.lock().signed = None;
            let _ = auth::delete_token();
            let events = events.clone();
            let shared = shared.clone();
            tokio::spawn(async move { shared.report_account(&events).await });
        }
        e.message()
    })
}

/// Answers requests until `deadline` (or forever). Returns what interrupted it.
async fn wait(ctx: &mut Ctx, deadline: Option<Instant>) -> Option<Interrupt> {
    loop {
        let sleeper = async {
            match deadline {
                Some(d) => sleep_until(d).await,
                None => std::future::pending().await,
            }
        };
        tokio::select! {
            _ = sleeper => return None,
            Some(i) = ctx.wake.recv() => return Some(i),
            req = ctx.requests.recv() => {
                let Some(req) = req else { return Some(Interrupt::Stop) };
                if let Some(i) = answer(ctx, req).await {
                    return Some(i);
                }
            }
        }
    }
}

/// Handles one request. Channel changes interrupt; everything else is refused politely.
async fn answer(ctx: &mut Ctx, req: Request) -> Option<Interrupt> {
    match req.command {
        Command::Join { channel } => match clean_channel(&channel) {
            Some(c) => {
                ctx.events.done(req.id, Ok(None)).await;
                Some(Interrupt::Switch(Some(c)))
            }
            None => {
                ctx.events
                    .done(
                        req.id,
                        Err(format!(
                            "Kick: \"{channel}\" isn't a channel name. Use the name from kick.com/<name>."
                        )),
                    )
                    .await;
                None
            }
        },
        Command::SignOut => {
            sign_out(ctx).await;
            ctx.events.done(req.id, Ok(None)).await;
            Some(Interrupt::Switch(None))
        }
        Command::SignIn => {
            match ctx.exchange.clone() {
                Some(exchange) => {
                    tokio::spawn(sign_in(
                        ctx.http.clone(),
                        exchange,
                        ctx.events.clone(),
                        ctx.shared.clone(),
                        ctx.wake_tx.clone(),
                        req.id,
                    ));
                }
                None => {
                    ctx.events.done(req.id, Err(NO_APP.into())).await;
                }
            }
            None
        }
        _ => {
            let id = req.id;
            if ctx.work.try_send(req).is_err() {
                ctx.events
                    .done(
                        id,
                        Err("Kick: too many requests waiting; try again in a moment.".into()),
                    )
                    .await;
            }
            None
        }
    }
}

/// Reads one channel until told to stop or switch, reconnecting with backoff.
async fn follow(ctx: &mut Ctx, slug: &str) -> Interrupt {
    let mut backoff = Backoff::chat();
    // The channel's 7TV emotes, loaded once and kept current while Rill reads this channel.
    let mut emotes: Option<rill_emotes::Follow> = None;
    {
        let mut s = ctx.shared.lock();
        s.slug = Some(slug.to_string());
        s.broadcaster = None;
        s.logins.clear();
    }
    ctx.shared
        .report(&ctx.events, ConnectionStatus::Connecting)
        .await;
    loop {
        let result = match lookup(&ctx.http, slug).await {
            Ok(channel) => {
                ctx.shared.lock().broadcaster = channel.broadcaster_id;
                if emotes.is_none() {
                    tokio::spawn(load_emotes(
                        ctx.http.clone(),
                        channel.slug.clone(),
                        ctx.shared.clone(),
                        ctx.events.clone(),
                    ));
                }
                if emotes.is_none() {
                    let f = rill_emotes::follow(
                        ctx.http.clone(),
                        rill_emotes::Config::default(),
                        Some(rill_emotes::Channel {
                            platform: "kick",
                            ids: channel.seven_tv_ids.clone(),
                        }),
                        Some(ctx.events.clone()),
                    );
                    ctx.events.set_enrich(Some(f.enrich()));
                    emotes = Some(f);
                }
                match socket(&ctx.http, &ctx.client_id).await {
                    Ok(transport) => {
                        tracing::debug!(transport = transport.name(), "Kick chat socket chosen");
                        connection(ctx, &channel, &transport).await
                    }
                    Err(why) => Err(why),
                }
            }
            Err(Lookup::Missing) => {
                let why = format!(
                    "Kick: there's no channel named \"{slug}\". Check the name in Accounts."
                );
                ctx.shared
                    .report(&ctx.events, ConnectionStatus::Failed(why))
                    .await;
                return match wait(ctx, None).await {
                    Some(i) => i,
                    None => Interrupt::Stop,
                };
            }
            Err(Lookup::Retry(why)) => Err(why),
        };
        match result {
            Ok(Some(i)) => return i,
            Ok(None) => backoff.reset(),
            Err(why) if why.starts_with(REFUSED) => {
                tracing::error!(err = %why, channel = slug, "Kick chat socket refused");
                ctx.shared
                    .report(&ctx.events, ConnectionStatus::Failed(why))
                    .await;
                return wait(ctx, None).await.unwrap_or(Interrupt::Stop);
            }
            Err(why) => {
                tracing::warn!(err = %why, channel = slug, "Kick chat connection lost");
                let attempt = backoff.attempt() + 1;
                ctx.shared
                    .report(&ctx.events, ConnectionStatus::Reconnecting { attempt })
                    .await;
                if attempt == 1 {
                    ctx.events.notice(Level::Warn, why).await;
                }
                let until = Instant::now() + backoff.next_delay();
                if let Some(i) = wait(ctx, Some(until)).await {
                    return i;
                }
            }
        }
    }
}

enum Lookup {
    Missing,
    Retry(String),
}

/// Loads the channel's Kick emotes for the composer's `:` suggestions and for sending.
async fn load_emotes(http: reqwest::Client, slug: String, shared: Shared, events: EventSink) {
    let res = http
        .get(format!("https://kick.com/emotes/{slug}"))
        .header("Accept", "application/json")
        .send()
        .await;
    let v: Value = match res {
        Ok(r) if r.status().is_success() => match r.json().await {
            Ok(v) => v,
            Err(e) => {
                tracing::info!(err = %e, "Kick emotes unreadable");
                return;
            }
        },
        Ok(r) => {
            tracing::info!(status = r.status().as_u16(), "Kick emotes not loaded");
            return;
        }
        Err(e) => {
            tracing::info!(err = %e, "Kick emotes not loaded");
            return;
        }
    };
    let list = events::emote_list(&v);
    tracing::debug!(count = list.len(), "Kick emotes loaded");
    let options = list
        .iter()
        .map(|(name, id)| rill_core::EmoteOption {
            name: name.clone(),
            url: Some(events::emote_url(&id.to_string())),
            size: None,
        })
        .collect();
    shared.lock().kick_emotes = list.into_iter().collect();
    events
        .send(Event::Emotes {
            platform: Platform::Kick,
            source: rill_core::EmoteSource::Platform,
            emotes: options,
        })
        .await;
}

/// Finds a channel's chatroom from its public page data.
async fn lookup(http: &reqwest::Client, slug: &str) -> Result<Channel, Lookup> {
    let res = http
        .get(format!("https://kick.com/api/v2/channels/{slug}"))
        .header("Accept", "application/json")
        .send()
        .await
        .map_err(|e| {
            Lookup::Retry(format!(
                "Kick: can't reach kick.com ({e}). Check the internet connection."
            ))
        })?;
    match res.status().as_u16() {
        200 => {}
        404 => return Err(Lookup::Missing),
        s => {
            return Err(Lookup::Retry(format!(
                "Kick: kick.com refused the channel lookup ({s}). Rill will keep trying."
            )));
        }
    }
    let v: Value = res
        .json()
        .await
        .map_err(|e| Lookup::Retry(format!("Kick: unexpected channel data ({e}).")))?;
    let chatroom_id = v["chatroom"]["id"].as_u64().ok_or_else(|| {
        Lookup::Retry("Kick: the channel data has no chatroom; Kick may have changed it.".into())
    })?;
    let sub_badges = v["subscriber_badges"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|b| {
            Some((
                b["months"].as_u64()?,
                b["badge_image"]["src"].as_str()?.to_string(),
            ))
        })
        .collect();
    let seven_tv_ids = [&v["user_id"], &v["id"]]
        .into_iter()
        .filter_map(|id| id.as_u64().map(|n| n.to_string()))
        .collect();
    Ok(Channel {
        slug: slug.to_string(),
        chatroom_id,
        broadcaster_id: v["user_id"].as_u64(),
        channel_id: v["id"].as_u64(),
        sub_badges,
        seven_tv_ids,
    })
}

/// Posts JSON to web.kick.com. A 4xx other than 429 means Kick refused Rill's request outright.
async fn post_json(
    http: &reqwest::Client,
    url: &str,
    body: &Value,
    what: &str,
) -> Result<Value, String> {
    let res = http
        .post(url)
        .header("Accept", "application/json")
        .header("x-app-platform", "web")
        .json(body)
        .send()
        .await
        .map_err(|e| {
            format!("Kick: can't reach web.kick.com ({e}). Check the internet connection.")
        })?;
    let status = res.status();
    if !status.is_success() {
        let why = format!("Kick refused the {what} request ({status})");
        return Err(
            if status.is_client_error() && status != reqwest::StatusCode::TOO_MANY_REQUESTS {
                format!("{REFUSED} ({why})")
            } else {
                format!("{why}. Rill will keep trying.")
            },
        );
    }
    res.json()
        .await
        .map_err(|e| format!("Kick: unexpected {what} answer ({e})."))
}

/// Asks Kick which chat socket to use, as an anonymous viewer would.
async fn socket(http: &reqwest::Client, client_id: &str) -> Result<Transport, String> {
    let body = realtime::connection_body(client_id, &realtime::PROVIDERS);
    let v = post_json(http, realtime::CONNECTION_URL, &body, "chat settings").await?;
    realtime::transport(&v, &realtime::PROVIDERS).map_err(|why| format!("{REFUSED} ({why})"))
}

/// A guest token for Kick's Centrifugo socket.
async fn guest_token(ctx: &Ctx) -> Result<String, String> {
    let body = realtime::token_body(&ctx.client_id);
    let v = post_json(&ctx.http, &ctx.token_url, &body, "chat token").await?;
    realtime::token(&v).map_err(|why| format!("{REFUSED} ({why})"))
}

/// One socket connection. `Ok(Some)` when interrupted, `Ok(None)` after a long healthy run, `Err` when lost early.
async fn connection(
    ctx: &mut Ctx,
    channel: &Channel,
    transport: &Transport,
) -> Result<Option<Interrupt>, String> {
    let started = Instant::now();
    let mut subscribed = false;
    let end = match transport {
        Transport::Pusher { url } => pusher_session(ctx, channel, url, &mut subscribed).await,
        Transport::Centrifugo { url } => {
            centrifugo_session(ctx, channel, url, &mut subscribed).await
        }
    };
    match end {
        Err(why) if subscribed && started.elapsed() > Duration::from_secs(60) => {
            tracing::info!(err = %why, "Kick chat dropped after a healthy run");
            Ok(None)
        }
        other => other,
    }
}

type Socket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// Opens the socket. It sends no Origin header, which Kick's Centrifugo accepts from apps.
async fn open(url: &str) -> Result<Socket, String> {
    let (ws, _) = timeout(
        Duration::from_secs(15),
        tokio_tungstenite::connect_async(url),
    )
    .await
    .map_err(|_| "Kick: the chat socket didn't answer in 15 s.".to_string())?
    .map_err(|e| format!("Kick: can't reach the chat socket ({e})."))?;
    Ok(ws)
}

/// What a socket message turned out to be, before protocol decoding.
enum Incoming {
    Text(String),
    Skip,
    Lost(String),
}

/// Answers transport pings and turns closes and errors into reasons.
async fn incoming(ws: &mut Socket, msg: Option<Result<Message, WsError>>) -> Incoming {
    match msg {
        Some(Ok(Message::Text(t))) => Incoming::Text(t.to_string()),
        Some(Ok(Message::Ping(p))) => {
            let _ = ws.send(Message::Pong(p)).await;
            Incoming::Skip
        }
        Some(Ok(Message::Close(frame))) => Incoming::Lost(match frame {
            Some(f) if f.reason.is_empty() => {
                format!("Kick closed the chat socket (code {}).", u16::from(f.code))
            }
            Some(f) => format!(
                "Kick closed the chat socket (code {}: {}).",
                u16::from(f.code),
                f.reason
            ),
            None => "Kick closed the chat socket.".into(),
        }),
        None => Incoming::Lost("Kick closed the chat socket.".into()),
        Some(Ok(_)) => Incoming::Skip,
        Some(Err(e)) => Incoming::Lost(format!("Kick: chat socket error ({e}).")),
    }
}

/// Passes a Kick event on. False when Rill is shutting down.
async fn deliver(ctx: &Ctx, channel: &Channel, event: &str, data: &Value) -> bool {
    if !events::is_known(event) && ctx.shared.lock().unknown_events.insert(event.to_string()) {
        let fields: Vec<&str> = data
            .as_object()
            .map(|o| o.keys().map(String::as_str).collect())
            .unwrap_or_default();
        tracing::info!(event, fields = %fields.join(","), "Kick event Rill doesn't show yet");
    }
    match events::to_event(event, data, &channel.sub_badges) {
        Some(e) => {
            if let Event::Message(m) = &e
                && let Ok(id) = m.author.id.parse::<u64>()
            {
                let mut s = ctx.shared.lock();
                if s.logins.len() >= LOGINS_MAX {
                    s.logins.clear();
                }
                s.logins.insert(m.author.login.to_lowercase(), id);
            }
            ctx.events.send(e).await
        }
        None => true,
    }
}

/// Channels with subs, gifts, hosts and redemptions; a refusal on one never ends the chat.
fn event_channels(channel: &Channel) -> Vec<String> {
    let mut out = vec![format!("chatroom_{}", channel.chatroom_id)];
    if let Some(id) = channel.channel_id {
        out.push(format!("channel.{id}"));
        out.push(format!("channel_{id}"));
    }
    out
}

async fn connected(ctx: &Ctx, channel: &Channel, transport: &str) {
    tracing::info!(channel = %channel.slug, transport, "Kick chat connected");
    ctx.shared
        .report(&ctx.events, ConnectionStatus::Connected)
        .await;
}

/// Reads chat over Pusher: the client pings when the socket goes quiet.
async fn pusher_session(
    ctx: &mut Ctx,
    channel: &Channel,
    url: &str,
    subscribed: &mut bool,
) -> Result<Option<Interrupt>, String> {
    let mut ws = open(url).await?;
    let mut activity = Duration::from_secs(120);
    let mut last = Instant::now();
    let mut pinged = false;
    let end: Result<Option<Interrupt>, String> = loop {
        let quiet_until = last + activity;
        tokio::select! {
            msg = ws.next() => {
                last = Instant::now();
                pinged = false;
                let text = match incoming(&mut ws, msg).await {
                    Incoming::Text(t) => t,
                    Incoming::Skip => continue,
                    Incoming::Lost(why) => break Err(why),
                };
                match pusher::parse(&text) {
                    Ok(Frame::Connected { activity_timeout }) => {
                        activity = Duration::from_secs(activity_timeout.clamp(10, 300));
                        if ws.send(Message::text(pusher::subscribe(channel.chatroom_id))).await.is_err() {
                            break Err("Kick: couldn't subscribe to the chatroom.".into());
                        }
                        for name in event_channels(channel) {
                            let _ = ws.send(Message::text(pusher::subscribe_to(&name))).await;
                        }
                    }
                    Ok(Frame::Subscribed) if *subscribed => {}
                    Ok(Frame::Subscribed) => {
                        *subscribed = true;
                        connected(ctx, channel, "pusher").await;
                    }
                    Ok(Frame::Ping) => {
                        let _ = ws.send(Message::text(pusher::pong())).await;
                    }
                    Ok(Frame::Pong) => {}
                    Ok(Frame::Error(m)) => break Err(format!("Kick: the chat socket reported an error ({m}).")),
                    Ok(Frame::Kick { event, data }) => {
                        if !deliver(ctx, channel, &event, &data).await {
                            break Ok(Some(Interrupt::Stop));
                        }
                    }
                    Err(e) => tracing::warn!(err = %e, len = text.len(), "Kick socket message skipped"),
                }
            }
            _ = sleep_until(quiet_until) => {
                if pinged {
                    break Err("Kick: the chat socket went quiet.".into());
                }
                pinged = true;
                last = Instant::now();
                if ws.send(Message::text(pusher::ping())).await.is_err() {
                    break Err("Kick: the chat socket closed.".into());
                }
            }
            Some(i) = ctx.wake.recv() => break Ok(Some(i)),
            req = ctx.requests.recv() => {
                let Some(req) = req else { break Ok(Some(Interrupt::Stop)) };
                if let Some(i) = answer(ctx, req).await {
                    break Ok(Some(i));
                }
            }
        }
    };
    let _ = ws.close(None).await;
    end
}

/// Reads chat over Centrifugo: the server pings, and the guest token is renewed before it runs out.
async fn centrifugo_session(
    ctx: &mut Ctx,
    channel: &Channel,
    url: &str,
    subscribed: &mut bool,
) -> Result<Option<Interrupt>, String> {
    let token = guest_token(ctx).await?;
    let mut ws = open(url).await?;
    let mut last_id = 1u32;
    let mut next_id = || {
        last_id += 1;
        last_id
    };
    if ws
        .send(Message::text(centrifugo::connect(1, &token)))
        .await
        .is_err()
    {
        let _ = ws.close(None).await;
        return Err("Kick: the chat socket closed during the handshake.".into());
    }
    // Until the handshake says otherwise; Kick's server pings every 25 s.
    let mut quiet = Duration::from_secs(25) + PING_SLACK;
    let mut last = Instant::now();
    let mut refresh_at: Option<Instant> = None;
    // The event channels' subscribe requests, whose errors don't matter.
    let mut extra_subs: Vec<u64> = Vec::new();
    let end: Result<Option<Interrupt>, String> = 'session: loop {
        let refresh_due = async {
            match refresh_at {
                Some(t) => sleep_until(t).await,
                None => std::future::pending().await,
            }
        };
        tokio::select! {
            msg = ws.next() => {
                last = Instant::now();
                let text = match incoming(&mut ws, msg).await {
                    Incoming::Text(t) => t,
                    Incoming::Skip => continue,
                    Incoming::Lost(why) => break Err(why),
                };
                for reply in centrifugo::parse(&text) {
                    match reply {
                        Ok(Reply::Ping) => {
                            if ws.send(Message::text(centrifugo::PING)).await.is_err() {
                                break 'session Err("Kick: the chat socket closed.".into());
                            }
                        }
                        Ok(Reply::Connected { ping_secs, ttl_secs }) => {
                            if let Some(p) = ping_secs {
                                quiet = Duration::from_secs(p.clamp(5, 300)) + PING_SLACK;
                            }
                            refresh_at = ttl_secs.map(refresh_time);
                            let sub = centrifugo::subscribe(next_id(), channel.chatroom_id);
                            if ws.send(Message::text(sub)).await.is_err() {
                                break 'session Err("Kick: couldn't subscribe to the chatroom.".into());
                            }
                            for name in event_channels(channel) {
                                let id = next_id();
                                extra_subs.push(u64::from(id));
                                let _ = ws.send(Message::text(centrifugo::subscribe_to(id, &name))).await;
                            }
                        }
                        Ok(Reply::Subscribed { id }) if id.is_some_and(|id| extra_subs.contains(&id)) => {}
                        Ok(Reply::Subscribed { .. }) => {
                            *subscribed = true;
                            connected(ctx, channel, "centrifugo").await;
                        }
                        Ok(Reply::Refreshed { ttl_secs }) => refresh_at = ttl_secs.map(refresh_time),
                        Ok(Reply::Error { id: Some(id), code, message }) if extra_subs.contains(&id) => {
                            tracing::debug!(code, err = %message, "Kick event channel not followed");
                        }
                        Ok(Reply::Error { code, message, .. }) => {
                            break 'session Err(format!("Kick: the chat socket refused a request ({code}: {message})."));
                        }
                        Ok(Reply::Disconnect { code, reason }) => {
                            break 'session Err(format!("Kick closed the chat socket (code {code}: {reason})."));
                        }
                        Ok(Reply::Unsubscribed { channel: name, code }) if !name.starts_with("chatrooms.") => {
                            tracing::debug!(code, channel = %name, "Kick event channel dropped");
                        }
                        Ok(Reply::Unsubscribed { code, .. }) => {
                            break 'session Err(format!("Kick dropped the chatroom subscription (code {code})."));
                        }
                        Ok(Reply::Kick { event, data }) => {
                            if !deliver(ctx, channel, &event, &data).await {
                                break 'session Ok(Some(Interrupt::Stop));
                            }
                        }
                        Ok(Reply::Other) => {}
                        Err(e) => tracing::warn!(err = %e, len = text.len(), "Kick socket message skipped"),
                    }
                }
            }
            _ = sleep_until(last + quiet) => break Err("Kick: the chat socket went quiet.".into()),
            _ = refresh_due => {
                refresh_at = None;
                // On failure the server disconnects when the token runs out, and the reconnect gets a new one.
                match guest_token(ctx).await {
                    Ok(token) => {
                        if ws.send(Message::text(centrifugo::refresh(next_id(), &token))).await.is_err() {
                            break Err("Kick: the chat socket closed.".into());
                        }
                    }
                    Err(e) => tracing::warn!(err = %e, "Kick chat token not renewed"),
                }
            }
            Some(i) = ctx.wake.recv() => break Ok(Some(i)),
            req = ctx.requests.recv() => {
                let Some(req) = req else { break Ok(Some(Interrupt::Stop)) };
                if let Some(i) = answer(ctx, req).await {
                    break Ok(Some(i));
                }
            }
        }
    };
    let _ = ws.close(None).await;
    end
}

/// How long past the server's ping interval a silent socket counts as lost.
const PING_SLACK: Duration = Duration::from_secs(15);

/// When to renew a token that lasts `ttl_secs`: at three quarters of its life.
fn refresh_time(ttl_secs: u64) -> Instant {
    Instant::now() + Duration::from_secs(ttl_secs.max(4) * 3 / 4)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_ids_look_like_uuids() {
        let id = random_client_id();
        let parts: Vec<&str> = id.split('-').collect();
        assert_eq!(
            parts.iter().map(|p| p.len()).collect::<Vec<_>>(),
            [8, 4, 4, 4, 12]
        );
        assert!(parts[2].starts_with('4'));
    }

    #[test]
    fn cleans_channel_names_and_urls() {
        assert_eq!(clean_channel(" xQc "), Some("xqc".into()));
        assert_eq!(
            clean_channel("https://kick.com/Some_One/"),
            Some("some_one".into())
        );
        assert_eq!(
            clean_channel("@trainwreckstv"),
            Some("trainwreckstv".into())
        );
        assert_eq!(clean_channel("not a name"), None);
        assert_eq!(clean_channel(""), None);
    }
}

/// Both socket sessions against local stand-ins for Kick's servers.
#[cfg(test)]
mod sessions {
    use super::*;
    use rill_core::Event;
    use serde_json::json;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;
    use tokio_tungstenite::tungstenite::protocol::CloseFrame;
    use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;

    fn ctx(token_url: &str) -> (Ctx, mpsc::Receiver<Event>, mpsc::Sender<Request>) {
        let (events, rx) = EventSink::channel(Platform::Kick, 100);
        let (tx, requests) = mpsc::channel(4);
        let (work, _) = mpsc::channel(4);
        let (wake_tx, wake) = mpsc::channel(4);
        let ctx = Ctx {
            client_id: "client-1".into(),
            token_url: token_url.into(),
            events,
            requests,
            http: reqwest::Client::new(),
            shared: Shared::default(),
            exchange: None,
            work,
            wake,
            wake_tx,
        };
        (ctx, rx, tx)
    }

    fn channel() -> Channel {
        Channel {
            slug: "someone".into(),
            chatroom_id: 42,
            broadcaster_id: Some(7),
            channel_id: Some(8),
            sub_badges: Vec::new(),
            seven_tv_ids: Vec::new(),
        }
    }

    /// Answers every HTTP request with a fresh token, `t1`, `t2`, … and checks the body.
    async fn token_server() -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            for n in 1.. {
                let (mut s, _) = listener.accept().await.unwrap();
                let mut buf = vec![0; 4096];
                let mut got = 0;
                while !String::from_utf8_lossy(&buf[..got]).contains("client-1") {
                    let r = s.read(&mut buf[got..]).await.unwrap();
                    assert!(r > 0, "token request had no client_id");
                    got += r;
                }
                let body = json!({"data": {"token": format!("t{n}")}}).to_string();
                let res = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                s.write_all(res.as_bytes()).await.unwrap();
            }
        });
        format!("http://{addr}/token")
    }

    type Server = tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>;

    /// A one-connection socket server; `script` plays Kick's side.
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

    /// The next message, skipping subscriptions to the event channels (tested on their own).
    async fn recv_json(ws: &mut Server) -> Value {
        loop {
            match ws.next().await.unwrap().unwrap() {
                Message::Text(t) => {
                    let v: Value = serde_json::from_str(&t).unwrap();
                    let channel = v["subscribe"]["channel"]
                        .as_str()
                        .or(v["data"]["channel"].as_str())
                        .unwrap_or_default();
                    if !channel.is_empty() && !channel.starts_with("chatrooms.") {
                        continue;
                    }
                    return v;
                }
                Message::Ping(_) | Message::Pong(_) => continue,
                other => panic!("unexpected {other:?}"),
            }
        }
    }

    async fn send(ws: &mut Server, text: &str) {
        ws.send(Message::text(text)).await.unwrap();
    }

    /// Collects what the session reported once it ends.
    fn drain(rx: &mut mpsc::Receiver<Event>) -> (bool, Vec<String>) {
        let (mut connected, mut messages) = (false, Vec::new());
        while let Ok(e) = rx.try_recv() {
            match e {
                Event::Status { status, .. } => connected |= status == ConnectionStatus::Connected,
                Event::Message(m) => messages.push(m.plain_text()),
                _ => {}
            }
        }
        (connected, messages)
    }

    const CHAT: &str = r##"{"push":{"channel":"chatrooms.42.v2","pub":{"data":{"event":"App\\Events\\ChatMessageSentEvent","data":"{\"id\":\"m1\",\"content\":\"hello [emote:1:KEKW]\",\"type\":\"message\",\"sender\":{\"id\":7,\"username\":\"Viewer\",\"slug\":\"viewer\",\"identity\":{\"color\":\"#ffffff\",\"badges\":[]}}}"}}}}"##;

    #[tokio::test]
    async fn centrifugo_connects_reads_answers_pings_and_renews_its_token() {
        let token_url = token_server().await;
        let url = socket_server(|mut ws| async move {
            let c = recv_json(&mut ws).await;
            assert_eq!(c["id"], 1);
            assert_eq!(c["connect"], json!({"token": "t1", "name": "js"}));
            send(
                &mut ws,
                r#"{"id":1,"connect":{"client":"c","version":"6","expires":true,"ttl":4,"ping":25,"pong":true}}"#,
            )
            .await;
            let s = recv_json(&mut ws).await;
            assert_eq!(s["subscribe"]["channel"], "chatrooms.42.v2");
            let id = s["id"].as_u64().unwrap();
            // A reply and a ping in one frame.
            send(&mut ws, &format!("{{\"id\":{id},\"subscribe\":{{}}}}\n{{}}")).await;
            assert_eq!(recv_json(&mut ws).await, json!({}));
            send(&mut ws, CHAT).await;
            let r = recv_json(&mut ws).await;
            assert_eq!(r["refresh"]["token"], "t2");
            send(
                &mut ws,
                &format!(r#"{{"id":{},"refresh":{{"expires":true,"ttl":2700}}}}"#, r["id"]),
            )
            .await;
            send(&mut ws, r#"{"push":{"disconnect":{"code":3001,"reason":"shutdown"}}}"#).await;
            let _ = ws.next().await;
        })
        .await;
        let (mut ctx, mut rx, _tx) = ctx(&token_url);
        let mut subscribed = false;
        let end = timeout(
            Duration::from_secs(10),
            centrifugo_session(&mut ctx, &channel(), &url, &mut subscribed),
        )
        .await
        .expect("session hung");
        let err = end.expect_err("a server disconnect ends the session");
        assert!(err.contains("3001") && err.contains("shutdown"), "{err}");
        assert!(subscribed);
        let (connected, messages) = drain(&mut rx);
        assert!(connected);
        assert_eq!(messages, ["hello KEKW"]);
    }

    #[tokio::test]
    async fn centrifugo_reports_a_refused_subscription() {
        let token_url = token_server().await;
        let url = socket_server(|mut ws| async move {
            recv_json(&mut ws).await;
            send(&mut ws, r#"{"id":1,"connect":{"client":"c"}}"#).await;
            let s = recv_json(&mut ws).await;
            send(
                &mut ws,
                &format!(
                    r#"{{"id":{},"error":{{"code":103,"message":"permission denied"}}}}"#,
                    s["id"]
                ),
            )
            .await;
            let _ = ws.next().await;
        })
        .await;
        let (mut ctx, mut rx, _tx) = ctx(&token_url);
        let mut subscribed = false;
        let err = centrifugo_session(&mut ctx, &channel(), &url, &mut subscribed)
            .await
            .unwrap_err();
        assert!(
            err.contains("103") && err.contains("permission denied"),
            "{err}"
        );
        assert!(!subscribed && !drain(&mut rx).0);
    }

    #[tokio::test]
    async fn refused_event_channels_leave_the_chat_running() {
        let token_url = token_server().await;
        let url = socket_server(|mut ws| async move {
            ws.next().await;
            send(&mut ws, r#"{"id":1,"connect":{"client":"c"}}"#).await;
            // The chatroom subscribes fine; each event channel is refused.
            let mut extras = 0;
            while extras < 3 {
                let Some(Ok(Message::Text(t))) = ws.next().await else {
                    panic!("closed")
                };
                let v: Value = serde_json::from_str(&t).unwrap();
                let id = v["id"].clone();
                if v["subscribe"]["channel"] == "chatrooms.42.v2" {
                    send(&mut ws, &format!(r#"{{"id":{id},"subscribe":{{}}}}"#)).await;
                } else {
                    extras += 1;
                    send(
                        &mut ws,
                        &format!(
                            r#"{{"id":{id},"error":{{"code":103,"message":"permission denied"}}}}"#
                        ),
                    )
                    .await;
                }
            }
            send(&mut ws, CHAT).await;
            send(
                &mut ws,
                r#"{"push":{"channel":"channel.8","unsubscribe":{"code":2500}}}"#,
            )
            .await;
            send(&mut ws, CHAT).await;
            send(
                &mut ws,
                r#"{"push":{"disconnect":{"code":3001,"reason":"shutdown"}}}"#,
            )
            .await;
            while let Some(Ok(_)) = ws.next().await {}
        })
        .await;
        let (mut ctx, mut rx, _tx) = ctx(&token_url);
        let mut subscribed = false;
        let end = timeout(
            Duration::from_secs(10),
            centrifugo_session(&mut ctx, &channel(), &url, &mut subscribed),
        )
        .await
        .expect("session hung");
        assert!(matches!(&end, Err(why) if why.contains("3001")), "{end:?}");
        let (connected, messages) = drain(&mut rx);
        assert!(connected);
        assert_eq!(messages.len(), 2, "chat kept flowing: {messages:?}");
    }

    #[tokio::test]
    async fn centrifugo_stops_when_asked_to_switch_channels() {
        let token_url = token_server().await;
        let url = socket_server(|mut ws| async move {
            recv_json(&mut ws).await;
            send(&mut ws, r#"{"id":1,"connect":{"client":"c"}}"#).await;
            let s = recv_json(&mut ws).await;
            send(
                &mut ws,
                &format!(r#"{{"id":{},"subscribe":{{}}}}"#, s["id"]),
            )
            .await;
            // Listen (to the event-channel subscriptions, too) until Rill hangs up.
            while let Some(Ok(_)) = ws.next().await {}
        })
        .await;
        let (mut ctx, mut rx, tx) = ctx(&token_url);
        tokio::spawn(async move {
            sleep_until(Instant::now() + Duration::from_millis(300)).await;
            let command = Command::Join {
                channel: "other".into(),
            };
            let id = rill_core::RequestId(1);
            tx.send(Request { id, command }).await.unwrap();
        });
        let mut subscribed = false;
        let end = centrifugo_session(&mut ctx, &channel(), &url, &mut subscribed).await;
        assert!(matches!(end, Ok(Some(Interrupt::Switch(Some(c)))) if c == "other"));
        assert!(drain(&mut rx).0);
    }

    #[tokio::test]
    async fn pusher_subscribes_reads_and_reports_close_codes() {
        let url = socket_server(|mut ws| async move {
            send(
                &mut ws,
                r#"{"event":"pusher:connection_established","data":"{\"socket_id\":\"1.2\",\"activity_timeout\":120}"}"#,
            )
            .await;
            let s = recv_json(&mut ws).await;
            assert_eq!(s["event"], "pusher:subscribe");
            assert_eq!(s["data"]["channel"], "chatrooms.42.v2");
            send(
                &mut ws,
                r#"{"event":"pusher_internal:subscription_succeeded","data":"{}","channel":"chatrooms.42.v2"}"#,
            )
            .await;
            send(&mut ws, r#"{"event":"pusher:ping","data":{}}"#).await;
            assert_eq!(recv_json(&mut ws).await["event"], "pusher:pong");
            let chat = r#"{"event":"App\\Events\\ChatMessageSentEvent","channel":"chatrooms.42.v2","data":"{\"id\":\"m1\",\"content\":\"hi\",\"sender\":{\"id\":7,\"username\":\"Viewer\"}}"}"#;
            send(&mut ws, chat).await;
            let frame = CloseFrame {
                code: CloseCode::from(4001),
                reason: "App key not in this cluster".into(),
            };
            ws.send(Message::Close(Some(frame))).await.unwrap();
            let _ = ws.next().await;
        })
        .await;
        let (mut ctx, mut rx, _tx) = ctx("http://127.0.0.1:9/unused");
        let mut subscribed = false;
        let err = pusher_session(&mut ctx, &channel(), &url, &mut subscribed)
            .await
            .unwrap_err();
        assert!(err.contains("4001") && err.contains("cluster"), "{err}");
        let (connected, messages) = drain(&mut rx);
        assert!(connected && subscribed);
        assert_eq!(messages, ["hi"]);
    }
}

#[cfg(test)]
mod live {
    use super::*;
    use rill_core::Event;

    /// Reads a live Kick channel for 20 s: `KICK_LIVE=<channel> cargo test -p rill-kick -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn reads_a_live_channel() {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        let channel = std::env::var("KICK_LIVE").unwrap_or_else(|_| "xqc".into());
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let (sink, mut rx) = EventSink::channel(Platform::Kick, 10_000);
            let (_tx, requests) = mpsc::channel(4);
            let adapter = Box::new(KickAdapter {
                channel: Some(channel.clone()),
                exchange: None,
            });
            tokio::spawn(adapter.run(AdapterIo {
                events: sink,
                requests,
            }));
            let until = Instant::now() + Duration::from_secs(20);
            let (mut connected, mut messages) = (false, 0);
            while let Ok(Some(e)) = tokio::time::timeout_at(until, rx.recv()).await {
                match e {
                    Event::Status { status, .. } => {
                        eprintln!("status: {status:?}");
                        connected |= status == ConnectionStatus::Connected;
                    }
                    Event::Message(m) => {
                        messages += 1;
                        if messages <= 5 {
                            eprintln!("{}: {}", m.author.display_name, m.plain_text());
                        }
                    }
                    other => eprintln!("{other:?}"),
                }
            }
            eprintln!("{messages} messages from {channel} in 20 s");
            assert!(connected, "never connected to {channel}");
        });
    }
}

/// Sign-in, sending and moderation against a local stand-in for Kick's official API.
#[cfg(test)]
mod official_api {
    use super::*;
    use serde_json::json;
    use std::sync::Mutex as StdMutex;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    /// A request the fake API saw: method, path, form or JSON body, bearer token.
    #[derive(Debug, Clone)]
    struct Seen {
        method: String,
        path: String,
        body: String,
        bearer: String,
    }

    type Log = Arc<StdMutex<Vec<Seen>>>;

    /// A fake Kick API whose first /users call gets a 401 and first ban a 429.
    async fn kick_server() -> (String, Log) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let log: Log = Arc::default();
        let seen = log.clone();
        let users = Arc::new(AtomicUsize::new(0));
        let bans = Arc::new(AtomicUsize::new(0));
        tokio::spawn(async move {
            loop {
                let (mut s, _) = listener.accept().await.unwrap();
                let (seen, users, bans) = (seen.clone(), users.clone(), bans.clone());
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
                    let bearer = head
                        .lines()
                        .find_map(|l| l.strip_prefix("authorization: Bearer "))
                        .unwrap_or_default()
                        .to_string();
                    let (status, reply) = match (method.as_str(), path.as_str()) {
                        ("POST", "/oauth/token") => (
                            200,
                            json!({"access_token": "fresh", "refresh_token": "r2", "expires_in": 3600, "token_type": "Bearer"}),
                        ),
                        ("GET", "/users") if users.fetch_add(1, Ordering::SeqCst) == 0 => {
                            (401, json!({"message": "Unauthorized"}))
                        }
                        ("GET", "/users") => {
                            (200, json!({"data": [{"user_id": 55, "name": "RillDev"}]}))
                        }
                        ("POST", "/chat") => {
                            (200, json!({"data": {"is_sent": true, "message_id": "m-1"}}))
                        }
                        ("DELETE", p) if p.starts_with("/chat/") => (204, Value::Null),
                        ("POST", "/moderation/bans")
                            if bans.fetch_add(1, Ordering::SeqCst) == 0 =>
                        {
                            (429, json!({"message": "slow down"}))
                        }
                        ("DELETE", "/moderation/bans") if body.contains("404") => {
                            (400, json!({"message": "User is not banned"}))
                        }
                        ("POST" | "DELETE", "/moderation/bans") => (200, json!({"data": {}})),
                        ("GET", "/channels?slug=some-one") => (
                            200,
                            json!({"data": [{"broadcaster_user_id": 321, "slug": "some-one"}]}),
                        ),
                        ("GET", "/channels?slug=nobody") => (200, json!({"data": []})),
                        ("PATCH", "/channels") => (204, Value::Null),
                        ("POST", "/ads/ad-break") => (
                            202,
                            json!({"data": {"ad_break_id": "ab1", "remaining_ad_breaks": 2}}),
                        ),
                        ("GET", "/ads/ad-break-status") => (
                            200,
                            json!({"data": {"ad_breaks": [
                                {"ad_break_id": "ab9", "break_duration_seconds": 60,
                                 "created_at": (chrono::Utc::now() - chrono::Duration::seconds(10)).to_rfc3339(),
                                 "source": "AUTO", "status": "active"},
                                {"ad_break_id": "old", "break_duration_seconds": 30,
                                 "created_at": "2026-01-01T00:00:00Z", "source": "manual"}
                            ]}}),
                        ),
                        ("GET", p) if p.starts_with("/v2/categories") => (200, json!({"data": []})),
                        ("GET", p) if p.starts_with("/categories") => {
                            (200, json!({"data": [{"id": 15, "name": "Just Chatting"}]}))
                        }
                        _ => (404, json!({"message": "no route"})),
                    };
                    seen.lock().unwrap().push(Seen {
                        method,
                        path,
                        body,
                        bearer,
                    });
                    let reply = if reply.is_null() {
                        String::new()
                    } else {
                        reply.to_string()
                    };
                    let res = format!(
                        "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nRetry-After: 1\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}",
                        reply.len()
                    );
                    s.write_all(res.as_bytes()).await.unwrap();
                });
            }
        });
        (format!("http://{addr}"), log)
    }

    fn exchange() -> Exchange {
        Exchange::Direct {
            client_id: "cid".into(),
            secret: "shh".into(),
        }
    }

    fn signed(base: &str) -> Shared {
        let token = auth::Token {
            access: "old".into(),
            refresh: "r1".into(),
            expires_at_ms: rill_core::now_ms() + 3_600_000,
        };
        let api = Api::new(reqwest::Client::new(), exchange(), token).local(base);
        let shared = Shared::default();
        {
            let mut s = shared.lock();
            s.signed = Some(Signed {
                api,
                account: account(55, "RillDev"),
            });
            s.broadcaster = Some(7);
            s.logins.insert("viewer".into(), 99);
        }
        shared
    }

    fn user(id: Option<&str>, login: &str) -> UserRef {
        UserRef {
            id: id.map(str::to_string),
            login: login.into(),
        }
    }

    #[tokio::test]
    async fn sends_and_moderates_with_refresh_and_rate_limit_waits() {
        let (base, log) = kick_server().await;
        let shared = signed(&base);
        let (events, _rx) = EventSink::channel(Platform::Kick, 100);
        let api = shared.lock().signed.clone().unwrap().api;
        // The first call meets a 401: Rill refreshes once and retries.
        assert_eq!(api.me().await.unwrap(), (55, "RillDev".to_string()));

        let send = Command::Send {
            text: "hello".into(),
            reply_to: Some("parent".into()),
        };
        assert_eq!(
            perform(&shared, &events, send).await,
            Ok(Some("m-1".into()))
        );
        let delete = Command::Delete {
            message_id: "m-9".into(),
        };
        assert_eq!(perform(&shared, &events, delete).await, Ok(None));
        // By name, from a recent chatter; 90 s rounds up to 2 minutes; the 429 is waited out.
        let timeout_cmd = Command::Timeout {
            user: user(None, "@Viewer"),
            seconds: 90,
            reason: Some("x".repeat(150)),
        };
        assert_eq!(perform(&shared, &events, timeout_cmd).await, Ok(None));
        let ban = Command::Ban {
            user: user(Some("123"), "whoever"),
            reason: None,
        };
        assert_eq!(perform(&shared, &events, ban).await, Ok(None));
        let unban = Command::Unban {
            user: user(Some("123"), "whoever"),
        };
        assert_eq!(perform(&shared, &events, unban).await, Ok(None));
        // Someone who hasn't chatted is looked up by name.
        let stranger = Command::Ban {
            user: user(None, "Some_One"),
            reason: None,
        };
        assert_eq!(perform(&shared, &events, stranger).await, Ok(None));
        let unknown = Command::Ban {
            user: user(None, "nobody"),
            reason: None,
        };
        let err = perform(&shared, &events, unknown).await.unwrap_err();
        assert!(err.contains("no user named"), "{err}");
        let not_banned = Command::Unban {
            user: user(Some("404"), "Calm"),
        };
        assert_eq!(
            perform(&shared, &events, not_banned).await,
            Ok(Some("Calm is not currently banned.".into()))
        );
        let poll = Command::Poll {
            title: "q".into(),
            choices: vec![],
            seconds: 60,
        };
        assert!(
            perform(&shared, &events, poll)
                .await
                .unwrap_err()
                .contains("kick.com")
        );

        let log = log.lock().unwrap().clone();
        let refresh = log.iter().find(|r| r.path == "/oauth/token").unwrap();
        assert!(refresh.body.contains("grant_type=refresh_token"));
        assert!(refresh.body.contains("client_secret=shh"));
        assert!(
            log.iter()
                .filter(|r| r.path != "/oauth/token")
                .skip(1)
                .all(|r| r.bearer == "fresh")
        );
        let chat: Value =
            serde_json::from_str(&log.iter().find(|r| r.path == "/chat").unwrap().body).unwrap();
        assert_eq!(
            chat,
            json!({"type": "user", "broadcaster_user_id": 7, "content": "hello", "reply_to_message_id": "parent"})
        );
        assert!(
            log.iter()
                .any(|r| r.method == "DELETE" && r.path == "/chat/m-9")
        );
        let bans: Vec<Value> = log
            .iter()
            .filter(|r| r.method == "POST" && r.path == "/moderation/bans")
            .map(|r| serde_json::from_str(&r.body).unwrap())
            .collect();
        assert_eq!(bans.len(), 4, "the rate-limited ban was sent again");
        assert_eq!(bans[3]["user_id"], 321, "found by name");
        assert_eq!(bans[1]["user_id"], 99);
        assert_eq!(bans[1]["duration"], 2);
        assert_eq!(bans[1]["reason"].as_str().unwrap().len(), 100);
        assert_eq!(bans[2], json!({"broadcaster_user_id": 7, "user_id": 123}));
        let unban: Value = serde_json::from_str(
            &log.iter()
                .find(|r| r.method == "DELETE" && r.path == "/moderation/bans")
                .unwrap()
                .body,
        )
        .unwrap();
        assert_eq!(unban, json!({"broadcaster_user_id": 7, "user_id": 123}));
    }

    #[tokio::test]
    async fn changes_the_title_and_finds_categories_by_search() {
        let (base, log) = kick_server().await;
        let shared = signed(&base);
        let (events, _rx) = EventSink::channel(Platform::Kick, 100);
        let title = Command::SetTitle {
            title: "Ranked".into(),
        };
        assert_eq!(perform(&shared, &events, title).await, Ok(None));
        // No exact name match, so the search finds it, and the result says what it matched.
        let category = Command::SetCategory {
            name: "chatting".into(),
        };
        assert_eq!(
            perform(&shared, &events, category).await,
            Ok(Some("matched \"Just Chatting\"".into()))
        );
        let log = log.lock().unwrap().clone();
        let patches: Vec<Value> = log
            .iter()
            .filter(|r| r.method == "PATCH" && r.path == "/channels")
            .map(|r| serde_json::from_str(&r.body).unwrap())
            .collect();
        assert_eq!(
            patches,
            [
                json!({"stream_title": "Ranked"}),
                json!({"category_id": 15})
            ]
        );
        assert!(log.iter().any(|r| r.path == "/v2/categories?name=chatting"));
        assert!(log.iter().any(|r| r.path == "/categories?q=chatting"));
    }

    #[tokio::test]
    async fn starts_ads_and_sees_kicks_automatic_ones() {
        let (base, log) = kick_server().await;
        let shared = signed(&base);
        let (events, mut rx) = EventSink::channel(Platform::Kick, 100);
        let ads = Command::StartAds { seconds: 120 };
        assert_eq!(perform(&shared, &events, ads).await, Ok(None));
        let Some(Event::Notice { text, .. }) = rx.recv().await else {
            panic!("no notice")
        };
        assert_eq!(text, "Ads started on Kick: 90s (started by you)");
        let body: Value = serde_json::from_str(
            &log.lock()
                .unwrap()
                .iter()
                .find(|r| r.path == "/ads/ad-break")
                .unwrap()
                .body,
        )
        .unwrap();
        assert_eq!(body["break_duration_seconds"], 90, "Kick's longest break");
        assert_eq!(body["id"].as_str().unwrap().len(), 36, "a UUID");
        assert!(shared.lock().ads_seen.contains("ab1"));

        // The status shows a running automatic break and an old one; only the running one counts.
        let api = shared.lock().signed.clone().unwrap().api;
        let breaks = api.ad_breaks().await.unwrap();
        let now = chrono::Utc::now();
        let running: Vec<_> = breaks
            .iter()
            .filter_map(|b| b.remaining(now).map(|r| (b, r)))
            .collect();
        assert_eq!(running.len(), 1);
        assert_eq!(running[0].0.id, "ab9");
        assert!(running[0].0.source.contains("auto"));
        assert!((45..=50).contains(&running[0].1), "{} s left", running[0].1);
    }

    #[tokio::test]
    async fn refuses_without_a_sign_in() {
        let (events, _rx) = EventSink::channel(Platform::Kick, 10);
        let send = Command::Send {
            text: "hi".into(),
            reply_to: None,
        };
        let err = perform(&Shared::default(), &events, send)
            .await
            .unwrap_err();
        assert!(err.contains("sign in to Kick"), "{err}");
    }

    #[tokio::test]
    async fn signs_in_through_the_fixed_redirect() {
        let (base, log) = kick_server().await;
        let pending = auth::start(&exchange()).await.unwrap();
        let url = reqwest::Url::parse(&pending.url).unwrap();
        let q: HashMap<String, String> = url.query_pairs().into_owned().collect();
        assert_eq!(q["redirect_uri"], auth::REDIRECT_URI);
        assert_eq!(q["code_challenge_method"], "S256");
        assert!(q["scope"].contains("chat:write") && q["scope"].contains("moderation:ban"));
        let state = q["state"].clone();
        // A browser that stops at the redirect to the website's page.
        let browser = tokio::spawn(async move {
            let client = reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .unwrap();
            let res = client
                .get(format!(
                    "{}?code=the-code&state={state}",
                    auth::REDIRECT_URI
                ))
                .send()
                .await
                .unwrap();
            format!(
                "{} Location: {}",
                res.status(),
                res.headers()["location"].to_str().unwrap()
            )
        });
        let token = auth::finish(
            &reqwest::Client::new(),
            &exchange(),
            &format!("{base}/oauth/token"),
            pending,
        )
        .await
        .unwrap();
        assert_eq!(
            (token.access.as_str(), token.refresh.as_str()),
            ("fresh", "r2")
        );
        assert!(
            browser
                .await
                .unwrap()
                .contains("Location: https://rill.mobasity.com/signed-in/kick")
        );
        let log = log.lock().unwrap().clone();
        let form = &log.iter().find(|r| r.path == "/oauth/token").unwrap().body;
        for part in [
            "grant_type=authorization_code",
            "code=the-code",
            "code_verifier=",
            "client_secret=shh",
        ] {
            assert!(form.contains(part), "{part} missing from {form}");
        }
    }
}
