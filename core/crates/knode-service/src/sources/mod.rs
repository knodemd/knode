//! Sources in Knode Service: the importers and connectors of every space, their settings and
//! their state, and the runtime that keeps connectors running.
//!
//! A connector implements [`knode_ingest::sources::Connector`]. Its settings for a space, the
//! person's choices, live in the space itself, so they follow it to every device:
//!
//! - the setting `source.<id>` of the space holds a [`SourceSetting`] as JSON: whether it is
//!   on, the history chosen when connecting and the day it was connected, and the chats,
//!   channels or folders excluded. No setting means the source was never connected and is off.
//!
//! What a connector did on this machine lives in the Knode home, because it describes this
//! machine's copy of the source (a row of this Mac's Messages database means nothing on
//! another device):
//!
//! - `sources/state.json` ([`STATE_FILE`]) holds a [`RunState`] per space and source: the
//!   cursor, when it last ran, how many items it wrote, its last error and when it runs next.
//!   It is only readable by its owner. Losing it only means reading again what was read: the
//!   importers find what they wrote by the service's own ids and add nothing twice, and the
//!   same is true for two devices running the same connector into one space.
//!
//! Secrets connectors need are kept in the credential store agent keys use (the Keychain on
//! macOS, the Secret Service on Linux), through [`SourceSecrets`], never in a space, a settings
//! file or a log.
//!
//! The functions here work on an open space, so the CLI can use them when the service cannot
//! run; the service calls them for `sources.*` and wakes the runtime after each change. Only a
//! person changes the settings of a source.

pub mod apple_mail;
pub mod gmail;
pub mod imap;
mod mail_live;
#[cfg(test)]
mod mail_test_support;
pub(crate) mod runtime;
pub mod telegram;

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use knode_format::{Provenance, Space, Timestamp};
pub use knode_ingest::sources::{
    Batch, Connector, Container, Cursor, FetchRequest, Fetched, FileStamp, History, NoSecrets,
    Permission, Secrets, SignInState, SignInStep, SourceError, SourceInfo, SourceKind, Trigger,
    Written, is_source_id, provenance, sqlite, start_of_today,
};
use serde::{Deserialize, Serialize};

use crate::import::ImportSource;
use crate::keys::{KeyStore, default_key_store, write_private};

pub(crate) use runtime::SourcesTask;

/// The prefix of the setting of a source in a space: `source.<id>`.
pub const SETTING_PREFIX: &str = "source.";
/// The folder in the Knode home where the service keeps what connectors did.
pub const SOURCES_DIR: &str = "sources";
/// The file in [`SOURCES_DIR`] with the [`RunState`] of every space and source.
pub const STATE_FILE: &str = "state.json";

/// The connectors built into this version of Knode, for this system.
///
/// Each connector adds itself here once it implements [`Connector`].
// Off macOS the connectors of the Mac are left out, and the first push follows `Vec::new()`.
#[allow(clippy::vec_init_then_push)]
pub fn builtin_connectors() -> Vec<Arc<dyn Connector>> {
    let mut connectors: Vec<Arc<dyn Connector>> = Vec::new();
    #[cfg(target_os = "macos")]
    if let Some(apple_mail) = apple_mail::AppleMailConnector::local() {
        connectors.push(Arc::new(apple_mail));
    }
    #[cfg(target_os = "macos")]
    if let Some(imessage) = knode_ingest::imessage::IMessageConnector::local() {
        connectors.push(Arc::new(imessage));
    }
    // WhatsApp for Mac, in the person's home folder. Listing it reads nothing: it runs only
    // in the spaces it is switched on in.
    #[cfg(target_os = "macos")]
    if let Some(home) = std::env::var_os("HOME").filter(|h| !h.is_empty()) {
        use knode_ingest::whatsapp_mac::{WhatsAppMac, WhatsAppMacConnector};
        connectors.push(Arc::new(WhatsAppMacConnector::new(
            WhatsAppMac::for_home(home),
            "Me",
        )));
    }
    connectors.push(Arc::new(telegram::connector()));
    connectors.push(Arc::new(gmail::GmailConnector::new()));
    connectors.push(Arc::new(imap::ImapConnector::new()));
    connectors
}

/// How Knode Service runs connectors.
#[derive(Clone)]
pub struct SourcesOptions {
    /// The connectors it knows. [`builtin_connectors`] by default.
    pub connectors: Vec<Arc<dyn Connector>>,
    /// How often `config.toml` and the settings of the spaces are read again, besides when a
    /// client changes them.
    pub settings_poll: Duration,
    /// The first wait after an error; it doubles with every error in a row.
    pub backoff: Duration,
    /// The longest wait after errors.
    pub max_backoff: Duration,
    /// How long files a connector watches must be quiet before it runs.
    pub quiet: Duration,
    /// How often a connector that watches files is looked at, whatever its own `every` says:
    /// macOS reports writes to a file another app keeps open late, so the events alone do not
    /// bring things in quickly. It runs then only when its files changed, or its last run did
    /// not read everything. A minute by default.
    pub watch_every: Duration,
    /// The most items a connector is asked for in one batch.
    pub max_items: usize,
    /// The most batches in one run, so one connector never keeps the others waiting long.
    pub max_batches: usize,
}

impl Default for SourcesOptions {
    fn default() -> Self {
        SourcesOptions {
            connectors: builtin_connectors(),
            settings_poll: Duration::from_secs(5),
            backoff: Duration::from_secs(30),
            max_backoff: Duration::from_secs(3600),
            quiet: Duration::from_secs(2),
            watch_every: Duration::from_secs(60),
            max_items: 500,
            max_batches: 20,
        }
    }
}

