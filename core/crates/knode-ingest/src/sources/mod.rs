//! Sources: one interface for everything that brings things into a space.
//!
//! A *source* is an importer (it runs once, on a file or folder the person gives it) or a
//! *connector* (it keeps running in Knode Service and brings in what is new). Every source has
//! an id, a name, its kind, what it reads and the permissions it needs ([`SourceInfo`]), and
//! writes nodes only through the space API, with provenance naming it ([`provenance`]).
//!
//! A connector implements [`Connector`]. Knode Service runs it for every space it is switched
//! on in, and keeps everything around it: the history chosen when connecting ([`History`]), the
//! chats, channels or folders the person excluded ([`Container`]), where it stopped
//! ([`Cursor`]), when it last ran and what went wrong, with backoff after errors. A run is:
//!
//! 1. [`Connector::check`]: a connector that reads another app's database checks the tables
//!    and columns it expects. If they changed it returns [`SourceError::NeedsUpdate`]: nothing
//!    is written and the person is told the connector needs an update.
//! 2. [`Connector::fetch`] with the saved cursor: the connector reads what is new after it,
//!    leaving out excluded containers and, for [`History::FromToday`], anything older than the
//!    day it was connected, and returns a [`Batch`] and the cursor after it.
//! 3. The service writes the batch into the space with [`Batch::write`], which hands the items
//!    to the importer of that source, and only then saves the new cursor. A run cut short
//!    after the write but before the cursor is saved reads the same items again; the importers
//!    find what they wrote before by the service's own ids (a chat's JID, a channel id, a
//!    mail's `Message-ID`) and add nothing twice. The same holds for two devices running the
//!    same connector into one space.
//!
//! Connectors that read another app's database read a copy of it, never the live file, and
//! never write to it: see [`sqlite`].
//!
//! Secrets a connector needs (a session, an OAuth token, an app password) are kept by the
//! service in the system keychain and reached through [`Secrets`]; a connector never writes
//! them into a space, a settings file or a log.
//!
//! A connector that reads an account ([`Permission::SignIn`]) is signed in step by step with
//! [`Connector::sign_in`] ([`SignInStep`], [`SignInState`]); the account belongs to the
//! machine, not to a space.
//!
//! # Plugins
//!
//! The plugin API ([knode#61](https://github.com/knodemd/knode/issues/61)) exposes this same
//! interface instead of inventing its own, so a first-party source could later ship as a plugin
//! and a third-party source looks the same in Settings > Sources:
//!
//! - A plugin declares its [`SourceInfo`] and [`Trigger`] in its manifest, and the service
//!   asks for the permissions it lists when the person connects it, never before.
//! - The plugin host, sandboxed in WebAssembly, implements [`Connector`] for the plugin:
//!   [`Connector::fetch`] calls the plugin's `fetch` export with the request as JSON (the cursor,
//!   the history, `since` and the excluded containers) and reads back the cursor and the items.
//!   Everything here is plain data and serializes for that boundary; [`Secrets`] becomes a host
//!   import scoped to the plugin.
//! - A plugin's items are not written by the plugin: they are written by a built-in importer of
//!   a known shape (chat messages, mail, notes) chosen in its manifest, through the space API,
//!   with provenance naming the plugin. A plugin never opens a space.
//!
//! See decisions 0010 (sources and plugins) and 0013 (live connectors) of the Knode handbook.

pub mod sqlite;

use std::fmt;
use std::io;
use std::path::PathBuf;
use std::time::Duration;

use knode_format::{ActorKind, Provenance, Space, Timestamp};
use serde::{Deserialize, Serialize};

/// Whether a source runs once or keeps running.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SourceKind {
    /// Runs once on a file, a folder or a URL the person gives it.
    Importer,
    /// Keeps running in Knode Service and brings in what is new.
    Connector,
}

/// A permission a source needs from the system or the person, asked for when the source is
/// switched on, never before (decision 0010).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum Permission {
    /// macOS Full Disk Access, to read another app's files (Mail, Messages, WhatsApp).
    FullDiskAccess,
    /// Signing in to an account of the service, once; the session is kept in the keychain.
    SignIn,
    /// Reaching the service over the network.
    Network,
    /// The calendars of the system.
    Calendar,
    /// The contacts of the system.
    Contacts,
    /// The microphone.
    Microphone,
    /// The photo library.
    Photos,
}

impl Permission {
    /// The permission for people: `Full Disk Access`, `Signing in`...
    pub fn describe(self) -> &'static str {
        match self {
            Permission::FullDiskAccess => "Full Disk Access",
            Permission::SignIn => "Signing in",
            Permission::Network => "Network",
            Permission::Calendar => "Calendar",
            Permission::Contacts => "Contacts",
            Permission::Microphone => "Microphone",
            Permission::Photos => "Photos",
        }
    }
}

