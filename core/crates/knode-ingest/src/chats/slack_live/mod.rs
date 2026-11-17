//! Slack, live: [`SlackConnector`] reads the conversations the person is in through the Slack
//! Web API, with a user token that can only read, and writes them into the same nodes as the
//! Slack export importer: one `chat` node per conversation, found by its channel id, one per
//! thread, `part-of` its channel, found by the channel id and the `ts` of its first message,
//! and a `person` node per member, found by their member id. An export and the connector bring
//! in the same nodes, and neither writes a message twice.
//!
//! # Signing in
//!
//! OAuth 2.0 with PKCE and a loopback redirect, with no client secret: see [`oauth`]. The Slack
//! app's client id comes from the build (`KNODE_SLACK_CLIENT_ID` when Knode is built) or the
//! environment of Knode Service; a build without one says it has no Slack app. The token is
//! kept with the connector's secrets, in the system keychain.
//!
//! # Reading
//!
//! Every run, within Slack's rate limits:
//!
//! 1. `users.list` (every hour) and `conversations.list` (every ten minutes): the members of
//!    the workspace and every public and private channel, group message and direct message
//!    the person is in. Lists longer than a run allows go on in the next run.
//! 2. Threads with replies not read yet: `conversations.replies` after the last one read.
//! 3. Each conversation in turn, leaving out the excluded ones: `conversations.history` after
//!    the last message read. Messages are written oldest first, so a conversation with more
//!    new messages than one page first walks back page by page, keeping only where each page
//!    starts in the cursor, and then reads the pages forward.
//! 4. When there is time left, threads active in the last week are checked again for new
//!    replies.
//!
//! Slack limits `conversations.history` and `conversations.replies` of apps not in the Slack
//! Marketplace to one call a minute and 15 messages ([`Limits::NotListed`], the default);
//! Marketplace apps get 50 calls a minute ([`Limits::Marketplace`]). The connector spaces its
//! calls to fit, never waits more than a few seconds in a run, stops a run when Slack answers
//! `429` and waits the `Retry-After` it gives before calling that method again. What does not
//! fit in a run is read in the next ones: the cursor says where each conversation is.
//!
//! The cursor holds only ids and times (channel ids, message `ts`), never a token or what was
//! said.

mod api;
pub mod oauth;

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use knode_format::{ChangeOp, Provenance, Space, Timestamp};
use serde::{Deserialize, Serialize};

pub use api::{API_BASE, Clock, HttpResponse, SlackHttp, SystemClock, UreqHttp};
pub use oauth::{Account, PendingSignIn};

use super::files::Files;
use super::slack::{
    Conversation, Message, User, Workspace, clean_title, mark, time, wanted, write_conversation,
    write_people,
};
use super::write::ChatWriter;
use super::{ChatFormat, ChatImportError, ChatImportReport};
use crate::sources::{
    Batch, Connector, Container, Cursor, FetchRequest, Fetched, Permission, Secrets, SourceError,
    SourceInfo, Trigger, Written,
};
use api::{ApiConversation, ApiError, ConversationsList, Messages, UserInfo, UsersList, decode};
use oauth::Token;

/// The id of the connector.
pub const SOURCE_ID: &str = "slack";

/// The variable with the client id of the Slack app, read when Knode is built and, to override
/// it, by Knode Service when it starts.
pub const CLIENT_ID_VARIABLE: &str = "KNODE_SLACK_CLIENT_ID";

/// The variable that says the Slack app is in the Slack Marketplace (`marketplace`), so the
/// connector may call at Marketplace rates. Read when Knode is built and when it starts.
pub const LIMITS_VARIABLE: &str = "KNODE_SLACK_LIMITS";

/// How long a run may wait for Slack's rate limits, in all.
const RUN_BUDGET: Duration = Duration::from_secs(10);
/// The longest wait before one call; a call that would wait longer is left for the next run.
const MAX_WAIT: Duration = Duration::from_secs(4);
/// How long the list of members is used before it is read again.
const USERS_TTL: Duration = Duration::from_secs(3600);
/// How long the list of conversations is used before it is read again.
const CONVERSATIONS_TTL: Duration = Duration::from_secs(600);
/// How long after its last reply a thread is checked again for new replies.
const THREAD_WATCH: Duration = Duration::from_secs(7 * 24 * 3600);
/// The most threads the cursor follows; the least active ones are dropped first.
const MAX_THREADS: usize = 1000;
/// The most members asked for one by one in a run, for members `users.list` did not have.
const MAX_USER_LOOKUPS: usize = 20;

/// The rate limits the Slack app has.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Limits {
    /// An app not (yet) in the Slack Marketplace: `conversations.history` and
    /// `conversations.replies` once a minute, 15 messages a call.
    #[default]
    NotListed,
    /// A Slack Marketplace app: those methods 50 times a minute (Tier 3), 200 messages a call.
    Marketplace,
}

impl Limits {
    /// The limits named `marketplace`, or the lower ones for anything else.
    pub fn from_name(name: &str) -> Limits {
        if name.trim().eq_ignore_ascii_case("marketplace") {
            Limits::Marketplace
        } else {
            Limits::NotListed
        }
    }