impl SourcesOptions {
    /// The default options with these connectors instead of the built-in ones.
    pub fn with_connectors(connectors: Vec<Arc<dyn Connector>>) -> Self {
        SourcesOptions {
            connectors,
            ..SourcesOptions::default()
        }
    }

    fn ids(&self) -> Vec<String> {
        self.connectors.iter().map(|c| c.info().id).collect()
    }
}

impl std::fmt::Debug for SourcesOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SourcesOptions")
            .field("connectors", &self.ids())
            .field("settings_poll", &self.settings_poll)
            .field("backoff", &self.backoff)
            .field("max_backoff", &self.max_backoff)
            .field("quiet", &self.quiet)
            .field("watch_every", &self.watch_every)
            .field("max_items", &self.max_items)
            .field("max_batches", &self.max_batches)
            .finish()
    }
}

impl PartialEq for SourcesOptions {
    fn eq(&self, other: &Self) -> bool {
        self.ids() == other.ids()
            && self.settings_poll == other.settings_poll
            && self.backoff == other.backoff
            && self.max_backoff == other.max_backoff
            && self.quiet == other.quiet
            && self.watch_every == other.watch_every
            && self.max_items == other.max_items
            && self.max_batches == other.max_batches
    }
}

impl Eq for SourcesOptions {}

/// The importers of this version, as sources: listed with the connectors, run with
/// `import.run` (`knode import`).
pub fn importers() -> Vec<SourceInfo> {
    ImportSource::ALL.into_iter().map(importer_info).collect()
}

fn importer_info(source: ImportSource) -> SourceInfo {
    use Permission::{FullDiskAccess, Network};
    let (name, reads, permissions): (&str, &str, &[Permission]) = match source {
        ImportSource::Export => (
            "Data exports",
            "A Claude, ChatGPT or Gemini (Google Takeout) data export",
            &[],
        ),
        ImportSource::Markdown => (
            "Markdown",
            "A folder of Markdown files, such as an Obsidian vault, or one .md file",
            &[],
        ),
        ImportSource::Session => ("Agent sessions", "A Claude Code or Codex session file", &[]),
        ImportSource::Pdf => ("PDF", "A PDF document", &[]),
        ImportSource::Web => (
            "Web pages",
            "A saved web page, or one fetched from its address",
            &[Network],
        ),
        ImportSource::Kindle => (
            "Kindle",
            "Kindle highlights: My Clippings.txt or a notebook",
            &[],
        ),
        ImportSource::Readwise => ("Readwise", "A Readwise CSV or Markdown export", &[]),
        ImportSource::ReadwiseApi => (
            "Readwise API",
            "Readwise highlights through its API, what changed since the last sync",
            &[Network],
        ),
        ImportSource::AppleNotes => (
            "Apple Notes",
            "The notes of Apple Notes, or notes it exported",
            &[FullDiskAccess],
        ),
        ImportSource::Bear => (
            "Bear",
            "The notes of Bear: its database, a backup or an export",
            &[FullDiskAccess],
        ),
        ImportSource::Notion => ("Notion", "A Notion Markdown & CSV export", &[]),
        ImportSource::Evernote => ("Evernote", "Evernote .enex files", &[]),
        ImportSource::Mbox => (
            "Mail (mbox)",
            "An .mbox file: Gmail through Google Takeout, or a mailbox another app exported",
            &[],
        ),
        ImportSource::AppleMail => (
            "Apple Mail",
            "The mail of Apple Mail on this Mac, or a folder of .emlx files",
            &[FullDiskAccess],
        ),
        ImportSource::WhatsApp => ("WhatsApp export", "A WhatsApp chat export", &[]),
        ImportSource::Telegram => ("Telegram export", "A Telegram Desktop export in JSON", &[]),
        ImportSource::Slack => ("Slack export", "A Slack workspace export", &[]),
    };
    SourceInfo {
        id: source.id().to_string(),
        name: name.to_string(),
        kind: SourceKind::Importer,
        reads: reads.to_string(),
        permissions: permissions.to_vec(),
        container: None,
    }
}

/// The settings of a connector in a space, kept as JSON in its `source.<id>` setting: the
/// person's choices, the same on every device.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceSetting {
    /// Whether it runs: off when paused.
    pub on: bool,
    /// The history chosen when it was connected. It never changes; disconnecting and
    /// connecting again chooses again.
    pub history: History,
    /// When it was connected.
    pub connected_at: Timestamp,
    /// The start of that day, in local time: with [`History::FromToday`], nothing older is
    /// brought in.
    pub since: Timestamp,
    /// The chats, channels or folders excluded: never read.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exclude: Vec<Container>,
}

/// What a connector did on this machine for one space, kept in [`STATE_FILE`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunState {
    /// Where it stopped. Never shown: only the connector reads it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<Cursor>,
    /// When it last ran, well or not.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_run: Option<Timestamp>,
    /// When it last ran without an error.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_success: Option<Timestamp>,
    /// The items its last run wrote.
    #[serde(default)]
    pub last_items: usize,
    /// The items it wrote in all, since it was connected.
    #[serde(default)]
    pub items: u64,
    /// The error of its last run, if it failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<LastError>,
    /// What the connector said the person should know after its last run that went well.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notice: Option<String>,
    /// How many runs in a row failed.
    #[serde(default)]
    pub failures: u32,
    /// When it runs next, at the latest.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_run: Option<Timestamp>,
}

