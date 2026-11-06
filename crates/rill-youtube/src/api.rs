//! The YouTube Data API calls Rill uses, with token refresh and a running quota estimate.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use reqwest::Method;
use serde_json::{Value, json};
use tokio::sync::Mutex;

use crate::auth::{self, AuthError, Client, Token};

pub const API: &str = "https://www.googleapis.com/youtube/v3";

/// Approximate quota units per call, from Google's quota table. Default daily quota: 10,000.
pub mod cost {
    pub const LIST_MESSAGES: u64 = 5;
    pub const LIST_BROADCASTS: u64 = 1;
    pub const WRITE: u64 = 50;
}

/// Why an API call failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApiError {
    Unauthorized,
    /// The project's daily quota is used up.
    Quota,
    /// The chat ended or no longer exists.
    ChatEnded,
    Api {
        status: u16,
        reason: String,
        message: String,
    },
    Network(String),
}

impl ApiError {
    pub fn message(&self) -> String {
        match self {
            ApiError::Unauthorized => AuthError::Invalid.message(),
            ApiError::Quota => "YouTube: today's API quota is used up. Chat resumes after the quota resets (midnight Pacific time).".into(),
            ApiError::ChatEnded => "YouTube: the live chat has ended.".into(),
            ApiError::Api { status: 403, message, .. } => format!(
                "YouTube: not allowed ({message}). Moderator actions need the channel owner or a moderator."
            ),
            ApiError::Api { status, message, .. } => format!("YouTube: {message} ({status})."),
            ApiError::Network(m) => m.clone(),
        }
    }
}

/// A Data API client sharing one token.
#[derive(Clone)]
pub struct Api {
    http: reqwest::Client,
    /// The API's address; tests use a local server.
    base: String,
    client: Client,
    token: Arc<Mutex<Token>>,
    units: Arc<AtomicU64>,
}

