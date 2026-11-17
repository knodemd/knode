//! Gmail, live, through the Gmail API with Sign in with Google: [`GmailConnector`].
//!
//! - **Signing in**: OAuth 2.0 for installed apps with PKCE and a loopback redirect
//!   ([`oauth`]), asking only for `gmail.readonly`. The refresh token and the address are kept
//!   in the system keychain (`_source-gmail-refresh-token`, `_source-gmail-account`); the
//!   access token lives only in memory. Signing out or disconnecting forgets them and revokes
//!   the token at Google. Mail goes from Google straight to this machine.
//! - **Knode's Google client**: its id comes from `KNODE_GOOGLE_CLIENT_ID` when Knode is
//!   built, or at run time. A build without one cannot sign in with Google and says so: the
//!   IMAP connector with an app password reads Gmail too.
//! - **New mail**: the first run lists the messages (`users.messages.list`, with
//!   `after:<day connected>` for [`History::FromToday`]), keeping the mailbox's `historyId` from
//!   before the listing; later runs ask what was added since (`users.history.list` from that
//!   `historyId`). Each message is read in `raw` format, its RFC 822 bytes, and handed to the
//!   mail importer like a message of a Takeout `.mbox`. When Google no longer has the history
//!   from the saved `historyId` (it keeps about a week; the answer is 404), the connector lists
//!   again what arrived from a day before its last successful run, at most
//!   [`RESYNC_MAX`] messages, and goes on from a new `historyId`.
//! - **Containers** are the labels, by their id (`INBOX`, `Label_12`). Every label comes in,
//!   new ones too, except the excluded ones and spam and trash; a message with an excluded label
//!   is left out even when it has other labels. A message's labels are its folders, named as
//!   Google Takeout names them (`Inbox`, `Sent`, `Category Updates`, the label's name), so the
//!   same message from an export and from the API is in the same folders.
//! - A message already in the space from a Takeout `.mbox`, Apple Mail or the IMAP connector
//!   is found by its `Message-ID` and not added again. Attachments are not attached: the files
//!   are named in the thread.

pub mod http;
pub mod oauth;

use std::collections::{BTreeMap, HashSet};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use base64::Engine;
use knode_format::Timestamp;
use knode_ingest::sources::{
    Connector, Container, Cursor, FetchRequest, Fetched, History, Permission, Secrets, SignInState,
    SignInStep, SourceError, SourceInfo, Trigger,
};
use knode_vault::mail::{MailSource, RawMail};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use self::http::{Http, HttpResponse, UreqHttp, encode};
use self::oauth::{GoogleClient, Pending};
use super::mail_live::{MailBatch, gmail_system_label, is_gmail_junk};

/// The id of the connector.
pub const ID: &str = "gmail";

/// The secret holding the refresh token.
pub const REFRESH_TOKEN_SECRET: &str = "refresh-token";
/// The secret holding the address signed in.
pub const ACCOUNT_SECRET: &str = "account";

/// How often Gmail is asked for new mail.
pub const POLL_EVERY: Duration = Duration::from_secs(60);
/// The most messages a resync reads after Google lost the history.
pub const RESYNC_MAX: usize = 2000;
/// The most messages one fetch reads, whatever the service asks: a fetch must end soon.
const MAX_PER_FETCH: usize = 100;
/// How many history records one fetch reads.
const HISTORY_PAGE: usize = 100;
/// How long `answer` waits for the browser to come back.
const ANSWER_WAIT: Duration = Duration::from_secs(30);

/// Where Google is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoints {
    /// The page the browser opens to sign in.
    pub auth: String,
    /// Where codes and refresh tokens become access tokens.
    pub token: String,
    /// Where tokens are revoked.
    pub revoke: String,
    /// The Gmail API of the person signed in.
    pub api: String,
}

impl Default for Endpoints {
    fn default() -> Self {
        Endpoints {
            auth: "https://accounts.google.com/o/oauth2/v2/auth".into(),
            token: "https://oauth2.googleapis.com/token".into(),
            revoke: "https://oauth2.googleapis.com/revoke".into(),
            api: "https://gmail.googleapis.com/gmail/v1/users/me".into(),
        }
    }
}

