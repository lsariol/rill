//! Google sign-in for desktop apps: browser consent with a loopback redirect and PKCE.

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// Read and send chat, delete messages and ban users.
pub const SCOPE: &str = "https://www.googleapis.com/auth/youtube.force-ssl";
const AUTH_URL: &str = "https://accounts.google.com/o/oauth2/v2/auth";
const TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
const REVOKE_URL: &str = "https://oauth2.googleapis.com/revoke";
/// The credential store key for YouTube tokens.
pub const CREDENTIAL_KEY: &str = "youtube";

/// The app's Google OAuth client. Desktop client secrets aren't confidential, per Google.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Client {
    pub id: String,
    pub secret: String,
}

/// Stored tokens. Google access tokens last about an hour.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Token {
    pub access: String,
    pub refresh: String,
    /// When `access` expires, in Unix milliseconds.
    pub expires_at_ms: i64,
}

impl Token {
    /// Whether the access token expires within a minute.
    pub fn stale(&self) -> bool {
        rill_core::now_ms() > self.expires_at_ms - 60_000
    }
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    expires_in: i64,
}

/// Why an auth call failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthError {
    /// The refresh token is no longer valid; sign in again.
    Invalid,
    Temporary(String),
    Fatal(String),
}

impl AuthError {
    pub fn message(&self) -> String {
        match self {
            AuthError::Invalid => {
                "YouTube: your sign-in expired. Open Accounts and connect YouTube again.".into()
            }
            AuthError::Temporary(m) | AuthError::Fatal(m) => m.clone(),
        }
    }
}

fn network(e: reqwest::Error) -> AuthError {
    AuthError::Temporary(format!(
        "YouTube: can't reach Google ({e}). Check the internet connection."
    ))
}

/// A sign-in in progress: the page to open and the local listener waiting for Google's redirect.
pub struct Pending {
    pub url: String,
    listener: TcpListener,
    redirect_uri: String,
    verifier: String,
    state: String,
}

fn random_token(bytes: usize) -> Result<String, AuthError> {
    let mut buf = vec![0u8; bytes];
    getrandom::fill(&mut buf).map_err(|e| {
        AuthError::Fatal(format!(
            "YouTube: no secure random numbers available ({e})."
        ))
    })?;
    Ok(URL_SAFE_NO_PAD.encode(buf))
}

/// The PKCE challenge for a verifier (S256).
pub fn challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

/// Opens a loopback listener and builds the consent page URL.
pub async fn start(client: &Client) -> Result<Pending, AuthError> {
    let listener = TcpListener::bind("127.0.0.1:0").await.map_err(|e| {
        AuthError::Fatal(format!(
            "YouTube: can't listen on 127.0.0.1 for the sign-in redirect ({e}). Check firewall software."
        ))
    })?;
    let port = listener
        .local_addr()
        .map_err(|e| AuthError::Fatal(format!("YouTube: loopback port unknown ({e}).")))?
        .port();
    let redirect_uri = format!("http://127.0.0.1:{port}");
    let verifier = random_token(48)?;
    let state = random_token(16)?;
    let url = reqwest::Url::parse_with_params(
        AUTH_URL,
        &[
            ("client_id", client.id.as_str()),
            ("redirect_uri", redirect_uri.as_str()),
            ("response_type", "code"),
            ("scope", SCOPE),
            ("code_challenge", challenge(&verifier).as_str()),
            ("code_challenge_method", "S256"),
            ("access_type", "offline"),
            ("prompt", "consent"),
            ("state", state.as_str()),
        ],
    )
    .map_err(|e| AuthError::Fatal(format!("YouTube: bad sign-in URL ({e}).")))?
    .to_string();
    Ok(Pending {
        url,
        listener,
        redirect_uri,
        verifier,
        state,
    })
}

