//! Signing in to Slack: OAuth 2.0 with PKCE, as Slack offers it to apps that cannot keep a
//! client secret, such as a desktop app.
//!
//! 1. [`begin`] makes a random verifier and its SHA-256 challenge, listens on a loopback port
//!    and gives the URL of Slack's authorization page, asking for read scopes of a user token
//!    only ([`USER_SCOPES`]). The person opens it in their browser and allows Knode.
//! 2. Slack sends the browser back to `http://localhost:<port>/slack/callback` with a code and
//!    the `state` given in step 1. [`PendingSignIn::finish`] takes it, checks the `state`, and
//!    exchanges the code and the verifier for the token with `oauth.v2.access`, with no client
//!    secret. The token goes into the keychain and nowhere else; the code, the verifier and
//!    the token are never logged.
//!
//! With token rotation on, the token lasts 12 hours and comes with a refresh token, which
//! lasts 30 days for apps with PKCE; the connector refreshes it before it expires
//! ([`refresh`]), with no secret either.

use std::io::{self, Read, Write};
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr, TcpListener, TcpStream};
use std::time::{Duration, Instant};

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::api::{ApiError, OAuthAccess, SlackHttp, decode};
use crate::sources::{Secrets, SourceError};

/// Slack's authorization page.
pub const AUTHORIZE_URL: &str = "https://slack.com/oauth/v2/authorize";

/// The scopes of the user token: reading the conversations the person is in and the members of
/// the workspace. Nothing that writes.
pub const USER_SCOPES: [&str; 9] = [
    "channels:history",
    "groups:history",
    "im:history",
    "mpim:history",
    "channels:read",
    "groups:read",
    "im:read",
    "mpim:read",
    "users:read",
];

/// The loopback ports the redirect can use, tried in order: each must be a redirect URL of the
/// Slack app (`http://localhost:<port>/slack/callback`).
pub const REDIRECT_PORTS: [u16; 3] = [47821, 47822, 47823];

/// The path of the redirect.
pub const REDIRECT_PATH: &str = "/slack/callback";

/// The name of the secret that holds the token.
pub(crate) const TOKEN_SECRET: &str = "token";

/// The token and who it is for, as kept in the keychain.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Token {
    pub access_token: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<String>,
    /// When the access token expires, in seconds since 1970, with token rotation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<u64>,
    pub user_id: String,
    pub team_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub team_name: Option<String>,
}

impl std::fmt::Debug for Token {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The tokens never reach a log.
        f.debug_struct("Token")
            .field("user_id", &self.user_id)
            .field("team_id", &self.team_id)
            .finish_non_exhaustive()
    }
}

impl Token {
    /// The token kept in `secrets`, if any.
    pub fn load(secrets: &dyn Secrets) -> Result<Option<Token>, SourceError> {
        let Some(text) = secrets.get(TOKEN_SECRET)? else {
            return Ok(None);
        };
        serde_json::from_str(&text).map(Some).map_err(|_| {
            SourceError::SignIn("the Slack sign-in kept on this machine cannot be read".into())
        })
    }

    /// Keeps it in `secrets`.
    pub fn save(&self, secrets: &dyn Secrets) -> Result<(), SourceError> {
        let text = serde_json::to_string(self)
            .map_err(|e| SourceError::Failed(format!("the Slack sign-in: {e}")))?;
        secrets.set(TOKEN_SECRET, &text)?;
        Ok(())
    }
}

/// Who signed in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Account {
    /// The workspace id, `T...`.
    pub team_id: String,
    /// The workspace's name, when Slack gave it.
    pub team_name: Option<String>,
    /// The person's member id, `U...`.
    pub user_id: String,
}

/// A sign-in waiting for the browser to come back.
pub struct PendingSignIn {
    listeners: Vec<TcpListener>,
    client_id: String,
    redirect_uri: String,
    verifier: String,
    state: String,
    url: String,
}

