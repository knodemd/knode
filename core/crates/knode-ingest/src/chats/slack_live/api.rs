//! The Slack Web API as the connector uses it: a few read methods and the OAuth exchange, over
//! a small HTTP trait ([`SlackHttp`]) so tests answer without a network, and a clock
//! ([`Clock`]) so they wait without time passing.

use std::io::{self, Read};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::Deserialize;
use serde::de::DeserializeOwned;

use super::super::slack::{Message, User};

/// Where the methods of the Web API are.
pub const API_BASE: &str = "https://slack.com/api/";

/// The most bytes read from one answer.
const MAX_ANSWER: u64 = 32 * 1024 * 1024;

/// How long one call may take, from connecting to the last byte.
const CALL_TIMEOUT: Duration = Duration::from_secs(20);

/// What an HTTP call answered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpResponse {
    /// Its status code.
    pub status: u16,
    /// Its `Retry-After` header, in seconds, when it has one.
    pub retry_after: Option<u64>,
    /// Its body.
    pub body: Vec<u8>,
}

impl HttpResponse {
    /// A `200 OK` with this JSON body.
    pub fn ok(body: impl Into<Vec<u8>>) -> Self {
        HttpResponse {
            status: 200,
            retry_after: None,
            body: body.into(),
        }
    }

    /// A `429 Too Many Requests` asking to wait `seconds`.
    pub fn rate_limited(seconds: u64) -> Self {
        HttpResponse {
            status: 429,
            retry_after: Some(seconds),
            body: br#"{"ok":false,"error":"ratelimited"}"#.to_vec(),
        }
    }
}

/// Calls methods of the Slack Web API: a `POST` of a form to `https://slack.com/api/<method>`.
pub trait SlackHttp: Send + Sync {
    /// Calls `method` (such as `conversations.history`) with the form `params`, with `token`
    /// as its bearer token when given. An implementation never logs the token or the params,
    /// which hold the OAuth code and verifier when signing in.
    fn post(
        &self,
        method: &str,
        token: Option<&str>,
        params: &[(&str, String)],
    ) -> io::Result<HttpResponse>;
}

/// [`SlackHttp`] over HTTPS, with a timeout on every call.
pub struct UreqHttp {
    agent: ureq::Agent,
    base: String,
}

impl UreqHttp {
    /// Calls to `https://slack.com/api/`.
    pub fn new() -> Self {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(CALL_TIMEOUT))
            .user_agent(concat!("Knode/", env!("CARGO_PKG_VERSION")))
            .http_status_as_error(false)
            .build()
            .into();
        UreqHttp {
            agent,
            base: API_BASE.to_string(),
        }
    }
}

impl Default for UreqHttp {
    fn default() -> Self {
        UreqHttp::new()
    }
}

impl SlackHttp for UreqHttp {
    fn post(
        &self,
        method: &str,
        token: Option<&str>,
        params: &[(&str, String)],
    ) -> io::Result<HttpResponse> {
        let url = format!("{}{method}", self.base);
        let mut request = self.agent.post(&url);
        if let Some(token) = token {
            request = request.header("Authorization", &format!("Bearer {token}"));
        }
        // ureq's errors name the URL, never the headers or the form, so secrets stay out.
        let response = request
            .send_form(params.iter().map(|(k, v)| (*k, v.as_str())))
            .map_err(|e| io::Error::other(format!("Slack could not be reached: {e}")))?;
        let status = response.status().as_u16();
        let retry_after = response
            .headers()
            .get("retry-after")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.trim().parse::<u64>().ok());
        let mut body = Vec::new();
        response
            .into_body()
            .into_reader()
            .take(MAX_ANSWER)
            .read_to_end(&mut body)
            .map_err(|e| io::Error::other(format!("Slack's answer could not be read: {e}")))?;
        Ok(HttpResponse {
            status,
            retry_after,
            body,
        })
    }
}

/// The time, for the connector: how long it has been running, to space calls, and the time of
/// day. Tests use one that moves only when told.
pub trait Clock: Send + Sync {
    /// The time since some fixed moment; it never goes back.
    fn elapsed(&self) -> Duration;
    /// Waits.
    fn sleep(&self, duration: Duration);
    /// The time since 1970.
    fn unix(&self) -> Duration;
}

/// The clock of the system.
pub struct SystemClock {
    start: Instant,
}

impl SystemClock {
    /// The clock, starting now.
    pub fn new() -> Self {
        SystemClock {
            start: Instant::now(),
        }
    }
}

impl Default for SystemClock {
    fn default() -> Self {
        SystemClock::new()
    }
}

impl Clock for SystemClock {
    fn elapsed(&self) -> Duration {
        self.start.elapsed()
    }

    fn sleep(&self, duration: Duration) {
        std::thread::sleep(duration);
    }

    fn unix(&self) -> Duration {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
    }
}

/// Why a call gave nothing.
#[derive(Debug)]
pub(crate) enum ApiError {
    /// Slack asks to wait this long before calling the method again.
    RateLimited(Duration),
    /// The token was refused: sign in again.
    SignIn(String),
    /// The conversation or thread is gone, or the person left it.
    Gone,
    /// Something else, such as the network: try again later.
    Failed(String),
}