/// Waits for Google's redirect, then trades the code for tokens.
pub async fn finish(
    http: &reqwest::Client,
    client: &Client,
    pending: Pending,
) -> Result<Token, AuthError> {
    let code = loop {
        let (mut sock, _) = pending.listener.accept().await.map_err(|e| {
            AuthError::Temporary(format!("YouTube: the sign-in redirect failed ({e})."))
        })?;
        let mut buf = vec![0u8; 8192];
        let n = sock.read(&mut buf).await.unwrap_or(0);
        let request = String::from_utf8_lossy(&buf[..n]);
        let outcome = parse_redirect(&request, &pending.state);
        // The browser goes on to the website's page for how it ended.
        let reply = match &outcome {
            Some(r) => rill_core::sign_in_reply("youtube", r.is_ok()),
            None => "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                .to_string(),
        };
        let _ = sock.write_all(reply.as_bytes()).await;
        match outcome {
            Some(Ok(code)) => break code,
            Some(Err(why)) => return Err(AuthError::Fatal(why)),
            None => {}
        }
    };
    let res = http
        .post(TOKEN_URL)
        .form(&[
            ("client_id", client.id.as_str()),
            ("client_secret", client.secret.as_str()),
            ("code", code.as_str()),
            ("code_verifier", pending.verifier.as_str()),
            ("grant_type", "authorization_code"),
            ("redirect_uri", pending.redirect_uri.as_str()),
        ])
        .send()
        .await
        .map_err(network)?;
    if !res.status().is_success() {
        let status = res.status();
        let body = res.text().await.unwrap_or_default();
        return Err(AuthError::Fatal(format!(
            "YouTube: Google refused the sign-in ({status}: {}). Check youtube.client_id and client_secret in settings.json.",
            body.chars().take(200).collect::<String>()
        )));
    }
    let t: TokenResponse = res.json().await.map_err(network)?;
    let refresh = t.refresh_token.ok_or_else(|| {
        AuthError::Fatal("YouTube: Google sent no refresh token. Try connecting again.".into())
    })?;
    Ok(Token {
        access: t.access_token,
        refresh,
        expires_at_ms: rill_core::now_ms() + t.expires_in * 1000,
    })
}

/// Reads the code from Google's redirect request. `None` for unrelated requests (like a favicon).
pub fn parse_redirect(request: &str, state: &str) -> Option<Result<String, String>> {
    let target = request.lines().next()?.split_whitespace().nth(1)?;
    let query = target.strip_prefix("/?")?;
    let mut code = None;
    let mut got_state = None;
    let mut error = None;
    for pair in query.split('&') {
        let (k, v) = pair.split_once('=')?;
        let v = percent_decode(v);
        match k {
            "code" => code = Some(v),
            "state" => got_state = Some(v),
            "error" => error = Some(v),
            _ => {}
        }
    }
    if let Some(e) = error {
        return Some(Err(format!("YouTube: sign-in was declined ({e}).")));
    }
    if got_state.as_deref() != Some(state) {
        return Some(Err(
            "YouTube: the sign-in reply didn't match this request. Try again.".into(),
        ));
    }
    code.map(Ok)
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < bytes.len() => match u8::from_str_radix(&s[i + 1..i + 3], 16) {
                Ok(b) => {
                    out.push(b);
                    i += 2;
                }
                Err(_) => out.push(b'%'),
            },
            b => out.push(b),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Gets a fresh access token with the refresh token.
pub async fn refresh(
    http: &reqwest::Client,
    client: &Client,
    token: &Token,
) -> Result<Token, AuthError> {
    let res = http
        .post(TOKEN_URL)
        .form(&[
            ("client_id", client.id.as_str()),
            ("client_secret", client.secret.as_str()),
            ("grant_type", "refresh_token"),
            ("refresh_token", token.refresh.as_str()),
        ])
        .send()
        .await
        .map_err(network)?;
    match res.status().as_u16() {
        200 => {}
        400 | 401 => return Err(AuthError::Invalid),
        s => {
            return Err(AuthError::Temporary(format!(
                "YouTube: refreshing the sign-in failed ({s}). Rill will try again."
            )));
        }
    }
    let t: TokenResponse = res.json().await.map_err(network)?;
    Ok(Token {
        access: t.access_token,
        refresh: t.refresh_token.unwrap_or_else(|| token.refresh.clone()),
        expires_at_ms: rill_core::now_ms() + t.expires_in * 1000,
    })
}

/// Revokes the refresh token on sign-out; failures are only logged.
pub async fn revoke(http: &reqwest::Client, token: &Token) {
    if let Err(e) = http
        .post(REVOKE_URL)
        .form(&[("token", token.refresh.as_str())])
        .send()
        .await
    {
        tracing::warn!(err = %e, "YouTube token revoke failed");
    }
}

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

pub fn save_token(token: &Token) -> Result<(), String> {
    let raw = serde_json::to_string(token).map_err(|e| format!("YouTube: token not saved: {e}"))?;
    rill_os::credentials::save(CREDENTIAL_KEY, &raw)
}

pub fn delete_token() -> Result<(), String> {
    rill_os::credentials::delete(CREDENTIAL_KEY)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkce_challenge_matches_rfc_7636_example() {
        assert_eq!(
            challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn reads_the_redirect() {
        let req = "GET /?state=abc&code=4%2F0AX&scope=x HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n";
        assert_eq!(parse_redirect(req, "abc"), Some(Ok("4/0AX".into())));
        assert!(matches!(parse_redirect(req, "other"), Some(Err(_))));
        let denied = "GET /?error=access_denied&state=abc HTTP/1.1\r\n";
        assert!(matches!(parse_redirect(denied, "abc"), Some(Err(m)) if m.contains("declined")));
        assert_eq!(parse_redirect("GET /favicon.ico HTTP/1.1\r\n", "abc"), None);
    }
}
