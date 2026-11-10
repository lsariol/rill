//! Splits one chat firehose connection into many local ones, for parallel load tests.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use futures_util::{SinkExt, StreamExt};
use rill_core::backoff::Backoff;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::broadcast;
use tokio_tungstenite::tungstenite::Message;

const USAGE: &str = "Usage: firehose-relay [--upstream <wss url>] [--port <n>]

  --upstream <url>  The firehose (default wss://firehose.catquery.com/firehose?jsonBasic=true)
  --port <n>        Where tests connect: ws://127.0.0.1:<n> (default 9300)";

/// How many messages a slow test may fall behind before it skips ahead.
const BUFFER: usize = 20_000;

#[derive(Default)]
struct Counts {
    received: AtomicU64,
    clients: AtomicUsize,
    skipped: AtomicU64,
    drops: AtomicU64,
}

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() -> std::process::ExitCode {
    let mut upstream = "wss://firehose.catquery.com/firehose?jsonBasic=true".to_string();
    let mut port = 9300u16;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--upstream" => match args.next() {
                Some(u) => upstream = u,
                None => return usage("--upstream needs a URL."),
            },
            "--port" => match args.next().and_then(|p| p.parse().ok()) {
                Some(p) => port = p,
                None => return usage("--port needs a number."),
            },
            "-h" | "--help" => {
                println!("{USAGE}");
                return std::process::ExitCode::SUCCESS;
            }
            other => return usage(&format!("Unknown option {other}.")),
        }
    }
    // rustls needs a crypto provider chosen before the first TLS connection.
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let listener = match TcpListener::bind(("127.0.0.1", port)).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("✗ Can't listen on 127.0.0.1:{port} ({e}). Pick another with --port.");
            return std::process::ExitCode::FAILURE;
        }
    };
    let (tx, _) = broadcast::channel::<Arc<str>>(BUFFER);
    let counts = Arc::new(Counts::default());
    eprintln!("✓ Relaying {upstream}");
    eprintln!("  Tests connect to ws://127.0.0.1:{port}");
    tokio::spawn(read_upstream(upstream, tx.clone(), counts.clone()));
    tokio::spawn(report(counts.clone()));
    loop {
        match listener.accept().await {
            Ok((socket, _)) => {
                tokio::spawn(serve(socket, tx.subscribe(), counts.clone()));
            }
            Err(e) => eprintln!("! A test couldn't connect ({e})."),
        }
    }
}

fn usage(why: &str) -> std::process::ExitCode {
    eprintln!("✗ {why}\n\n{USAGE}");
    std::process::ExitCode::FAILURE
}

/// Keeps one connection to the firehose and hands every message to all tests.
async fn read_upstream(url: String, tx: broadcast::Sender<Arc<str>>, counts: Arc<Counts>) {
    let mut backoff = Backoff::chat();
    loop {
        match tokio_tungstenite::connect_async(url.as_str()).await {
            Ok((mut ws, _)) => {
                eprintln!("✓ Upstream connected");
                let opened = Instant::now();
                while let Some(msg) = ws.next().await {
                    match msg {
                        Ok(Message::Text(t)) => {
                            counts.received.fetch_add(1, Ordering::Relaxed);
                            // No tests connected is fine: the message just goes nowhere.
                            let _ = tx.send(Arc::from(t.as_str()));
                        }
                        Ok(_) => {}
                        Err(e) => {
                            eprintln!("! Upstream dropped ({e})");
                            break;
                        }
                    }
                }
                counts.drops.fetch_add(1, Ordering::Relaxed);
                if opened.elapsed() > Duration::from_secs(60) {
                    backoff.reset();
                }
            }
            Err(e) => eprintln!("! Upstream unreachable ({e})"),
        }
        let wait = backoff.next_delay();
        eprintln!("  Reconnecting in {} s", wait.as_secs());
        tokio::time::sleep(wait).await;
    }
}

/// Sends one test every message from now on; a test that falls far behind skips ahead.
async fn serve(socket: TcpStream, mut rx: broadcast::Receiver<Arc<str>>, counts: Arc<Counts>) {
    let Ok(mut ws) = tokio_tungstenite::accept_async(socket).await else {
        return;
    };
    counts.clients.fetch_add(1, Ordering::Relaxed);
    loop {
        tokio::select! {
            msg = rx.recv() => match msg {
                Ok(text) => {
                    if ws.send(Message::text(text.as_ref())).await.is_err() {
                        break;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    counts.skipped.fetch_add(n, Ordering::Relaxed);
                }
                Err(broadcast::error::RecvError::Closed) => break,
            },
            incoming = ws.next() => match incoming {
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                Some(Ok(_)) => {}
            },
        }
    }
    counts.clients.fetch_sub(1, Ordering::Relaxed);
}

/// One status line every 10 seconds.
async fn report(counts: Arc<Counts>) {
    let mut last = 0;
    loop {
        tokio::time::sleep(Duration::from_secs(10)).await;
        let received = counts.received.load(Ordering::Relaxed);
        let rate = (received - last) / 10;
        last = received;
        eprintln!(
            "  {rate} msg/s, {} tests connected, {} skipped by slow tests, {} upstream drops",
            counts.clients.load(Ordering::Relaxed),
            counts.skipped.load(Ordering::Relaxed),
            counts.drops.load(Ordering::Relaxed),
        );
    }
}