    /// The most messages asked for in one call.
    pub fn page(self) -> usize {
        match self {
            Limits::NotListed => 15,
            Limits::Marketplace => 200,
        }
    }

    /// The time between two calls of a method of `tier`.
    fn interval(self, tier: Tier) -> Duration {
        match (tier, self) {
            (Tier::Messages, Limits::NotListed) => Duration::from_secs(60),
            (Tier::Messages, Limits::Marketplace) => Duration::from_millis(1200),
            (Tier::Two, _) => Duration::from_secs(3),
            (Tier::Four, _) => Duration::from_millis(600),
        }
    }

    /// How often a thread active in the last week is checked again.
    fn recheck(self) -> Duration {
        match self {
            Limits::NotListed => Duration::from_secs(6 * 3600),
            Limits::Marketplace => Duration::from_secs(1800),
        }
    }
}

/// How the connector reaches Slack.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SlackConfig {
    /// The client id of the Slack app; `None` in a build with no Slack app.
    pub client_id: Option<String>,
    /// The rate limits of the app.
    pub limits: Limits,
}

impl SlackConfig {
    /// The client id and limits of this build ([`CLIENT_ID_VARIABLE`], [`LIMITS_VARIABLE`]),
    /// overridden by the environment.
    pub fn from_env() -> Self {
        let read = |name: &str, built: Option<&'static str>| {
            std::env::var(name)
                .ok()
                .or(built.map(str::to_string))
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
        };
        SlackConfig {
            client_id: read(CLIENT_ID_VARIABLE, option_env!("KNODE_SLACK_CLIENT_ID")),
            limits: read(LIMITS_VARIABLE, option_env!("KNODE_SLACK_LIMITS"))
                .map(|l| Limits::from_name(&l))
                .unwrap_or_default(),
        }
    }
}

/// The groups of methods Slack limits together.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Tier {
    /// `conversations.list`, `users.list`: 20 a minute.
    Two,
    /// `users.info`: 100 a minute.
    Four,
    /// `conversations.history` and `conversations.replies`.
    Messages,
}

/// What the connector keeps between runs on this machine, besides the cursor: when each group
/// of methods may be called again, and the lists of members and conversations.
#[derive(Default)]
struct Memory {
    next_call: BTreeMap<Tier, Duration>,
    /// The workspace the lists are of.
    team: Option<String>,
    users: Listing<User>,
    conversations: Listing<ApiConversation>,
}

/// A list read page by page: the last complete one, and the one being read.
struct Listing<T> {
    current: Option<Vec<T>>,
    read_at: Option<Duration>,
    reading: Option<(Vec<T>, Option<String>)>,
}

impl<T> Default for Listing<T> {
    fn default() -> Self {
        Listing {
            current: None,
            read_at: None,
            reading: None,
        }
    }
}

impl<T> Listing<T> {
    fn fresh(&self, now: Duration, ttl: Duration) -> bool {
        self.current.is_some() && self.read_at.is_some_and(|at| now.saturating_sub(at) < ttl)
    }
}

/// The Slack connector. One value serves every space it is connected to; the account is the
/// machine's.
pub struct SlackConnector {
    config: SlackConfig,
    http: Arc<dyn SlackHttp>,
    clock: Arc<dyn Clock>,
    memory: Mutex<Memory>,
}

impl SlackConnector {
    /// The connector of this build, reaching Slack over HTTPS.
    pub fn from_env() -> Self {
        SlackConnector::new(
            SlackConfig::from_env(),
            Arc::new(UreqHttp::new()),
            Arc::new(SystemClock::new()),
        )
    }

    /// A connector with this configuration, HTTP client and clock.
    pub fn new(config: SlackConfig, http: Arc<dyn SlackHttp>, clock: Arc<dyn Clock>) -> Self {
        SlackConnector {
            config,
            http,
            clock,
            memory: Mutex::new(Memory::default()),
        }
    }

    /// Its configuration.
    pub fn config(&self) -> &SlackConfig {
        &self.config
    }

    fn client_id(&self) -> Result<&str, SourceError> {
        self.config.client_id.as_deref().ok_or_else(|| {
            SourceError::Unavailable(format!(
                "this build of Knode has no Slack app ({CLIENT_ID_VARIABLE} was not set when \
                 it was built)"
            ))
        })
    }

    fn memory(&self) -> MutexGuard<'_, Memory> {
        self.memory.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Starts signing in: listens on a loopback port and gives the page of Slack to open in
    /// the browser. [`finish_sign_in`](Self::finish_sign_in) waits for it.
    pub fn begin_sign_in(&self) -> Result<PendingSignIn, SourceError> {
        self.begin_sign_in_on(&oauth::REDIRECT_PORTS)
    }

    /// The same, on the first free one of these ports (`0` for any).
    pub fn begin_sign_in_on(&self, ports: &[u16]) -> Result<PendingSignIn, SourceError> {
        oauth::begin(self.client_id()?, ports)
    }

