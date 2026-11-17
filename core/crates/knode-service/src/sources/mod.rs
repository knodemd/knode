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

pub(crate) mod runtime;

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use knode_format::{Provenance, Space, Timestamp};
pub use knode_ingest::sources::{
    Batch, Connector, Container, Cursor, FetchRequest, Fetched, History, NoSecrets, Permission,
    Secrets, SourceError, SourceInfo, SourceKind, Trigger, Written, is_source_id, provenance,
    start_of_today,
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
pub fn builtin_connectors() -> Vec<Arc<dyn Connector>> {
    #[allow(unused_mut)]
    let mut connectors: Vec<Arc<dyn Connector>> = Vec::new();
    #[cfg(target_os = "macos")]
    if let Some(imessage) = knode_ingest::imessage::IMessageConnector::local() {
        connectors.push(Arc::new(imessage));
    }
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
        ImportSource::WhatsApp => ("WhatsApp export", "A WhatsApp chat export", &[]),
        ImportSource::Telegram => ("Telegram export", "A Telegram Desktop export in JSON", &[]),
        ImportSource::Slack => ("Slack export", "A Slack workspace export", &[]),
        ImportSource::Mbox => ("Mail (mbox)", "An .mbox file, such as a Gmail Takeout", &[]),
        ImportSource::AppleMail => (
            "Apple Mail",
            "The mail of Apple Mail on this Mac",
            &[FullDiskAccess],
        ),
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
    /// The id of a chat, channel or folder, for `sources.exclude` and `sources.include`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// Its name, for `sources.exclude`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Who makes the change.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub by: Option<Provenance>,
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
            next_run: run.next_run,
        }
    }

    /// The containers of a connected source, with the excluded ones marked; excluded ones the
    /// source no longer lists are kept at the end, so they can be included again.
    pub(crate) fn containers(
        &self,
        space: &Space,
        id: &str,
    ) -> Result<Vec<ContainerInfo>, SourceError> {
        let connector = self
            .connector(id)
            .ok_or_else(|| {
                SourceError::Unavailable(format!("no connector {id:?} on this machine"))
            })?
            .clone();
        let setting = source_setting(space, id)?.ok_or_else(|| not_connected(id))?;
        let space_id = space.manifest().id.clone();
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

    pub(crate) fn entry(&mut self, key: &str) -> &mut RunState {
        self.sources.entry(key.to_string()).or_default()
    }

    fn remove(&mut self, key: &str) {
        self.sources.remove(key);
    }
}

/// The secrets of one connector, in the credential store agent keys use: the Keychain on
/// macOS; the Secret Service through `secret-tool` on Linux, or, where there is none, a file
/// only its owner can read in the Knode home (`agent-keys/`, mode `0600`); files everywhere
/// with `KNODE_KEY_STORE=file`. Each secret is filed as `_source-<id>-<name>`, which is never
/// the name of an agent.
pub struct SourceSecrets {
    store: Box<dyn KeyStore>,
    source: String,
}

impl SourceSecrets {
    /// The secrets of the source `source_id` for the Knode home `home`.
    pub fn new(home: &Path, source_id: &str) -> Self {
        SourceSecrets {
            store: default_key_store(home),
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
        self.store.set(&self.name(name)?, value)
    }

    fn delete(&self, name: &str) -> io::Result<bool> {
        self.store.delete(&self.name(name)?)
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

fn unknown(id: &str) -> knode_format::Error {
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
        setting.on = true;
        write_setting(space, id, Some(&setting), by)?;
        return Ok(setting);
    }
    let setting = SourceSetting {
        on: true,
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
    }
}
