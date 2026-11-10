//! The chat overlay: a page on 127.0.0.1 for OBS, fed chat lines over a WebSocket.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use rill_core::feed::{Entry, Item};
use rill_core::message::bigger_image;
use rill_core::{ChatMessage, Fragment, MessageKind, Platform};
use rill_os::settings::Overlay as Settings;
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{broadcast, watch};
use tokio_tungstenite::tungstenite::Message;

/// The page OBS loads.
const PAGE: &str = include_str!("page.html");

/// Lines a newly opened page starts with, so a source that reloads isn't empty.
const BACKLOG: usize = 50;

/// Well-known chat bots, left out with `hide_bots`.
const BOTS: &[&str] = &[
    "nightbot",
    "streamelements",
    "streamlabs",
    "moobot",
    "fossabot",
    "wizebot",
    "botrix",
    "botrixoficial",
    "kickbot",
    "sery_bot",
    "soundalerts",
    "streamlootsbot",
    "pokemoncommunitygame",
    "commanderroot",
    "blerp",
];

/// The platform icons the page shows.
const ICONS: &[(&str, &str)] = &[
    (
        "twitch",
        include_str!("../../rill-app/assets/icons/twitch.svg"),
    ),
    (
        "youtube",
        include_str!("../../rill-app/assets/icons/youtube.svg"),
    ),
    ("kick", include_str!("../../rill-app/assets/icons/kick.svg")),
    (
        "rumble",
        include_str!("../../rill-app/assets/icons/rumble.svg"),
    ),
];

/// What a newly connected page is sent first.
struct Shared {
    config: String,
    backlog: VecDeque<(u64, String)>,
}

/// A running overlay. Dropping it stops the server.
pub struct Overlay {
    port: u16,
    tx: broadcast::Sender<Arc<str>>,
    shared: Arc<Mutex<Shared>>,
    stop: watch::Sender<bool>,
}

impl Overlay {
    /// Starts serving on 127.0.0.1:`port` on a thread of its own. `config` is from [`config`].
    pub fn start(port: u16, config: String) -> Result<Overlay, String> {
        let listener = std::net::TcpListener::bind(("127.0.0.1", port)).map_err(|e| {
            format!(
                "Overlay: port {port} is taken ({e}). Close the program using it (or another \
                 Rill), or pick another port in Settings → Overlay."
            )
        })?;
        listener
            .set_nonblocking(true)
            .map_err(|e| format!("Overlay: can't set up port {port} ({e})."))?;
        let (tx, _) = broadcast::channel(1024);
        let shared = Arc::new(Mutex::new(Shared {
            config,
            backlog: VecDeque::new(),
        }));
        let (stop, stopped) = watch::channel(false);
        let (task_tx, task_shared) = (tx.clone(), shared.clone());
        std::thread::Builder::new()
            .name("rill-overlay".into())
            .spawn(move || {
                let rt = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(rt) => rt,
                    Err(e) => {
                        tracing::error!(err = %e, "overlay runtime failed");
                        return;
                    }
                };
                rt.block_on(serve(listener, port, task_tx, task_shared, stopped));
            })
            .map_err(|e| format!("Overlay: can't start its thread ({e})."))?;
        tracing::info!(port, "overlay started");
        Ok(Overlay {
            port,
            tx,
            shared,
            stop,
        })
    }

    /// The address to paste into OBS.
    pub fn url(&self) -> String {
        format!("http://127.0.0.1:{}/", self.port)
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    /// Sends new settings (from [`config`]) to every open page.
    pub fn set_config(&self, config: String) {
        lock(&self.shared).config = config.clone();
        let _ = self.tx.send(config.into());
    }

    /// Shows a line (from [`line`]), with the feed entry's sequence number as its ID.
    pub fn line(&self, id: u64, json: String) {
        {
            let mut s = lock(&self.shared);
            s.backlog.push_back((id, json.clone()));
            while s.backlog.len() > BACKLOG {
                s.backlog.pop_front();
            }
        }
        let _ = self.tx.send(json.into());
    }

    /// Takes lines away, such as deleted messages.
    pub fn delete(&self, ids: &[u64]) {
        lock(&self.shared)
            .backlog
            .retain(|(id, _)| !ids.contains(id));
        let _ = self
            .tx
            .send(json!({ "t": "delete", "ids": ids }).to_string().into());
    }

    /// Empties the overlay.
    pub fn clear(&self) {
        lock(&self.shared).backlog.clear();
        let _ = self.tx.send(json!({ "t": "clear" }).to_string().into());
    }
}