    /// Waits up to `timeout` for the browser to come back from Slack, exchanges the code for
    /// the token and keeps it in `secrets`.
    pub fn finish_sign_in(
        &self,
        pending: PendingSignIn,
        secrets: &dyn Secrets,
        timeout: Duration,
    ) -> Result<Account, SourceError> {
        self.client_id()?;
        let account = pending.finish(self.http.as_ref(), secrets, self.clock.unix(), timeout)?;
        *self.memory() = Memory::default();
        Ok(account)
    }

    /// Who is signed in, if anyone.
    pub fn account(&self, secrets: &dyn Secrets) -> Result<Option<Account>, SourceError> {
        Ok(Token::load(secrets)?.map(|t| Account {
            team_id: t.team_id,
            team_name: t.team_name,
            user_id: t.user_id,
        }))
    }

    /// Signs out: asks Slack to revoke the token, when `revoke`, and forgets it. Returns
    /// whether there was one.
    pub fn sign_out(&self, secrets: &dyn Secrets, revoke: bool) -> Result<bool, SourceError> {
        let token = Token::load(secrets).ok().flatten();
        if revoke && let Some(token) = &token {
            // Forgotten here whatever Slack answers.
            let _ = self
                .http
                .post("auth.revoke", Some(&token.access_token), &[]);
        }
        *self.memory() = Memory::default();
        Ok(secrets.delete(oauth::TOKEN_SECRET)?)
    }

    /// The token, refreshed when it is about to expire.
    fn token(&self, secrets: &dyn Secrets) -> Result<Token, SourceError> {
        let client_id = self.client_id()?;
        let token = Token::load(secrets)?.ok_or_else(|| {
            SourceError::SignIn(
                "Slack is not signed in on this machine: `knode sources sign-in slack`".into(),
            )
        })?;
        match oauth::refresh(self.http.as_ref(), client_id, &token, self.clock.unix())? {
            Some(refreshed) => {
                refreshed.save(secrets)?;
                Ok(refreshed)
            }
            None => Ok(token),
        }
    }
}

impl Connector for SlackConnector {
    fn info(&self) -> SourceInfo {
        SourceInfo::connector(
            SOURCE_ID,
            "Slack",
            "The channels, group messages and direct messages you are in on Slack",
        )
        .needing([Permission::SignIn, Permission::Network])
        .excluding("channel")
    }

    fn trigger(&self) -> Trigger {
        Trigger::Interval(Duration::from_secs(60))
    }

    fn check(&self) -> Result<(), SourceError> {
        self.client_id().map(|_| ())
    }

    fn containers(&self, request: &FetchRequest<'_>) -> Result<Vec<Container>, SourceError> {
        let token = self.token(request.secrets)?;
        let mut memory = self.memory();
        let mut run = Run::new(self, &mut memory, &token, Duration::from_secs(30));
        run.users()?;
        run.conversations()?;
        if let Some(error) = run.failure.take() {
            return Err(error);
        }
        let (Some(users), Some(list)) = (
            run.memory.users.current.as_ref(),
            run.memory.conversations.current.as_ref(),
        ) else {
            return Err(SourceError::Failed(
                "Slack asks to slow down: the list of channels comes in a minute".into(),
            ));
        };
        let workspace = Workspace::new(users.iter().cloned(), BTreeMap::new());
        let mut out: Vec<Container> = list
            .iter()
            .map(|c| {
                let (conversation, kind) = shape(c, &token.user_id, users);
                Container::new(
                    c.id.clone(),
                    container_name(&workspace, &conversation, kind),
                )
            })
            .collect();
        out.sort_by_key(|a| a.name.to_lowercase());
        Ok(out)
    }

    fn fetch(&self, request: &FetchRequest<'_>) -> Result<Fetched, SourceError> {
        let token = self.token(request.secrets)?;
        let mut state = request
            .cursor
            .and_then(|c| serde_json::from_str::<State>(c.as_str()).ok())
            .filter(|s| s.team.as_deref() == Some(token.team_id.as_str()))
            .unwrap_or_else(|| State {
                team: Some(token.team_id.clone()),
                ..State::default()
            });
        let mut memory = self.memory();
        let mut run = Run::new(self, &mut memory, &token, RUN_BUDGET);
        let mut batch = LiveBatch::default();
        run.read(request, &mut state, &mut batch)?;
        let failure = run.failure.take();
        let blocked = run.blocked;
        if batch.is_empty()
            && let Some(error) = failure
        {
            return Err(error);
        }
        if !batch.is_empty() {
            batch.users = run.memory.users.current.clone().unwrap_or_default();
        }
        let cursor = Cursor::new(serde_json::to_string(&state).map_err(|e| {
            SourceError::Failed(format!("the Slack cursor could not be written: {e}"))
        })?);
        let more = !blocked && batch.len() >= request.max_items.max(1);
        if batch.is_empty() {
            // The cursor may have moved (conversations walked back, threads checked).
            return Ok(Fetched {
                batch: None,
                cursor,
                more: false,
            });
        }
        Ok(Fetched::batch(batch, cursor).with_more(more))
    }
}

