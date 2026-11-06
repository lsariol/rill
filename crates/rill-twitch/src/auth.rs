//! Twitch sign-in: the device code flow for public clients, refresh, validation and revocation.

use std::time::Duration;

use serde::{Deserialize, Serialize};

/// The scopes Rill asks for: read and send chat, moderate, run polls, change chat modes, post
/// announcements and give shoutouts.
pub const SCOPES: &str = "user:read:chat user:write:chat moderator:manage:banned_users \
moderator:manage:chat_messages channel:manage:polls user:read:moderated_channels \
moderator:manage:chat_settings moderator:manage:announcements moderator:manage:shoutouts";

const DEVICE_URL: &str = "https://id.twitch.tv/oauth2/device";
const TOKEN_URL: &str = "https://id.twitch.tv/oauth2/token";
const VALIDATE_URL: &str = "https://id.twitch.tv/oauth2/validate";
const REVOKE_URL: &str = "https://id.twitch.tv/oauth2/revoke";
/// The credential store key for Twitch tokens.
pub const CREDENTIAL_KEY: &str = "twitch";

/// A signed-in user's tokens, as stored in the credential store.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Token {
    pub access: String,
    pub refresh: String,
}

/// A device code waiting for the user to approve it.
#[derive(Debug, Clone, Deserialize)]
pub struct DeviceCode {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    pub expires_in: u64,
    pub interval: u64,
}

/// What `oauth2/validate` reports about a token.
#[derive(Debug, Clone, Deserialize)]
pub struct Validated {
    pub user_id: String,
    pub login: String,
    #[serde(default)]
    pub scopes: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: String,
}

#[derive(Debug, Deserialize)]
struct OAuthError {
    #[serde(default)]
    message: String,
}

/// Why an auth call failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthError {
    /// The token or refresh token is no longer valid; sign in again.
    Invalid,
    /// The network or Twitch failed; try again later.
    Temporary(String),
    /// Something the user or settings must fix.
    Fatal(String),
}

impl AuthError {
    pub fn message(&self) -> String {
        match self {
            AuthError::Invalid => {
                "Twitch: your sign-in expired. Click the Twitch icon to sign in again.".into()
            }
            AuthError::Temporary(m) | AuthError::Fatal(m) => m.clone(),
        }
    }
}

fn network(e: reqwest::Error) -> AuthError {
    AuthError::Temporary(format!(
        "Twitch: can't reach id.twitch.tv ({e}). Check the internet connection."
    ))
}

/// Starts the device code flow.
pub async fn start_device(
    http: &reqwest::Client,
    client_id: &str,
) -> Result<DeviceCode, AuthError> {
    let res = http
        .post(DEVICE_URL)
        .form(&[("client_id", client_id), ("scopes", SCOPES)])
        .send()
        .await
        .map_err(network)?;
    let status = res.status();
    if !status.is_success() {
        let body: OAuthError = res.json().await.unwrap_or(OAuthError {
            message: String::new(),
        });
        return Err(AuthError::Fatal(format!(
            "Twitch: sign-in couldn't start ({status}: {}). Check twitch.client_id in settings.json; the app must be a Public client.",
            body.message
        )));
    }
    res.json().await.map_err(network)
}

/// One poll of the device code. `Ok(None)` while the user hasn't approved yet.
pub async fn poll_device(
    http: &reqwest::Client,
    client_id: &str,
    device: &DeviceCode,
) -> Result<Option<Token>, AuthError> {
    let res = http
        .post(TOKEN_URL)
        .form(&[
            ("client_id", client_id),
            ("scopes", SCOPES),
            ("device_code", device.device_code.as_str()),
            ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
        ])
        .send()
        .await
        .map_err(network)?;
    if res.status().is_success() {
        let t: TokenResponse = res.json().await.map_err(network)?;
        return Ok(Some(Token {
            access: t.access_token,
            refresh: t.refresh_token,
        }));
    }
    let body: OAuthError = res.json().await.unwrap_or(OAuthError {
        message: String::new(),
    });
    match body.message.as_str() {
        "authorization_pending" | "slow_down" => Ok(None),
        "invalid device code" | "expired_token" => Err(AuthError::Fatal(
            "Twitch: the sign-in code expired. Click the Twitch icon to get a new one.".into(),
        )),
        other => Err(AuthError::Fatal(format!(
            "Twitch: sign-in was refused ({other}). Try again from the Twitch icon."
        ))),
    }
}

