//! Spotify OAuth (authorization code + PKCE) for the iPad.
//!
//! Uses the same public client and loopback redirect as librespot's own examples, so the
//! resulting token can log a librespot `Session` in. No client secret and no password: the user
//! signs in on Spotify's page in an `ASWebAuthenticationSession`, and the redirect to
//! `http://127.0.0.1:8898/login` is answered by a one-shot listener inside the app.
use std::time::Duration;

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use rand::RngCore;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

/// librespot's "keymaster" client id (`librespot-core` `config::KEYMASTER_CLIENT_ID`).
pub const CLIENT_ID: &str = "65b708073fc0480ea92a077233ca87bd";
pub const REDIRECT_PORT: u16 = 8898;
pub const REDIRECT_URI: &str = "http://127.0.0.1:8898/login";
pub const SCOPES: &str = "streaming";
const AUTHORIZE_URL: &str = "https://accounts.spotify.com/authorize";
const TOKEN_URL: &str = "https://accounts.spotify.com/api/token";
/// How long the sign-in page may stay open before the listener gives up.
const SIGN_IN_TIMEOUT: Duration = Duration::from_secs(300);

/// One sign-in attempt: the PKCE verifier and the anti-CSRF `state`.
#[derive(Clone, Debug)]
pub struct Pkce {
    pub verifier: String,
    pub challenge: String,
    pub state: String,
}

fn random_token(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    rand::thread_rng().fill_bytes(&mut buf);
    URL_SAFE_NO_PAD.encode(buf)
}

impl Pkce {
    pub fn new() -> Self {
        Self::from_verifier(random_token(48), random_token(24))
    }

    /// RFC 7636 S256 challenge for `verifier`.
    pub fn from_verifier(verifier: String, state: String) -> Self {
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
        Self { verifier, challenge, state }
    }

    pub fn authorize_url(&self) -> String {
        let mut url = url::Url::parse(AUTHORIZE_URL).expect("static URL");
        url.query_pairs_mut()
            .append_pair("response_type", "code")
            .append_pair("client_id", CLIENT_ID)
            .append_pair("redirect_uri", REDIRECT_URI)
            .append_pair("scope", SCOPES)
            .append_pair("code_challenge_method", "S256")
            .append_pair("code_challenge", &self.challenge)
            .append_pair("state", &self.state);
        url.into()
    }
}

/// Why sign-in did not produce a token.
#[derive(Debug, PartialEq)]
pub enum AuthError {
    /// Spotify sent the user back with `error=...` (e.g. `access_denied`).
    Denied(String),
    /// The callback's `state` did not match this attempt.
    StateMismatch,
    /// The callback had no `code`.
    MissingCode,
    /// The listener could not start or read the callback.
    Listener(String),
    /// The user did not finish in time.
    TimedOut,
    /// The token request failed.
    Exchange(String),
}

impl std::fmt::Display for AuthError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AuthError::Denied(e) if e == "access_denied" => f.write_str("Sign-in was cancelled."),
            AuthError::Denied(e) => write!(f, "Spotify refused sign-in ({e})."),
            AuthError::StateMismatch => f.write_str("Sign-in check failed. Please try again."),
            AuthError::MissingCode => f.write_str("Spotify did not return a sign-in code."),
            AuthError::Listener(e) => write!(f, "Sign-in could not start ({e})."),
            AuthError::TimedOut => f.write_str("Sign-in timed out."),
            AuthError::Exchange(e) => write!(f, "Sign-in failed ({e})."),
        }
    }
}

/// Extracts the authorization code from the request target of the redirect
/// (`/login?code=...&state=...`), checking `state`.
pub fn parse_callback(target: &str, expected_state: &str) -> Result<String, AuthError> {
    let url = url::Url::parse(&format!("http://127.0.0.1{target}"))
        .map_err(|_| AuthError::MissingCode)?;
    let get = |key: &str| {
        url.query_pairs()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.into_owned())
    };
    if let Some(error) = get("error") {
        return Err(AuthError::Denied(error));
    }
    if get("state").as_deref() != Some(expected_state) {
        return Err(AuthError::StateMismatch);
    }
    get("code").filter(|c| !c.is_empty()).ok_or(AuthError::MissingCode)
}

const DONE_PAGE: &str = "<!doctype html><meta name=viewport content='width=device-width'>\
<body style='font:20px -apple-system;background:#0b0b0d;color:#fff;text-align:center;padding-top:30vh'>\
<p>Signed in to Spotify.</p><p style='color:#999'>You can return to ihomeview.</p>";

/// Binds the loopback redirect listener. Call before opening the sign-in page.
pub async fn bind_listener() -> Result<TcpListener, AuthError> {
    TcpListener::bind(("127.0.0.1", REDIRECT_PORT))
        .await
        .map_err(|e| AuthError::Listener(e.to_string()))
}