/// What kind of conversation `c` is, and its shape for the importer: a direct message has its
/// two people as members, a group message the members its name lists.
fn shape(c: &ApiConversation, me: &str, users: &[User]) -> (Conversation, &'static str) {
    if c.is_im {
        let mut members: Vec<String> = c.user.iter().cloned().collect();
        if c.user.as_deref() != Some(me) {
            members.push(me.to_string());
        }
        return (
            Conversation {
                id: c.id.clone(),
                name: None,
                members,
            },
            "personal",
        );
    }
    if c.is_mpim {
        // `mpdm-ada--grace--lin-1`: the handles of the members.
        let handles: Vec<&str> = c
            .name
            .as_deref()
            .and_then(|n| n.strip_prefix("mpdm-"))
            .map(|n| n.rsplit_once('-').map_or(n, |(h, _)| h))
            .map(|n| n.split("--").collect())
            .unwrap_or_default();
        let members = handles
            .iter()
            .filter_map(|h| users.iter().find(|u| u.name == *h).map(|u| u.id.clone()))
            .collect();
        return (
            Conversation {
                id: c.id.clone(),
                name: c.name.clone(),
                members,
            },
            "group",
        );
    }
    let kind = if c.is_private || c.is_group {
        "private-channel"
    } else {
        "channel"
    };
    (
        Conversation {
            id: c.id.clone(),
            name: c.name.clone(),
            members: Vec::new(),
        },
        kind,
    )
}

/// The name of a conversation in the list to exclude from: `#general`, or the people.
fn container_name(workspace: &Workspace, conversation: &Conversation, kind: &str) -> String {
    match (kind, &conversation.name) {
        ("channel" | "private-channel", Some(name)) => format!("#{}", clean_title(name)),
        _ => {
            let names: Vec<String> = conversation
                .members
                .iter()
                .map(|m| workspace_name(workspace, m))
                .collect();
            if names.is_empty() {
                conversation
                    .name
                    .clone()
                    .unwrap_or_else(|| conversation.id.clone())
            } else {
                names.join(", ")
            }
        }
    }
}

fn workspace_name(workspace: &Workspace, id: &str) -> String {
    workspace.name(id).unwrap_or_else(|| id.to_string())
}

/// The cursor: where each conversation and thread is.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
struct State {
    /// The workspace the ids belong to.
    #[serde(default)]
    team: Option<String>,
    /// Conversations by channel id.
    #[serde(default)]
    conversations: BTreeMap<String, ConversationState>,
    /// Threads by `<channel id>/<ts of its first message>`.
    #[serde(default)]
    threads: BTreeMap<String, ThreadState>,
    /// The conversation whose history is read first in the next run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    next: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
struct ConversationState {
    /// The `ts` of the newest message read: the next call asks for what is after it.
    done: String,
    /// Where the pages walked back start, newest first, while there is more than a page to
    /// read: the next call asks for what is before the last one.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    seek: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
struct ThreadState {
    /// The `ts` of the newest reply read, once any was.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    done: Option<String>,
    /// The `ts` of its last reply, as Slack last said.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    latest: Option<String>,
    /// Whether the last call said there are more replies.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    more: bool,
    /// When it was last read, in seconds since 1970.
    #[serde(default)]
    checked: u64,
    /// When it was last active, in seconds since 1970.
    #[serde(default)]
    active: u64,
}

impl ThreadState {
    /// Whether it has replies not read yet.
    fn due(&self) -> bool {
        match (&self.done, &self.latest) {
            (None, _) => true,
            _ if self.more => true,
            (Some(done), Some(latest)) => mark(latest) > mark(done),
            (Some(_), None) => false,
        }
    }
}

/// The seconds of a `ts`.
fn seconds(ts: &str) -> u64 {
    ts.split('.')
        .next()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0)
}

/// The later of two `ts`.
fn later(a: Option<String>, b: &str) -> String {
    match a {
        Some(a) if mark(&a) >= mark(b) => a,
        _ => b.to_string(),
    }
}

/// An answer, or why there is none.
enum Answer<T> {
    Got(T),
    /// The rate limits, the time of the run or an error stopped the run.
    Stopped,
    /// The conversation or thread is gone.
    Gone,
}

/// One run: the calls it makes, within the rate limits and the time it has.
struct Run<'a> {
    connector: &'a SlackConnector,
    memory: &'a mut Memory,
    token: &'a Token,
    deadline: Duration,
    /// Whether a rate limit or the time stopped the run.
    blocked: bool,
    /// An error that stopped the run, given back when it read nothing.
    failure: Option<SourceError>,
}

