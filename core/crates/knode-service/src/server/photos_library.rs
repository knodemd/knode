//! `photos_library.*`: the Photos library source, run by the Knode app
//! ([`crate::sources::photos_library`]). Blocking: called on a blocking thread, like
//! [`super::call`]. Only a person calls them: the app, as the person who owns the Knode home.

use std::path::PathBuf;
use std::time::Duration;

use knode_format::{Provenance, Space, Timestamp};
use knode_ingest::media::AddedFile;
use knode_ingest::photos_library::{self, Ledger, LibraryAsset, Marked, SOURCE_ID};
use knode_ingest::storage;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{State, last_change, lock_ignoring_poison, params, to_value};
use crate::protocol::{RpcError, SpaceParams, codes, methods};
use crate::sources::photos_library::{library_id, load_ledger, save_ledger};
use crate::sources::{
    self, Container, Cursor, History, LastError, SourceSetting, SourceStatus, state_key,
};
use crate::spaces::SpaceEventKind;

/// How long after a run the app is asked to run the Photos library again, at the latest. The app
/// also runs it when the library changes.
pub(crate) const SYNC_EVERY: Duration = Duration::from_secs(15 * 60);

/// `photos_library.plan`: what a space wants from the Photos library now.
#[derive(Debug, Clone, Serialize)]
struct Plan {
    /// Whether the source is on in the space: when off, the app reads nothing for it.
    on: bool,
    /// The id of this Mac's library: every node it adds keeps it.
    library: String,
    /// The albums picked on this Mac, by their own identifiers: only these are read. None until
    /// the person picks one.
    albums: Vec<Container>,
    /// Whether albums are picked only on other Macs (picks are kept per library): this Mac
    /// reads nothing until the person picks albums here.
    elsewhere: bool,
    /// With the history `from-today`, the start of the day it was connected (Unix milliseconds):
    /// older assets are left out.
    #[serde(skip_serializing_if = "Option::is_none")]
    since: Option<i64>,
    /// Whether assets kept only in iCloud are downloaded; else they wait.
    icloud: bool,
    /// Whether the space has room under its soft limit: when not, nothing new is added.
    room: bool,
    /// The change token the app saved after its last run, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    token: Option<String>,
    /// When the service asks the app to run it next, at the latest.
    #[serde(skip_serializing_if = "Option::is_none")]
    next_run: Option<Timestamp>,
}

#[derive(Debug, Deserialize)]
struct AlbumsParams {
    albums: Vec<Container>,
}

