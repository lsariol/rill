//! The adapter contract, and the hub that runs each adapter on its own Tokio runtime.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, RwLock};

use tokio::runtime::Runtime;
use tokio::sync::mpsc;

use crate::{
    Account, Capabilities, ChatMessage, ConnectionStatus, Event, Level, Platform, Request,
    RequestId,
};

/// Events waiting for the UI before adapters have to wait.
const EVENT_QUEUE: usize = 8_192;
/// Requests waiting for one adapter before sends are refused.
const COMMAND_QUEUE: usize = 2_048;

/// A boxed, sendable future.
pub type BoxFuture = Pin<Box<dyn Future<Output = ()> + Send>>;

/// A platform connection: reads chat, carries out requests, and reconnects by itself.
pub trait Adapter: Send + 'static {
    fn platform(&self) -> Platform;

    /// Runs until `io.requests` closes. Must never return early on errors: report them and retry.
    fn run(self: Box<Self>, io: AdapterIo) -> BoxFuture;
}

/// An adapter's channels to the core.
pub struct AdapterIo {
    pub events: EventSink,
    pub requests: mpsc::Receiver<Request>,
}

/// Changes a message on its way to the core, such as swapping in third-party emotes.
pub trait Enrich: Send + Sync {
    fn enrich(&self, message: &mut ChatMessage);
}

/// Sends one adapter's events to the core.
#[derive(Clone)]
pub struct EventSink {
    platform: Platform,
    tx: mpsc::Sender<Event>,
    /// Shared by clones, so an adapter can set it once for all its tasks.
    enrich: Arc<RwLock<Option<Arc<dyn Enrich>>>>,
}

impl EventSink {
    fn new(platform: Platform, tx: mpsc::Sender<Event>) -> EventSink {
        EventSink {
            platform,
            tx,
            enrich: Arc::default(),
        }
    }

    /// Sets (or clears) what every later message passes through.
    pub fn set_enrich(&self, enrich: Option<Arc<dyn Enrich>>) {
        *self.enrich.write().unwrap_or_else(|e| e.into_inner()) = enrich;
    }

    pub fn platform(&self) -> Platform {
        self.platform
    }

    /// Sends an event; returns false once the core is gone.
    pub async fn send(&self, mut event: Event) -> bool {
        if let Event::Message(m) = &mut event {
            let enrich = self
                .enrich
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .clone();
            if let Some(enrich) = enrich {
                enrich.enrich(m);
            }
        }
        self.tx.send(event).await.is_ok()
    }

    pub async fn status(
        &self,
        status: ConnectionStatus,
        account: Option<Account>,
        capabilities: Capabilities,
    ) -> bool {
        self.send(Event::Status {
            platform: self.platform,
            status,
            account,
            capabilities,
        })
        .await
    }

    /// Says the connection needs the user's attention (`None` once it doesn't).
    pub async fn attention(&self, text: Option<String>) -> bool {
        self.send(Event::Attention {
            platform: self.platform,
            text,
        })
        .await
    }

    pub async fn message(&self, message: ChatMessage) -> bool {
        self.send(Event::Message(message)).await
    }

    pub async fn notice(&self, level: Level, text: impl Into<String>) -> bool {
        self.send(Event::Notice {
            platform: self.platform,
            level,
            text: text.into(),
        })
        .await
    }

    /// Posts "Ads started on <platform>: <length> (<why>)" now, and "Ads finished" when they end.
    pub async fn ads_started(&self, seconds: u32, why: &str) {
        let length = crate::command::format_duration(seconds.max(1));
        let platform = self.platform;
        self.notice(
            Level::Event,
            format!("Ads started on {platform}: {length} ({why})"),
        )
        .await;
        let sink = self.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_secs(u64::from(seconds))).await;
            sink.notice(Level::Event, format!("Ads finished on {platform}"))
                .await;
        });
    }

    pub async fn done(&self, request: RequestId, result: Result<Option<String>, String>) -> bool {
        self.send(Event::Done {
            platform: self.platform,
            request,
            result,
        })
        .await
    }

    /// A sink for tests and tools, with its receiving end.
    pub fn channel(platform: Platform, size: usize) -> (EventSink, mpsc::Receiver<Event>) {
        let (tx, rx) = mpsc::channel(size);
        (EventSink::new(platform, tx), rx)
    }
}

/// Runs adapters, each on its own Tokio runtime so stopping one ends everything it started, and
/// routes requests to them.
pub struct Hub {
    /// For work that isn't a platform's, such as the image HTTP client.
    runtime: Runtime,
    events: mpsc::Sender<Event>,
    running: HashMap<Platform, Running>,
}

/// A started adapter.
struct Running {
    requests: mpsc::Sender<Request>,
    runtime: Runtime,
}