impl<'a> Run<'a> {
    fn new(
        connector: &'a SlackConnector,
        memory: &'a mut Memory,
        token: &'a Token,
        budget: Duration,
    ) -> Self {
        if memory.team.as_deref() != Some(token.team_id.as_str()) {
            *memory = Memory {
                next_call: std::mem::take(&mut memory.next_call),
                team: Some(token.team_id.clone()),
                ..Memory::default()
            };
        }
        let deadline = connector.clock.elapsed() + budget;
        Run {
            connector,
            memory,
            token,
            deadline,
            blocked: false,
            failure: None,
        }
    }

    fn stopped(&self) -> bool {
        self.blocked || self.failure.is_some()
    }

    fn limits(&self) -> Limits {
        self.connector.config.limits
    }

    /// Calls `method` when its rate limit and the time of the run allow it.
    fn call<T: serde::de::DeserializeOwned>(
        &mut self,
        tier: Tier,
        method: &str,
        params: &[(&str, String)],
    ) -> Result<Answer<T>, SourceError> {
        if self.stopped() {
            return Ok(Answer::Stopped);
        }
        let clock = &self.connector.clock;
        let now = clock.elapsed();
        let at = self
            .memory
            .next_call
            .get(&tier)
            .copied()
            .unwrap_or_default();
        let wait = at.saturating_sub(now);
        if wait > MAX_WAIT || now + wait > self.deadline {
            self.blocked = true;
            return Ok(Answer::Stopped);
        }
        if !wait.is_zero() {
            clock.sleep(wait);
        }
        let now = clock.elapsed();
        self.memory
            .next_call
            .insert(tier, now + self.limits().interval(tier));
        let response =
            match self
                .connector
                .http
                .post(method, Some(&self.token.access_token), params)
            {
                Ok(response) => response,
                Err(error) => {
                    self.failure = Some(SourceError::Failed(error.to_string()));
                    return Ok(Answer::Stopped);
                }
            };
        match decode::<T>(method, &response) {
            Ok(answer) => Ok(Answer::Got(answer)),
            Err(ApiError::RateLimited(wait)) => {
                let until = clock.elapsed() + wait;
                let entry = self.memory.next_call.entry(tier).or_default();
                *entry = (*entry).max(until);
                self.blocked = true;
                Ok(Answer::Stopped)
            }
            Err(ApiError::SignIn(what)) => Err(SourceError::SignIn(what)),
            Err(ApiError::Gone) => Ok(Answer::Gone),
            Err(ApiError::Failed(what)) => {
                self.failure = Some(SourceError::Failed(what));
                Ok(Answer::Stopped)
            }
        }
    }

    /// Reads the members of the workspace when the list is older than an hour.
    fn users(&mut self) -> Result<(), SourceError> {
        let now = self.connector.clock.elapsed();
        if self.memory.users.fresh(now, USERS_TTL) {
            return Ok(());
        }
        loop {
            let cursor = self
                .memory
                .users
                .reading
                .as_ref()
                .and_then(|(_, c)| c.clone());
            let mut params = vec![("limit", "200".to_string())];
            params.extend(cursor.map(|c| ("cursor", c)));
            let Answer::Got(page) = self.call::<UsersList>(Tier::Two, "users.list", &params)?
            else {
                return Ok(());
            };
            let next = api::next_cursor(&page.response_metadata);
            let listing = &mut self.memory.users;
            let (items, _) = listing.reading.get_or_insert_with(|| (Vec::new(), None));
            items.extend(page.members);
            match next {
                Some(next) => listing.reading.as_mut().expect("reading").1 = Some(next),
                None => {
                    listing.current = listing.reading.take().map(|(items, _)| items);
                    listing.read_at = Some(self.connector.clock.elapsed());
                    return Ok(());
                }
            }
        }
    }

    /// Reads the conversations the person is in when the list is older than ten minutes.
    fn conversations(&mut self) -> Result<(), SourceError> {
        let now = self.connector.clock.elapsed();
        if self.memory.conversations.fresh(now, CONVERSATIONS_TTL) {
            return Ok(());
        }
        loop {
            let cursor = self
                .memory
                .conversations
                .reading
                .as_ref()
                .and_then(|(_, c)| c.clone());
            let mut params = vec![
                (
                    "types",
                    "public_channel,private_channel,mpim,im".to_string(),
                ),
                ("exclude_archived", "true".to_string()),
                ("limit", "200".to_string()),
            ];
            params.extend(cursor.map(|c| ("cursor", c)));
            let Answer::Got(page) =
                self.call::<ConversationsList>(Tier::Two, "conversations.list", &params)?
            else {
                return Ok(());
            };
            let next = api::next_cursor(&page.response_metadata);
            let listing = &mut self.memory.conversations;
            let (items, _) = listing.reading.get_or_insert_with(|| (Vec::new(), None));
            items.extend(page.channels.into_iter().filter(ApiConversation::is_in));
            match next {
                Some(next) => listing.reading.as_mut().expect("reading").1 = Some(next),
                None => {
                    listing.current = listing.reading.take().map(|(items, _)| items);
                    listing.read_at = Some(self.connector.clock.elapsed());
                    return Ok(());
                }
            }
        }
    }

