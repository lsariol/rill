//! Rill's Kick adapter. Reads a channel's chat from Kick's unofficial socket (Centrifugo or Pusher,
//! whichever Kick offers); read-only.

pub mod centrifugo;
pub mod events;
pub mod pusher;
pub mod realtime;

use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use rill_core::backoff::Backoff;
use rill_core::{
    Adapter, AdapterIo, Capabilities, Command, ConnectionStatus, EventSink, Level, Platform,
    Request, adapter::BoxFuture,
};
use serde_json::Value;
use tokio::sync::mpsc;
use tokio::time::{Instant, sleep_until, timeout};
use tokio_tungstenite::tungstenite::{Error as WsError, Message};

use crate::centrifugo::Reply;
use crate::events::SubBadges;
use crate::pusher::Frame;
use crate::realtime::Transport;

/// Prefix of errors where Kick's socket refused Rill outright; retrying won't help.
const REFUSED: &str = "Kick: Kick's chat refused Rill; Kick may have changed it. Reading Kick chat needs a Rill update.";
const READ_ONLY: &str =
    "Kick is read-only in this version of Rill: send and moderate from kick.com for now.";

/// The Kick connection.
pub struct KickAdapter {
    channel: Option<String>,
}

impl KickAdapter {
    pub fn new(settings: &rill_os::settings::KickSettings) -> KickAdapter {
        KickAdapter {
            channel: settings.channel.as_deref().and_then(clean_channel),
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
    sub_badges: SubBadges,
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
    let mut ctx = Ctx {
        client_id: random_client_id(),
        token_url: realtime::TOKEN_URL.into(),
        events,
        requests,
        http,
    };
    let mut channel = adapter.channel;
    loop {
        let Some(slug) = channel.clone() else {
            ctx.events
                .status(ConnectionStatus::Disconnected, None, Capabilities::NONE)
                .await;
            match wait(&mut ctx, None).await {
                Some(Interrupt::Switch(next)) => channel = next,
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

fn remember(channel: Option<&str>) {
    let result = rill_os::settings::update(|s| s.kick.channel = channel.map(str::to_string));
    if let Err(e) = result {
        tracing::error!(err = %e, "Kick channel not saved");
    }
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
            ctx.events.done(req.id, Ok(None)).await;
            Some(Interrupt::Switch(None))
        }
        Command::SignIn => {
            ctx.events
                .done(
                    req.id,
                    Err("Kick: enter a channel name in Accounts to read its chat.".into()),
                )
                .await;
            None
        }
        _ => {
            ctx.events.done(req.id, Err(READ_ONLY.into())).await;
            None
        }
    }
}

/// Reads one channel until told to stop or switch, reconnecting with backoff.
async fn follow(ctx: &mut Ctx, slug: &str) -> Interrupt {
    let mut backoff = Backoff::chat();
    ctx.events
        .status(ConnectionStatus::Connecting, None, Capabilities::NONE)
        .await;
    loop {
        let result = match lookup(&ctx.http, slug).await {
            Ok(channel) => match socket(&ctx.http, &ctx.client_id).await {
                Ok(transport) => {
                    tracing::debug!(transport = transport.name(), "Kick chat socket chosen");
                    connection(ctx, &channel, &transport).await
                }
                Err(why) => Err(why),
            },
            Err(Lookup::Missing) => {
                let why = format!(
                    "Kick: there's no channel named \"{slug}\". Check the name in Accounts."
                );
                ctx.events
                    .status(ConnectionStatus::Failed(why), None, Capabilities::NONE)
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
                ctx.events
                    .status(ConnectionStatus::Failed(why), None, Capabilities::NONE)
                    .await;
                return wait(ctx, None).await.unwrap_or(Interrupt::Stop);
            }
            Err(why) => {
                tracing::warn!(err = %why, channel = slug, "Kick chat connection lost");
                let attempt = backoff.attempt() + 1;
                ctx.events
                    .status(
                        ConnectionStatus::Reconnecting { attempt },
                        None,
                        Capabilities::NONE,
                    )
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
    Ok(Channel {
        slug: slug.to_string(),
        chatroom_id,
        sub_badges,
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
    match events::to_event(event, data, &channel.sub_badges) {
        Some(e) => ctx.events.send(e).await,
        None => true,
    }
}

async fn connected(ctx: &Ctx, channel: &Channel, transport: &str) {
    tracing::info!(channel = %channel.slug, transport, "Kick chat connected");
    ctx.events
        .status(ConnectionStatus::Connected, None, Capabilities::NONE)
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
                    }
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
                        }
                        Ok(Reply::Subscribed) => {
                            *subscribed = true;
                            connected(ctx, channel, "centrifugo").await;
                        }
                        Ok(Reply::Refreshed { ttl_secs }) => refresh_at = ttl_secs.map(refresh_time),
                        Ok(Reply::Error { code, message }) => {
                            break 'session Err(format!("Kick: the chat socket refused a request ({code}: {message})."));
                        }
                        Ok(Reply::Disconnect { code, reason }) => {
                            break 'session Err(format!("Kick closed the chat socket (code {code}: {reason})."));
                        }
                        Ok(Reply::Unsubscribed { code }) => {
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
        let ctx = Ctx {
            client_id: "client-1".into(),
            token_url: token_url.into(),
            events,
            requests,
            http: reqwest::Client::new(),
        };
        (ctx, rx, tx)
    }

    fn channel() -> Channel {
        Channel {
            slug: "someone".into(),
            chatroom_id: 42,
            sub_badges: Vec::new(),
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

    async fn recv_json(ws: &mut Server) -> Value {
        loop {
            match ws.next().await.unwrap().unwrap() {
                Message::Text(t) => return serde_json::from_str(&t).unwrap(),
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
            let _ = ws.next().await;
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