fn runtime(name: &str, threads: usize) -> std::io::Result<Runtime> {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(threads)
        .thread_name(name)
        .enable_all()
        .build()
}

impl Hub {
    /// Starts the adapters. Returns the hub and the stream of every adapter's events.
    pub fn start(adapters: Vec<Box<dyn Adapter>>) -> std::io::Result<(Hub, mpsc::Receiver<Event>)> {
        let (events, rx) = mpsc::channel(EVENT_QUEUE);
        let mut hub = Hub {
            runtime: runtime("rill-net", 1)?,
            events,
            running: HashMap::new(),
        };
        for adapter in adapters {
            hub.add(adapter)?;
        }
        Ok((hub, rx))
    }

    /// Starts an adapter, replacing a running one for the same platform.
    pub fn add(&mut self, adapter: Box<dyn Adapter>) -> std::io::Result<()> {
        let platform = adapter.platform();
        self.stop(platform);
        let runtime = runtime(&format!("rill-{}", platform.name().to_lowercase()), 1)?;
        let (requests, req_rx) = mpsc::channel(COMMAND_QUEUE);
        let io = AdapterIo {
            events: EventSink::new(platform, self.events.clone()),
            requests: req_rx,
        };
        runtime.spawn(adapter.run(io));
        self.running.insert(platform, Running { requests, runtime });
        Ok(())
    }

    /// Stops a platform's adapter and every task it started, without waiting. Events it had
    /// already queued may still arrive.
    pub fn stop(&mut self, platform: Platform) -> bool {
        match self.running.remove(&platform) {
            Some(r) => {
                r.runtime.shutdown_background();
                true
            }
            None => false,
        }
    }

    /// Hands a request to a platform's adapter without waiting.
    pub fn send(&self, platform: Platform, request: Request) -> Result<(), String> {
        let tx = self
            .running
            .get(&platform)
            .map(|r| &r.requests)
            .ok_or_else(|| format!("{platform} isn't turned on in Rill."))?;
        tx.try_send(request).map_err(|e| match e {
            mpsc::error::TrySendError::Full(_) => {
                format!("{platform} has too many requests waiting; try again in a moment.")
            }
            mpsc::error::TrySendError::Closed(_) => {
                format!("{platform}'s connection stopped; restart Rill.")
            }
        })
    }

    /// Whether a platform has an adapter running.
    pub fn has(&self, platform: Platform) -> bool {
        self.running.contains_key(&platform)
    }

    /// The runtime, for other background work.
    pub fn handle(&self) -> tokio::runtime::Handle {
        self.runtime.handle().clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// Sends a notice every few milliseconds from a task of its own.
    struct Ticker(Platform);

    impl Adapter for Ticker {
        fn platform(&self) -> Platform {
            self.0
        }

        fn run(self: Box<Self>, mut io: AdapterIo) -> BoxFuture {
            Box::pin(async move {
                let events = io.events.clone();
                tokio::spawn(async move {
                    loop {
                        events.notice(Level::Info, "tick").await;
                        tokio::time::sleep(Duration::from_millis(5)).await;
                    }
                });
                while io.requests.recv().await.is_some() {}
            })
        }
    }

    fn drain(rx: &mut mpsc::Receiver<Event>) -> Vec<Platform> {
        std::iter::from_fn(|| rx.try_recv().ok())
            .map(|e| e.platform())
            .collect()
    }

    #[test]
    fn stopping_a_platform_ends_the_tasks_it_started() {
        let (mut hub, mut rx) = Hub::start(vec![
            Box::new(Ticker(Platform::Twitch)),
            Box::new(Ticker(Platform::Kick)),
        ])
        .unwrap();
        std::thread::sleep(Duration::from_millis(60));
        let seen = drain(&mut rx);
        assert!(seen.contains(&Platform::Twitch) && seen.contains(&Platform::Kick));

        assert!(hub.stop(Platform::Kick));
        assert!(!hub.has(Platform::Kick));
        std::thread::sleep(Duration::from_millis(30));
        drain(&mut rx);
        std::thread::sleep(Duration::from_millis(60));
        let seen = drain(&mut rx);
        assert!(seen.contains(&Platform::Twitch));
        assert!(!seen.contains(&Platform::Kick), "Kick's ticker stopped too");
        assert!(
            hub.send(
                Platform::Kick,
                Request {
                    id: RequestId(1),
                    command: crate::Command::SignIn,
                }
            )
            .is_err()
        );

        hub.add(Box::new(Ticker(Platform::Kick))).unwrap();
        std::thread::sleep(Duration::from_millis(60));
        assert!(drain(&mut rx).contains(&Platform::Kick), "back on");
    }
}
