//! Kick's official public API: who's signed in, sending, deleting, bans and timeouts.

use std::sync::Arc;
use std::time::Duration;

use reqwest::Method;
use serde_json::{Value, json};
use tokio::sync::Mutex;

use crate::auth::{self, AuthError, Exchange, Token};

pub const API: &str = "https://api.kick.com/public/v1";
/// Kick's longest timeout, in minutes (7 days).
pub const MAX_TIMEOUT_MINUTES: u32 = 10_080;
/// Kick's limit on a ban or timeout reason.
const REASON_MAX: usize = 100;

/// Why an API call failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApiError {
    /// The sign-in is no longer valid.
    Unauthorized,
    Api {
        status: u16,
        message: String,
    },
    Network(String),
}

impl ApiError {
    pub fn message(&self) -> String {
        match self {
            ApiError::Unauthorized => AuthError::Invalid.message(),
            ApiError::Api {
                status: 403,
                message,
            } => format!(
                "Kick: not allowed ({message}). Moderation needs the broadcaster or a moderator of the channel."
            ),
            ApiError::Api { status, message } => format!("Kick: {message} ({status})."),
            ApiError::Network(m) => m.clone(),
        }
    }
}

/// A signed-in API client; clones share the token.
#[derive(Clone)]
pub struct Api {
    http: reqwest::Client,
    base: String,
    exchange: Exchange,
    token_url: String,
    token: Arc<Mutex<Token>>,
    /// Save refreshed tokens to the credential store; tests don't.
    persist: bool,
}

impl Api {
    pub fn new(http: reqwest::Client, exchange: Exchange, token: Token) -> Api {
        Api {
            http,
            base: API.into(),
            exchange,
            token_url: auth::TOKEN_URL.into(),
            token: Arc::new(Mutex::new(token)),
            persist: true,
        }
    }

    /// Points the client at local stand-ins, without touching the credential store.
    #[cfg(test)]
    pub(crate) fn local(mut self, base: &str) -> Api {
        self.base = base.into();
        self.token_url = format!("{base}/oauth/token");
        self.persist = false;
        self
    }

    pub async fn token(&self) -> Token {
        self.token.lock().await.clone()
    }

    /// A valid access token, refreshing it first when it's about to expire.
    async fn access(&self, force: bool) -> Result<String, ApiError> {
        let mut t = self.token.lock().await;
        if force || t.stale() {
            let fresh = auth::refresh(&self.http, &self.exchange, &self.token_url, &t)
                .await
                .map_err(|e| match e {
                    AuthError::Invalid => ApiError::Unauthorized,
                    e => ApiError::Network(e.message()),
                })?;
            if self.persist
                && let Err(e) = auth::save_token(&fresh)
            {
                tracing::warn!(err = %e, "Kick token not saved");
            }
            *t = fresh;
        }
        Ok(t.access.clone())
    }

    async fn call(
        &self,
        method: Method,
        path: &str,
        body: Option<Value>,
    ) -> Result<Value, ApiError> {
        let mut refreshed = false;
        let mut limited = 0;
        loop {
            let access = self.access(false).await?;
            let mut req = self
                .http
                .request(method.clone(), format!("{}{path}", self.base))
                .bearer_auth(&access);
            if let Some(b) = &body {
                req = req.json(b);
            }
            let res = req.send().await.map_err(|e| {
                ApiError::Network(format!(
                    "Kick: can't reach api.kick.com ({e}). Check the internet connection."
                ))
            })?;
            let status = res.status().as_u16();
            match status {
                401 if !refreshed => {
                    refreshed = true;
                    self.access(true).await?;
                    continue;
                }
                401 => return Err(ApiError::Unauthorized),
                // Big batches (a nuke) wait out short limits instead of failing.
                429 if limited < 3 => {
                    limited += 1;
                    let wait = res
                        .headers()
                        .get("Retry-After")
                        .and_then(|v| v.to_str().ok())
                        .and_then(|v| v.parse::<u64>().ok())
                        .unwrap_or(2)
                        .clamp(1, 60);
                    tokio::time::sleep(Duration::from_secs(wait)).await;
                    continue;
                }
                204 => return Ok(Value::Null),
                200..=299 => {
                    return Ok(res.json().await.unwrap_or(Value::Null));
                }
                _ => {
                    let body: Value = res.json().await.unwrap_or(Value::Null);
                    let message = body["message"]
                        .as_str()
                        .filter(|m| !m.is_empty())
                        .unwrap_or("no reason given")
                        .to_string();
                    return Err(ApiError::Api { status, message });
                }
            }
        }
    }