impl Drop for Overlay {
    fn drop(&mut self) {
        let _ = self.stop.send(true);
        tracing::info!(port = self.port, "overlay stopped");
    }
}

fn lock(shared: &Mutex<Shared>) -> MutexGuard<'_, Shared> {
    shared.lock().unwrap_or_else(|e| e.into_inner())
}

/// The page's settings: the look, and each enabled highlight's `#rrggbb` color.
pub fn config(settings: &Settings, highlights: &[(String, String)]) -> String {
    let hl: serde_json::Map<String, Value> = highlights
        .iter()
        .map(|(k, c)| (k.clone(), Value::String(c.clone())))
        .collect();
    json!({
        "t": "config",
        "font": settings.font_size.clamp(10, 72),
        "fade": settings.fade_seconds,
        "badges": settings.badges,
        "icons": settings.platform_icons,
        "background": settings.background,
        "hl": hl,
    })
    .to_string()
}

/// The overlay's copy of a feed line, or `None` when the overlay leaves it out.
pub fn line(
    entry: &Entry,
    settings: &Settings,
    own: impl Fn(Platform) -> Option<String>,
) -> Option<String> {
    let mut out = match &entry.item {
        Item::Chat(m) => chat_line(m, entry, settings)?,
        Item::Sent(s) => {
            let platforms: Vec<Platform> = s.deliveries.iter().map(|d| d.platform).collect();
            let name = platforms.iter().find_map(|&p| own(p)).unwrap_or_default();
            // Badges and color come from the platform's echo, so they match received lines.
            let author = s.author.as_ref();
            json!({
                "p": platforms.iter().map(|p| key(*p)).collect::<Vec<_>>(),
                "name": name,
                "color": author.and_then(|a| a.color).map(|c| format!("#{c:06x}")),
                "badges": author.map(|a| badges(&a.badges)).unwrap_or_default(),
                "parts": parts(&s.fragments),
            })
        }
        Item::Notice(_) => return None,
    };
    out["t"] = "line".into();
    out["id"] = entry.seq.into();
    Some(out.to_string())
}

fn chat_line(m: &ChatMessage, entry: &Entry, settings: &Settings) -> Option<Value> {
    let event = matches!(m.kind, MessageKind::Event(_));
    if !event {
        if settings.hide_bots && BOTS.contains(&m.author.login.to_lowercase().as_str()) {
            return None;
        }
        let starts_with_bang = match m.fragments.first() {
            Some(Fragment::Text(t)) => t.trim_start().starts_with('!'),
            _ => false,
        };
        if settings.hide_commands && starts_with_bang {
            return None;
        }
    }
    let mut kinds = Vec::new();
    if entry.highlighted {
        kinds.push("mention");
    }
    if entry.first {
        kinds.push("first");
    }
    if let MessageKind::Event(k) = m.kind {
        kinds.push(k.key());
    }
    let badges = badges(&m.author.badges);
    Some(json!({
        "p": [key(m.platform)],
        "name": m.author.display_name,
        "color": m.author.color.map(|c| format!("#{c:06x}")),
        "badges": badges,
        "parts": parts(&m.fragments),
        "a": m.kind == MessageKind::Action,
        "ev": event,
        "kinds": kinds,
    }))
}

/// Badges as the page draws them: an image, or a lettered square for one without (Kick's roles).
fn badges(badges: &[rill_core::Badge]) -> Vec<Value> {
    badges
        .iter()
        .take(4)
        .filter_map(|b| match b.image_url.as_deref() {
            Some(url) => Some(json!({ "u": bigger_image(url), "t": b.title })),
            None => {
                let (letter, color) = match b.id.split('/').next().unwrap_or("") {
                    "broadcaster" => ("B", "#e91916"),
                    "moderator" => ("M", "#00ad03"),
                    "vip" => ("V", "#e005b9"),
                    "subscriber" | "founder" => ("S", "#8205b4"),
                    _ => return None,
                };
                Some(json!({ "l": letter, "c": color, "t": b.title }))
            }
        })
        .collect()
}

