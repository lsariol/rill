//! The adapter contract, and the hub that runs adapters on a Tokio runtime.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;

use tokio::runtime::Runtime;
use tokio::sync::mpsc;

use crate::{
    Account, Capabilities, ChatMessage, ConnectionStatus, Event, Level, Platform, Request,
    RequestId,
};

/// Events waiting for the UI before adapters have to wait.
const EVENT_QUEUE: usize = 8_192;
/// Requests waiting for one adapter before sends are refused.
const COMMAND_QUEUE: usize = 256;

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

/// Sends one adapter's events to the core.
#[derive(Clone)]
pub struct EventSink {
    platform: Platform,
    tx: mpsc::Sender<Event>,
}

impl EventSink {
    pub fn platform(&self) -> Platform {
        self.platform
    }

    /// Sends an event; returns false once the core is gone.
    pub async fn send(&self, event: Event) -> bool {
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
        (EventSink { platform, tx }, rx)
    }
}

/// Runs adapters on their own Tokio runtime and routes requests to them.
pub struct Hub {
    runtime: Runtime,
    requests: HashMap<Platform, mpsc::Sender<Request>>,
}

impl Hub {
    /// Starts the adapters. Returns the hub and the stream of every adapter's events.
    pub fn start(adapters: Vec<Box<dyn Adapter>>) -> std::io::Result<(Hub, mpsc::Receiver<Event>)> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("rill-net")
            .enable_all()
            .build()?;
        let (tx, rx) = mpsc::channel(EVENT_QUEUE);
        let mut requests = HashMap::new();
        for adapter in adapters {
            let platform = adapter.platform();
            let (req_tx, req_rx) = mpsc::channel(COMMAND_QUEUE);
            requests.insert(platform, req_tx);
            let io = AdapterIo {
                events: EventSink {
                    platform,
                    tx: tx.clone(),
                },
                requests: req_rx,
            };
            runtime.spawn(adapter.run(io));
        }
        Ok((Hub { runtime, requests }, rx))
    }

    /// Hands a request to a platform's adapter without waiting.
    pub fn send(&self, platform: Platform, request: Request) -> Result<(), String> {
        let tx = self
            .requests
            .get(&platform)
            .ok_or_else(|| format!("{platform} isn't set up in Rill."))?;
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
        self.requests.contains_key(&platform)
    }

    /// The runtime, for other background work.
    pub fn handle(&self) -> tokio::runtime::Handle {
        self.runtime.handle().clone()
    }
}