/// The error of a run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LastError {
    /// A stable name: `needs-update`, `unavailable`, `permission`, `sign-in`, `failed` or
    /// `space` (see [`SourceError::kind`]).
    pub kind: String,
    /// What went wrong, for people.
    pub message: String,
    /// When.
    pub at: Timestamp,
}

/// What a source is doing in a space.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SourceState {
    /// An importer: it runs when the person imports something.
    Importer,
    /// Not connected to the space.
    Off,
    /// Connected, and paused.
    Paused,
    /// Connected, but this machine has no such connector (another system, or an older Knode).
    Unavailable,
    /// Connected, but `sources` or `capture` is off in `config.toml`, or Knode Service is not
    /// running it.
    Stopped,
    /// Running now.
    Running,
    /// Waiting for its next run.
    Idle,
    /// Its last run failed; it tries again later.
    Failing,
    /// The other app's database changed: nothing is written until Knode is updated.
    NeedsUpdate,
    /// The person has to do something: grant a permission, or sign in again.
    NeedsAttention,
}

/// A source in a space: what it is, the person's settings and what it did here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceStatus {
    /// What the source is.
    #[serde(flatten)]
    pub info: SourceInfo,
    /// What it is doing.
    pub state: SourceState,
    /// Its settings in the space, when it is connected.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub setting: Option<SourceSetting>,
    /// When it last ran.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_run: Option<Timestamp>,
    /// When it last ran without an error.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_success: Option<Timestamp>,
    /// The items its last run wrote.
    #[serde(default)]
    pub last_items: usize,
    /// The items it wrote in all.
    #[serde(default)]
    pub items: u64,
    /// The error of its last run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<LastError>,
    /// What the connector said the person should know after its last run that went well, such
    /// as what it could not bring in yet.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notice: Option<String>,
    /// When it runs next, at the latest.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_run: Option<Timestamp>,
}

/// The sources of one space, as `sources.status` returns them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpaceSources {
    /// The space, as the service opened it.
    pub space: PathBuf,
    /// Its name.
    pub name: String,
    /// The connectors connected to it.
    pub sources: Vec<SourceStatus>,
}

/// A chat, channel or folder of a source, and whether it is excluded: `sources.containers`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContainerInfo {
    /// The container.
    #[serde(flatten)]
    pub container: Container,
    /// Whether the person excluded it.
    pub excluded: bool,
}

/// The params of every `sources.*` method; each uses the fields it needs.
#[derive(Debug, Default, Serialize, Deserialize)]
pub(crate) struct SourceParams {
    /// The space. Every method but `sources.status` needs it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub space: Option<PathBuf>,
    /// The id of the source.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// The history, for `sources.connect`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub history: Option<History>,
    /// Whether `sources.connect` turns the source on (the default) or connects it paused.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on: Option<bool>,
    /// The id of a chat, channel or folder, for `sources.exclude` and `sources.include`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// Its name, for `sources.exclude`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Who makes the change.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub by: Option<Provenance>,
    /// The step of signing in, for `sources.sign_in`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step: Option<SignInStep>,
}

/// The connectors a service knows, with what they did on this machine.
pub(crate) struct Registry {
    connectors: Vec<Arc<dyn Connector>>,
    home: PathBuf,
    states: Mutex<StateFile>,
    /// The keys of the sources running now: `<space id>/<source id>`.
    running: Mutex<Vec<String>>,
    /// Whether the runtime runs connectors: false when it is not started, or `sources` or
    /// `capture` is off.
    active: std::sync::atomic::AtomicBool,
}

impl Registry {
    pub(crate) fn new(home: &Path, connectors: Vec<Arc<dyn Connector>>) -> Self {
        Registry {
            connectors,
            home: home.to_path_buf(),
            states: Mutex::new(StateFile::load(home)),
            running: Mutex::new(Vec::new()),
            active: std::sync::atomic::AtomicBool::new(false),
        }
    }

    pub(crate) fn connector(&self, id: &str) -> Option<&Arc<dyn Connector>> {
        self.connectors.iter().find(|c| c.info().id == id)
    }

    pub(crate) fn connectors(&self) -> &[Arc<dyn Connector>] {
        &self.connectors
    }

    pub(crate) fn home(&self) -> &Path {
        &self.home
    }

    pub(crate) fn states(&self) -> MutexGuard<'_, StateFile> {
        self.states.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub(crate) fn set_running(&self, key: &str, running: bool) {
        let mut list = self.running.lock().unwrap_or_else(|e| e.into_inner());
        list.retain(|k| k != key);
        if running {
            list.push(key.to_string());
        }
    }

    fn is_running(&self, key: &str) -> bool {
        self.running
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .any(|k| k == key)
    }

    pub(crate) fn set_active(&self, active: bool) {
        self.active
            .store(active, std::sync::atomic::Ordering::SeqCst);
    }

