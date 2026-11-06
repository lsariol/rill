//! Kick sign-in: browser consent with PKCE and a fixed loopback redirect, then a token exchange
//! either directly (with the app's client secret) or through Rill's sign-in helper.

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

const AUTH_URL: &str = "https://id.kick.com/oauth/authorize";
pub const TOKEN_URL: &str = "https://id.kick.com/oauth/token";
const REVOKE_URL: &str = "https://id.kick.com/oauth/revoke";
/// Kick needs the exact redirect registered with the app, so the port is fixed.
pub const REDIRECT_PORT: u16 = 47831;
pub const REDIRECT_URI: &str = "http://localhost:47831/callback";
/// Send chat, read the signed-in user, delete messages, ban and time out.
pub const SCOPES: &str = "user:read channel:read channel:write chat:write moderation:ban moderation:chat_message:manage ads:read ads:write";
/// The credential store key for Kick tokens.
pub const CREDENTIAL_KEY: &str = "kick";

/// How Rill trades a sign-in code (or a refresh token) for tokens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Exchange {
    /// With the app's client secret, for development builds.
    Direct { client_id: String, secret: String },
    /// Through Rill's sign-in helper, which adds the secret: `<url>/v1/kick/token`.
    Helper { client_id: String, url: String },
}

impl Exchange {
    pub fn client_id(&self) -> &str {
        match self {
            Exchange::Direct { client_id, .. } | Exchange::Helper { client_id, .. } => client_id,
        }
    }

    /// The token endpoint and the form fields that identify the app there.
    fn endpoint(&self, token_url: &str) -> (String, Vec<(&'static str, String)>) {
        match self {
            Exchange::Direct { client_id, secret } => (
                token_url.to_string(),
                vec![
                    ("client_id", client_id.clone()),
                    ("client_secret", secret.clone()),
                ],
            ),
            Exchange::Helper { client_id, url } => (
                format!("{}/v1/kick/token", url.trim_end_matches('/')),
                vec![("client_id", client_id.clone())],
            ),
        }
    }
}

/// Stored tokens.
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
    /// Seconds.
    #[serde(default)]
    expires_in: Option<i64>,
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
                "Kick: your sign-in expired. Open Accounts and connect Kick again.".into()
            }
            AuthError::Temporary(m) | AuthError::Fatal(m) => m.clone(),
        }
    }
}

fn network(e: reqwest::Error) -> AuthError {
    AuthError::Temporary(format!(
        "Kick: can't reach Kick's sign-in ({e}). Check the internet connection."
    ))
}

/// A sign-in in progress: the page to open and the listeners waiting for Kick's redirect.
pub struct Pending {
    pub url: String,
    listeners: Vec<TcpListener>,
    verifier: String,
    state: String,
}

fn random_token(bytes: usize) -> Result<String, AuthError> {
    let mut buf = vec![0u8; bytes];
    getrandom::fill(&mut buf).map_err(|e| {
        AuthError::Fatal(format!("Kick: no secure random numbers available ({e})."))
    })?;
    Ok(URL_SAFE_NO_PAD.encode(buf))
}

/// The PKCE challenge for a verifier (S256).
pub fn challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

/// Listens on the redirect port (IPv4 and, where possible, IPv6) and builds the consent URL.
pub async fn start(exchange: &Exchange) -> Result<Pending, AuthError> {
    let mut listeners = Vec::new();
    for addr in [
        format!("127.0.0.1:{REDIRECT_PORT}"),
        format!("[::1]:{REDIRECT_PORT}"),
    ] {
        if let Ok(l) = TcpListener::bind(&addr).await {
            listeners.push(l);
        }
    }
    if listeners.is_empty() {
        return Err(AuthError::Fatal(format!(
            "Kick: port {REDIRECT_PORT} is busy, so Rill can't receive the sign-in. Close the program using it (or another Rill) and try again."
        )));
    }
    let verifier = random_token(48)?;
    let state = random_token(16)?;
    let url = reqwest::Url::parse_with_params(
        AUTH_URL,
        &[
            ("response_type", "code"),
            ("client_id", exchange.client_id()),
            ("redirect_uri", REDIRECT_URI),
            ("scope", SCOPES),
            ("state", state.as_str()),
            ("code_challenge", challenge(&verifier).as_str()),
            ("code_challenge_method", "S256"),
        ],
    )
    .map_err(|e| AuthError::Fatal(format!("Kick: bad sign-in URL ({e}).")))?
    .to_string();
    Ok(Pending {
        url,
        listeners,
        verifier,
        state,
    })
}

async fn accept(listeners: &[TcpListener]) -> std::io::Result<TcpStream> {
    match listeners {
        [one] => one.accept().await.map(|(s, _)| s),
        [a, b, ..] => tokio::select! {
            r = a.accept() => r.map(|(s, _)| s),
            r = b.accept() => r.map(|(s, _)| s),
        },
        [] => std::future::pending().await,
    }
}

/// The page the browser shows after the redirect.
fn page(ok: bool) -> String {
    let (title, text) = if ok {
        (
            "Signed in to Kick",
            "Rill can now send and moderate on Kick. You can close this tab.",
        )
    } else {
        (
            "Kick sign-in didn't finish",
            "Go back to Rill and connect Kick again.",
        )
    };
    format!(
        "<!doctype html><meta charset=utf-8><title>Rill</title>\
         <body style=\"font:16px system-ui;background:#18181b;color:#e4e4e7;display:grid;place-items:center;height:90vh\">\
         <div><h2 style=\"color:#53fc18\">{title}</h2><p>{text}</p></div>"
    )
}