    /// Asks for members the list did not have, a few a run.
    fn lookup_users(&mut self, ids: &BTreeSet<String>) -> Result<(), SourceError> {
        let missing: Vec<String> = {
            let known = self.memory.users.current.as_deref().unwrap_or_default();
            ids.iter()
                .filter(|id| !known.iter().any(|u| &u.id == *id))
                .filter(|id| id.starts_with(['U', 'W']))
                .take(MAX_USER_LOOKUPS)
                .cloned()
                .collect()
        };
        for id in missing {
            match self.call::<UserInfo>(Tier::Four, "users.info", &[("user", id)])? {
                Answer::Got(info) => {
                    if let Some(users) = self.memory.users.current.as_mut() {
                        users.push(info.user);
                    }
                }
                Answer::Gone => {}
                Answer::Stopped => break,
            }
        }
        Ok(())
    }

    /// Reads what is new into `batch` and moves `state` past it.
    fn read(
        &mut self,
        request: &FetchRequest<'_>,
        state: &mut State,
        batch: &mut LiveBatch,
    ) -> Result<(), SourceError> {
        self.users()?;
        self.conversations()?;
        let (Some(_), Some(list)) = (
            self.memory.users.current.as_ref(),
            self.memory.conversations.current.clone(),
        ) else {
            // Nothing is written before the members and conversations are known.
            return Ok(());
        };
        let users = self.memory.users.current.clone().unwrap_or_default();
        let max = request.max_items.max(1);
        let page = self.limits().page();
        let now = self.connector.clock.unix().as_secs();

        // What the person is in, without what they excluded.
        let wanted: Vec<&ApiConversation> = list
            .iter()
            .filter(|c| !request.is_excluded(&c.id))
            .collect();
        let present: BTreeSet<&str> = wanted.iter().map(|c| c.id.as_str()).collect();
        state
            .conversations
            .retain(|id, _| present.contains(id.as_str()));
        state.threads.retain(|key, t| {
            let channel = key.split('/').next().unwrap_or_default();
            present.contains(channel)
                && (t.due() || now.saturating_sub(t.active) <= THREAD_WATCH.as_secs())
        });
        if state.threads.len() > MAX_THREADS {
            let mut by_activity: Vec<(u64, String)> = state
                .threads
                .iter()
                .map(|(k, t)| (t.active, k.clone()))
                .collect();
            by_activity.sort();
            let drop = by_activity.len() - MAX_THREADS;
            for (_, key) in by_activity.into_iter().take(drop) {
                state.threads.remove(&key);
            }
        }
        let start = match request.history {
            crate::sources::History::Everything => "0".to_string(),
            crate::sources::History::FromToday => {
                format!("{}.000000", request.since.0.max(0) / 1000)
            }
        };
        let by_id: BTreeMap<&str, &ApiConversation> =
            wanted.iter().map(|c| (c.id.as_str(), *c)).collect();
        let shape_of = |id: &str| by_id.get(id).map(|c| shape(c, &self.token.user_id, &users));

        // 1. Threads with replies not read yet.
        let due: Vec<String> = state
            .threads
            .iter()
            .filter(|(_, t)| t.due())
            .map(|(k, _)| k.clone())
            .collect();
        for key in due {
            if self.stopped() || batch.len() >= max {
                break;
            }
            self.thread(request, state, batch, &key, page, now, &shape_of)?;
        }

        // 2. The history of each conversation in turn, one call each, starting where the
        // last run stopped.
        let ids: Vec<&str> = wanted.iter().map(|c| c.id.as_str()).collect();
        let first = state
            .next
            .as_deref()
            .and_then(|n| ids.iter().position(|id| *id == n))
            .unwrap_or(0);
        let order: Vec<&str> = ids[first..].iter().chain(&ids[..first]).copied().collect();
        let mut visited = 0;
        for id in &order {
            if self.stopped() || batch.len() >= max {
                break;
            }
            let Some((conversation, kind)) = shape_of(id) else {
                continue;
            };
            let conv = state
                .conversations
                .entry(id.to_string())
                .or_insert_with(|| ConversationState {
                    done: start.clone(),
                    seek: Vec::new(),
                })
                .clone();
            let mut params = vec![
                ("channel", id.to_string()),
                ("oldest", conv.done.clone()),
                ("inclusive", "false".to_string()),
                ("limit", page.to_string()),
            ];
            params.extend(conv.seek.last().map(|l| ("latest", l.clone())));
            let answer = self.call::<Messages>(Tier::Messages, "conversations.history", &params)?;
            let history = match answer {
                Answer::Stopped => break,
                Answer::Gone => {
                    state.conversations.remove(*id);
                    visited += 1;
                    continue;
                }
                Answer::Got(history) => history,
            };
            visited += 1;
            let entry = state.conversations.get_mut(*id).expect("the conversation");
            if history.has_more
                && let Some(oldest) = history
                    .messages
                    .iter()
                    .map(|m| m.ts.clone())
                    .min_by_key(|ts| mark(ts))
                && entry.seek.last() != Some(&oldest)
            {
                // More than a page: walk back first, so messages are written oldest first.
                entry.seek.push(oldest);
                continue;
            }
            entry.seek.pop();
            let mut newest = entry.done.clone();
            let mut messages = Vec::new();
            for m in history.messages {
                newest = later(Some(newest), &m.ts);
                if !wanted_at(request, &m.ts) {
                    continue;
                }
                if let Some(thread_ts) = m.thread_ts.clone() {
                    let key = format!("{id}/{thread_ts}");
                    if m.is_reply() {
                        // A reply also sent to the channel: its thread has news.
                        let t = state.threads.entry(key).or_default();
                        t.latest = Some(later(t.latest.take(), &m.ts));
                        t.active = t.active.max(seconds(&m.ts));
                        continue;
                    }
                    if m.reply_count.unwrap_or(0) > 0 {
                        let t = state.threads.entry(key).or_default();
                        if let Some(latest) = &m.latest_reply {
                            t.latest = Some(later(t.latest.take(), latest));
                            t.active = t.active.max(seconds(latest));
                        }
                        batch
                            .entry(conversation.clone(), kind)
                            .threaded
                            .insert(m.ts.clone());
                    }
                }
                messages.push(m);
            }
            state
                .conversations
                .get_mut(*id)
                .expect("the conversation")
                .done = newest;
            batch.add(conversation, kind, messages);
        }
        if !order.is_empty() {
            state.next = Some(order[visited % order.len()].to_string());
        }

        // 3. Threads active in the last week, checked again now and then.
        let recheck = self.limits().recheck().as_secs();
        let stale: Vec<String> = state
            .threads
            .iter()
            .filter(|(_, t)| !t.due() && now.saturating_sub(t.checked) >= recheck)
            .map(|(k, _)| k.clone())
            .collect();
        for key in stale {
            if self.stopped() || batch.len() >= max {
                break;
            }
            self.thread(request, state, batch, &key, page, now, &shape_of)?;
        }

        // Members the list did not have, such as people of other workspaces in a shared
        // channel: a lookup that has to wait does not stop the run.
        let writers = wanted_people(batch);
        let blocked = std::mem::replace(&mut self.blocked, false);
        self.lookup_users(&writers)?;
        self.blocked = blocked;
        Ok(())
    }

