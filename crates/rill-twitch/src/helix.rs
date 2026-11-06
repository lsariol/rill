//! The Helix API calls Rill uses, with token refresh on 401 and rate-limit reporting.

use std::collections::HashMap;
use std::sync::Arc;

use reqwest::Method;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::Mutex;

use crate::auth::{self, AuthError, Token};

const HELIX: &str = "https://api.twitch.tv/helix";

/// Why a Helix call failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HelixError {
    /// The sign-in is no longer valid, even after a refresh.
    Unauthorized,
    /// Too many requests; the bucket refills in this many seconds.
    RateLimited { wait_secs: i64 },
    /// Twitch refused the request with a reason.
    Api { status: u16, message: String },
    /// The network or Twitch is down.
    Network(String),
}

impl HelixError {
    /// A message for the user, saying how to fix it.
    pub fn message(&self) -> String {
        match self {
            HelixError::Unauthorized => {
                "Twitch: your sign-in expired. Click the Twitch icon to sign in again.".into()
            }
            HelixError::RateLimited { wait_secs } => format!(
                "Twitch: too many requests. Wait {} s and try again.",
                wait_secs.max(&1)
            ),
            HelixError::Api {
                status: 403,
                message,
            } => format!(
                "Twitch: not allowed ({message}). Moderator actions need a moderator account in that channel."
            ),
            HelixError::Api { status, message } => format!("Twitch: {message} ({status})."),
            HelixError::Network(m) => m.clone(),
        }
    }
}

/// A Twitch user.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct User {
    pub id: String,
    pub login: String,
    pub display_name: String,
}

/// A badge's title and image, keyed by `set_id/id`.
pub type Badges = HashMap<String, (String, String)>;

/// A Helix client sharing one token, refreshed when it expires.
#[derive(Clone)]
pub struct Helix {
    http: reqwest::Client,
    /// Helix's address; tests use a local server.
    base: String,
    client_id: String,
    token: Arc<Mutex<Token>>,
}

impl Helix {
    pub fn new(http: reqwest::Client, client_id: String, token: Token) -> Helix {
        Helix {
            http,
            base: HELIX.into(),
            client_id,
            token: Arc::new(Mutex::new(token)),
        }
    }

    #[cfg(test)]
    pub(crate) fn with_base(mut self, base: &str) -> Helix {
        self.base = base.into();
        self
    }

    /// The current access token.
    pub async fn access(&self) -> String {
        self.token.lock().await.access.clone()
    }

    pub fn client_id(&self) -> &str {
        &self.client_id
    }

    /// Refreshes the token unless someone already replaced `stale`.
    pub async fn refresh(&self, stale: &str) -> Result<(), HelixError> {
        let mut token = self.token.lock().await;
        if token.access != stale {
            return Ok(());
        }
        match auth::refresh(&self.http, &self.client_id, &token).await {
            Ok(new) => {
                if let Err(e) = auth::save_token(&new) {
                    tracing::error!(err = %e, "Twitch token not saved after refresh");
                }
                *token = new;
                Ok(())
            }
            Err(AuthError::Invalid) => Err(HelixError::Unauthorized),
            Err(e) => Err(HelixError::Network(e.message())),
        }
    }