/// A message's pieces: `{x}` for text (`m` marks a mention), `{e, n, h, o}` for an emote.
fn parts(fragments: &[Fragment]) -> Vec<Value> {
    fragments
        .iter()
        .map(|f| match f {
            Fragment::Text(t) | Fragment::Link(t) => json!({ "x": t }),
            Fragment::Mention(n) => json!({ "x": format!("@{n}"), "m": true }),
            Fragment::Emote {
                name,
                url: Some(url),
                size,
                overlay,
                ..
            } => json!({
                "e": bigger_image(url),
                "n": name,
                "h": size.map_or(28, |(_, h)| h),
                "o": overlay,
            }),
            Fragment::Emote { name, .. } => json!({ "x": name }),
        })
        .collect()
}

/// A platform as the page and its icon names it.
fn key(p: Platform) -> String {
    p.name().to_lowercase()
}

/// Accepts connections until told to stop.
async fn serve(
    listener: std::net::TcpListener,
    port: u16,
    tx: broadcast::Sender<Arc<str>>,
    shared: Arc<Mutex<Shared>>,
    mut stop: watch::Receiver<bool>,
) {
    let listener = match TcpListener::from_std(listener) {
        Ok(l) => l,
        Err(e) => {
            tracing::error!(err = %e, "overlay listener failed");
            return;
        }
    };
    loop {
        tokio::select! {
            _ = stop.changed() => return,
            accepted = listener.accept() => {
                if let Ok((sock, _)) = accepted {
                    tokio::spawn(handle(sock, port, tx.subscribe(), shared.clone(), stop.clone()));
                }
            }
        }
    }
}

/// One connection: the page, an icon, or the page's WebSocket.
async fn handle(
    mut sock: TcpStream,
    port: u16,
    rx: broadcast::Receiver<Arc<str>>,
    shared: Arc<Mutex<Shared>>,
    stop: watch::Receiver<bool>,
) {
    let Some(head) = peek_head(&sock).await else {
        return;
    };
    let request = Request::parse(&head);
    // Only addresses on this computer: a web page elsewhere can't reach it by another name.
    let host_ok = [format!("127.0.0.1:{port}"), format!("localhost:{port}")]
        .iter()
        .any(|h| request.host.eq_ignore_ascii_case(h));
    // Only the overlay's own page may listen: another site open in a browser can't.
    let origin_ok = request.origin.as_deref().is_none_or(|o| {
        [
            format!("http://127.0.0.1:{port}"),
            format!("http://localhost:{port}"),
        ]
        .iter()
        .any(|ok| o.eq_ignore_ascii_case(ok))
    });
    if request.path == "/ws" && request.upgrade && host_ok && origin_ok {
        websocket(sock, rx, shared, stop).await;
        return;
    }
    // Read the request off the socket first; closing with it unread resets the connection.
    let mut consumed = vec![0u8; head.len()];
    if sock.read_exact(&mut consumed).await.is_err() {
        return;
    }
    let (status, kind, body) = if !host_ok {
        (
            "403 Forbidden",
            "text/plain",
            "Open the overlay at 127.0.0.1.",
        )
    } else if request.path == "/" {
        ("200 OK", "text/html; charset=utf-8", PAGE)
    } else if let Some((_, svg)) = request
        .path
        .strip_prefix("/icons/")
        .and_then(|f| f.strip_suffix(".svg"))
        .and_then(|name| ICONS.iter().find(|(n, _)| *n == name))
    {
        ("200 OK", "image/svg+xml", *svg)
    } else {
        ("404 Not Found", "text/plain", "Not found.")
    };
    let reply = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = sock.write_all(reply.as_bytes()).await;
    let _ = sock.shutdown().await;
}

