//! 7TV's event socket: live additions, removals and renames in a channel's emote set.

use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;

use crate::Emotes;
use crate::seven_tv::{self, ActiveJson, Config};

/// How a connection ended.
#[derive(Debug)]
pub(crate) enum End {
    /// The channel's 7TV account changed (perhaps a new active set): load it again now.
    Changed,
    /// Reconnect after the usual backoff.
    Lost(String),
    /// 7TV is down for maintenance: wait this long first.
    Wait(Duration, String),
    /// 7TV says Rill did something wrong; reconnecting would repeat it.
    Stop(String),
}

/// Close codes that mean a bug on Rill's side, so reconnecting won't help.
const CLIENT_BUGS: [u16; 6] = [4001, 4002, 4003, 4004, 4009, 4010];
const MAINTENANCE: u16 = 4007;

fn closed(code: u16, reason: &str) -> End {
    let why = format!("7TV closed the event socket ({code}: {reason}).");
    if CLIENT_BUGS.contains(&code) {
        End::Stop(why)
    } else if code == MAINTENANCE {
        End::Wait(Duration::from_secs(300), why)
    } else {
        End::Lost(why)
    }
}

fn subscribe(kind: &str, object_id: &str) -> Message {
    Message::text(
        json!({"op": 35, "d": {"type": kind, "condition": {"object_id": object_id}}}).to_string(),
    )
}

/// Follows `set_id` (and the 7TV user, for set switches) until the connection ends.
pub(crate) async fn follow(
    http: &reqwest::Client,
    cfg: &Config,
    set_id: &str,
    user_id: Option<&str>,
    emotes: &Emotes,
) -> End {
    let (mut ws, _) = match timeout(
        Duration::from_secs(15),
        tokio_tungstenite::connect_async(cfg.events.as_str()),
    )
    .await
    {
        Ok(Ok(c)) => c,
        Ok(Err(e)) => return End::Lost(format!("7TV: can't reach the event socket ({e}).")),
        Err(_) => return End::Lost("7TV: the event socket didn't answer in 15 s.".into()),
    };
    // Hello carries the heartbeat interval; missing three heartbeats means the socket is dead.
    let hello = timeout(Duration::from_secs(15), async {
        while let Some(Ok(msg)) = ws.next().await {
            if let Message::Text(t) = msg
                && let Ok(v) = serde_json::from_str::<Value>(&t)
                && v["op"] == 1
            {
                return v["d"]["heartbeat_interval"].as_u64();
            }
        }
        None
    })
    .await;
    let Ok(Some(interval_ms)) = hello else {
        return End::Lost("7TV: the event socket sent no hello.".into());
    };
    let quiet =
        Duration::from_millis(interval_ms.clamp(1_000, 120_000) * 3) + Duration::from_secs(5);
    if ws
        .send(subscribe("emote_set.update", set_id))
        .await
        .is_err()
    {
        return End::Lost("7TV: the event socket closed while subscribing.".into());
    }
    if let Some(user_id) = user_id
        && ws.send(subscribe("user.update", user_id)).await.is_err()
    {
        return End::Lost("7TV: the event socket closed while subscribing.".into());
    }
    tracing::debug!(set_id, "7TV event socket following the emote set");
    loop {
        let msg = match timeout(quiet, ws.next()).await {
            Err(_) => return End::Lost("7TV: the event socket went quiet.".into()),
            Ok(None) => return End::Lost("7TV: the event socket closed.".into()),
            Ok(Some(Err(e))) => return End::Lost(format!("7TV: the event socket broke ({e}).")),
            Ok(Some(Ok(msg))) => msg,
        };
        let text = match msg {
            Message::Text(t) => t,
            Message::Close(frame) => {
                let (code, reason) = frame
                    .map(|f| (u16::from(f.code), f.reason.to_string()))
                    .unwrap_or((u16::from(CloseCode::Abnormal), String::new()));
                return closed(code, &reason);
            }
            _ => continue,
        };
        let Ok(v) = serde_json::from_str::<Value>(&text) else {
            tracing::debug!(len = text.len(), "7TV event skipped: not JSON");
            continue;
        };
        match v["op"].as_u64() {
            Some(0) => match v["d"]["type"].as_str() {
                Some("emote_set.update") => apply(http, cfg, &v["d"]["body"], emotes).await,
                Some("user.update") => return End::Changed,
                _ => {}
            },
            Some(4) => return End::Lost("7TV asked Rill to reconnect.".into()),
            Some(6) => tracing::warn!(msg = %v["d"]["message"], "7TV event socket error"),
            Some(7) => {
                let code = v["d"]["code"].as_u64().unwrap_or(4000) as u16;
                return closed(code, v["d"]["message"].as_str().unwrap_or(""));
            }
            _ => {}
        }
    }
}