    async fn call(
        &self,
        method: Method,
        path: &str,
        query: &[(&str, &str)],
        body: Option<Value>,
    ) -> Result<Value, HelixError> {
        let mut refreshed = false;
        let mut limited = 0;
        loop {
            let access = self.access().await;
            let mut req = self
                .http
                .request(method.clone(), format!("{}{path}", self.base))
                .header("Client-Id", &self.client_id)
                .bearer_auth(&access)
                .query(query);
            if let Some(b) = &body {
                req = req.json(b);
            }
            let res = req.send().await.map_err(|e| {
                HelixError::Network(format!(
                    "Twitch: can't reach api.twitch.tv ({e}). Check the internet connection."
                ))
            })?;
            let status = res.status().as_u16();
            match status {
                401 if !refreshed => {
                    refreshed = true;
                    self.refresh(&access).await?;
                    continue;
                }
                401 => return Err(HelixError::Unauthorized),
                429 => {
                    let reset = res
                        .headers()
                        .get("Ratelimit-Reset")
                        .and_then(|v| v.to_str().ok())
                        .and_then(|v| v.parse::<i64>().ok())
                        .unwrap_or(0);
                    let wait_secs = reset - chrono::Utc::now().timestamp();
                    // Big batches (a nuke) wait out short limits instead of failing.
                    if limited < 3 && wait_secs <= 60 {
                        limited += 1;
                        let wait = u64::try_from(wait_secs.max(1)).unwrap_or(1);
                        tokio::time::sleep(std::time::Duration::from_secs(wait)).await;
                        continue;
                    }
                    return Err(HelixError::RateLimited { wait_secs });
                }
                204 => return Ok(Value::Null),
                200..=299 => {
                    return res.json().await.map_err(|e| {
                        HelixError::Network(format!("Twitch: unexpected reply from {path}: {e}"))
                    });
                }
                _ => {
                    let body: Value = res.json().await.unwrap_or(Value::Null);
                    let message = body["message"]
                        .as_str()
                        .filter(|m| !m.is_empty())
                        .unwrap_or("no reason given")
                        .to_string();
                    return Err(HelixError::Api { status, message });
                }
            }
        }
    }

    /// Looks up users by login.
    pub async fn user_by_login(&self, login: &str) -> Result<Option<User>, HelixError> {
        let v = self
            .call(Method::GET, "/users", &[("login", login)], None)
            .await?;
        Ok(first(&v))
    }

    /// Looks up a user by ID.
    pub async fn user_by_id(&self, id: &str) -> Result<Option<User>, HelixError> {
        let v = self
            .call(Method::GET, "/users", &[("id", id)], None)
            .await?;
        Ok(first(&v))
    }

    /// Whether `user_id` moderates `channel_id` (first 100 channels).
    pub async fn moderates(&self, user_id: &str, channel_id: &str) -> Result<bool, HelixError> {
        let v = self
            .call(
                Method::GET,
                "/moderation/channels",
                &[("user_id", user_id), ("first", "100")],
                None,
            )
            .await?;
        Ok(v["data"]
            .as_array()
            .is_some_and(|a| a.iter().any(|c| c["broadcaster_id"] == channel_id)))
    }

    /// Sends a chat message. Returns its ID, or Twitch's reason for dropping it.
    pub async fn send_message(
        &self,
        broadcaster: &str,
        sender: &str,
        text: &str,
        reply_to: Option<&str>,
    ) -> Result<String, HelixError> {
        let mut body = json!({"broadcaster_id": broadcaster, "sender_id": sender, "message": text});
        if let Some(r) = reply_to {
            body["reply_parent_message_id"] = json!(r);
        }
        let v = self
            .call(Method::POST, "/chat/messages", &[], Some(body))
            .await?;
        let d = &v["data"][0];
        if d["is_sent"].as_bool() == Some(true) {
            return Ok(d["message_id"].as_str().unwrap_or_default().to_string());
        }
        let reason = d["drop_reason"]["message"]
            .as_str()
            .unwrap_or("Twitch dropped the message")
            .to_string();
        Err(HelixError::Api {
            status: 200,
            message: reason,
        })
    }

    pub async fn delete_message(
        &self,
        broadcaster: &str,
        moderator: &str,
        message_id: &str,
    ) -> Result<(), HelixError> {
        self.call(
            Method::DELETE,
            "/moderation/chat",
            &[
                ("broadcaster_id", broadcaster),
                ("moderator_id", moderator),
                ("message_id", message_id),
            ],
            None,
        )
        .await
        .map(|_| ())
    }

    /// Bans a user, or times them out when `seconds` is set.
    pub async fn ban(
        &self,
        broadcaster: &str,
        moderator: &str,
        user_id: &str,
        seconds: Option<u32>,
        reason: Option<&str>,
    ) -> Result<(), HelixError> {
        let mut data = json!({"user_id": user_id});
        if let Some(s) = seconds {
            data["duration"] = json!(s);
        }
        if let Some(r) = reason {
            data["reason"] = json!(r);
        }
        self.call(
            Method::POST,
            "/moderation/bans",
            &[("broadcaster_id", broadcaster), ("moderator_id", moderator)],
            Some(json!({ "data": data })),
        )
        .await
        .map(|_| ())
    }