/// Reads the request's head without taking it off the socket; `None` unless a small HTTP request.
async fn peek_head(sock: &TcpStream) -> Option<String> {
    let mut buf = [0u8; 8192];
    for _ in 0..200 {
        let n = sock.peek(&mut buf).await.ok()?;
        if n == 0 {
            return None;
        }
        let text = String::from_utf8_lossy(&buf[..n]);
        if let Some(end) = text.find("\r\n\r\n") {
            return Some(text[..end + 4].to_string());
        }
        if n == buf.len() {
            return None;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    None
}

/// What the server needs from a request.
struct Request {
    path: String,
    host: String,
    /// The page that opened the connection, when a browser says.
    origin: Option<String>,
    upgrade: bool,
}

impl Request {
    fn parse(head: &str) -> Request {
        let mut lines = head.lines();
        let target = lines
            .next()
            .and_then(|l| l.split_whitespace().nth(1))
            .unwrap_or("");
        let path = target.split('?').next().unwrap_or("").to_string();
        let mut host = String::new();
        let mut origin = None;
        let mut upgrade = false;
        for line in lines {
            if let Some((name, value)) = line.split_once(':') {
                let value = value.trim();
                if name.eq_ignore_ascii_case("host") {
                    host = value.to_string();
                } else if name.eq_ignore_ascii_case("origin") {
                    origin = Some(value.to_string());
                } else if name.eq_ignore_ascii_case("upgrade") {
                    upgrade = value.eq_ignore_ascii_case("websocket");
                }
            }
        }
        Request {
            path,
            host,
            origin,
            upgrade,
        }
    }
}

/// Sends the settings and recent lines, then every new message, until the page goes away.
async fn websocket(
    sock: TcpStream,
    mut rx: broadcast::Receiver<Arc<str>>,
    shared: Arc<Mutex<Shared>>,
    mut stop: watch::Receiver<bool>,
) {
    let Ok(mut ws) = tokio_tungstenite::accept_async(sock).await else {
        return;
    };
    let first: Vec<String> = {
        let s = lock(&shared);
        std::iter::once(s.config.clone())
            .chain(s.backlog.iter().map(|(_, j)| j.clone()))
            .collect()
    };
    for text in first {
        if ws.send(Message::text(text)).await.is_err() {
            return;
        }
    }
    loop {
        tokio::select! {
            _ = stop.changed() => break,
            next = rx.recv() => match next {
                Ok(text) => {
                    if ws.send(Message::text(text.as_ref())).await.is_err() {
                        break;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => {}
                Err(broadcast::error::RecvError::Closed) => break,
            },
            incoming = ws.next() => match incoming {
                Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break,
                _ => {}
            },
        }
    }
    let _ = ws.close(None).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use rill_core::message::{Author, Badge};

    fn chat(login: &str, text: &str, kind: MessageKind) -> Entry {
        Entry {
            seq: 7,
            item: Item::Chat(ChatMessage {
                platform: Platform::Twitch,
                id: "m1".into(),
                author: Author {
                    id: "1".into(),
                    login: login.into(),
                    display_name: login.into(),
                    color: Some(0xff0000),
                    badges: vec![Badge {
                        id: "moderator/1".into(),
                        title: "Moderator".into(),
                        image_url: Some("https://static-cdn.jtvnw.net/badges/v1/abc/1".into()),
                    }],
                    ..Default::default()
                },
                fragments: vec![
                    Fragment::Text(text.into()),
                    Fragment::emote(
                        "Kappa",
                        Some(
                            "https://static-cdn.jtvnw.net/emoticons/v2/25/default/dark/1.0".into(),
                        ),
                    ),
                ],
                kind,
                sent_at_ms: 0,
                reply_to: None,
            }),
            received_ms: 0,
            highlighted: true,
            first: false,
            deleted: false,
        }
    }

    #[test]
    fn a_chat_line_carries_what_the_page_draws() {
        let line = line(
            &chat("Foo", "hi ", MessageKind::Chat),
            &Settings::default(),
            |_| None,
        )
        .unwrap();
        let v: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(v["t"], "line");
        assert_eq!(v["id"], 7);
        assert_eq!(v["p"][0], "twitch");
        assert_eq!(v["color"], "#ff0000");
        assert_eq!(v["kinds"][0], "mention");
        assert_eq!(
            v["badges"][0]["u"],
            "https://static-cdn.jtvnw.net/badges/v1/abc/3"
        );
        assert_eq!(v["parts"][0]["x"], "hi ");
        assert_eq!(
            v["parts"][1]["e"],
            "https://static-cdn.jtvnw.net/emoticons/v2/25/default/dark/3.0"
        );
    }

    #[test]
    fn a_badge_without_an_image_is_a_lettered_square_and_own_lines_have_badges() {
        let mut entry = chat("Foo", "hi", MessageKind::Chat);
        let Item::Chat(m) = &mut entry.item else {
            unreachable!()
        };
        let author = Author {
            badges: vec![Badge {
                id: "broadcaster/1".into(),
                title: "Broadcaster".into(),
                image_url: None,
            }],
            ..m.author.clone()
        };
        m.author = author.clone();
        let v: Value =
            serde_json::from_str(&line(&entry, &Settings::default(), |_| None).unwrap()).unwrap();
        assert_eq!(v["badges"][0]["l"], "B");

        // A message sent from Rill takes its badges from the platform's echo.
        entry.item = Item::Sent(rill_core::feed::SentMessage {
            text: "hi".into(),
            fragments: vec![Fragment::Text("hi".into())],
            deliveries: vec![rill_core::feed::Delivery {
                platform: Platform::Kick,
                state: rill_core::feed::DeliveryState::Sent,
            }],
            author: Some(author),
        });
        let v: Value = serde_json::from_str(
            &line(&entry, &Settings::default(), |_| Some("Mobasity".into())).unwrap(),
        )
        .unwrap();
        assert_eq!(v["name"], "Mobasity");
        assert_eq!(v["badges"][0]["l"], "B");
    }

    #[test]
    fn commands_and_bots_are_left_out_when_asked() {
        let mut s = Settings::default();
        assert!(
            line(&chat("nightbot", "!uptime", MessageKind::Chat), &s, |_| {
                None
            })
            .is_some()
        );
        s.hide_commands = true;
        assert!(line(&chat("viewer", "!uptime", MessageKind::Chat), &s, |_| None).is_none());
        s.hide_bots = true;
        assert!(line(&chat("Nightbot", "hello", MessageKind::Chat), &s, |_| None).is_none());
        // Events always show.
        let sub = MessageKind::Event(rill_core::EventKind::Subscription);
        assert!(line(&chat("viewer", "!subscribed", sub), &s, |_| None).is_some());
    }

    #[test]
    fn requests_are_read_for_path_host_and_upgrade() {
        let r = Request::parse(
            "GET /ws?x=1 HTTP/1.1\r\nHost: 127.0.0.1:47832\r\nUpgrade: websocket\r\n\r\n",
        );
        assert_eq!(r.path, "/ws");
        assert_eq!(r.host, "127.0.0.1:47832");
        assert!(r.upgrade);
    }

    #[tokio::test]
    async fn serves_the_page_and_pushes_lines() {
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let overlay = Overlay::start(port, config(&Settings::default(), &[])).unwrap();
        overlay.line(1, r#"{"t":"line","id":1}"#.into());

        let mut sock = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        sock.write_all(format!("GET / HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n\r\n").as_bytes())
            .await
            .unwrap();
        let mut page = String::new();
        sock.read_to_string(&mut page).await.unwrap();
        assert!(page.starts_with("HTTP/1.1 200 OK"), "{page}");
        assert!(page.contains("<title>Rill chat overlay</title>"));

        let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{port}/ws"))
            .await
            .unwrap();
        let config = ws.next().await.unwrap().unwrap().into_text().unwrap();
        assert!(config.contains(r#""t":"config""#));
        let backlog = ws.next().await.unwrap().unwrap().into_text().unwrap();
        assert_eq!(backlog.as_str(), r#"{"t":"line","id":1}"#);
        overlay.delete(&[1]);
        let deleted = ws.next().await.unwrap().unwrap().into_text().unwrap();
        assert!(deleted.contains(r#""ids":[1]"#));

        // Another site's page can't listen in.
        let mut sock = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        sock.write_all(
            format!(
                "GET /ws HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nOrigin: https://evil.example\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n\r\n"
            )
            .as_bytes(),
        )
        .await
        .unwrap();
        let mut reply = String::new();
        sock.read_to_string(&mut reply).await.unwrap();
        assert!(!reply.starts_with("HTTP/1.1 101"), "{reply}");

        // Another name for the address is refused.
        let mut sock = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        sock.write_all(b"GET / HTTP/1.1\r\nHost: evil.example:80\r\n\r\n")
            .await
            .unwrap();
        let mut reply = String::new();
        sock.read_to_string(&mut reply).await.unwrap();
        assert!(reply.starts_with("HTTP/1.1 403"), "{reply}");
    }
}