#[derive(Debug, Deserialize)]
struct KnownParams {
    space: PathBuf,
    ids: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct AddParams {
    space: PathBuf,
    asset: LibraryAsset,
}

#[derive(Debug, Serialize)]
struct AddResult {
    #[serde(flatten)]
    added: AddedFile,
    /// Whether the space still has room under its soft limit.
    room: bool,
}

#[derive(Debug, Deserialize)]
struct SyncParams {
    space: PathBuf,
    /// Every album the app has, for `sources.containers`.
    #[serde(default)]
    albums: Vec<Container>,
    /// Every asset identifier in the albums picked now. Left out when the app could not list
    /// them all (an album picked could not be found): then nothing is marked.
    #[serde(default)]
    present: Option<Vec<String>>,
    /// Assets that can never be added (too large, not supported): not exported again.
    #[serde(default)]
    failed: Vec<FailedAsset>,
    /// The change token of the library after this run, kept as the cursor.
    #[serde(default)]
    token: Option<String>,
    /// How many assets the run added.
    #[serde(default)]
    added: usize,
    /// How many wait for their original to be on this Mac (kept only in iCloud).
    #[serde(default)]
    waiting: usize,
    /// How many were held back because the space is past its soft limit.
    #[serde(default)]
    held: usize,
}

#[derive(Debug, Deserialize)]
struct FailedAsset {
    id: String,
    #[serde(default)]
    reason: String,
}

#[derive(Debug, Serialize)]
struct SyncResult {
    marked: Marked,
    source: SourceStatus,
}

#[derive(Debug, Deserialize)]
struct FailParams {
    space: PathBuf,
    kind: String,
    #[serde(default)]
    message: String,
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
struct Options {
    /// Whether assets kept only in iCloud are downloaded.
    icloud: bool,
    /// Whether albums are picked only on other Macs: this one reads nothing yet.
    elsewhere: bool,
}

#[derive(Debug, Deserialize)]
struct ConfigureParams {
    space: PathBuf,
    #[serde(default)]
    icloud: Option<bool>,
    by: Provenance,
}

pub(super) fn call(state: &State, method: &str, raw: Value) -> Result<Value, RpcError> {
    match method {
        methods::PHOTOS_LIBRARY_PLAN => {
            let p: SpaceParams = params(method, raw)?;
            let space = state.space(&p.space)?;
            let space = lock_ignoring_poison(&space);
            to_value(plan(state, &space)?)
        }
        methods::PHOTOS_LIBRARY_ALBUMS => {
            let p: AlbumsParams = params(method, raw)?;
            report(state, p.albums)?;
            to_value(true)
        }
        methods::PHOTOS_LIBRARY_KNOWN => {
            let p: KnownParams = params(method, raw)?;
            let space = state.space(&p.space)?;
            let space = lock_ignoring_poison(&space);
            setting(&space)?;
            let library = library(state)?;
            let mut ledger = load_ledger(state.registry().home(), &space.manifest().id, &library);
            // Nodes deleted since the last run are tombstones before anything is exported.
            if ledger.reconcile(&space)? > 0 {
                save(state, &space, &ledger)?;
            }
            to_value(photos_library::known(&space, &library, &p.ids, &ledger)?)
        }
        methods::PHOTOS_LIBRARY_ADD => add(state, params(method, raw)?),
        methods::PHOTOS_LIBRARY_SYNC => sync(state, params(method, raw)?),
        methods::PHOTOS_LIBRARY_FAIL => fail(state, params(method, raw)?),
        methods::PHOTOS_LIBRARY_OPTIONS => {
            let p: SpaceParams = params(method, raw)?;
            let space = state.space(&p.space)?;
            let space = lock_ignoring_poison(&space);
            to_value(options(state, &space)?)
        }
        methods::PHOTOS_LIBRARY_CONFIGURE => {
            let p: ConfigureParams = params(method, raw)?;
            let space = state.space(&p.space)?;
            let mut space = lock_ignoring_poison(&space);
            if let Some(on) = p.icloud
                && photos_library::set_icloud(&mut space, on, &p.by)?
            {
                state.notify_space(&space, SpaceEventKind::Settings, |e| {
                    e.key = Some(photos_library::ICLOUD_SETTING.to_string());
                });
                ask_to_run(state, &space)?;
            }
            to_value(options(state, &space)?)
        }
        _ => Err(RpcError::new(
            codes::METHOD_NOT_FOUND,
            format!("no method named {method:?}"),
        )),
    }
}

/// The settings of the source in the space, or `invalid` when it is not connected.
fn setting(space: &Space) -> Result<SourceSetting, RpcError> {
    sources::source_setting(space, SOURCE_ID)?.ok_or_else(|| {
        RpcError::from(knode_format::Error::Invalid(format!(
            "{SOURCE_ID} is not connected to this space: connect it in Settings > Sources"
        )))
    })
}

/// Whether `config.toml` lets connectors run (`sources` and `capture`).
fn sources_on(state: &State) -> bool {
    let home = state.registry().home();
    knode_config::Config::load_from(&home.join(knode_config::CONFIG_FILE))
        .map(|c| c.sources_enabled())
        .unwrap_or(false)
}

/// The id of this Mac's library.
fn library(state: &State) -> Result<String, RpcError> {
    library_id(state.registry().home()).map_err(|e| {
        RpcError::new(
            codes::STORAGE,
            format!("the id of the Photos library could not be kept: {e}"),
        )
    })
}

/// The prefix of the identifiers of this library's albums in the settings of a space.
fn album_prefix(library: &str) -> String {
    format!("{library}/")
}

/// Keeps the albums the app has now, named with this library's id, for `sources.containers`:
/// picks are then made per library.
fn report(state: &State, albums: Vec<Container>) -> Result<(), RpcError> {
    let prefix = album_prefix(&library(state)?);
    let albums = albums
        .into_iter()
        .map(|a| Container::new(format!("{prefix}{}", a.id), a.name))
        .collect();
    state
        .registry()
        .report_containers(SOURCE_ID, Some(prefix), albums);
    Ok(())
}

/// The albums picked on this Mac, by their own identifiers, and whether others are picked on
/// other Macs only.
fn picked_here(setting: &SourceSetting, library: &str) -> (Vec<Container>, bool) {
    let prefix = album_prefix(library);
    let here: Vec<Container> = setting
        .include
        .iter()
        .filter_map(|c| {
            c.id.strip_prefix(&prefix)
                .map(|id| Container::new(id, c.name.clone()))
        })
        .collect();
    let elsewhere = here.is_empty() && !setting.include.is_empty();
    (here, elsewhere)
}

fn options(state: &State, space: &Space) -> Result<Options, RpcError> {
    let elsewhere = match sources::source_setting(space, SOURCE_ID)? {
        Some(setting) => picked_here(&setting, &library(state)?).1,
        None => false,
    };
    Ok(Options {
        icloud: photos_library::icloud_enabled(space)?,
        elsewhere,
    })
}

fn save(state: &State, space: &Space, ledger: &Ledger) -> Result<(), RpcError> {
    save_ledger(state.registry().home(), &space.manifest().id, ledger).map_err(|e| {
        RpcError::new(
            codes::STORAGE,
            format!("what {SOURCE_ID} did could not be saved: {e}"),
        )
    })
}

fn since(setting: &SourceSetting) -> Option<i64> {
    (setting.history == History::FromToday).then_some(setting.since.0)
}

fn room(space: &Space) -> Result<bool, RpcError> {
    Ok(!storage::check(space, 0)?.warn)
}

fn plan(state: &State, space: &Space) -> Result<Plan, RpcError> {
    let setting = setting(space)?;
    let key = state_key(&space.manifest().id, SOURCE_ID);
    let run = state.registry().states().get(&key).cloned();
    let library = library(state)?;
    let (albums, elsewhere) = picked_here(&setting, &library);
    Ok(Plan {
        on: setting.on && sources_on(state),
        since: since(&setting),
        library,
        albums,
        elsewhere,
        icloud: photos_library::icloud_enabled(space)?,
        room: room(space)?,
        token: run
            .as_ref()
            .and_then(|r| r.cursor.as_ref())
            .map(|c| c.as_str().to_string())
            .filter(|t| !t.is_empty()),
        next_run: run.and_then(|r| r.next_run),
    })
}

fn add(state: &State, p: AddParams) -> Result<Value, RpcError> {
    if !p.asset.path.is_absolute() {
        return Err(RpcError::invalid_params(format!(
            "path must be an absolute path: {}",
            p.asset.path.display()
        )));
    }
    let space = state.space(&p.space)?;
    let mut space = lock_ignoring_poison(&space);
    let setting = setting(&space)?;
    if !setting.on || !sources_on(state) {
        return Err(RpcError::from(knode_format::Error::Invalid(format!(
            "{SOURCE_ID} is paused in this space"
        ))));
    }
    if let (Some(since), Some(at)) = (since(&setting), p.asset.created_at)
        && at < since
    {
        return Err(RpcError::from(knode_format::Error::Invalid(
            "the asset was taken before the day the source was connected, and the space brings \
             in only what is new"
                .into(),
        )));
    }
    let library = library(state)?;
    let home = state.registry().home();
    let mut ledger = load_ledger(home, &space.manifest().id, &library);
    let deleted = ledger.deleted.contains(p.asset.id.trim());
    let by = sources::provenance(SOURCE_ID);
    let mark = last_change(&space)?;
    let result = photos_library::add(&mut space, &library, &p.asset, &mut ledger, &by);
    state.notify_since(&space, mark)?;
    // A file that is there but cannot be added (too large, not supported) never will be.
    if let Err(knode_format::Error::Invalid(why)) = &result
        && !deleted
        && p.asset.path.is_file()
    {
        ledger.fail(&p.asset.id, why);
    }
    save(state, &space, &ledger)?;
    let added = result?;
    to_value(AddResult {
        added,
        room: room(&space)?,
    })
}

fn sync(state: &State, p: SyncParams) -> Result<Value, RpcError> {
    let registry = state.registry();
    if !p.albums.is_empty() {
        report(state, p.albums.clone())?;
    }
    let space = state.space(&p.space)?;
    let mut space = lock_ignoring_poison(&space);
    let setting = setting(&space)?;
    let mut marked = Marked::default();
    if setting.on && sources_on(state) {
        let library = library(state)?;
        let mut ledger = load_ledger(registry.home(), &space.manifest().id, &library);
        ledger.reconcile(&space)?;
        for failed in &p.failed {
            ledger.fail(&failed.id, &failed.reason);
        }
        save(state, &space, &ledger)?;
        if let Some(present) = &p.present {
            let by = sources::provenance(SOURCE_ID);
            let mark = last_change(&space)?;
            let result = photos_library::mark(&mut space, &library, present, &by);
            state.notify_since(&space, mark)?;
            marked = match result {
                Ok(marked) => marked,
                Err(e) => {
                    record(state, &space, Err(("space", e.to_string())), None)?;
                    return Err(e.into());
                }
            };
        }
        let mut notes = Vec::new();
        if p.waiting > 0 {
            notes.push(format!(
                "{} wait for their original to download from iCloud to this Mac",
                p.waiting
            ));
        }
        if p.held > 0 {
            notes.push(format!(
                "{} wait because the space is past its size limit (Storage)",
                p.held
            ));
        }
        if !ledger.failed.is_empty() {
            notes.push(format!(
                "{} cannot be added (too large or not supported) and are skipped",
                ledger.failed.len()
            ));
        }
        let notice = (!notes.is_empty()).then(|| notes.join("; "));
        let cursor = Cursor::new(p.token.clone().unwrap_or_default());
        record(state, &space, Ok((p.added, cursor)), notice)?;
    }
    to_value(SyncResult {
        marked,
        source: registry.get(&space, SOURCE_ID)?,
    })
}

fn fail(state: &State, p: FailParams) -> Result<Value, RpcError> {
    let kind = match p.kind.as_str() {
        kind @ ("permission" | "unavailable" | "failed") => kind,
        other => {
            return Err(RpcError::invalid_params(format!(
                "kind is permission, unavailable or failed, not {other:?}"
            )));
        }
    };
    let space = state.space(&p.space)?;
    let space = lock_ignoring_poison(&space);
    setting(&space)?;
    let message = match p.message.trim() {
        "" => "the Photos library could not be read".to_string(),
        message => message.to_string(),
    };
    record(state, &space, Err((kind, message)), None)?;
    to_value(state.registry().get(&space, SOURCE_ID)?)
}

/// Records a run in the space on this machine: what it added and where it stopped, or why it
/// failed. Either way the app is asked to run it again within [`SYNC_EVERY`].
fn record(
    state: &State,
    space: &Space,
    outcome: Result<(usize, Cursor), (&str, String)>,
    notice: Option<String>,
) -> Result<(), RpcError> {
    let registry = state.registry();
    let key = state_key(&space.manifest().id, SOURCE_ID);
    let now = Timestamp::now();
    let mut states = registry.states();
    let run = states.entry(&key);
    run.last_run = Some(now);
    run.next_run = Some(Timestamp(now.0 + SYNC_EVERY.as_millis() as i64));
    match outcome {
        Ok((items, cursor)) => {
            run.last_success = Some(now);
            run.last_items = items;
            run.items += items as u64;
            run.last_error = None;
            run.failures = 0;
            run.cursor = Some(cursor);
            run.notice = notice;
        }
        Err((kind, message)) => {
            run.failures += 1;
            run.last_error = Some(LastError {
                kind: kind.to_string(),
                message,
                at: now,
            });
        }
    }
    states.save(registry.home()).map_err(|e| {
        RpcError::new(
            codes::STORAGE,
            format!("the state of {SOURCE_ID} could not be saved: {e}"),
        )
    })
}

/// Asks the app to run the source for the space now, when it is connected.
fn ask_to_run(state: &State, space: &Space) -> Result<(), RpcError> {
    if sources::source_setting(space, SOURCE_ID)?.is_none() {
        return Ok(());
    }
    let registry = state.registry();
    let mut states = registry.states();
    states
        .entry(&state_key(&space.manifest().id, SOURCE_ID))
        .next_run = Some(Timestamp::now());
    states.save(registry.home()).map_err(|e| {
        RpcError::new(
            codes::STORAGE,
            format!("the state of {SOURCE_ID} could not be saved: {e}"),
        )
    })
}