/// Waits for the browser's redirect, replies with a short page and returns the code.
/// Ignores unrelated requests (such as `/favicon.ico`).
pub async fn wait_for_code(listener: TcpListener, expected_state: &str) -> Result<String, AuthError> {
    let accept = async {
        loop {
            let (mut stream, _) = listener
                .accept()
                .await
                .map_err(|e| AuthError::Listener(e.to_string()))?;
            let mut buf = vec![0u8; 8192];
            let n = stream
                .read(&mut buf)
                .await
                .map_err(|e| AuthError::Listener(e.to_string()))?;
            let request = String::from_utf8_lossy(&buf[..n]);
            let target = request
                .lines()
                .next()
                .and_then(|line| line.split_whitespace().nth(1))
                .unwrap_or("")
                .to_owned();
            if !target.starts_with("/login") {
                let _ = stream
                    .write_all(b"HTTP/1.1 404 Not Found\r\ncontent-length: 0\r\nconnection: close\r\n\r\n")
                    .await;
                continue;
            }
            let result = parse_callback(&target, expected_state);
            let body = if result.is_ok() { DONE_PAGE } else { "Sign-in failed. Return to ihomeview." };
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: text/html; charset=utf-8\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(response.as_bytes()).await;
            let _ = stream.shutdown().await;
            return result;
        }
    };
    tokio::time::timeout(SIGN_IN_TIMEOUT, accept)
        .await
        .unwrap_or(Err(AuthError::TimedOut))
}

/// Tokens from Spotify's token endpoint.
#[derive(Clone, Debug, Deserialize)]
pub struct Tokens {
    pub access_token: String,
    #[serde(default)]
    pub refresh_token: Option<String>,
    #[serde(default = "default_expiry")]
    pub expires_in: u64,
}

fn default_expiry() -> u64 {
    3600
}

async fn token_request(form: &[(&str, &str)]) -> Result<Tokens, AuthError> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(20))
        .build()
        .map_err(|e| AuthError::Exchange(e.to_string()))?;
    let response = client
        .post(TOKEN_URL)
        .form(form)
        .send()
        .await
        .map_err(|e| {
            let e = e.without_url();
            crate::debug_log::error(format!("token request: {e:?}"));
            AuthError::Exchange(e.to_string())
        })?;
    let status = response.status();
    if !status.is_success() {
        crate::debug_log::error(format!("token request: HTTP {}", status.as_u16()));
        // Never include the body: it may echo request data.
        return Err(AuthError::Exchange(format!("HTTP {}", status.as_u16())));
    }
    response
        .json::<Tokens>()
        .await
        .map_err(|_| AuthError::Exchange("unexpected response".into()))
}

/// Exchanges an authorization code for tokens.
pub async fn exchange_code(code: &str, pkce: &Pkce) -> Result<Tokens, AuthError> {
    token_request(&[
        ("grant_type", "authorization_code"),
        ("code", code),
        ("redirect_uri", REDIRECT_URI),
        ("client_id", CLIENT_ID),
        ("code_verifier", &pkce.verifier),
    ])
    .await
}

/// Gets a fresh access token from a stored refresh token.
pub async fn refresh(refresh_token: &str) -> Result<Tokens, AuthError> {
    token_request(&[
        ("grant_type", "refresh_token"),
        ("refresh_token", refresh_token),
        ("client_id", CLIENT_ID),
    ])
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkce_challenge_matches_rfc7636_example() {
        // RFC 7636 appendix B.
        let pkce = Pkce::from_verifier(
            "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk".into(),
            "s".into(),
        );
        assert_eq!(pkce.challenge, "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM");
    }

    #[test]
    fn random_pkce_values_are_long_and_unique() {
        let a = Pkce::new();
        let b = Pkce::new();
        assert!(a.verifier.len() >= 43 && a.verifier.len() <= 128);
        assert_ne!(a.verifier, b.verifier);
        assert_ne!(a.state, b.state);
    }

    #[test]
    fn authorize_url_has_all_parameters() {
        let pkce = Pkce::from_verifier("v".repeat(50), "st4te".into());
        let url = url::Url::parse(&pkce.authorize_url()).unwrap();
        let q: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
        assert_eq!(url.host_str(), Some("accounts.spotify.com"));
        assert_eq!(q["response_type"], "code");
        assert_eq!(q["client_id"], CLIENT_ID);
        assert_eq!(q["redirect_uri"], REDIRECT_URI);
        assert_eq!(q["scope"], SCOPES);
        assert_eq!(q["code_challenge_method"], "S256");
        assert_eq!(q["code_challenge"], pkce.challenge);
        assert_eq!(q["state"], "st4te");
    }

    #[test]
    fn callback_parsing() {
        assert_eq!(parse_callback("/login?code=abc&state=s1", "s1"), Ok("abc".into()));
        assert_eq!(
            parse_callback("/login?code=abc&state=evil", "s1"),
            Err(AuthError::StateMismatch)
        );
        assert_eq!(parse_callback("/login?state=s1", "s1"), Err(AuthError::MissingCode));
        assert_eq!(
            parse_callback("/login?error=access_denied&state=s1", "s1"),
            Err(AuthError::Denied("access_denied".into()))
        );
    }

    #[tokio::test]
    async fn listener_returns_code_and_skips_other_requests() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move { wait_for_code(listener, "s9").await });
        for path in ["/favicon.ico", "/login?code=xyz&state=s9"] {
            let mut s = tokio::net::TcpStream::connect(("127.0.0.1", port)).await.unwrap();
            s.write_all(format!("GET {path} HTTP/1.1\r\nhost: x\r\n\r\n").as_bytes())
                .await
                .unwrap();
            let mut reply = String::new();
            s.read_to_string(&mut reply).await.unwrap();
            assert!(reply.starts_with("HTTP/1.1"));
        }
        assert_eq!(server.await.unwrap(), Ok("xyz".into()));
    }
}