    pub async fn unban(
        &self,
        broadcaster: &str,
        moderator: &str,
        user_id: &str,
    ) -> Result<(), HelixError> {
        self.call(
            Method::DELETE,
            "/moderation/bans",
            &[
                ("broadcaster_id", broadcaster),
                ("moderator_id", moderator),
                ("user_id", user_id),
            ],
            None,
        )
        .await
        .map(|_| ())
    }

    pub async fn create_poll(
        &self,
        broadcaster: &str,
        title: &str,
        choices: &[String],
        seconds: u32,
    ) -> Result<(), HelixError> {
        let choices: Vec<Value> = choices.iter().map(|c| json!({"title": c})).collect();
        let body = json!({
            "broadcaster_id": broadcaster,
            "title": title,
            "choices": choices,
            "duration": seconds.clamp(15, 1800),
        });
        self.call(Method::POST, "/polls", &[], Some(body))
            .await
            .map(|_| ())
    }

    /// Global and channel badges; channel badges win.
    pub async fn badges(&self, broadcaster: &str) -> Result<Badges, HelixError> {
        let mut out = Badges::new();
        let global = self
            .call(Method::GET, "/chat/badges/global", &[], None)
            .await?;
        add_badges(&mut out, &global);
        let channel = self
            .call(
                Method::GET,
                "/chat/badges",
                &[("broadcaster_id", broadcaster)],
                None,
            )
            .await?;
        add_badges(&mut out, &channel);
        Ok(out)
    }

    /// Subscribes an EventSub WebSocket session to one event type.
    pub async fn subscribe(
        &self,
        session_id: &str,
        kind: &str,
        broadcaster: &str,
        user: &str,
    ) -> Result<(), HelixError> {
        let body = json!({
            "type": kind,
            "version": "1",
            "condition": {"broadcaster_user_id": broadcaster, "user_id": user},
            "transport": {"method": "websocket", "session_id": session_id},
        });
        self.call(Method::POST, "/eventsub/subscriptions", &[], Some(body))
            .await
            .map(|_| ())
    }
}

fn first(v: &Value) -> Option<User> {
    v["data"]
        .as_array()?
        .first()
        .and_then(|u| serde_json::from_value(u.clone()).ok())
}

/// Adds a badges response (`data[].set_id`, `versions[]`) to the map.
pub fn add_badges(out: &mut Badges, v: &Value) {
    let Some(sets) = v["data"].as_array() else {
        return;
    };
    for set in sets {
        let Some(set_id) = set["set_id"].as_str() else {
            continue;
        };
        for ver in set["versions"].as_array().into_iter().flatten() {
            if let (Some(id), Some(url)) = (ver["id"].as_str(), ver["image_url_2x"].as_str()) {
                let title = ver["title"].as_str().unwrap_or(set_id).to_string();
                out.insert(format!("{set_id}/{id}"), (title, url.to_string()));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn badges_parse_and_channel_overrides() {
        let mut b = Badges::new();
        add_badges(
            &mut b,
            &json!({"data":[{"set_id":"subscriber","versions":[{"id":"0","title":"Subscriber","image_url_1x":"g1","image_url_2x":"g2"}]}]}),
        );
        add_badges(
            &mut b,
            &json!({"data":[{"set_id":"subscriber","versions":[{"id":"0","title":"Sub","image_url_2x":"c2"}]}]}),
        );
        assert_eq!(b["subscriber/0"], ("Sub".to_string(), "c2".to_string()));
    }

    #[test]
    fn errors_say_how_to_fix() {
        assert!(HelixError::Unauthorized.message().contains("sign in again"));
        assert!(
            HelixError::RateLimited { wait_secs: 0 }
                .message()
                .contains("Wait 1 s")
        );
        assert!(
            HelixError::Api {
                status: 403,
                message: "x".into()
            }
            .message()
            .contains("moderator")
        );
    }
}