    fn active(&self) -> bool {
        self.active.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Every source of the space: the connectors this machine has or the space has connected,
    /// then the importers.
    pub(crate) fn list(&self, space: &Space) -> knode_format::Result<Vec<SourceStatus>> {
        let settings = source_settings(space)?;
        let space_id = space.manifest().id.clone();
        let mut out = Vec::new();
        for connector in &self.connectors {
            let info = connector.info();
            let setting = settings.get(&info.id).cloned();
            out.push(self.status(&space_id, info, setting));
        }
        for (id, setting) in settings {
            if self.connector(&id).is_none() {
                let info = SourceInfo::connector(id.clone(), id.clone(), "");
                out.push(self.status(&space_id, info, Some(setting)));
            }
        }
        out.extend(importers().into_iter().map(|info| SourceStatus {
            info,
            state: SourceState::Importer,
            setting: None,
            last_run: None,
            last_success: None,
            last_items: 0,
            items: 0,
            last_error: None,
            notice: None,
            next_run: None,
        }));
        Ok(out)
    }

    /// The connectors connected to the space.
    pub(crate) fn connected(&self, space: &Space) -> knode_format::Result<Vec<SourceStatus>> {
        Ok(self
            .list(space)?
            .into_iter()
            .filter(|s| s.setting.is_some())
            .collect())
    }

    /// One source of the space.
    pub(crate) fn get(&self, space: &Space, id: &str) -> knode_format::Result<SourceStatus> {
        self.list(space)?
            .into_iter()
            .find(|s| s.info.id == id)
            .ok_or_else(|| unknown(id))
    }

    fn status(
        &self,
        space_id: &str,
        info: SourceInfo,
        setting: Option<SourceSetting>,
    ) -> SourceStatus {
        let key = state_key(space_id, &info.id);
        let run = self.states().get(&key).cloned().unwrap_or_default();
        let available = self.connector(&info.id).is_some();
        let state = match &setting {
            None => SourceState::Off,
            Some(_) if !available => SourceState::Unavailable,
            Some(s) if !s.on => SourceState::Paused,
            Some(_) if self.is_running(&key) => SourceState::Running,
            Some(_) if !self.active() => SourceState::Stopped,
            Some(_) => match &run.last_error {
                None => SourceState::Idle,
                Some(e) if e.kind == "needs-update" => SourceState::NeedsUpdate,
                Some(e) if matches!(e.kind.as_str(), "unavailable" | "permission" | "sign-in") => {
                    SourceState::NeedsAttention
                }
                Some(_) => SourceState::Failing,
            },
        };
        SourceStatus {
            info,
            state,
            setting,
            last_run: run.last_run,
            last_success: run.last_success,
            last_items: run.last_items,
            items: run.items,
            last_error: run.last_error,
            notice: run.notice,
            next_run: run.next_run,
        }
    }

    /// What listing the containers of `id` needs from the space: its id and the source's
    /// settings. Taken with the space locked, so [`containers_of`](Self::containers_of) can
    /// run without the lock.
    pub(crate) fn containers_request(
        &self,
        space: &Space,
        id: &str,
    ) -> Result<(String, SourceSetting), SourceError> {
        if self.connector(id).is_none() {
            return Err(SourceError::Unavailable(format!(
                "no connector {id:?} on this machine"
            )));
        }
        let setting = source_setting(space, id)?.ok_or_else(|| not_connected(id))?;
        Ok((space.manifest().id.clone(), setting))
    }

    /// The containers of `id` in the space `space_id` with these settings, the excluded ones
    /// marked; excluded ones the source no longer lists are kept at the end, so they can be
    /// included again. Needs no space:
    /// connectors may take long to list them (over the network), and the space stays free for
    /// everyone else meanwhile.
    pub(crate) fn containers_of(
        &self,
        space_id: &str,
        setting: &SourceSetting,
        id: &str,
    ) -> Result<Vec<ContainerInfo>, SourceError> {
        let connector = self
            .connector(id)
            .ok_or_else(|| {
                SourceError::Unavailable(format!("no connector {id:?} on this machine"))
            })?
            .clone();
        let space_id = space_id.to_string();
        let key = state_key(&space_id, id);
        let cursor = self.states().get(&key).and_then(|s| s.cursor.clone());
        let secrets = SourceSecrets::new(&self.home, id);
        let request = FetchRequest {
            space_id: &space_id,
            cursor: cursor.as_ref(),
            history: setting.history,
            since: setting.since,
            excluded: &setting.exclude,
            max_items: 0,
            secrets: &secrets,
        };
        connector.check()?;
        let mut out: Vec<ContainerInfo> = connector
            .containers(&request)?
            .into_iter()
            .map(|container| ContainerInfo {
                excluded: setting.exclude.iter().any(|e| e.id == container.id),
                container,
            })
            .collect();
        for excluded in &setting.exclude {
            if !out.iter().any(|c| c.container.id == excluded.id) {
                out.push(ContainerInfo {
                    container: excluded.clone(),
                    excluded: true,
                });
            }
        }
        Ok(out)
    }

    /// Forgets what the source did in the space on this machine: its cursor and status.
    pub(crate) fn forget(&self, space_id: &str, id: &str) -> io::Result<()> {
        let mut states = self.states();
        states.remove(&state_key(space_id, id));
        states.save(&self.home)
    }
}

/// The key of a space and source in [`STATE_FILE`]: `<space id>/<source id>`.
pub(crate) fn state_key(space_id: &str, source_id: &str) -> String {
    format!("{space_id}/{source_id}")
}

/// The [`RunState`] of every space and source, as in [`STATE_FILE`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct StateFile {
    #[serde(default)]
    sources: BTreeMap<String, RunState>,
}

impl StateFile {
    fn path(home: &Path) -> PathBuf {
        home.join(SOURCES_DIR).join(STATE_FILE)
    }

