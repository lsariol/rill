//! Kick's current chat socket: Centrifugo's JSON protocol.

use serde_json::{Value, json};

use crate::events::payload;

/// What the server sends as a ping, and what the client answers.
pub const PING: &str = "{}";

/// Centrifugo's "already subscribed" error: harmless.
const ALREADY_SUBSCRIBED: u64 = 105;

/// One decoded reply or push. A frame can hold several, one per line.
#[derive(Debug, PartialEq)]
pub enum Reply {
    Ping,
    /// The connect handshake: the server's ping interval, and when the token runs out.
    Connected {
        ping_secs: Option<u64>,
        ttl_secs: Option<u64>,
    },
    Subscribed,
    /// A renewed token, and when it runs out.
    Refreshed {
        ttl_secs: Option<u64>,
    },
    Error {
        code: u64,
        message: String,
    },
    /// The server is closing the connection.
    Disconnect {
        code: u64,
        reason: String,
    },
    /// The server dropped the chatroom subscription.
    Unsubscribed {
        code: u64,
    },
    Kick {
        event: String,
        data: Value,
    },
    /// Anything else (joins, leaves, replies Rill doesn't need).
    Other,
}

/// Decodes one text frame into its replies; an unreadable line is an `Err` in place.
pub fn parse(text: &str) -> Vec<Result<Reply, String>> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(parse_line)
        .collect()
}

fn parse_line(line: &str) -> Result<Reply, String> {
    let v: Value =
        serde_json::from_str(line).map_err(|e| format!("Kick: unreadable socket message: {e}"))?;
    let Some(o) = v.as_object() else {
        return Err("Kick: unreadable socket message: not an object".into());
    };
    if o.is_empty() {
        return Ok(Reply::Ping);
    }
    let code = |e: &Value| e["code"].as_u64().unwrap_or(0);
    let text = |e: &Value, key: &str| e[key].as_str().unwrap_or_default().to_string();
    let ttl = |r: &Value| (r["expires"] == true).then(|| r["ttl"].as_u64()).flatten();
    if let Some(e) = o.get("error") {
        return Ok(match code(e) {
            ALREADY_SUBSCRIBED => Reply::Other,
            c => Reply::Error {
                code: c,
                message: text(e, "message"),
            },
        });
    }
    if let Some(c) = o.get("connect") {
        return Ok(Reply::Connected {
            ping_secs: c["ping"].as_u64().filter(|p| *p > 0),
            ttl_secs: ttl(c),
        });
    }
    if o.contains_key("subscribe") {
        return Ok(Reply::Subscribed);
    }
    if let Some(r) = o.get("refresh") {
        return Ok(Reply::Refreshed { ttl_secs: ttl(r) });
    }
    let Some(push) = o.get("push") else {
        return Ok(Reply::Other);
    };
    if let Some(p) = push.get("pub") {
        let d = payload(&p["data"]);
        return Ok(Reply::Kick {
            event: d["event"].as_str().unwrap_or_default().to_string(),
            data: payload(&d["data"]),
        });
    }
    if let Some(d) = push.get("disconnect") {
        return Ok(Reply::Disconnect {
            code: code(d),
            reason: text(d, "reason"),
        });
    }
    if let Some(u) = push.get("unsubscribe") {
        return Ok(Reply::Unsubscribed { code: code(u) });
    }
    Ok(Reply::Other)
}

/// The first command: a guest token, under the name Kick's web player uses.
pub fn connect(id: u32, token: &str) -> String {
    json!({"id": id, "connect": {"token": token, "name": "js"}}).to_string()
}

/// Subscribes to a chatroom. Kick's chatrooms are public: no channel token.
pub fn subscribe(id: u32, chatroom_id: u64) -> String {
    json!({"id": id, "subscribe": {"channel": format!("chatrooms.{chatroom_id}.v2")}}).to_string()
}

/// Hands the server a fresh token before the old one runs out.
pub fn refresh(id: u32, token: &str) -> String {
    json!({"id": id, "refresh": {"token": token}}).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_handshake() {
        let r = parse(
            r#"{"id":1,"connect":{"client":"c","version":"6.9.7 PRO","expires":true,"ttl":2699,"ping":25,"pong":true}}"#,
        );
        assert_eq!(
            r,
            vec![Ok(Reply::Connected {
                ping_secs: Some(25),
                ttl_secs: Some(2699)
            })]
        );
        let r = parse(r#"{"id":1,"connect":{"client":"c"}}"#);
        assert_eq!(
            r,
            vec![Ok(Reply::Connected {
                ping_secs: None,
                ttl_secs: None
            })]
        );
        assert_eq!(
            parse(r#"{"id":2,"subscribe":{}}"#),
            vec![Ok(Reply::Subscribed)]
        );
        assert_eq!(
            parse(r#"{"id":3,"refresh":{"expires":true,"ttl":2700}}"#),
            vec![Ok(Reply::Refreshed {
                ttl_secs: Some(2700)
            })]
        );
    }

    #[test]
    fn splits_frames_into_lines() {
        let frame = "{}\n{\"id\":2,\"subscribe\":{}}\n\nnot json";
        let r = parse(frame);
        assert_eq!(r.len(), 3);
        assert_eq!(r[0], Ok(Reply::Ping));
        assert_eq!(r[1], Ok(Reply::Subscribed));
        assert!(r[2].is_err());
    }

    #[test]
    fn parses_chat_pushes_with_string_data() {
        let frame = r#"{"push":{"channel":"chatrooms.42.v2","pub":{"data":{"event":"App\\Events\\ChatMessageSentEvent","data":"{\"id\":\"m1\",\"content\":\"hello\"}"}}}}"#;
        let [Ok(Reply::Kick { event, data })] = &parse(frame)[..] else {
            panic!("expected a Kick event");
        };
        assert_eq!(event, "App\\Events\\ChatMessageSentEvent");
        assert_eq!(data["content"], "hello");
    }

    #[test]
    fn parses_errors_and_disconnects() {
        assert_eq!(
            parse(r#"{"id":2,"error":{"code":106,"message":"limit exceeded"}}"#),
            vec![Ok(Reply::Error {
                code: 106,
                message: "limit exceeded".into()
            })]
        );
        assert_eq!(
            parse(r#"{"id":2,"error":{"code":105,"message":"already subscribed"}}"#),
            vec![Ok(Reply::Other)]
        );
        assert_eq!(
            parse(r#"{"push":{"disconnect":{"code":3005,"reason":"token expired"}}}"#),
            vec![Ok(Reply::Disconnect {
                code: 3005,
                reason: "token expired".into()
            })]
        );
        assert_eq!(
            parse(r#"{"push":{"channel":"chatrooms.42.v2","unsubscribe":{"code":2500}}}"#),
            vec![Ok(Reply::Unsubscribed { code: 2500 })]
        );
    }

    #[test]
    fn builds_commands() {
        let c: Value = serde_json::from_str(&connect(1, "jwt")).unwrap();
        assert_eq!(
            c,
            json!({"id": 1, "connect": {"token": "jwt", "name": "js"}})
        );
        let s: Value = serde_json::from_str(&subscribe(2, 42)).unwrap();
        assert_eq!(
            s,
            json!({"id": 2, "subscribe": {"channel": "chatrooms.42.v2"}})
        );
        let r: Value = serde_json::from_str(&refresh(3, "jwt2")).unwrap();
        assert_eq!(r["refresh"]["token"], "jwt2");
    }
}