    /// Reads the replies of one thread after the last one read.
    #[allow(clippy::too_many_arguments)]
    fn thread(
        &mut self,
        request: &FetchRequest<'_>,
        state: &mut State,
        batch: &mut LiveBatch,
        key: &str,
        page: usize,
        now: u64,
        shape_of: &dyn Fn(&str) -> Option<(Conversation, &'static str)>,
    ) -> Result<(), SourceError> {
        let Some((channel, thread_ts)) = key.split_once('/') else {
            state.threads.remove(key);
            return Ok(());
        };
        let Some(t) = state.threads.get(key).cloned() else {
            return Ok(());
        };
        let mut params = vec![
            ("channel", channel.to_string()),
            ("ts", thread_ts.to_string()),
            ("limit", page.to_string()),
        ];
        if let Some(done) = &t.done {
            params.push(("oldest", done.clone()));
            params.push(("inclusive", "false".to_string()));
        }
        let replies =
            match self.call::<Messages>(Tier::Messages, "conversations.replies", &params)? {
                Answer::Stopped => return Ok(()),
                Answer::Gone => {
                    state.threads.remove(key);
                    return Ok(());
                }
                Answer::Got(replies) => replies,
            };
        let Some((conversation, kind)) = shape_of(channel) else {
            state.threads.remove(key);
            return Ok(());
        };
        let entry = state.threads.get_mut(key).expect("the thread");
        let mut newest = entry.done.clone();
        let mut messages = Vec::new();
        let mut parent = false;
        for m in replies.messages {
            if m.ts == thread_ts {
                parent = true;
                if !wanted_at(request, &m.ts) {
                    continue;
                }
                messages.push(m);
                continue;
            }
            if !m.is_reply() || m.thread_ts.as_deref() != Some(thread_ts) {
                continue;
            }
            if newest.as_ref().is_some_and(|n| mark(&m.ts) <= mark(n)) {
                continue;
            }
            newest = Some(later(newest.take(), &m.ts));
            if wanted_at(request, &m.ts) {
                messages.push(m);
            }
        }
        let advanced = newest != entry.done;
        entry.more = replies.has_more && advanced;
        if let Some(newest) = &newest {
            entry.latest = Some(later(entry.latest.take(), newest));
            entry.active = entry.active.max(seconds(newest));
        }
        entry.done = newest.or_else(|| Some(thread_ts.to_string()));
        entry.active = entry.active.max(seconds(thread_ts));
        entry.checked = now;
        if messages.iter().any(Message::is_reply) {
            if parent {
                batch
                    .entry(conversation.clone(), kind)
                    .threaded
                    .insert(thread_ts.to_string());
            }
            batch.add(conversation, kind, messages);
        }
        Ok(())
    }
}

/// Whether a message sent at `ts` is in the history chosen.
fn wanted_at(request: &FetchRequest<'_>, ts: &str) -> bool {
    if time(ts).is_none() {
        return false;
    }
    let millis = ts
        .split_once('.')
        .map_or(ts, |(s, _)| s)
        .parse::<i64>()
        .map(|s| s * 1000)
        .unwrap_or(0);
    request.wants(Timestamp(millis))
}

fn wanted_people(batch: &LiveBatch) -> BTreeSet<String> {
    wanted(batch.conversations.values().flat_map(|c| &c.messages))
}

/// What a run read, for the Slack importer to write.
#[derive(Default)]
struct LiveBatch {
    users: Vec<User>,
    conversations: BTreeMap<String, LiveConversation>,
}

struct LiveConversation {
    conversation: Conversation,
    kind: &'static str,
    messages: Vec<Message>,
    threaded: BTreeSet<String>,
}

impl LiveBatch {
    fn entry(&mut self, conversation: Conversation, kind: &'static str) -> &mut LiveConversation {
        self.conversations
            .entry(conversation.id.clone())
            .or_insert_with(|| LiveConversation {
                conversation,
                kind,
                messages: Vec::new(),
                threaded: BTreeSet::new(),
            })
    }