/// Waits for Kick's redirect, then trades the code for tokens.
pub async fn finish(
    http: &reqwest::Client,
    exchange: &Exchange,
    token_url: &str,
    pending: Pending,
) -> Result<Token, AuthError> {
    let code = loop {
        let mut sock = accept(&pending.listeners).await.map_err(|e| {
            AuthError::Temporary(format!("Kick: the sign-in redirect failed ({e})."))
        })?;
        let mut buf = vec![0u8; 8192];
        let n = sock.read(&mut buf).await.unwrap_or(0);
        let request = String::from_utf8_lossy(&buf[..n]);
        let outcome = parse_redirect(&request, &pending.state);
        let body = match &outcome {
            Some(r) => page(r.is_ok()),
            None => String::new(),
        };
        let reply = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let _ = sock.write_all(reply.as_bytes()).await;
        match outcome {
            Some(Ok(code)) => break code,
            Some(Err(why)) => return Err(AuthError::Fatal(why)),
            None => {}
        }
    };
    let (url, mut form) = exchange.endpoint(token_url);
    form.extend([
        ("grant_type", "authorization_code".to_string()),
        ("code", code),
        ("redirect_uri", REDIRECT_URI.to_string()),
        ("code_verifier", pending.verifier),
    ]);
    let res = http.post(&url).form(&form).send().await.map_err(network)?;
    if !res.status().is_success() {
        let status = res.status();
        let body = res.text().await.unwrap_or_default();
        return Err(AuthError::Fatal(format!(
            "Kick: Kick refused the sign-in ({status}: {}). Check kick.client_id, the client secret, and that the app's redirect URL is {REDIRECT_URI}.",
            body.chars().take(200).collect::<String>()
        )));
    }
    let t: TokenResponse = res.json().await.map_err(network)?;
    let refresh = t.refresh_token.ok_or_else(|| {
        AuthError::Fatal("Kick: Kick sent no refresh token. Try connecting again.".into())
    })?;
    Ok(Token {
        access: t.access_token,
        refresh,
        expires_at_ms: expiry(t.expires_in),
    })
}

/// When a token expires; an hour when Kick doesn't say.
fn expiry(expires_in: Option<i64>) -> i64 {
    rill_core::now_ms() + expires_in.unwrap_or(3600).max(60) * 1000
}

/// Reads the code from Kick's redirect request. `None` for unrelated requests (like a favicon).
pub fn parse_redirect(request: &str, state: &str) -> Option<Result<String, String>> {
    let target = request.lines().next()?.split_whitespace().nth(1)?;
    let query = target.strip_prefix("/callback?")?;
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
        return Some(Err(format!("Kick: sign-in was declined ({e}).")));
    }
    if got_state.as_deref() != Some(state) {
        return Some(Err(
            "Kick: the sign-in reply didn't match this request. Try again.".into(),
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
    exchange: &Exchange,
    token_url: &str,
    token: &Token,
) -> Result<Token, AuthError> {
    let (url, mut form) = exchange.endpoint(token_url);
    form.extend([
        ("grant_type", "refresh_token".to_string()),
        ("refresh_token", token.refresh.clone()),
    ]);
    let res = http.post(&url).form(&form).send().await.map_err(network)?;
    match res.status().as_u16() {
        200 => {}
        400 | 401 => return Err(AuthError::Invalid),
        s => {
            return Err(AuthError::Temporary(format!(
                "Kick: refreshing the sign-in failed ({s}). Rill will try again."
            )));
        }
    }
    let t: TokenResponse = res.json().await.map_err(network)?;
    Ok(Token {
        access: t.access_token,
        refresh: t.refresh_token.unwrap_or_else(|| token.refresh.clone()),
        expires_at_ms: expiry(t.expires_in),
    })
}

/// Revokes the refresh token on sign-out; failures are only logged.
pub async fn revoke(http: &reqwest::Client, token: &Token) {
    let form = [
        ("token", token.refresh.as_str()),
        ("token_hint_type", "refresh_token"),
    ];
    if let Err(e) = http.post(REVOKE_URL).form(&form).send().await {
        tracing::warn!(err = %e, "Kick token revoke failed");
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
    let raw = serde_json::to_string(token).map_err(|e| format!("Kick: token not saved: {e}"))?;
    rill_os::credentials::save(CREDENTIAL_KEY, &raw)
}

pub fn delete_token() -> Result<(), String> {
    rill_os::credentials::delete(CREDENTIAL_KEY)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_redirect() {
        let req = "GET /callback?code=a%2Fb&state=xyz HTTP/1.1\r\nHost: localhost\r\n\r\n";
        assert_eq!(parse_redirect(req, "xyz"), Some(Ok("a/b".into())));
        assert!(matches!(parse_redirect(req, "other"), Some(Err(_))));
        let denied = "GET /callback?error=access_denied&state=xyz HTTP/1.1\r\n";
        assert!(matches!(parse_redirect(denied, "xyz"), Some(Err(m)) if m.contains("declined")));
        assert_eq!(parse_redirect("GET /favicon.ico HTTP/1.1\r\n", "xyz"), None);
    }

    #[test]
    fn the_helper_never_gets_a_secret_and_direct_always_does() {
        let direct = Exchange::Direct {
            client_id: "id".into(),
            secret: "s".into(),
        };
        let (url, form) = direct.endpoint(TOKEN_URL);
        assert_eq!(url, TOKEN_URL);
        assert!(form.iter().any(|(k, _)| *k == "client_secret"));
        let helper = Exchange::Helper {
            client_id: "id".into(),
            url: "https://auth.example.com/".into(),
        };
        let (url, form) = helper.endpoint(TOKEN_URL);
        assert_eq!(url, "https://auth.example.com/v1/kick/token");
        assert!(form.iter().all(|(k, _)| *k != "client_secret"));
    }
}
