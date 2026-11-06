//! Kick's older chat socket: the Pusher protocol.

use serde_json::{Value, json};

use crate::events::payload;

/// A Pusher socket URL for an app key and cluster.
pub fn socket_url(app_key: &str, cluster: &str) -> String {
    format!(
        "wss://ws-{cluster}.pusher.com/app/{app_key}?protocol=7&client=js&version=8.4.0&flash=false"
    )
}

/// A decoded socket frame.
#[derive(Debug, PartialEq)]
pub enum Frame {
    Connected { activity_timeout: u64 },
    Subscribed,
    Ping,
    Pong,
    Error(String),
    Kick { event: String, data: Value },
}

/// Decodes one text frame. Pusher sends `data` as a JSON string inside JSON.
pub fn parse(text: &str) -> Result<Frame, String> {
    let v: Value =
        serde_json::from_str(text).map_err(|e| format!("Kick: unreadable socket message: {e}"))?;
    let event = v["event"].as_str().unwrap_or_default();
    let data = payload(&v["data"]);
    Ok(match event {
        "pusher:connection_established" => Frame::Connected {
            activity_timeout: data["activity_timeout"].as_u64().unwrap_or(120),
        },
        "pusher_internal:subscription_succeeded" => Frame::Subscribed,
        "pusher:ping" => Frame::Ping,
        "pusher:pong" => Frame::Pong,
        "pusher:error" => Frame::Error(
            data["message"]
                .as_str()
                .unwrap_or("unknown error")
                .to_string(),
        ),
        _ => Frame::Kick {
            event: event.to_string(),
            data,
        },
    })
}

/// The frame that subscribes to a chatroom.
pub fn subscribe(chatroom_id: u64) -> String {
    json!({"event": "pusher:subscribe", "data": {"auth": "", "channel": format!("chatrooms.{chatroom_id}.v2")}})
        .to_string()
}

pub fn ping() -> String {
    json!({"event": "pusher:ping", "data": {}}).to_string()
}

pub fn pong() -> String {
    json!({"event": "pusher:pong", "data": {}}).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_pusher_frames_with_string_data() {
        let c = r#"{"event":"pusher:connection_established","data":"{\"socket_id\":\"1.2\",\"activity_timeout\":120}"}"#;
        assert_eq!(
            parse(c).unwrap(),
            Frame::Connected {
                activity_timeout: 120
            }
        );
        assert_eq!(
            parse(r#"{"event":"pusher:ping","data":{}}"#).unwrap(),
            Frame::Ping
        );
        let sub = r#"{"event":"pusher_internal:subscription_succeeded","data":"{}","channel":"chatrooms.668.v2"}"#;
        assert_eq!(parse(sub).unwrap(), Frame::Subscribed);
    }
}