impl fmt::Display for Permission {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.describe())
    }
}

/// What a source is, for the list of sources.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceInfo {
    /// Its id: lower case letters, digits and `-`, such as `whatsapp`, `imessage` or
    /// `apple-mail`. It never changes: the settings of a space and the saved cursors use it. A
    /// connector and the importer it hands its items to may share an id (`telegram`); ids are
    /// unique among connectors.
    pub id: String,
    /// Its name for people: `WhatsApp`, `Messages`, `Apple Mail`.
    pub name: String,
    /// Importer or connector.
    pub kind: SourceKind,
    /// What it reads, for people: "The chats of WhatsApp for Mac".
    pub reads: String,
    /// The permissions it needs.
    #[serde(default)]
    pub permissions: Vec<Permission>,
    /// What can be excluded, in the singular, for people: `chat`, `channel`, `folder`. `None`
    /// when nothing can be.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub container: Option<String>,
}

impl SourceInfo {
    /// A connector with this id, name and description, needing no permission.
    pub fn connector(
        id: impl Into<String>,
        name: impl Into<String>,
        reads: impl Into<String>,
    ) -> Self {
        SourceInfo {
            id: id.into(),
            name: name.into(),
            kind: SourceKind::Connector,
            reads: reads.into(),
            permissions: Vec::new(),
            container: None,
        }
    }

    /// The same source, needing these permissions.
    pub fn needing(mut self, permissions: impl IntoIterator<Item = Permission>) -> Self {
        self.permissions.extend(permissions);
        self
    }

    /// The same source, whose `container`s (`chat`, `channel`, `folder`) can be excluded.
    pub fn excluding(mut self, container: impl Into<String>) -> Self {
        self.container = Some(container.into());
        self
    }
}

/// Whether `id` can be the id of a source: 1 to 48 lower case ASCII letters, digits and `-`,
/// starting with a letter.
pub fn is_source_id(id: &str) -> bool {
    (1..=48).contains(&id.len())
        && id.starts_with(|c: char| c.is_ascii_lowercase())
        && id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// The provenance of every node a source writes: Knode itself (`system`), acting as the source,
/// named by its id. Importers and batches may add where the content came from with
/// [`Provenance::with_source`].
pub fn provenance(source_id: &str) -> Provenance {
    Provenance::new(ActorKind::System, source_id)
}

/// When Knode Service runs a connector.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Trigger {
    /// When a file under one of these paths changes (a local database and its `-wal`), and at
    /// least every `every`, in case an event was missed.
    Watch {
        /// The files or folders to watch.
        paths: Vec<PathBuf>,
        /// The longest time between two runs.
        every: Duration,
    },
    /// Every so often: for network APIs.
    Interval(Duration),
}

impl Default for Trigger {
    /// Every five minutes.
    fn default() -> Self {
        Trigger::Interval(Duration::from_secs(300))
    }
}

/// How much history a connector brings in, chosen when it is connected to a space and kept
/// after that. Older history can still come from an export.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum History {
    /// What happens from the day it was connected on. The default.
    #[default]
    FromToday,
    /// Everything the source has.
    Everything,
}

impl History {
    /// Its id: `from-today` or `everything`.
    pub fn id(self) -> &'static str {
        match self {
            History::FromToday => "from-today",
            History::Everything => "everything",
        }
    }

    /// The history with this [`id`](Self::id) (`today` is taken for `from-today`).
    pub fn from_id(id: &str) -> Option<History> {
        match id.trim().to_ascii_lowercase().replace('_', "-").as_str() {
            "from-today" | "today" => Some(History::FromToday),
            "everything" | "all" => Some(History::Everything),
            _ => None,
        }
    }
}

/// The start of today in the local time of this machine, in milliseconds since 1970: the
/// [`FetchRequest::since`] of a connector connected now with [`History::FromToday`].
pub fn start_of_today() -> Timestamp {
    use chrono::{Local, TimeZone};
    let now = Local::now();
    let midnight = now
        .date_naive()
        .and_hms_opt(0, 0, 0)
        .and_then(|t| Local.from_local_datetime(&t).earliest())
        .unwrap_or(now);
    Timestamp(midnight.timestamp_millis())
}

/// Where a connector stopped in its source, for one space: an opaque text only the connector
/// reads (a row id, a timestamp, an API's page token, JSON...). It must not hold a secret or
/// the content of what was read.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Cursor(pub String);