    /// Reads the file; a missing or broken one is empty, so connectors read again from where
    /// the importers find what is new.
    pub(crate) fn load(home: &Path) -> StateFile {
        match fs::read(Self::path(home)) {
            Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|e| {
                eprintln!(
                    "Knode Service: {STATE_FILE} of sources cannot be read ({e}); starting again"
                );
                StateFile::default()
            }),
            Err(_) => StateFile::default(),
        }
    }

    pub(crate) fn save(&self, home: &Path) -> io::Result<()> {
        let dir = home.join(SOURCES_DIR);
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&dir)?;
        let bytes = serde_json::to_vec_pretty(self).map_err(io::Error::other)?;
        write_private(&Self::path(home), &bytes)
    }

    pub(crate) fn get(&self, key: &str) -> Option<&RunState> {
        self.sources.get(key)
    }

    pub(crate) fn get_mut(&mut self, key: &str) -> Option<&mut RunState> {
        self.sources.get_mut(key)
    }

    pub(crate) fn entry(&mut self, key: &str) -> &mut RunState {
        self.sources.entry(key.to_string()).or_default()
    }

    fn remove(&mut self, key: &str) {
        self.sources.remove(key);
    }

    /// The keys of the source `source_id` in every space it ran in.
    pub(crate) fn keys_of(&self, source_id: &str) -> Vec<String> {
        let suffix = format!("/{source_id}");
        self.sources
            .keys()
            .filter(|k| k.ends_with(&suffix))
            .cloned()
            .collect()
    }
}

/// What a connector is told when it cannot keep a secret because the system has no credential
/// store.
pub const NO_CREDENTIAL_STORE: &str = "this system has no keychain to keep the secrets of \
     connectors in: install a Secret Service, e.g. GNOME Keyring or KeePassXC, and sign in \
     again (or set `source_secrets_in_files = true` in config.toml to keep them in a file only \
     you can read)";

/// The secrets of one connector, in the system's credential store: the Keychain on macOS, the
/// Secret Service through `secret-tool` on Linux. Unlike agent keys, they never fall back to a
/// file where Linux has no Secret Service: keeping one fails with [`NO_CREDENTIAL_STORE`],
/// unless the person set `source_secrets_in_files = true` in `config.toml`, which keeps them
/// in a file only its owner can read in the Knode home (`agent-keys/`, mode `0600`). Files are
/// used everywhere with `KNODE_KEY_STORE=file`. Each secret is filed as
/// `_source-<id>-<name>`, which is never the name of an agent.
pub struct SourceSecrets {
    store: Box<dyn KeyStore>,
    source: String,
}

impl SourceSecrets {
    /// The secrets of the source `source_id` for the Knode home `home`.
    pub fn new(home: &Path, source_id: &str) -> Self {
        SourceSecrets {
            store: source_key_store(home),
            source: source_id.to_string(),
        }
    }

    /// The secrets of the source in this store, for tests.
    pub fn in_store(store: Box<dyn KeyStore>, source_id: &str) -> Self {
        SourceSecrets {
            store,
            source: source_id.to_string(),
        }
    }

    /// Where the secrets are kept, for people.
    pub fn describe(&self) -> String {
        self.store.describe()
    }

    fn name(&self, name: &str) -> io::Result<String> {
        let plain = !name.is_empty()
            && name.len() <= 64
            && name
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
        if !plain || !is_source_id(&self.source) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{name:?} is not the name of a secret: lower case letters, digits and -"),
            ));
        }
        Ok(format!("_source-{}-{name}", self.source))
    }
}

impl Secrets for SourceSecrets {
    fn get(&self, name: &str) -> io::Result<Option<String>> {
        self.store.get(&self.name(name)?)
    }

    fn set(&self, name: &str, value: &str) -> io::Result<()> {
        self.store
            .set(&self.name(name)?, value)
            .map_err(|e| match e.kind() {
                io::ErrorKind::Unsupported => {
                    io::Error::new(io::ErrorKind::Unsupported, NO_CREDENTIAL_STORE)
                }
                _ => e,
            })
    }

    fn delete(&self, name: &str) -> io::Result<bool> {
        self.store.delete(&self.name(name)?)
    }
}

/// The store of the secrets of connectors for the Knode home `home`: as agent keys', but with
/// no file to fall back to where Linux has no Secret Service, unless `config.toml` asks for
/// one.
fn source_key_store(home: &Path) -> Box<dyn KeyStore> {
    if std::env::var(crate::keys::KEY_STORE_VAR).is_ok_and(|v| v == "file") {
        return default_key_store(home);
    }
    #[cfg(target_os = "macos")]
    {
        default_key_store(home)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let in_files = knode_config::Config::load_from(&home.join(knode_config::CONFIG_FILE))
            .is_ok_and(|c| c.source_secrets_in_files());
        if in_files {
            default_key_store(home)
        } else {
            Box::new(crate::keys::SecretToolKeyStore::without_files(home))
        }
    }
}

/// The settings of every source connected to the space, by source id. A setting that is not
/// valid JSON is left out.
pub fn source_settings(space: &Space) -> knode_format::Result<BTreeMap<String, SourceSetting>> {
    let mut out = BTreeMap::new();
    for (key, value) in space.settings()? {
        let Some(id) = key.strip_prefix(SETTING_PREFIX) else {
            continue;
        };
        match serde_json::from_str::<SourceSetting>(&value) {
            Ok(setting) => {
                out.insert(id.to_string(), setting);
            }
            Err(e) => eprintln!(
                "Knode Service: the setting {key} of {} cannot be read: {e}",
                space.path().display()
            ),
        }
    }
    Ok(out)
}