    /// The signed-in user's ID and name.
    pub async fn me(&self) -> Result<(u64, String), ApiError> {
        let v = self.call(Method::GET, "/users", None).await?;
        let u = &v["data"][0];
        let id = u["user_id"].as_u64().ok_or_else(|| ApiError::Api {
            status: 200,
            message: "the user data has no user_id".into(),
        })?;
        Ok((id, u["name"].as_str().unwrap_or_default().to_string()))
    }

    /// The signed-in user's own channel name, if they have one.
    pub async fn own_slug(&self) -> Result<Option<String>, ApiError> {
        let v = self.call(Method::GET, "/channels", None).await?;
        Ok(v["data"][0]["slug"].as_str().map(str::to_string))
    }

    /// Sends a message as the signed-in user; returns its ID.
    pub async fn send(
        &self,
        broadcaster: u64,
        content: &str,
        reply_to: Option<&str>,
    ) -> Result<String, ApiError> {
        let mut body =
            json!({"type": "user", "broadcaster_user_id": broadcaster, "content": content});
        if let Some(id) = reply_to {
            body["reply_to_message_id"] = json!(id);
        }
        let v = self.call(Method::POST, "/chat", Some(body)).await?;
        if v["data"]["is_sent"] == false {
            return Err(ApiError::Api {
                status: 200,
                message:
                    "Kick didn't post the message (slow mode, followers-only or a blocked word?)"
                        .into(),
            });
        }
        Ok(v["data"]["message_id"]
            .as_str()
            .unwrap_or_default()
            .to_string())
    }

    pub async fn delete(&self, message_id: &str) -> Result<(), ApiError> {
        self.call(Method::DELETE, &format!("/chat/{message_id}"), None)
            .await
            .map(|_| ())
    }

    /// Bans, or times out for `minutes` (clamped to Kick's 1 minute to 7 days).
    pub async fn ban(
        &self,
        broadcaster: u64,
        user: u64,
        minutes: Option<u32>,
        reason: Option<&str>,
    ) -> Result<(), ApiError> {
        let mut body = json!({"broadcaster_user_id": broadcaster, "user_id": user});
        if let Some(m) = minutes {
            body["duration"] = json!(m.clamp(1, MAX_TIMEOUT_MINUTES));
        }
        if let Some(r) = reason.filter(|r| !r.is_empty()) {
            body["reason"] = json!(r.chars().take(REASON_MAX).collect::<String>());
        }
        self.call(Method::POST, "/moderation/bans", Some(body))
            .await
            .map(|_| ())
    }

    /// Changes the signed-in user's stream title or category.
    pub async fn set_channel(&self, body: Value) -> Result<(), ApiError> {
        self.call(Method::PATCH, "/channels", Some(body))
            .await
            .map(|_| ())
    }

    /// A category's ID and name: an exact name match, else the first search result.
    pub async fn find_category(&self, name: &str) -> Result<(u64, String), ApiError> {
        let q: String = reqwest::Url::parse_with_params("http://x/", &[("name", name)])
            .map(|u| u.query().unwrap_or_default().to_string())
            .unwrap_or_default();
        let v = self
            .call(Method::GET, &format!("/../v2/categories?{q}"), None)
            .await?;
        let pick = |v: &Value| {
            v["data"].as_array().and_then(|d| {
                d.iter()
                    .find(|c| {
                        c["name"]
                            .as_str()
                            .is_some_and(|n| n.eq_ignore_ascii_case(name))
                    })
                    .or_else(|| d.first())
                    .and_then(|c| Some((c["id"].as_u64()?, c["name"].as_str()?.to_string())))
            })
        };
        if let Some(found) = pick(&v) {
            return Ok(found);
        }
        let q = q.replacen("name=", "q=", 1);
        let v = self
            .call(Method::GET, &format!("/categories?{q}"), None)
            .await?;
        pick(&v).ok_or_else(|| ApiError::Api {
            status: 404,
            message: format!("there's no category called \"{name}\""),
        })
    }

    pub async fn unban(&self, broadcaster: u64, user: u64) -> Result<(), ApiError> {
        let body = json!({"broadcaster_user_id": broadcaster, "user_id": user});
        self.call(Method::DELETE, "/moderation/bans", Some(body))
            .await
            .map(|_| ())
    }
}

/// Seconds to Kick's whole minutes, rounding up.
pub fn minutes(seconds: u32) -> u32 {
    seconds.div_ceil(60).clamp(1, MAX_TIMEOUT_MINUTES)
}