impl Cursor {
    /// A cursor with this text.
    pub fn new(text: impl Into<String>) -> Self {
        Cursor(text.into())
    }

    /// Its text.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A chat, channel or mail folder of a source, as the person sees it: its id in the service
/// (a WhatsApp JID, a Slack channel id, a mailbox path) and its name. Exclusions are lists of
/// these.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Container {
    /// Its id in the service. Never changes.
    pub id: String,
    /// Its name for people, when it was excluded or listed.
    pub name: String,
}

impl Container {
    /// A container with this id and name.
    pub fn new(id: impl Into<String>, name: impl Into<String>) -> Self {
        Container {
            id: id.into(),
            name: name.into(),
        }
    }
}

/// Where a connector keeps its secrets: the system keychain, through Knode Service. Names are
/// the connector's own (`session`, `refresh-token`) and never collide with another
/// connector's.
pub trait Secrets: Send + Sync {
    /// The secret, or `None` when there is none.
    fn get(&self, name: &str) -> io::Result<Option<String>>;
    /// Keeps the secret, replacing the one it had.
    fn set(&self, name: &str, value: &str) -> io::Result<()>;
    /// Forgets the secret. Returns whether there was one.
    fn delete(&self, name: &str) -> io::Result<bool>;
}

/// No secrets at all: every `get` is `None` and `set` fails. For connectors that need none, and
/// for tests.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoSecrets;

impl Secrets for NoSecrets {
    fn get(&self, _name: &str) -> io::Result<Option<String>> {
        Ok(None)
    }

    fn set(&self, _name: &str, _value: &str) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "this source keeps no secrets",
        ))
    }

    fn delete(&self, _name: &str) -> io::Result<bool> {
        Ok(false)
    }
}

/// What a connector is asked to read, for one space.
pub struct FetchRequest<'a> {
    /// The id of the space being written (its `space.id`).
    pub space_id: &'a str,
    /// Where the last run stopped, or `None` for the first run.
    pub cursor: Option<&'a Cursor>,
    /// The history chosen when the connector was connected.
    pub history: History,
    /// The start of the day it was connected (local time), in milliseconds since 1970. With
    /// [`History::FromToday`] nothing older than this is read.
    pub since: Timestamp,
    /// The containers the person excluded. They are never read.
    pub excluded: &'a [Container],
    /// The most items one batch should hold; a connector with more sets [`Fetched::more`].
    pub max_items: usize,
    /// The connector's secrets.
    pub secrets: &'a dyn Secrets,
}

impl FetchRequest<'_> {
    /// Whether the container `id` was excluded: if so, nothing of it is read.
    pub fn is_excluded(&self, id: &str) -> bool {
        self.excluded.iter().any(|c| c.id == id)
    }

    /// Whether something that happened at `at` is in the history chosen: always with
    /// [`History::Everything`], from [`since`](Self::since) on with [`History::FromToday`].
    pub fn wants(&self, at: Timestamp) -> bool {
        match self.history {
            History::Everything => true,
            History::FromToday => at >= self.since,
        }
    }
}

impl fmt::Debug for FetchRequest<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FetchRequest")
            .field("space_id", &self.space_id)
            .field("cursor", &self.cursor)
            .field("history", &self.history)
            .field("since", &self.since)
            .field("excluded", &self.excluded)
            .field("max_items", &self.max_items)
            .finish_non_exhaustive()
    }
}

/// What a connector read: the items to write and where to start next time.
pub struct Fetched {
    /// The items read, written into the space through the source's importer. `None` when
    /// there was nothing new.
    pub batch: Option<Box<dyn Batch>>,
    /// Where the next fetch starts. Saved only once the batch is written.
    pub cursor: Cursor,
    /// Whether there is more to read now: the service fetches again at once.
    pub more: bool,
}

impl Fetched {
    /// Nothing new; the cursor stays where it is.
    pub fn nothing(cursor: Cursor) -> Self {
        Fetched {
            batch: None,
            cursor,
            more: false,
        }
    }

    /// A batch, and the cursor after it.
    pub fn batch(batch: impl Batch + 'static, cursor: Cursor) -> Self {
        Fetched {
            batch: Some(Box::new(batch)),
            cursor,
            more: false,
        }
    }

    /// The same, saying there is more to read now.
    pub fn with_more(mut self, more: bool) -> Self {
        self.more = more;
        self
    }
}

impl fmt::Debug for Fetched {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Fetched")
            .field("items", &self.batch.as_ref().map(|b| b.len()))
            .field("cursor", &self.cursor)
            .field("more", &self.more)
            .finish()
    }
}