#[derive(Deserialize)]
struct Envelope {
    #[serde(default)]
    ok: bool,
    #[serde(default)]
    error: Option<String>,
}

/// The answer of `method`, read as `T`.
pub(crate) fn decode<T: DeserializeOwned>(
    method: &str,
    response: &HttpResponse,
) -> Result<T, ApiError> {
    if response.status == 429 {
        return Err(ApiError::RateLimited(Duration::from_secs(
            response.retry_after.unwrap_or(60).clamp(1, 3600),
        )));
    }
    if response.status >= 500 {
        return Err(ApiError::Failed(format!(
            "Slack answered {method} with HTTP {}",
            response.status
        )));
    }
    let envelope: Envelope = serde_json::from_slice(&response.body).map_err(|_| {
        ApiError::Failed(format!(
            "Slack answered {method} with HTTP {} and no JSON",
            response.status
        ))
    })?;
    if !envelope.ok {
        let code = envelope.error.unwrap_or_else(|| "unknown_error".into());
        return Err(match code.as_str() {
            "ratelimited" => ApiError::RateLimited(Duration::from_secs(
                response.retry_after.unwrap_or(60).clamp(1, 3600),
            )),
            "invalid_auth"
            | "not_authed"
            | "token_revoked"
            | "token_expired"
            | "account_inactive"
            | "missing_scope"
            | "no_permission"
            | "invalid_refresh_token"
            | "team_access_not_granted"
            | "ekm_access_denied"
            | "two_factor_setup_required" => {
                ApiError::SignIn(format!("Slack refused the sign-in ({code})"))
            }
            "channel_not_found" | "not_in_channel" | "thread_not_found" | "is_archived" => {
                ApiError::Gone
            }
            _ => ApiError::Failed(format!("Slack refused {method}: {code}")),
        });
    }
    serde_json::from_slice(&response.body)
        .map_err(|e| ApiError::Failed(format!("Slack sent an unexpected answer to {method}: {e}")))
}

/// `response_metadata` of a page.
#[derive(Debug, Default, Deserialize)]
pub(crate) struct Metadata {
    #[serde(default)]
    pub next_cursor: Option<String>,
}

/// The cursor of the next page, when there is one.
pub(crate) fn next_cursor(metadata: &Option<Metadata>) -> Option<String> {
    metadata
        .as_ref()
        .and_then(|m| m.next_cursor.clone())
        .filter(|c| !c.is_empty())
}

/// A conversation as `conversations.list` has it.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct ApiConversation {
    pub id: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub is_im: bool,
    #[serde(default)]
    pub is_mpim: bool,
    #[serde(default)]
    pub is_private: bool,
    #[serde(default)]
    pub is_group: bool,
    #[serde(default)]
    pub is_archived: bool,
    /// Whether the person is in it, for channels.
    #[serde(default)]
    pub is_member: Option<bool>,
    /// The other person of a direct message.
    #[serde(default)]
    pub user: Option<String>,
}

impl ApiConversation {
    /// Whether the person is in it: `conversations.list` also lists public channels they are
    /// not in. Archived ones are left out.
    pub fn is_in(&self) -> bool {
        !self.is_archived && (self.is_im || self.is_mpim || self.is_member != Some(false))
    }
}

/// `conversations.list`.
#[derive(Debug, Deserialize)]
pub(crate) struct ConversationsList {
    #[serde(default)]
    pub channels: Vec<ApiConversation>,
    #[serde(default)]
    pub response_metadata: Option<Metadata>,
}

/// `users.list`.
#[derive(Debug, Deserialize)]
pub(crate) struct UsersList {
    #[serde(default)]
    pub members: Vec<User>,
    #[serde(default)]
    pub response_metadata: Option<Metadata>,
}

/// `users.info`.
#[derive(Debug, Deserialize)]
pub(crate) struct UserInfo {
    pub user: User,
}

/// `conversations.history` and `conversations.replies`.
#[derive(Debug, Deserialize)]
pub(crate) struct Messages {
    #[serde(default)]
    pub messages: Vec<Message>,
    #[serde(default)]
    pub has_more: bool,
}

/// The person who signed in, in the answer of `oauth.v2.access`.
#[derive(Deserialize)]
pub(crate) struct AuthedUser {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub access_token: Option<String>,
    #[serde(default)]
    pub refresh_token: Option<String>,
    #[serde(default)]
    pub expires_in: Option<u64>,
}

/// The workspace, in the answer of `oauth.v2.access`.
#[derive(Deserialize)]
pub(crate) struct Team {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
}

/// `oauth.v2.access`, for a code or a refresh token. A user token is under `authed_user`; a
/// refreshed one may also be at the top.
#[derive(Deserialize)]
pub(crate) struct OAuthAccess {
    #[serde(default)]
    pub authed_user: Option<AuthedUser>,
    #[serde(default)]
    pub team: Option<Team>,
    #[serde(default)]
    pub access_token: Option<String>,
    #[serde(default)]
    pub refresh_token: Option<String>,
    #[serde(default)]
    pub expires_in: Option<u64>,
    #[serde(default)]
    pub token_type: Option<String>,
}
