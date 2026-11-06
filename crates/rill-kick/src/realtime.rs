//! Asking Kick which chat socket to use, and guest tokens for it. Unofficial: Kick may change it.

use serde_json::{Value, json};

use crate::pusher;

/// Where Kick's web player asks which chat socket to use.
pub const CONNECTION_URL: &str = "https://web.kick.com/api/v1/realtime/connection";
/// Where it gets a guest token for Centrifugo: about 45 minutes, no sign-in.
pub const TOKEN_URL: &str = "https://web.kick.com/api/v1/realtime/auth/connection";

/// The providers Rill accepts, in the order Kick's web player offers them.
pub const PROVIDERS: [&str; 2] = ["centrifugo", "pusher"];

/// The chat socket Kick chose.
#[derive(Debug, Clone, PartialEq)]
pub enum Transport {
    Pusher { url: String },
    Centrifugo { url: String },
}

impl Transport {
    pub fn name(&self) -> &'static str {
        match self {
            Transport::Pusher { .. } => "pusher",
            Transport::Centrifugo { .. } => "centrifugo",
        }
    }
}

/// The lookup body: an anonymous web client and the providers it accepts.
pub fn connection_body(client_id: &str, providers: &[&str]) -> Value {
    let accepted: Vec<Value> = providers.iter().map(|p| json!({"provider": p})).collect();
    json!({
        "client": {"id": client_id, "type": "web"},
        "capabilities": {"accepted_providers": accepted},
    })
}

/// Reads Kick's answer: `data.connections`, or a single connection bare or under `data`.
pub fn transport(v: &Value, providers: &[&str]) -> Result<Transport, String> {
    let list: Vec<&Value> = if let Some(a) = v["data"]["connections"].as_array() {
        a.iter().collect()
    } else if v["data"]["provider"].is_string() {
        vec![&v["data"]]
    } else if v["provider"].is_string() {
        vec![v]
    } else {
        return Err(format!(
            "Kick's chat settings weren't in the expected shape (fields: {}; data: {})",
            field_names(v),
            field_names(&v["data"])
        ));
    };
    let mut offered = Vec::new();
    for c in list {
        let provider = c["provider"].as_str().unwrap_or_default();
        let creds = &c["credentials"];
        offered.push(provider.to_string());
        if !providers.contains(&provider) {
            continue;
        }
        match provider {
            "pusher" => {
                let (key, cluster) = (
                    creds["app_key"].as_str().unwrap_or_default(),
                    creds["cluster"].as_str().unwrap_or_default(),
                );
                if !key.is_empty() && !cluster.is_empty() {
                    return Ok(Transport::Pusher {
                        url: pusher::socket_url(key, cluster),
                    });
                }
            }
            "centrifugo" => {
                let url = creds["url"].as_str().unwrap_or_default();
                if url.starts_with("wss://") {
                    return Ok(Transport::Centrifugo {
                        url: url.to_string(),
                    });
                }
            }
            _ => {}
        }
    }
    Err(format!(
        "Kick offered no chat socket Rill can use (offered: {})",
        if offered.is_empty() {
            "none".into()
        } else {
            offered.join(", ")
        }
    ))
}

/// The guest token request body.
pub fn token_body(client_id: &str) -> Value {
    json!({ "client_id": client_id })
}

/// Reads a guest token from `data.token` (or a bare `token`).
pub fn token(v: &Value) -> Result<String, String> {
    [&v["data"]["token"], &v["token"]]
        .into_iter()
        .filter_map(Value::as_str)
        .find(|t| !t.is_empty())
        .map(str::to_string)
        .ok_or_else(|| {
            format!(
                "Kick's guest token wasn't in the expected shape (fields: {}; data: {})",
                field_names(v),
                field_names(&v["data"])
            )
        })
}

/// Field names, for diagnosing a changed response without logging its values.
fn field_names(v: &Value) -> String {
    v.as_object()
        .filter(|o| !o.is_empty())
        .map(|o| o.keys().cloned().collect::<Vec<_>>().join(", "))
        .unwrap_or_else(|| "none".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offers_providers_like_kicks_web_player() {
        let body = connection_body("id-1", &PROVIDERS);
        assert_eq!(body["client"]["type"], "web");
        assert_eq!(
            body["capabilities"]["accepted_providers"],
            json!([{"provider": "centrifugo"}, {"provider": "pusher"}])
        );
    }

    #[test]
    fn reads_connections() {
        let centrifugo = json!({"data": {"connections": [
            {"provider": "centrifugo", "credentials": {"url": "wss://rt.example/connection/websocket"}}
        ]}});
        assert_eq!(
            transport(&centrifugo, &PROVIDERS).unwrap(),
            Transport::Centrifugo {
                url: "wss://rt.example/connection/websocket".into()
            }
        );
        let pusher = json!({"data": {"connections": [
            {"provider": "pusher", "credentials": {"app_key": "abc", "cluster": "us2"}}
        ]}});
        assert_eq!(
            transport(&pusher, &PROVIDERS).unwrap(),
            Transport::Pusher {
                url: pusher::socket_url("abc", "us2")
            }
        );
        let single =
            json!({"provider": "pusher", "credentials": {"app_key": "abc", "cluster": "us2"}});
        assert!(transport(&single, &PROVIDERS).is_ok());
        assert!(transport(&json!({ "data": single }), &PROVIDERS).is_ok());
    }

    #[test]
    fn refuses_what_it_cant_use_without_leaking_values() {
        let only_pusher = json!({"data": {"connections": [
            {"provider": "centrifugo", "credentials": {"url": "wss://x"}}
        ]}});
        let err = transport(&only_pusher, &["pusher"]).unwrap_err();
        assert!(err.contains("offered: centrifugo"), "{err}");
        let insecure = json!({"data": {"connections": [
            {"provider": "centrifugo", "credentials": {"url": "ws://x"}}
        ]}});
        assert!(transport(&insecure, &PROVIDERS).is_err());
        let odd = json!({"token": "secret-value"});
        let err = transport(&odd, &PROVIDERS).unwrap_err();
        assert!(err.contains("token") && !err.contains("secret-value"));
    }

    #[test]
    fn reads_guest_tokens() {
        assert_eq!(token(&json!({"data": {"token": "jwt"}})).unwrap(), "jwt");
        let err = token(&json!({"data": {"other": "secret-value"}})).unwrap_err();
        assert!(err.contains("other") && !err.contains("secret-value"));
    }
}