impl std::fmt::Debug for PendingSignIn {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PendingSignIn")
            .field("redirect_uri", &self.redirect_uri)
            .finish_non_exhaustive()
    }
}

/// `count` random bytes, as URL-safe base64.
fn random(count: usize) -> Result<String, SourceError> {
    let mut bytes = vec![0u8; count];
    getrandom::fill(&mut bytes)
        .map_err(|e| SourceError::Failed(format!("no random bytes for signing in: {e}")))?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

/// The PKCE challenge of `verifier`: its SHA-256, as URL-safe base64 without padding.
pub(crate) fn challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

/// Starts signing in with the Slack app `client_id`, listening on the first of `ports` that is
/// free on the loopback addresses.
pub(crate) fn begin(client_id: &str, ports: &[u16]) -> Result<PendingSignIn, SourceError> {
    let mut chosen = None;
    for &port in ports {
        let v4 = TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, port)));
        let Ok(v4) = v4 else { continue };
        let port = v4.local_addr()?.port();
        let mut listeners = vec![v4];
        // Browsers may reach `localhost` over IPv6 first.
        if let Ok(v6) = TcpListener::bind(SocketAddr::from((Ipv6Addr::LOCALHOST, port))) {
            listeners.push(v6);
        }
        chosen = Some((port, listeners));
        break;
    }
    let (port, listeners) = chosen.ok_or_else(|| {
        SourceError::Failed(format!(
            "none of the ports for signing in to Slack is free ({})",
            ports
                .iter()
                .map(u16::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        ))
    })?;
    for listener in &listeners {
        listener.set_nonblocking(true)?;
    }
    let redirect_uri = format!("http://localhost:{port}{REDIRECT_PATH}");
    let verifier = random(32)?;
    let state = random(16)?;
    let url = format!(
        "{AUTHORIZE_URL}?client_id={}&scope=&user_scope={}&redirect_uri={}&state={}\
         &code_challenge={}&code_challenge_method=S256",
        encode(client_id),
        encode(&USER_SCOPES.join(",")),
        encode(&redirect_uri),
        encode(&state),
        encode(&challenge(&verifier)),
    );
    Ok(PendingSignIn {
        listeners,
        client_id: client_id.to_string(),
        redirect_uri,
        verifier,
        state,
        url,
    })
}

impl PendingSignIn {
    /// The page to open in the browser.
    pub fn url(&self) -> &str {
        &self.url
    }

    /// The redirect Slack sends the browser back to.
    pub fn redirect_uri(&self) -> &str {
        &self.redirect_uri
    }

    /// Waits up to `timeout` for the browser to come back, exchanges the code for the token
    /// and keeps it in `secrets`.
    pub(crate) fn finish(
        self,
        http: &dyn SlackHttp,
        secrets: &dyn Secrets,
        unix_now: Duration,
        timeout: Duration,
    ) -> Result<Account, SourceError> {
        let deadline = Instant::now() + timeout;
        loop {
            if Instant::now() >= deadline {
                return Err(SourceError::SignIn(
                    "Slack did not come back in time: sign in again".into(),
                ));
            }
            let mut stream = None;
            for listener in &self.listeners {
                match listener.accept() {
                    Ok((s, _)) => {
                        stream = Some(s);
                        break;
                    }
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
                    Err(e) => return Err(e.into()),
                }
            }
            let Some(mut stream) = stream else {
                std::thread::sleep(Duration::from_millis(50));
                continue;
            };
            let Some(query) = read_callback(&mut stream) else {
                respond(
                    &mut stream,
                    404,
                    "Not found",
                    "This is not the page Knode waits for.",
                );
                continue;
            };
            let param = |name: &str| {
                query
                    .iter()
                    .find(|(k, _)| k == name)
                    .map(|(_, v)| v.as_str())
            };
            // A callback without our `state` did not come from this sign-in: it is ignored.
            if param("state") != Some(self.state.as_str()) {
                respond(
                    &mut stream,
                    400,
                    "Not this sign-in",
                    "This page does not belong to the sign-in Knode started.",
                );
                continue;
            }
            if let Some(error) = param("error") {
                respond(
                    &mut stream,
                    200,
                    "Slack was not connected",
                    "Knode was not allowed to read Slack. You can close this tab.",
                );
                return Err(SourceError::SignIn(format!(
                    "Slack did not allow Knode ({})",
                    clean_code(error)
                )));
            }
            let Some(code) = param("code") else {
                respond(&mut stream, 400, "No code", "Slack sent no code.");
                continue;
            };
            let result = self.exchange(http, code, secrets, unix_now);
            match &result {
                Ok(account) => respond(
                    &mut stream,
                    200,
                    "Slack is connected",
                    &format!(
                        "Knode can now read {}. You can close this tab.",
                        html_escape(account.team_name.as_deref().unwrap_or("your workspace"))
                    ),
                ),
                Err(_) => respond(
                    &mut stream,
                    200,
                    "Slack was not connected",
                    "Knode could not finish signing in. Go back to Knode to try again.",
                ),
            }
            return result;
        }
    }