/// Items a connector read, ready to be written into a space by the importer of its source.
pub trait Batch: Send {
    /// How many items it holds (messages, mails, events).
    fn len(&self) -> usize;

    /// Whether it holds nothing.
    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Writes the items into `space` with the source's importer, as `by` (see [`provenance`]).
    /// Called with the space locked. Writing the same items twice must add nothing the second
    /// time: importers find what they wrote by the service's own ids.
    fn write(self: Box<Self>, space: &mut Space, by: &Provenance) -> Result<Written, SourceError>;
}

/// What writing a batch did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Written {
    /// Items written that were not in the space before (messages, mails, events).
    pub items: usize,
    /// Nodes created.
    pub created: usize,
    /// Nodes changed.
    pub updated: usize,
}

impl std::ops::AddAssign for Written {
    fn add_assign(&mut self, other: Written) {
        self.items += other.items;
        self.created += other.created;
        self.updated += other.updated;
    }
}

/// Why a connector could not run.
#[derive(Debug)]
#[non_exhaustive]
pub enum SourceError {
    /// The other app's database is not what the connector expects (its tables or columns
    /// changed): nothing was written, and the connector needs an update.
    NeedsUpdate(String),
    /// The source is not on this machine: the app is not installed, or this system has none.
    Unavailable(String),
    /// A permission is missing, such as Full Disk Access.
    Permission(Permission, String),
    /// The connector is not signed in, or its session was refused: connect it again.
    SignIn(String),
    /// Something else went wrong, such as the network: the service tries again later.
    Failed(String),
    /// The space could not be written.
    Space(knode_format::Error),
}

impl SourceError {
    /// A stable name for the error: `needs-update`, `unavailable`, `permission`, `sign-in`,
    /// `failed` or `space`.
    pub fn kind(&self) -> &'static str {
        match self {
            SourceError::NeedsUpdate(_) => "needs-update",
            SourceError::Unavailable(_) => "unavailable",
            SourceError::Permission(..) => "permission",
            SourceError::SignIn(_) => "sign-in",
            SourceError::Failed(_) => "failed",
            SourceError::Space(_) => "space",
        }
    }

    /// Whether the person has to do something (update Knode, grant a permission, sign in)
    /// before trying again helps.
    pub fn needs_person(&self) -> bool {
        !matches!(self, SourceError::Failed(_) | SourceError::Space(_))
    }
}

impl fmt::Display for SourceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SourceError::NeedsUpdate(what) => {
                write!(f, "this connector needs an update: {what}")
            }
            SourceError::Unavailable(what) => write!(f, "not available here: {what}"),
            SourceError::Permission(permission, what) => {
                write!(f, "{permission} is needed: {what}")
            }
            SourceError::SignIn(what) => write!(f, "sign in again: {what}"),
            SourceError::Failed(what) => f.write_str(what),
            SourceError::Space(error) => write!(f, "the space could not be written: {error}"),
        }
    }
}

impl std::error::Error for SourceError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            SourceError::Space(error) => Some(error),
            _ => None,
        }
    }
}

impl From<knode_format::Error> for SourceError {
    fn from(error: knode_format::Error) -> Self {
        SourceError::Space(error)
    }
}

impl From<io::Error> for SourceError {
    fn from(error: io::Error) -> Self {
        SourceError::Failed(error.to_string())
    }
}

/// A step of signing in to the account a connector reads, sent by the person: first the
/// account (a phone number), then the code the service sent, then a password if the account
/// has one. Never logged or kept: the connector keeps only the session it gets.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "step", rename_all = "kebab-case")]
pub enum SignInStep {
    /// Start signing in to this account, such as a phone number in international format.
    Start {
        /// The account.
        account: String,
    },
    /// The code the service sent.
    Code {
        /// The code.
        code: String,
    },
    /// The password of the account, when it has one (two-step verification).
    Password {
        /// The password.
        password: String,
    },
}

impl fmt::Debug for SignInStep {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Codes and passwords never reach a log.
        f.write_str(match self {
            SignInStep::Start { .. } => "SignInStep::Start",
            SignInStep::Code { .. } => "SignInStep::Code",
            SignInStep::Password { .. } => "SignInStep::Password",
        })
    }
}

/// Where signing in to a connector's account is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "kebab-case")]
pub enum SignInState {
    /// Not signed in: send [`SignInStep::Start`].
    SignedOut,
    /// The service sent a code: send [`SignInStep::Code`].
    CodeSent {
        /// Where it was sent, for people: "the Telegram app", "an SMS".
        #[serde(default, skip_serializing_if = "Option::is_none")]
        via: Option<String>,
    },
    /// The account has a password: send [`SignInStep::Password`].
    PasswordNeeded {
        /// The hint the person gave for it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        hint: Option<String>,
    },
    /// Signed in; the session is in the system keychain.
    SignedIn {
        /// The account, for people: a name.
        account: String,
    },
}