    fn add(&mut self, conversation: Conversation, kind: &'static str, messages: Vec<Message>) {
        if messages.is_empty() {
            return;
        }
        let entry = self.entry(conversation, kind);
        for m in messages {
            if !entry.messages.iter().any(|e| e.ts == m.ts) {
                entry.messages.push(m);
            }
        }
    }
}

impl Batch for LiveBatch {
    fn len(&self) -> usize {
        self.conversations.values().map(|c| c.messages.len()).sum()
    }

    fn write(self: Box<Self>, space: &mut Space, by: &Provenance) -> Result<Written, SourceError> {
        let batch = *self;
        let mut writer = ChatWriter {
            space,
            by: by.clone(),
            report: ChatImportReport::new(ChatFormat::Slack),
        };
        let channels = batch
            .conversations
            .values()
            .filter_map(|c| {
                c.conversation
                    .name
                    .clone()
                    .filter(|_| matches!(c.kind, "channel" | "private-channel"))
                    .map(|n| (c.conversation.id.clone(), n))
            })
            .collect();
        let mut workspace = Workspace::new(batch.users, channels);
        let people = wanted(batch.conversations.values().flat_map(|c| &c.messages));
        write_people(&mut workspace, &people, &mut writer).map_err(chat_error)?;
        let mut files = Files::empty();
        for (_, c) in batch.conversations {
            if c.messages.is_empty() {
                continue;
            }
            let mut messages = c.messages;
            messages.sort_by_key(|m| mark(&m.ts));
            write_conversation(
                &workspace,
                &c.conversation,
                c.kind,
                messages,
                &c.threaded,
                &mut files,
                &mut writer,
            )
            .map_err(chat_error)?;
        }
        let report = writer.report;
        let created = report
            .chats
            .iter()
            .filter(|c| c.op == ChangeOp::Create)
            .count()
            + report.people.len();
        let updated = report
            .chats
            .iter()
            .filter(|c| c.op == ChangeOp::Update)
            .count();
        Ok(Written {
            items: report.messages,
            created,
            updated,
        })
    }
}

fn chat_error(error: ChatImportError) -> SourceError {
    match error {
        ChatImportError::Space(error) => SourceError::Space(error),
        other => SourceError::Failed(other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limits_by_name() {
        assert_eq!(Limits::from_name("marketplace"), Limits::Marketplace);
        assert_eq!(Limits::from_name("Marketplace "), Limits::Marketplace);
        assert_eq!(Limits::from_name(""), Limits::NotListed);
        assert_eq!(Limits::NotListed.page(), 15);
        assert_eq!(
            Limits::NotListed.interval(Tier::Messages),
            Duration::from_secs(60)
        );
    }

    #[test]
    fn threads_are_due_until_read() {
        let mut t = ThreadState::default();
        assert!(t.due());
        t.done = Some("10.000001".into());
        t.latest = Some("10.000001".into());
        assert!(!t.due());
        t.latest = Some("11.000000".into());
        assert!(t.due());
        t.latest = None;
        assert!(!t.due());
        t.more = true;
        assert!(t.due());
    }

    #[test]
    fn group_messages_name_their_members() {
        let users = vec![
            User {
                id: "U1".into(),
                team_id: None,
                name: "ada".into(),
                real_name: None,
                profile: None,
            },
            User {
                id: "U2".into(),
                team_id: None,
                name: "grace".into(),
                real_name: None,
                profile: None,
            },
        ];
        let c = ApiConversation {
            id: "G1".into(),
            name: Some("mpdm-ada--grace-1".into()),
            is_im: false,
            is_mpim: true,
            is_private: true,
            is_group: false,
            is_archived: false,
            is_member: None,
            user: None,
        };
        let (conversation, kind) = shape(&c, "U1", &users);
        assert_eq!(kind, "group");
        assert_eq!(conversation.members, ["U1", "U2"]);
    }
}