    fn exchange(
        &self,
        http: &dyn SlackHttp,
        code: &str,
        secrets: &dyn Secrets,
        unix_now: Duration,
    ) -> Result<Account, SourceError> {
        let params = [
            ("client_id", self.client_id.clone()),
            ("code", code.to_string()),
            ("code_verifier", self.verifier.clone()),
            ("redirect_uri", self.redirect_uri.clone()),
            ("grant_type", "authorization_code".to_string()),
        ];
        let response = http.post("oauth.v2.access", None, &params)?;
        let access: OAuthAccess = decode("oauth.v2.access", &response).map_err(signin_error)?;
        let user = access
            .authed_user
            .ok_or_else(|| SourceError::SignIn("Slack sent no user token".into()))?;
        let (Some(access_token), Some(user_id)) = (user.access_token, user.id) else {
            return Err(SourceError::SignIn("Slack sent no user token".into()));
        };
        let team = access.team.unwrap_or(super::api::Team {
            id: None,
            name: None,
        });
        let team_id = team
            .id
            .ok_or_else(|| SourceError::SignIn("Slack named no workspace".into()))?;
        let token = Token {
            access_token,
            refresh_token: user.refresh_token,
            expires_at: user.expires_in.map(|s| unix_now.as_secs() + s),
            user_id: user_id.clone(),
            team_id: team_id.clone(),
            team_name: team.name.clone(),
        };
        token.save(secrets)?;
        Ok(Account {
            team_id,
            team_name: team.name,
            user_id,
        })
    }
}

/// A refreshed token, for a token that expires within five minutes and can be refreshed;
/// `None` when it need not be.
pub(crate) fn refresh(
    http: &dyn SlackHttp,
    client_id: &str,
    token: &Token,
    unix_now: Duration,
) -> Result<Option<Token>, SourceError> {
    let (Some(expires_at), Some(refresh_token)) = (token.expires_at, &token.refresh_token) else {
        return Ok(None);
    };
    if expires_at > unix_now.as_secs() + 300 {
        return Ok(None);
    }
    let params = [
        ("client_id", client_id.to_string()),
        ("grant_type", "refresh_token".to_string()),
        ("refresh_token", refresh_token.clone()),
    ];
    let response = http.post("oauth.v2.access", None, &params)?;
    let access: OAuthAccess = decode("oauth.v2.access", &response).map_err(signin_error)?;
    let user = access.authed_user;
    let from_user = |f: fn(&super::api::AuthedUser) -> Option<String>| user.as_ref().and_then(f);
    let access_token = from_user(|u| u.access_token.clone())
        .or(access
            .access_token
            .filter(|_| access.token_type.as_deref().is_none_or(|t| t == "user")))
        .ok_or_else(|| SourceError::SignIn("Slack did not refresh the sign-in".into()))?;
    let refresh_token = from_user(|u| u.refresh_token.clone()).or(access.refresh_token.clone());
    let expires_in = user
        .as_ref()
        .and_then(|u| u.expires_in)
        .or(access.expires_in);
    Ok(Some(Token {
        access_token,
        refresh_token: refresh_token.or_else(|| token.refresh_token.clone()),
        expires_at: expires_in.map(|s| unix_now.as_secs() + s),
        ..token.clone()
    }))
}