/// The settings of one source in the space, or `None` when it is not connected.
pub fn source_setting(space: &Space, id: &str) -> knode_format::Result<Option<SourceSetting>> {
    Ok(source_settings(space)?.remove(id))
}

fn invalid(message: impl Into<String>) -> knode_format::Error {
    knode_format::Error::Invalid(message.into())
}

pub(crate) fn unknown(id: &str) -> knode_format::Error {
    invalid(format!(
        "no source is named {id:?}: `knode sources list` shows them"
    ))
}

fn not_connected(id: &str) -> knode_format::Error {
    invalid(format!(
        "{id} is not connected to this space: connect it with `knode sources connect {id}`"
    ))
}

fn write_setting(
    space: &mut Space,
    id: &str,
    setting: Option<&SourceSetting>,
    by: &Provenance,
) -> knode_format::Result<bool> {
    let key = format!("{SETTING_PREFIX}{id}");
    let value = setting
        .map(serde_json::to_string)
        .transpose()
        .map_err(|e| invalid(e.to_string()))?;
    space.set_setting(&key, value.as_deref(), by)
}

fn person(by: &Provenance) -> knode_format::Result<()> {
    if by.kind == knode_format::ActorKind::Person {
        Ok(())
    } else {
        Err(invalid(
            "only a person can connect, pause or exclude sources",
        ))
    }
}

fn changeable(known: &[String], id: &str) -> knode_format::Result<()> {
    if known.iter().any(|k| k == id) {
        return Ok(());
    }
    if crate::import::ImportSource::from_id(id).is_some() {
        return Err(invalid(format!(
            "{id} is an importer: it runs once with `knode import --from {id}`, and has nothing \
             to connect"
        )));
    }
    Err(unknown(id))
}

/// Connects the connector `id` to the space with the history chosen, on from now. Connecting
/// one that is already connected keeps its settings, and turns it on; the history is chosen
/// once and asking for another one is refused (disconnect it first). `known` are the
/// connectors this machine has. Returns its settings.
pub fn connect(
    space: &mut Space,
    known: &[String],
    id: &str,
    history: Option<History>,
    by: &Provenance,
) -> knode_format::Result<SourceSetting> {
    connect_with(space, known, id, history, true, by)
}

/// As [`connect`], but on only when `on` is true. With `on` false the source is connected
/// paused: its history and the day it was connected are fixed and its containers can be
/// listed, so the person can exclude some before anything is read, and nothing runs until it
/// is resumed ([`set_on`]). Connecting paused one that is already connected pauses it.
pub fn connect_with(
    space: &mut Space,
    known: &[String],
    id: &str,
    history: Option<History>,
    on: bool,
    by: &Provenance,
) -> knode_format::Result<SourceSetting> {
    person(by)?;
    changeable(known, id)?;
    if let Some(mut setting) = source_setting(space, id)? {
        if let Some(history) = history.filter(|h| *h != setting.history) {
            return Err(invalid(format!(
                "{id} is connected with the history {}: to bring in {} instead, disconnect it \
                 and connect it again",
                setting.history.id(),
                history.id()
            )));
        }
        setting.on = on;
        write_setting(space, id, Some(&setting), by)?;
        return Ok(setting);
    }
    let setting = SourceSetting {
        on,
        history: history.unwrap_or_default(),
        connected_at: Timestamp::now(),
        since: start_of_today(),
        exclude: Vec::new(),
    };
    write_setting(space, id, Some(&setting), by)?;
    Ok(setting)
}

/// Pauses (`on` false) or resumes a connected source. Returns its settings.
pub fn set_on(
    space: &mut Space,
    id: &str,
    on: bool,
    by: &Provenance,
) -> knode_format::Result<SourceSetting> {
    person(by)?;
    let mut setting = source_setting(space, id)?.ok_or_else(|| not_connected(id))?;
    setting.on = on;
    write_setting(space, id, Some(&setting), by)?;
    Ok(setting)
}

/// Excludes a chat, channel or folder of a connected source: it is never read again. What was
/// already brought in from it stays in the space. Returns the settings.
pub fn exclude(
    space: &mut Space,
    id: &str,
    container: Container,
    by: &Provenance,
) -> knode_format::Result<SourceSetting> {
    person(by)?;
    let container_id = container.id.trim().to_string();
    if container_id.is_empty() {
        return Err(invalid("the id of what to exclude is empty"));
    }
    let mut setting = source_setting(space, id)?.ok_or_else(|| not_connected(id))?;
    let name = match container.name.trim() {
        "" => container_id.clone(),
        name => name.to_string(),
    };
    match setting.exclude.iter_mut().find(|c| c.id == container_id) {
        Some(existing) => existing.name = name,
        None => setting.exclude.push(Container::new(container_id, name)),
    }
    write_setting(space, id, Some(&setting), by)?;
    Ok(setting)
}

/// Includes again a chat, channel or folder excluded before. What was said in it while it was
/// excluded is not brought in later; an export brings it. Returns the settings.
pub fn include(
    space: &mut Space,
    id: &str,
    container_id: &str,
    by: &Provenance,
) -> knode_format::Result<SourceSetting> {
    person(by)?;
    let mut setting = source_setting(space, id)?.ok_or_else(|| not_connected(id))?;
    let before = setting.exclude.len();
    setting.exclude.retain(|c| c.id != container_id.trim());
    if setting.exclude.len() == before {
        return Err(invalid(format!(
            "{container_id:?} is not excluded from {id}"
        )));
    }
    write_setting(space, id, Some(&setting), by)?;
    Ok(setting)
}