/// The error of a connector with no account to sign in to.
fn no_account(info: &SourceInfo) -> SourceError {
    SourceError::Unavailable(format!("{} has no account to sign in to", info.name))
}

/// A source that keeps running in Knode Service and brings in what is new.
///
/// One value serves every space the connector is switched on in: what differs by space comes
/// in the [`FetchRequest`]. Methods are called from a blocking thread, one at a time per
/// connector; a connector that talks to an async API runs it on its own (for example with
/// `tokio::runtime::Handle::block_on` from the service's runtime).
pub trait Connector: Send + Sync {
    /// Its id, name, what it reads and the permissions it needs.
    fn info(&self) -> SourceInfo;

    /// When it runs. Every five minutes unless it says otherwise.
    fn trigger(&self) -> Trigger {
        Trigger::default()
    }

    /// Checks, before each run, that the source is what the connector expects: for a
    /// connector reading another app's database, that the tables and columns it reads are
    /// there (see [`sqlite::check_schema`]). [`SourceError::NeedsUpdate`] stops the run before
    /// anything is read or written. Fine unless the connector says otherwise.
    fn check(&self) -> Result<(), SourceError> {
        Ok(())
    }

    /// The chats, channels or folders the source has now, for the person to pick the ones to
    /// exclude. None unless the connector says otherwise.
    fn containers(&self, request: &FetchRequest<'_>) -> Result<Vec<Container>, SourceError> {
        let _ = request;
        Ok(Vec::new())
    }

    /// Reads what is new after `request.cursor`, at most about `request.max_items` items,
    /// leaving out excluded containers and what is older than the history chosen.
    fn fetch(&self, request: &FetchRequest<'_>) -> Result<Fetched, SourceError>;

    /// Whether the connector is signed in to the account it reads, for a connector that
    /// needs [`Permission::SignIn`]. The account is the same for every space: it belongs to
    /// this machine. Unavailable unless the connector says otherwise.
    fn sign_in_state(&self, secrets: &dyn Secrets) -> Result<SignInState, SourceError> {
        let _ = secrets;
        Err(no_account(&self.info()))
    }

    /// Takes one step of signing in and says what comes next. The session the connector gets
    /// at the end goes into `secrets`, nowhere else.
    fn sign_in(&self, step: SignInStep, secrets: &dyn Secrets) -> Result<SignInState, SourceError> {
        let _ = (step, secrets);
        Err(no_account(&self.info()))
    }

    /// Signs out of the account and forgets the session kept in `secrets`. Returns whether
    /// it was signed in.
    fn sign_out(&self, secrets: &dyn Secrets) -> Result<bool, SourceError> {
        let _ = secrets;
        Err(no_account(&self.info()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_ids() {
        assert!(is_source_id("whatsapp"));
        assert!(is_source_id("apple-mail"));
        assert!(!is_source_id("Apple"));
        assert!(!is_source_id("1mail"));
        assert!(!is_source_id(""));
        assert!(!is_source_id("a.b"));
    }

    #[test]
    fn history_ids() {
        assert_eq!(History::default(), History::FromToday);
        for h in [History::FromToday, History::Everything] {
            assert_eq!(History::from_id(h.id()), Some(h));
        }
        assert_eq!(History::from_id("today"), Some(History::FromToday));
        assert_eq!(History::from_id("never"), None);
    }

    #[test]
    fn a_request_says_what_is_wanted() {
        let excluded = [Container::new("c1", "Family")];
        let request = FetchRequest {
            space_id: "s",
            cursor: None,
            history: History::FromToday,
            since: Timestamp(1000),
            excluded: &excluded,
            max_items: 10,
            secrets: &NoSecrets,
        };
        assert!(request.is_excluded("c1"));
        assert!(!request.is_excluded("c2"));
        assert!(!request.wants(Timestamp(999)));
        assert!(request.wants(Timestamp(1000)));
        let everything = FetchRequest {
            history: History::Everything,
            ..request
        };
        assert!(everything.wants(Timestamp(0)));
    }

    #[test]
    fn today_starts_before_now() {
        let start = start_of_today();
        let now = Timestamp::now();
        assert!(start <= now);
        assert!(now.0 - start.0 <= 25 * 3600 * 1000);
    }

    #[test]
    fn provenance_names_the_source() {
        let by = provenance("whatsapp");
        assert_eq!(by.kind, ActorKind::System);
        assert_eq!(by.actor, "whatsapp");
    }
}