fn signin_error(error: ApiError) -> SourceError {
    match error {
        ApiError::SignIn(what) => SourceError::SignIn(what),
        ApiError::RateLimited(_) => {
            SourceError::Failed("Slack asks to slow down: try again in a minute".into())
        }
        ApiError::Gone => SourceError::SignIn("Slack refused the sign-in".into()),
        ApiError::Failed(what)
            if what.contains("invalid_code") || what.contains("bad_redirect") =>
        {
            SourceError::SignIn(what)
        }
        ApiError::Failed(what) => SourceError::Failed(what),
    }
}

/// An error code from a URL, kept to the characters codes have.
fn clean_code(code: &str) -> String {
    code.chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '_')
        .take(64)
        .collect()
}

/// The query of a `GET` of [`REDIRECT_PATH`], or `None` for any other request.
fn read_callback(stream: &mut TcpStream) -> Option<Vec<(String, String)>> {
    stream.set_nonblocking(false).ok()?;
    stream.set_read_timeout(Some(Duration::from_secs(5))).ok()?;
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 2048];
    while !buffer.windows(4).any(|w| w == b"\r\n\r\n") && buffer.len() < 16 * 1024 {
        let n = stream.read(&mut chunk).ok()?;
        if n == 0 {
            break;
        }
        buffer.extend_from_slice(&chunk[..n]);
    }
    let text = String::from_utf8_lossy(&buffer);
    let line = text.lines().next()?;
    let mut parts = line.split(' ');
    if parts.next()? != "GET" {
        return None;
    }
    let target = parts.next()?;
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    if path != REDIRECT_PATH {
        return None;
    }
    Some(
        query
            .split('&')
            .filter(|p| !p.is_empty())
            .map(|p| {
                let (k, v) = p.split_once('=').unwrap_or((p, ""));
                (decode_component(k), decode_component(v))
            })
            .collect(),
    )
}

fn respond(stream: &mut TcpStream, status: u16, title: &str, text: &str) {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        _ => "Not Found",
    };
    let body = format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><title>{title}</title></head>\
         <body style=\"font-family: system-ui, sans-serif; margin: 4rem auto; max-width: 32rem\">\
         <h1>{title}</h1><p>{text}</p></body></html>"
    );
    let _ = write!(
        stream,
        "HTTP/1.1 {status} {reason}\r\nContent-Type: text/html; charset=utf-8\r\n\
         Content-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.flush();
}

fn html_escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// `text` percent-encoded for a URL query.
pub(crate) fn encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char);
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// A percent-encoded query component, with `+` as a space.
fn decode_component(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < bytes.len() => {
                let hex = |b: u8| (b as char).to_digit(16);
                match (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                    (Some(high), Some(low)) => {
                        out.push((high * 16 + low) as u8);
                        i += 2;
                    }
                    _ => out.push(b'%'),
                }
            }
            b => out.push(b),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_challenge_is_rfc_7636s() {
        // The example of RFC 7636, appendix B.
        assert_eq!(
            challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn encodes_and_decodes_query_components() {
        assert_eq!(encode("a b/c:d,e"), "a%20b%2Fc%3Ad%2Ce");
        assert_eq!(decode_component("a%20b+c%2F"), "a b c/");
        assert_eq!(decode_component("bad%2"), "bad%2");
        assert_eq!(decode_component("%zz"), "%zz");
    }

    #[test]
    fn error_codes_are_cleaned() {
        assert_eq!(clean_code("access_denied<script>"), "access_deniedscript");
    }
}