impl Api {
    pub fn new(http: reqwest::Client, client: Client, token: Token, base: &str) -> Api {
        Api {
            http,
            base: base.into(),
            client,
            token: Arc::new(Mutex::new(token)),
            units: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Quota units spent since Rill started (an estimate).
    pub fn units(&self) -> u64 {
        self.units.load(Ordering::Relaxed)
    }

    pub async fn token(&self) -> Token {
        self.token.lock().await.clone()
    }

    async fn access(&self, force_refresh: bool) -> Result<String, ApiError> {
        let mut token = self.token.lock().await;
        if force_refresh || token.stale() {
            match auth::refresh(&self.http, &self.client, &token).await {
                Ok(new) => {
                    if let Err(e) = auth::save_token(&new) {
                        tracing::error!(err = %e, "YouTube token not saved after refresh");
                    }
                    *token = new;
                }
                Err(AuthError::Invalid) => return Err(ApiError::Unauthorized),
                Err(e) => return Err(ApiError::Network(e.message())),
            }
        }
        Ok(token.access.clone())
    }

    async fn call(
        &self,
        method: Method,
        path: &str,
        query: &[(&str, &str)],
        body: Option<Value>,
        units: u64,
    ) -> Result<Value, ApiError> {
        for attempt in 0..2 {
            let access = self.access(attempt == 1).await?;
            let mut req = self
                .http
                .request(method.clone(), format!("{}{path}", self.base))
                .bearer_auth(access)
                .query(query);
            if let Some(b) = &body {
                req = req.json(b);
            }
            self.units.fetch_add(units, Ordering::Relaxed);
            let res = req.send().await.map_err(|e| {
                ApiError::Network(format!(
                    "YouTube: can't reach the API ({e}). Check the internet connection."
                ))
            })?;
            let status = res.status().as_u16();
            if status == 401 && attempt == 0 {
                continue;
            }
            if status == 204 {
                return Ok(Value::Null);
            }
            let v: Value = res.json().await.unwrap_or(Value::Null);
            if (200..300).contains(&status) {
                return Ok(v);
            }
            return Err(classify(status, &v));
        }
        Err(ApiError::Unauthorized)
    }

    /// The signed-in channel's ID and title.
    pub async fn my_channel(&self) -> Result<(String, String), ApiError> {
        let v = self
            .call(
                Method::GET,
                "/channels",
                &[("part", "snippet"), ("mine", "true")],
                None,
                1,
            )
            .await?;
        let c = &v["items"][0];
        let id = c["id"].as_str().ok_or_else(|| ApiError::Api {
            status: 404,
            reason: "noChannel".into(),
            message: "this Google account has no YouTube channel".into(),
        })?;
        Ok((
            id.to_string(),
            c["snippet"]["title"].as_str().unwrap_or(id).to_string(),
        ))
    }

    /// The live chat of the user's active broadcast, if one is live.
    pub async fn active_chat(&self) -> Result<Option<String>, ApiError> {
        let v = self
            .call(
                Method::GET,
                "/liveBroadcasts",
                &[
                    ("part", "snippet"),
                    ("broadcastStatus", "active"),
                    ("broadcastType", "all"),
                ],
                None,
                cost::LIST_BROADCASTS,
            )
            .await?;
        Ok(v["items"]
            .as_array()
            .into_iter()
            .flatten()
            .find_map(|b| b["snippet"]["liveChatId"].as_str().map(str::to_string)))
    }

    /// One page of chat messages after `page_token`.
    pub async fn messages(&self, chat: &str, page_token: Option<&str>) -> Result<Value, ApiError> {
        let mut q = vec![
            ("liveChatId", chat),
            ("part", "snippet,authorDetails"),
            ("maxResults", "2000"),
        ];
        if let Some(t) = page_token {
            q.push(("pageToken", t));
        }
        self.call(
            Method::GET,
            "/liveChat/messages",
            &q,
            None,
            cost::LIST_MESSAGES,
        )
        .await
    }

    /// Sends a message; returns its ID.
    pub async fn send(&self, chat: &str, text: &str) -> Result<String, ApiError> {
        let body = json!({"snippet": {
            "liveChatId": chat,
            "type": "textMessageEvent",
            "textMessageDetails": {"messageText": text},
        }});
        let v = self
            .call(
                Method::POST,
                "/liveChat/messages",
                &[("part", "snippet")],
                Some(body),
                cost::WRITE,
            )
            .await?;
        Ok(v["id"].as_str().unwrap_or_default().to_string())
    }

    pub async fn delete(&self, message_id: &str) -> Result<(), ApiError> {
        self.call(
            Method::DELETE,
            "/liveChat/messages",
            &[("id", message_id)],
            None,
            cost::WRITE,
        )
        .await
        .map(|_| ())
    }

    /// Bans a channel, or times it out when `seconds` is set. Returns the ban's ID (needed to unban).
    pub async fn ban(
        &self,
        chat: &str,
        channel_id: &str,
        seconds: Option<u32>,
    ) -> Result<String, ApiError> {
        let mut snippet = json!({
            "liveChatId": chat,
            "type": if seconds.is_some() { "temporary" } else { "permanent" },
            "bannedUserDetails": {"channelId": channel_id},
        });
        if let Some(s) = seconds {
            snippet["banDurationSeconds"] = json!(s);
        }
        let v = self
            .call(
                Method::POST,
                "/liveChat/bans",
                &[("part", "snippet")],
                Some(json!({ "snippet": snippet })),
                cost::WRITE,
            )
            .await?;
        Ok(v["id"].as_str().unwrap_or_default().to_string())
    }

    pub async fn unban(&self, ban_id: &str) -> Result<(), ApiError> {
        self.call(
            Method::DELETE,
            "/liveChat/bans",
            &[("id", ban_id)],
            None,
            cost::WRITE,
        )
        .await
        .map(|_| ())
    }
}

/// Maps an error response to an [`ApiError`].
pub fn classify(status: u16, body: &Value) -> ApiError {
    let reason = body["error"]["errors"][0]["reason"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    let message = body["error"]["message"]
        .as_str()
        .unwrap_or("no reason given")
        .to_string();
    match reason.as_str() {
        "quotaExceeded" | "dailyLimitExceeded" => ApiError::Quota,
        "liveChatEnded" | "liveChatNotFound" | "liveChatDisabled" => ApiError::ChatEnded,
        _ if status == 401 => ApiError::Unauthorized,
        _ => ApiError::Api {
            status,
            reason,
            message,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_quota_and_ended_chats() {
        let quota = json!({"error": {"code": 403, "message": "quota", "errors": [{"reason": "quotaExceeded"}]}});
        assert_eq!(classify(403, &quota), ApiError::Quota);
        let ended = json!({"error": {"code": 403, "message": "ended", "errors": [{"reason": "liveChatEnded"}]}});
        assert_eq!(classify(403, &ended), ApiError::ChatEnded);
        let other = json!({"error": {"code": 400, "message": "Bad text", "errors": [{"reason": "invalidValue"}]}});
        assert!(classify(400, &other).message().contains("Bad text"));
    }
}