/// How often to poll, at least once a second.
pub fn poll_interval(device: &DeviceCode) -> Duration {
    Duration::from_secs(device.interval.max(1))
}

/// Trades a refresh token for new tokens. Public clients get a new refresh token each time.
pub async fn refresh(
    http: &reqwest::Client,
    client_id: &str,
    token: &Token,
) -> Result<Token, AuthError> {
    let res = http
        .post(TOKEN_URL)
        .form(&[
            ("client_id", client_id),
            ("grant_type", "refresh_token"),
            ("refresh_token", token.refresh.as_str()),
        ])
        .send()
        .await
        .map_err(network)?;
    let status = res.status();
    if status.as_u16() == 400 || status.as_u16() == 401 {
        return Err(AuthError::Invalid);
    }
    if !status.is_success() {
        return Err(AuthError::Temporary(format!(
            "Twitch: refreshing the sign-in failed ({status}). Rill will try again."
        )));
    }
    let t: TokenResponse = res.json().await.map_err(network)?;
    Ok(Token {
        access: t.access_token,
        refresh: t.refresh_token,
    })
}

/// Checks a token; Twitch asks apps to do this at start and hourly.
pub async fn validate(http: &reqwest::Client, access: &str) -> Result<Validated, AuthError> {
    let res = http
        .get(VALIDATE_URL)
        .header("Authorization", format!("OAuth {access}"))
        .send()
        .await
        .map_err(network)?;
    match res.status().as_u16() {
        200 => res.json().await.map_err(network),
        401 => Err(AuthError::Invalid),
        s => Err(AuthError::Temporary(format!(
            "Twitch: checking the sign-in failed ({s}). Rill will try again."
        ))),
    }
}

/// Revokes a token on sign-out. Failures are only logged: the local copy is deleted anyway.
pub async fn revoke(http: &reqwest::Client, client_id: &str, access: &str) {
    let res = http
        .post(REVOKE_URL)
        .form(&[("client_id", client_id), ("token", access)])
        .send()
        .await;
    if let Err(e) = res {
        tracing::warn!(err = %e, "Twitch token revoke failed");
    }
}

/// Loads the stored token, if any.
pub fn load_token() -> Result<Option<Token>, String> {
    let Some(raw) = rill_os::credentials::load(CREDENTIAL_KEY)? else {
        return Ok(None);
    };
    match serde_json::from_str(&raw) {
        Ok(t) => Ok(Some(t)),
        Err(_) => {
            rill_os::credentials::delete(CREDENTIAL_KEY)?;
            Ok(None)
        }
    }
}

/// Stores a token, replacing the old one.
pub fn save_token(token: &Token) -> Result<(), String> {
    let raw = serde_json::to_string(token).map_err(|e| format!("Twitch: token not saved: {e}"))?;
    rill_os::credentials::save(CREDENTIAL_KEY, &raw)
}

/// Deletes the stored token.
pub fn delete_token() -> Result<(), String> {
    rill_os::credentials::delete(CREDENTIAL_KEY)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_device_code_response() {
        let d: DeviceCode = serde_json::from_str(
            r#"{"device_code":"abc","expires_in":1800,"interval":5,"user_code":"ABCDEFGH","verification_uri":"https://www.twitch.tv/activate?public=true&device-code=ABCDEFGH"}"#,
        )
        .unwrap();
        assert_eq!(d.user_code, "ABCDEFGH");
        assert_eq!(poll_interval(&d), Duration::from_secs(5));
    }

    #[test]
    fn token_round_trips_through_json() {
        let t = Token {
            access: "a".into(),
            refresh: "r".into(),
        };
        let back: Token = serde_json::from_str(&serde_json::to_string(&t).unwrap()).unwrap();
        assert_eq!(back, t);
    }
}
