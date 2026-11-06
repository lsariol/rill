//! Rill's Kick adapter. Reads a channel's chat from Kick's (unofficial) Pusher socket; read-only.

pub mod pusher;

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
use tokio_tungstenite::tungstenite::Message;

use crate::pusher::{Frame, SubBadges};

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
    id: u64,
    chatroom_id: u64,
    sub_badges: SubBadges,
}

/// What interrupted a wait or a connection.
enum Interrupt {
    Stop,
    /// Switch to this channel, or forget the channel when `None`.
    Switch(Option<String>),
}

struct Ctx {
    /// A random ID for this run, like an anonymous viewer's.
    client_id: String,
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
            Ok(channel) => match socket(&ctx.http, &ctx.client_id, channel.id).await {
                Ok(url) => connection(ctx, &channel, &url).await,
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
    let id = v["id"].as_u64().ok_or_else(|| {
        Lookup::Retry("Kick: the channel data has no ID; Kick may have changed it.".into())
    })?;
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
        id,
        chatroom_id,
        sub_badges,
    })
}

/// Asks Kick where this channel's chat socket is, as an anonymous viewer would.
async fn socket(
    http: &reqwest::Client,
    client_id: &str,
    channel_id: u64,
) -> Result<String, String> {
    let res = http
        .post(pusher::connection_url(channel_id))
        .header("Accept", "application/json")
        .header("x-app-platform", "web")
        .json(&pusher::connection_body(client_id))
        .send()
        .await
        .map_err(|e| {
            format!("Kick: can't reach web.kick.com ({e}). Check the internet connection.")
        })?;
    let status = res.status();
    if !status.is_success() {
        let why = format!("Kick refused the chat settings request ({status})");
        return Err(if status.is_client_error() {
            format!("{REFUSED} ({why})")
        } else {
            format!("{why}. Rill will keep trying.")
        });
    }
    let v: Value = res
        .json()
        .await
        .map_err(|e| format!("Kick: unexpected chat settings ({e})."))?;
    pusher::socket_from_settings(&v).map_err(|why| format!("{REFUSED} ({why})"))
}

/// One socket connection. `Ok(Some)` when interrupted, `Ok(None)` after a long healthy run, `Err` when lost early.
async fn connection(
    ctx: &mut Ctx,
    channel: &Channel,
    url: &str,
) -> Result<Option<Interrupt>, String> {
    let (mut ws, _) = timeout(
        Duration::from_secs(15),
        tokio_tungstenite::connect_async(url),
    )
    .await
    .map_err(|_| "Kick: the chat socket didn't answer in 15 s.".to_string())?
    .map_err(|e| format!("Kick: can't reach the chat socket ({e})."))?;
    let started = Instant::now();
    let mut activity = Duration::from_secs(120);
    let mut last = Instant::now();
    let mut pinged = false;
    let mut subscribed = false;
    let end: Result<Option<Interrupt>, String> = loop {
        let quiet_until = last + activity;
        tokio::select! {
            msg = ws.next() => {
                last = Instant::now();
                pinged = false;
                let text = match msg {
                    Some(Ok(Message::Text(t))) => t,
                    Some(Ok(Message::Ping(p))) => {
                        let _ = ws.send(Message::Pong(p)).await;
                        continue;
                    }
                    Some(Ok(Message::Close(_))) | None => break Err("Kick closed the chat socket.".into()),
                    Some(Ok(_)) => continue,
                    Some(Err(e)) => break Err(format!("Kick: chat socket error ({e}).")),
                };
                match pusher::parse(&text) {
                    Ok(Frame::Connected { activity_timeout }) => {
                        activity = Duration::from_secs(activity_timeout.clamp(10, 300));
                        if ws.send(Message::text(pusher::subscribe(channel.chatroom_id))).await.is_err() {
                            break Err("Kick: couldn't subscribe to the chatroom.".into());
                        }
                    }
                    Ok(Frame::Subscribed) => {
                        subscribed = true;
                        tracing::info!(channel = %channel.slug, "Kick chat connected");
                        ctx.events.status(ConnectionStatus::Connected, None, Capabilities::NONE).await;
                    }
                    Ok(Frame::Ping) => {
                        let _ = ws.send(Message::text(pusher::pong())).await;
                    }
                    Ok(Frame::Pong) => {}
                    Ok(Frame::Error(m)) => break Err(format!("{REFUSED} ({m})")),
                    Ok(Frame::Kick { event, data }) => {
                        if let Some(e) = pusher::to_event(&event, &data, &channel.sub_badges)
                            && !ctx.events.send(e).await
                        {
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
    match end {
        Err(why) if subscribed && started.elapsed() > Duration::from_secs(60) => {
            tracing::info!(err = %why, "Kick chat dropped after a healthy run");
            Ok(None)
        }
        other => other,
    }
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