/// What a person reads when the connector is not signed in.
const NOT_SIGNED_IN: &str = "sign in with Google first: in the Mac app, Settings > Sources > \
     Gmail > Connect; or run knode sources sign-in gmail";

/// What a person reads when this build has no Google client.
pub const NO_CLIENT: &str = "Sign in with Google is not set up in this build of Knode (it has no \
     Google client id). Read Gmail with the Mail (IMAP) connector and an app password instead, \
     or set KNODE_GOOGLE_CLIENT_ID to the id of your own Google Cloud OAuth client";

/// Gmail through the Gmail API. See the [module](self).
pub struct GmailConnector {
    http: Arc<dyn Http>,
    client: Option<GoogleClient>,
    endpoints: Endpoints,
    /// The access token and when it expires, in memory only.
    token: Mutex<Option<(String, Instant)>>,
    /// The sign-in under way.
    pending: Mutex<Option<Pending>>,
}

impl std::fmt::Debug for GmailConnector {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GmailConnector")
            .field("client", &self.client)
            .field("endpoints", &self.endpoints)
            .finish_non_exhaustive()
    }
}

impl Default for GmailConnector {
    fn default() -> Self {
        Self::new()
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

fn failed(what: impl std::fmt::Display) -> SourceError {
    SourceError::Failed(what.to_string())
}

/// Why Google refused, from its JSON error.
fn google_error(response: &HttpResponse) -> String {
    let json: Value = serde_json::from_slice(&response.body).unwrap_or(Value::Null);
    let text = json["error_description"]
        .as_str()
        .or_else(|| json["error"]["message"].as_str())
        .or_else(|| json["error"].as_str())
        .unwrap_or("");
    if text.is_empty() {
        format!("Google answered {}", response.status)
    } else {
        format!("Google answered {}: {text}", response.status)
    }
}

/// A Gmail label.
#[derive(Debug, Clone)]
struct Label {
    id: String,
    name: String,
}

impl GmailConnector {
    /// The connector with this build's Google client, over the network.
    pub fn new() -> Self {
        Self::with(
            Arc::new(UreqHttp::default()),
            GoogleClient::from_env(),
            Endpoints::default(),
        )
    }

    /// The connector over `http`, with this client and these endpoints: Google in memory, in
    /// tests.
    pub fn with(http: Arc<dyn Http>, client: Option<GoogleClient>, endpoints: Endpoints) -> Self {
        GmailConnector {
            http,
            client,
            endpoints,
            token: Mutex::new(None),
            pending: Mutex::new(None),
        }
    }

    fn client(&self) -> Result<&GoogleClient, SourceError> {
        self.client
            .as_ref()
            .ok_or_else(|| SourceError::Unavailable(NO_CLIENT.into()))
    }

    fn post_token(&self, form: &[(&str, &str)]) -> Result<Value, SourceError> {
        let client = self.client()?;
        let mut form = form.to_vec();
        form.push(("client_id", &client.id));
        if let Some(secret) = &client.secret {
            form.push(("client_secret", secret));
        }
        let response = self
            .http
            .post_form(&self.endpoints.token, &form)
            .map_err(|e| failed(format!("Google could not be reached: {e}")))?;
        if response.ok() {
            return serde_json::from_slice(&response.body)
                .map_err(|_| failed("Google's answer could not be read"));
        }
        let json: Value = serde_json::from_slice(&response.body).unwrap_or(Value::Null);
        if matches!(response.status, 400 | 401)
            && matches!(
                json["error"].as_str(),
                Some("invalid_grant" | "unauthorized_client" | "invalid_client")
            )
        {
            return Err(SourceError::SignIn(format!(
                "Google no longer lets Knode read this account ({}): sign in with Google again",
                google_error(&response)
            )));
        }
        Err(failed(google_error(&response)))
    }

    fn keep_token(&self, json: &Value) -> Result<String, SourceError> {
        let token = json["access_token"]
            .as_str()
            .ok_or_else(|| failed("Google gave no access token"))?
            .to_string();
        let lifetime = json["expires_in"].as_u64().unwrap_or(3600);
        // Refreshed a minute before it expires.
        let expires = Instant::now() + Duration::from_secs(lifetime.saturating_sub(60));
        *lock(&self.token) = Some((token.clone(), expires));
        Ok(token)
    }

    /// An access token: the one in memory while it is good, else a new one from the refresh
    /// token.
    fn access_token(&self, secrets: &dyn Secrets) -> Result<String, SourceError> {
        if let Some((token, expires)) = lock(&self.token).as_ref()
            && Instant::now() < *expires
        {
            return Ok(token.clone());
        }
        let refresh = secrets
            .get(REFRESH_TOKEN_SECRET)?
            .ok_or_else(|| SourceError::SignIn(NOT_SIGNED_IN.into()))?;
        let json =
            self.post_token(&[("grant_type", "refresh_token"), ("refresh_token", &refresh)])?;
        self.keep_token(&json)
    }

    /// `GET` of the Gmail API (`path` after `users/me`); `None` when Google has nothing there
    /// (404). An access token Google refuses is refreshed once.
    fn api(&self, secrets: &dyn Secrets, path: &str) -> Result<Option<Value>, SourceError> {
        let url = format!("{}{path}", self.endpoints.api);
        for attempt in 0..2 {
            let token = self.access_token(secrets)?;
            let response = self
                .http
                .get(&url, &token)
                .map_err(|e| failed(format!("Gmail could not be reached: {e}")))?;
            match response.status {
                200..=299 => {
                    return serde_json::from_slice(&response.body)
                        .map(Some)
                        .map_err(|_| failed("Gmail's answer could not be read"));
                }
                404 => return Ok(None),
                401 if attempt == 0 => {
                    *lock(&self.token) = None;
                }
                401 => {
                    return Err(SourceError::SignIn(format!(
                        "{}: sign in with Google again",
                        google_error(&response)
                    )));
                }
                403 if !google_error(&response).to_lowercase().contains("rate") => {
                    return Err(SourceError::SignIn(format!(
                        "Google does not let Knode read this mail ({}): sign in with Google \
                         again and allow Knode to read your mail",
                        google_error(&response)
                    )));
                }
                _ => return Err(failed(google_error(&response))),
            }
        }
        Err(failed("Gmail refused the access token"))
    }

    fn need(&self, secrets: &dyn Secrets, path: &str) -> Result<Value, SourceError> {
        self.api(secrets, path)?
            .ok_or_else(|| failed(format!("Gmail has nothing at {path}")))
    }

    fn labels(&self, secrets: &dyn Secrets) -> Result<Vec<Label>, SourceError> {
        let json = self.need(secrets, "/labels")?;
        Ok(json["labels"]
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or_default()
            .iter()
            .filter_map(|l| {
                Some(Label {
                    id: l["id"].as_str()?.to_string(),
                    name: l["name"].as_str().unwrap_or_default().to_string(),
                })
            })
            .collect())
    }

    /// The profile: the address and the mailbox's `historyId` now.
    fn profile(&self, secrets: &dyn Secrets) -> Result<(String, String), SourceError> {
        let json = self.need(secrets, "/profile")?;
        let email = json["emailAddress"]
            .as_str()
            .ok_or_else(|| failed("Gmail gave no address"))?
            .to_string();
        let history =
            history_id(&json["historyId"]).ok_or_else(|| failed("Gmail gave no history id"))?;
        Ok((email, history))
    }

    /// Reads one message in `raw` format; `None` when it is gone, or left out (spam, trash, an
    /// excluded label, older than the history chosen).
    fn message(
        &self,
        secrets: &dyn Secrets,
        id: &str,
        names: &BTreeMap<String, String>,
        request: &FetchRequest<'_>,
    ) -> Result<Option<RawMail>, SourceError> {
        let Some(json) = self.api(secrets, &format!("/messages/{}?format=raw", encode(id)))? else {
            return Ok(None);
        };
        let labels: Vec<&str> = json["labelIds"]
            .as_array()
            .map(|a| a.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        if labels
            .iter()
            .any(|l| is_gmail_junk(l) || request.is_excluded(l))
        {
            return Ok(None);
        }
        let at = json["internalDate"]
            .as_str()
            .and_then(|d| d.parse::<i64>().ok())
            .map(Timestamp);
        if at.is_some_and(|at| !request.wants(at)) {
            return Ok(None);
        }
        let Some(raw) = json["raw"].as_str() else {
            return Ok(None);
        };
        let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(raw.trim_end_matches('='))
            .map_err(|_| failed("a message of Gmail could not be decoded"))?;
        let folders = labels
            .iter()
            .filter_map(|l| match gmail_system_label(l) {
                Some(name) => name.map(str::to_string),
                None => Some(names.get(*l).cloned().unwrap_or_else(|| l.to_string())),
            })
            .collect::<Vec<_>>();
        Ok(Some(RawMail::new(bytes, folders)))
    }
}

fn history_id(value: &Value) -> Option<String> {
    match value {
        Value::String(s) if !s.is_empty() => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

/// A listing of messages under way: the first run, or a resync.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
struct Listing {
    /// Gmail's search (`after:1763337600`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    query: Option<String>,
    /// The next page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    page: Option<String>,
    /// How many messages it may still read, for a resync.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    left: Option<usize>,
}

/// Where the connector stopped, for one account.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
struct GmailCursor {
    /// The address read: another account starts again.
    account: String,
    /// The `historyId` new mail is asked for from.
    history: String,
    /// The next page of history from it, when a fetch stopped in the middle.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    page: Option<String>,
    /// A listing under way.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    listing: Option<Listing>,
    /// When a run last read everything there was, in milliseconds since 1970.
    #[serde(default)]
    synced_at: i64,
}

impl GmailCursor {
    fn save(&self) -> Cursor {
        Cursor::new(serde_json::to_string(self).unwrap_or_default())
    }
}

/// `after:<seconds>`, Gmail's search for what was received from then on.
fn after(at: i64) -> String {
    format!("after:{}", at.div_euclid(1000).max(0))
}

impl Connector for GmailConnector {
    fn info(&self) -> SourceInfo {
        SourceInfo::connector(
            ID,
            "Gmail",
            "The mail of your Google account, through Sign in with Google",
        )
        .needing([Permission::SignIn, Permission::Network])
        .excluding("label")
    }

    fn trigger(&self) -> Trigger {
        Trigger::Interval(POLL_EVERY)
    }

    fn check(&self) -> Result<(), SourceError> {
        self.client().map(|_| ())
    }

    fn containers(&self, request: &FetchRequest<'_>) -> Result<Vec<Container>, SourceError> {
        let mut out: Vec<Container> = self
            .labels(request.secrets)?
            .into_iter()
            .filter_map(|l| match gmail_system_label(&l.id) {
                Some(Some(_)) if is_gmail_junk(&l.id) => None,
                Some(Some(name)) => Some(Container::new(l.id, name)),
                Some(None) => None,
                None => Some(Container::new(l.id, l.name)),
            })
            .collect();
        out.sort_by(|a, b| (a.id != "INBOX", &a.name).cmp(&(b.id != "INBOX", &b.name)));
        Ok(out)
    }

    fn sign_in_state(&self, secrets: &dyn Secrets) -> Result<SignInState, SourceError> {
        self.client()?;
        if secrets.get(REFRESH_TOKEN_SECRET)?.is_some()
            && let Some(account) = secrets.get(ACCOUNT_SECRET)?
        {
            return Ok(SignInState::SignedIn { account });
        }
        Ok(SignInState::SignedOut)
    }

    /// [`SignInStep::Start`] (the account, if given, picks it on Google's page) gives the page
    /// to open, [`SignInState::OpenBrowser`]; [`SignInStep::Browser`] once the person signed
    /// in there waits up to 30 seconds for the browser to come back, then keeps the refresh
    /// token.
    fn sign_in(&self, step: SignInStep, secrets: &dyn Secrets) -> Result<SignInState, SourceError> {
        match step {
            SignInStep::Start { account } => self.open_browser(&account),
            SignInStep::Browser => self.browser_came_back(secrets),
            _ => Err(SourceError::SignIn(
                "Gmail signs in with Google in the browser".into(),
            )),
        }
    }

    fn sign_out(&self, secrets: &dyn Secrets) -> Result<bool, SourceError> {
        *lock(&self.pending) = None;
        *lock(&self.token) = None;
        let refresh = secrets.get(REFRESH_TOKEN_SECRET)?;
        if let Some(token) = &refresh {
            // Best effort: the token is forgotten here whatever Google answers.
            let _ = self
                .http
                .post_form(&self.endpoints.revoke, &[("token", token)]);
        }
        let had_token = secrets.delete(REFRESH_TOKEN_SECRET)?;
        let had_account = secrets.delete(ACCOUNT_SECRET)?;
        Ok(refresh.is_some() || had_token || had_account)
    }

    fn fetch(&self, request: &FetchRequest<'_>) -> Result<Fetched, SourceError> {
        self.client()?;
        let secrets = request.secrets;
        let account = secrets
            .get(ACCOUNT_SECRET)?
            .ok_or_else(|| SourceError::SignIn(NOT_SIGNED_IN.into()))?;
        let budget = request.max_items.clamp(1, MAX_PER_FETCH);
        let mut cursor = request
            .cursor
            .and_then(|c| serde_json::from_str::<GmailCursor>(c.as_str()).ok())
            .filter(|c| c.account == account && !c.history.is_empty());
        let mut ids: Vec<String> = Vec::new();
        let mut more = false;
        if cursor.is_none() {
            // The history from before the listing, so nothing that arrives meanwhile is lost.
            let (_, history) = self.profile(secrets)?;
            cursor = Some(GmailCursor {
                account: account.clone(),
                history,
                page: None,
                listing: Some(Listing {
                    query: match request.history {
                        History::FromToday => Some(after(request.since.0)),
                        History::Everything => None,
                    },
                    page: None,
                    left: None,
                }),
                synced_at: 0,
            });
        }
        let mut cursor = cursor.unwrap_or_default();
        if cursor.listing.is_none() {
            let mut path = format!(
                "/history?startHistoryId={}&historyTypes=messageAdded&maxResults={HISTORY_PAGE}",
                encode(&cursor.history)
            );
            if let Some(page) = &cursor.page {
                path.push_str(&format!("&pageToken={}", encode(page)));
            }
            match self.api(secrets, &path)? {
                Some(json) => {
                    for record in json["history"]
                        .as_array()
                        .map(Vec::as_slice)
                        .unwrap_or_default()
                    {
                        for added in record["messagesAdded"]
                            .as_array()
                            .map(Vec::as_slice)
                            .unwrap_or_default()
                        {
                            if let Some(id) = added["message"]["id"].as_str() {
                                ids.push(id.to_string());
                            }
                        }
                    }
                    match json["nextPageToken"].as_str() {
                        Some(page) => {
                            cursor.page = Some(page.to_string());
                            more = true;
                        }
                        None => {
                            cursor.page = None;
                            if let Some(history) = history_id(&json["historyId"]) {
                                cursor.history = history;
                            }
                        }
                    }
                }
                None => {
                    // Google no longer has history from there: list again what arrived from a
                    // day before the last good run, and go on from now.
                    let (_, history) = self.profile(secrets)?;
                    let mut from = cursor.synced_at - 24 * 3600 * 1000;
                    if request.history == History::FromToday {
                        from = from.max(request.since.0);
                    }
                    cursor.history = history;
                    cursor.page = None;
                    cursor.listing = Some(Listing {
                        query: Some(after(from)),
                        page: None,
                        left: Some(RESYNC_MAX),
                    });
                }
            }
        }
        if let Some(mut listing) = cursor.listing.take() {
            let wanted = listing.left.map_or(budget, |left| left.min(budget));
            let mut path = format!("/messages?maxResults={wanted}&includeSpamTrash=false");
            if let Some(query) = &listing.query {
                path.push_str(&format!("&q={}", encode(query)));
            }
            if let Some(page) = &listing.page {
                path.push_str(&format!("&pageToken={}", encode(page)));
            }
            let json = self.need(secrets, &path)?;
            let listed: Vec<String> = json["messages"]
                .as_array()
                .map(Vec::as_slice)
                .unwrap_or_default()
                .iter()
                .filter_map(|m| m["id"].as_str().map(str::to_string))
                .collect();
            if let Some(left) = &mut listing.left {
                *left = left.saturating_sub(listed.len());
            }
            ids.extend(listed);
            listing.page = json["nextPageToken"].as_str().map(str::to_string);
            if listing.page.is_some() && listing.left != Some(0) {
                cursor.listing = Some(listing);
                more = true;
            }
        }
        let names: BTreeMap<String, String> = if ids.is_empty() {
            BTreeMap::new()
        } else {
            self.labels(secrets)?
                .into_iter()
                .map(|l| (l.id, l.name))
                .collect()
        };
        let mut seen = HashSet::new();
        let mut mails = Vec::new();
        for id in ids {
            if seen.insert(id.clone())
                && let Some(mail) = self.message(secrets, &id, &names, request)?
            {
                mails.push(mail);
            }
        }
        if !more {
            cursor.synced_at = Timestamp::now().0;
        }
        let saved = cursor.save();
        if mails.is_empty() {
            return Ok(Fetched::nothing(saved).with_more(more));
        }
        Ok(Fetched::batch(
            MailBatch {
                source: MailSource::Gmail,
                mails,
            },
            saved,
        )
        .with_more(more))
    }
}

impl GmailConnector {
    fn open_browser(&self, login_hint: &str) -> Result<SignInState, SourceError> {
        let client = self.client()?;
        let pending = oauth::begin(client, &self.endpoints.auth, login_hint)
            .map_err(|e| failed(format!("Knode could not start signing in: {e}")))?;
        let url = pending.url.clone();
        *lock(&self.pending) = Some(pending);
        Ok(SignInState::OpenBrowser { url })
    }

    fn browser_came_back(&self, secrets: &dyn Secrets) -> Result<SignInState, SourceError> {
        let mut guard = lock(&self.pending);
        let Some(pending) = guard.as_ref() else {
            return Err(SourceError::SignIn(
                "start signing in with Google first".into(),
            ));
        };
        match pending.codes.recv_timeout(ANSWER_WAIT) {
            Ok(Ok(code)) => {
                let pending = guard.take().expect("a sign-in is under way");
                drop(guard);
                self.finish(pending, &code, secrets)?;
                let account = secrets.get(ACCOUNT_SECRET)?.unwrap_or_default();
                Ok(SignInState::SignedIn { account })
            }
            Ok(Err(why)) => {
                *guard = None;
                Err(SourceError::SignIn(why))
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => Err(SourceError::SignIn(
                "finish signing in with Google in the browser first, then go on".into(),
            )),
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                *guard = None;
                Err(SourceError::SignIn(
                    "the browser did not come back: sign in again".into(),
                ))
            }
        }
    }

    fn finish(
        &self,
        pending: Pending,
        code: &str,
        secrets: &dyn Secrets,
    ) -> Result<(), SourceError> {
        let json = self.post_token(&[
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", &pending.redirect_uri),
            ("code_verifier", &pending.verifier),
        ])?;
        let scopes = json["scope"].as_str().unwrap_or(oauth::SCOPE);
        if !scopes.split(' ').any(|s| s == oauth::SCOPE) {
            return Err(SourceError::SignIn(
                "Google did not let Knode read your mail: sign in again and tick the box that \
                 lets Knode read your email"
                    .into(),
            ));
        }
        let refresh = json["refresh_token"].as_str().ok_or_else(|| {
            SourceError::SignIn(
                "Google gave no refresh token: remove Knode from your Google Account's \
                 third-party access, then sign in again"
                    .into(),
            )
        })?;
        self.keep_token(&json)?;
        secrets.set(REFRESH_TOKEN_SECRET, refresh)?;
        let (email, _) = match self.profile(secrets) {
            Ok(profile) => profile,
            Err(e) => {
                let _ = secrets.delete(REFRESH_TOKEN_SECRET);
                *lock(&self.token) = None;
                return Err(e);
            }
        };
        secrets.set(ACCOUNT_SECRET, &email)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests;