#[derive(Deserialize, Default)]
struct ChangeMap {
    #[serde(default)]
    pushed: Vec<ChangeField>,
    #[serde(default)]
    pulled: Vec<ChangeField>,
    #[serde(default)]
    updated: Vec<ChangeField>,
}

#[derive(Deserialize)]
struct ChangeField {
    key: String,
    #[serde(default)]
    value: Value,
    #[serde(default)]
    old_value: Value,
}

fn active(v: &Value) -> Option<ActiveJson> {
    serde_json::from_value(v.clone()).ok()
}

/// Applies one emote set change: removals, additions, then renames.
async fn apply(http: &reqwest::Client, cfg: &Config, body: &Value, emotes: &Emotes) {
    let Ok(change) = serde_json::from_value::<ChangeMap>(body.clone()) else {
        tracing::debug!("7TV emote set change skipped: unexpected shape");
        return;
    };
    for f in change.pulled.iter().filter(|f| f.key == "emotes") {
        if let Some(old) = active(&f.old_value) {
            emotes.change_channel(|set| {
                if set.get(&old.name).is_some_and(|e| e.id == old.id) {
                    set.remove(&old.name);
                }
            });
        }
    }
    for f in change.pushed.iter().filter(|f| f.key == "emotes") {
        let Some(new) = active(&f.value) else {
            continue;
        };
        let emote = match new.emote() {
            Some(e) => Some(e),
            None => seven_tv::emote(http, cfg, &new).await.unwrap_or_else(|e| {
                tracing::warn!(err = %e, "7TV emote not loaded");
                None
            }),
        };
        if let Some(emote) = emote {
            emotes.change_channel(|set| {
                set.insert(emote.name.clone(), emote);
            });
        }
    }
    for f in change.updated.iter().filter(|f| f.key == "emotes") {
        let (Some(old), Some(new)) = (active(&f.old_value), active(&f.value)) else {
            continue;
        };
        emotes.change_channel(|set| {
            let previous = set.remove(&old.name);
            let renamed = new.emote().or_else(|| {
                previous.map(|mut e| {
                    e.name = new.name.clone();
                    e
                })
            });
            if let Some(e) = renamed {
                set.insert(e.name.clone(), e);
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Emote, EmoteSet};
    use tokio::net::TcpListener;

    type Server = tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>;

    /// A one-connection event socket; `script` plays 7TV's side.
    async fn socket<F, Fut>(script: F) -> String
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

    fn cfg(events: String) -> Config {
        Config {
            api: "http://127.0.0.1:9".into(),
            events,
        }
    }

    fn hello(interval_ms: u64) -> Message {
        Message::text(
            json!({"op": 1, "d": {"heartbeat_interval": interval_ms, "session_id": "x"}})
                .to_string(),
        )
    }

    fn entry(id: &str, name: &str) -> Value {
        json!({"id": id, "name": name, "flags": 0, "data": {"id": id, "name": name, "flags": 0,
            "host": {"url": format!("//cdn.7tv.app/emote/{id}"),
                     "files": [{"name": "1x.webp", "width": 48, "height": 32}]}}})
    }

    fn dispatch(body: Value) -> Message {
        Message::text(json!({"op": 0, "d": {"type": "emote_set.update", "body": body}}).to_string())
    }

    fn loaded() -> Emotes {
        let e = Emotes::default();
        let old = Emote {
            id: "E1".into(),
            name: "old".into(),
            url: "https://cdn.7tv.app/emote/E1/1x.webp".into(),
            size: (32, 32),
            overlay: false,
        };
        let gone = Emote {
            id: "E2".into(),
            name: "gone".into(),
            ..old.clone()
        };
        e.set_channel(EmoteSet::from([("old".into(), old), ("gone".into(), gone)]));
        e
    }

    #[tokio::test]
    async fn subscribes_and_applies_additions_removals_and_renames() {
        let (seen_tx, seen_rx) = tokio::sync::oneshot::channel();
        let url = socket(|mut ws| async move {
            ws.send(hello(30_000)).await.unwrap();
            let mut subs = Vec::new();
            for _ in 0..2 {
                let Some(Ok(Message::Text(t))) = ws.next().await else { panic!("no subscribe") };
                subs.push(serde_json::from_str::<Value>(&t).unwrap());
            }
            seen_tx.send(subs).unwrap();
            ws.send(Message::text(json!({"op": 2, "d": {"count": 1}}).to_string())).await.unwrap();
            ws.send(dispatch(json!({"id": "S1", "kind": 3,
                "pushed": [{"key": "emotes", "index": 2, "value": entry("E3", "new")}],
                "pulled": [{"key": "emotes", "index": 1, "old_value": {"id": "E2", "name": "gone"}}],
                "updated": [{"key": "emotes", "index": 0,
                    "old_value": {"id": "E1", "name": "old"},
                    "value": {"id": "E1", "name": "renamed", "data": null}}]})))
                .await
                .unwrap();
            ws.send(Message::text(json!({"op": 0, "d": {"type": "user.update", "body": {}}}).to_string()))
                .await
                .unwrap();
            let _ = ws.next().await;
        })
        .await;
        let emotes = loaded();
        let end = timeout(
            Duration::from_secs(10),
            follow(
                &reqwest::Client::new(),
                &cfg(url),
                "S1",
                Some("U1"),
                &emotes,
            ),
        )
        .await
        .expect("follow hung");
        assert!(matches!(end, End::Changed), "{end:?}");
        let subs = seen_rx.await.unwrap();
        assert_eq!(
            subs[0],
            json!({"op": 35, "d": {"type": "emote_set.update", "condition": {"object_id": "S1"}}})
        );
        assert_eq!(subs[1]["d"]["type"], "user.update");
        assert_eq!(subs[1]["d"]["condition"]["object_id"], "U1");

        let new = emotes.find("new").expect("the pushed emote");
        assert_eq!(
            (new.url.as_str(), new.size),
            ("https://cdn.7tv.app/emote/E3/1x.webp", (48, 32))
        );
        assert!(emotes.find("gone").is_none());
        assert!(emotes.find("old").is_none());
        assert_eq!(
            emotes.find("renamed").unwrap().id,
            "E1",
            "a rename keeps the image"
        );
    }

    #[tokio::test]
    async fn close_codes_decide_whether_to_reconnect() {
        for (code, want) in [(4009u16, "stop"), (4007, "wait"), (4006, "lost")] {
            let url = socket(move |mut ws| async move {
                ws.send(hello(30_000)).await.unwrap();
                let _ = ws.next().await;
                ws.send(Message::text(
                    json!({"op": 7, "d": {"code": code, "message": "bye"}}).to_string(),
                ))
                .await
                .unwrap();
                let _ = ws.next().await;
            })
            .await;
            let emotes = Emotes::default();
            let end = follow(&reqwest::Client::new(), &cfg(url), "S1", None, &emotes).await;
            let got = match end {
                End::Stop(_) => "stop",
                End::Wait(..) => "wait",
                End::Lost(_) => "lost",
                End::Changed => "changed",
            };
            assert_eq!(got, want, "code {code}");
        }
    }

    #[tokio::test]
    async fn a_silent_socket_counts_as_lost() {
        let url = socket(|mut ws| async move {
            ws.send(hello(1_000)).await.unwrap();
            tokio::time::sleep(Duration::from_secs(30)).await;
            drop(ws);
        })
        .await;
        let emotes = Emotes::default();
        let end = timeout(
            Duration::from_secs(15),
            follow(&reqwest::Client::new(), &cfg(url), "S1", None, &emotes),
        )
        .await
        .expect("the heartbeat timeout never fired");
        assert!(matches!(end, End::Lost(why) if why.contains("quiet")));
    }
}