/// Disconnects a source from the space: its settings are removed and it stops. What it
/// brought in stays in the space. Returns whether it was connected. The caller forgets its
/// state on this machine ([`forget_state`]).
pub fn disconnect(space: &mut Space, id: &str, by: &Provenance) -> knode_format::Result<bool> {
    person(by)?;
    if source_setting(space, id)?.is_none() {
        return Ok(false);
    }
    write_setting(space, id, None, by)
}

/// Forgets what the source did in the space on this machine (its cursor and status), in the
/// Knode home `home`. For when the service is not running; the service does it itself.
pub fn forget_state(home: &Path, space_id: &str, id: &str) -> io::Result<()> {
    let mut states = StateFile::load(home);
    states.remove(&state_key(space_id, id));
    states.save(home)
}

/// Signs the connector `id` out of its account, removing its sign-in from the keychain, when
/// no space in `config.toml` of the Knode home `home` still has it connected. For a disconnect
/// made while Knode Service is not running; the service does it itself after
/// `sources.disconnect`, also counting the spaces it has open. Call it with no space open.
/// Returns whether it signed out.
pub fn sign_out_if_unused(home: &Path, id: &str) -> bool {
    sign_out_if_unused_with(home, &builtin_connectors(), id)
}

fn sign_out_if_unused_with(home: &Path, connectors: &[Arc<dyn Connector>], id: &str) -> bool {
    let Some(connector) = connectors.iter().find(|c| c.info().id == id) else {
        return false;
    };
    if !connector.info().permissions.contains(&Permission::SignIn) {
        return false;
    }
    let spaces: Vec<PathBuf> =
        knode_config::Config::load_from(&home.join(knode_config::CONFIG_FILE))
            .map(|c| c.known_spaces())
            .unwrap_or_default()
            .into_iter()
            .filter(|p| p.is_absolute() && p.exists())
            .collect();
    // A space that cannot be read counts as still having it: its sign-in is kept rather than
    // removed too early.
    let still = spaces.iter().any(|path| match Space::open(path) {
        Ok(space) => source_settings(&space).map_or(true, |s| s.contains_key(id)),
        Err(_) => true,
    });
    if still {
        return false;
    }
    match connector.sign_out(&SourceSecrets::new(home, id)) {
        Ok(_) => true,
        Err(e) => {
            eprintln!("{id} could not sign out: {e}");
            false
        }
    }
}

/// Every source of the space as the Knode home `home` has it, without the service: the
/// built-in connectors, those the space has connected, and the importers. Connected
/// connectors show as [`SourceState::Stopped`] since nothing runs them.
pub fn list_offline(home: &Path, space: &Space) -> knode_format::Result<Vec<SourceStatus>> {
    Registry::new(home, builtin_connectors()).list(space)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::MemoryKeyStore;

    #[cfg(target_os = "macos")]
    #[test]
    fn whatsapp_for_mac_is_built_in() {
        let connectors = builtin_connectors();
        let ids: Vec<String> = connectors.iter().map(|c| c.info().id).collect();
        assert!(ids.iter().any(|id| id == "whatsapp-mac"), "{ids:?}");
        // No two connectors share an id, and WhatsApp for Mac's is not the export importer's
        // (a connector may share its id only with the importer of the same source, as
        // `apple-mail` does).
        let mut unique = ids.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(unique.len(), ids.len(), "{ids:?}");
        assert!(importers().iter().all(|s| s.id != "whatsapp-mac"));
    }

    #[test]
    fn importers_are_sources_of_kind_importer() {
        let list = importers();
        assert_eq!(list.len(), ImportSource::ALL.len());
        assert!(list.iter().all(|s| s.kind == SourceKind::Importer));
        assert!(list.iter().any(|s| s.id == "whatsapp"));
    }

    #[test]
    fn secrets_are_filed_under_the_source() {
        let secrets = SourceSecrets::in_store(Box::new(MemoryKeyStore::new()), "telegram");
        secrets.set("session", "s3cret").unwrap();
        assert_eq!(secrets.get("session").unwrap().as_deref(), Some("s3cret"));
        assert_eq!(secrets.name("session").unwrap(), "_source-telegram-session");
        assert!(secrets.set("../x", "v").is_err());
        assert!(secrets.delete("session").unwrap());
        assert_eq!(secrets.get("session").unwrap(), None);
    }

    /// A store with no system credential store behind it.
    struct NoKeychain;

    impl KeyStore for NoKeychain {
        fn get(&self, _: &str) -> io::Result<Option<String>> {
            Ok(None)
        }
        fn set(&self, _: &str, _: &str) -> io::Result<()> {
            Err(io::Error::new(io::ErrorKind::Unsupported, "none"))
        }
        fn delete(&self, _: &str) -> io::Result<bool> {
            Ok(false)
        }
        fn describe(&self) -> String {
            "nothing".into()
        }
    }

    #[test]
    fn without_a_keychain_a_secret_is_refused_with_what_to_do() {
        let secrets = SourceSecrets::in_store(Box::new(NoKeychain), "telegram");
        let err = secrets.set("session", "s3cret").unwrap_err();
        let message = err.to_string();
        assert!(message.contains("GNOME Keyring or KeePassXC"), "{message}");
        assert!(
            message.contains("source_secrets_in_files = true"),
            "{message}"
        );
        assert!(!message.contains("s3cret"));
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn on_linux_secrets_go_to_files_only_when_config_asks() {
        if std::env::var(crate::keys::KEY_STORE_VAR).is_ok() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            source_key_store(dir.path()).describe(),
            "the Secret Service"
        );
        std::fs::write(
            dir.path().join(knode_config::CONFIG_FILE),
            "source_secrets_in_files = true\n",
        )
        .unwrap();
        assert!(source_key_store(dir.path()).describe().contains("a file"));
        // Agent keys keep their fallback.
        assert!(default_key_store(dir.path()).describe().contains("a file"));
    }

    /// A connector with an account, counting its sign-outs.
    struct Account(std::sync::atomic::AtomicUsize);

    impl Connector for Account {
        fn info(&self) -> SourceInfo {
            SourceInfo::connector("fake", "Fake", "Synthetic").needing([Permission::SignIn])
        }

        fn fetch(&self, _: &FetchRequest<'_>) -> Result<Fetched, SourceError> {
            Ok(Fetched::nothing(Cursor::new("0")))
        }

        fn sign_out(&self, _: &dyn Secrets) -> Result<bool, SourceError> {
            self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(true)
        }
    }

    #[test]
    fn without_the_service_the_sign_in_goes_with_the_last_space() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        fs::create_dir_all(&home).unwrap();
        let me = Provenance::person("me");
        let known = vec!["fake".to_string()];
        let account = Arc::new(Account(Default::default()));
        let connectors: Vec<Arc<dyn Connector>> = vec![account.clone()];
        let sign_outs = || account.0.load(std::sync::atomic::Ordering::SeqCst);
        let mut paths = Vec::new();
        for name in ["Personal", "Work"] {
            let path = dir.path().join(format!("{name}.knode"));
            let mut space = Space::create(&path, name).unwrap();
            connect(&mut space, &known, "fake", None, &me).unwrap();
            paths.push(fs::canonicalize(path).unwrap());
        }
        // A folder in config.toml that is not a space (yet) cannot be read.
        let broken = dir.path().join("Broken.knode");
        fs::create_dir(&broken).unwrap();
        let config = format!(
            "default_space = {:?}\nspaces = [{:?}, {:?}]\n",
            paths[0].to_str().unwrap(),
            paths[1].to_str().unwrap(),
            broken.to_str().unwrap()
        );
        fs::write(home.join(knode_config::CONFIG_FILE), config).unwrap();
        let disconnect_from = |path: &Path| {
            let mut space = Space::open(path).unwrap();
            assert!(disconnect(&mut space, "fake", &me).unwrap());
        };

        disconnect_from(&paths[0]);
        assert!(!sign_out_if_unused_with(&home, &connectors, "fake"));
        assert_eq!(sign_outs(), 0, "still connected to Work");
        disconnect_from(&paths[1]);
        assert!(!sign_out_if_unused_with(&home, &connectors, "fake"));
        assert_eq!(
            sign_outs(),
            0,
            "a space that cannot be read keeps the sign-in"
        );
        fs::remove_dir(&broken).unwrap();
        assert!(sign_out_if_unused_with(&home, &connectors, "fake"));
        assert_eq!(sign_outs(), 1);
        // A connector with no account, or none at all, signs nothing out.
        assert!(!sign_out_if_unused_with(&home, &connectors, "nope"));
    }

    #[test]
    fn settings_live_in_the_space() {
        let dir = tempfile::tempdir().unwrap();
        let mut space = Space::create(dir.path().join("S.knode"), "S").unwrap();
        let me = Provenance::person("me");
        let known = vec!["fake".to_string()];
        assert!(connect(&mut space, &known, "pdf", None, &me).is_err());
        assert!(connect(&mut space, &known, "nope", None, &me).is_err());
        assert!(connect(&mut space, &known, "fake", None, &Provenance::agent("x")).is_err());
        let s = connect(&mut space, &known, "fake", Some(History::Everything), &me).unwrap();
        assert_eq!(s.history, History::Everything);
        assert!(connect(&mut space, &known, "fake", Some(History::FromToday), &me).is_err());
        set_on(&mut space, "fake", false, &me).unwrap();
        let s = exclude(&mut space, "fake", Container::new("c1", "Family"), &me).unwrap();
        assert!(!s.on);
        assert_eq!(s.exclude, vec![Container::new("c1", "Family")]);
        let again = connect(&mut space, &known, "fake", None, &me).unwrap();
        assert!(again.on);
        assert_eq!(again.history, History::Everything);
        let paused = connect_with(&mut space, &known, "fake", None, false, &me).unwrap();
        assert!(!paused.on);
        assert_eq!(paused.exclude, again.exclude);
        assert_eq!(paused.connected_at, again.connected_at);
        assert!(connect(&mut space, &known, "fake", None, &me).unwrap().on);
        assert!(include(&mut space, "fake", "c2", &me).is_err());
        assert!(
            include(&mut space, "fake", "c1", &me)
                .unwrap()
                .exclude
                .is_empty()
        );
        assert!(disconnect(&mut space, "fake", &me).unwrap());
        assert!(!disconnect(&mut space, "fake", &me).unwrap());
        assert!(source_settings(&space).unwrap().is_empty());
        let paused = connect_with(&mut space, &known, "fake", None, false, &me).unwrap();
        assert!(!paused.on);
        assert_eq!(paused.history, History::FromToday);
        assert!(connect_with(&mut space, &known, "pdf", None, false, &me).is_err());
    }
}
