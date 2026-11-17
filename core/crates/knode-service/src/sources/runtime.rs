//! The thread that runs connectors: for every space it knows (those in `config.toml` and those
//! it has open) and every connector switched on there, it runs the connector when its files
//! change or its interval comes, writes what it read through the space's mutex, saves the
//! cursor only after the write, and waits longer after each error in a row.
//!
//! A file event runs a watching connector only when its files really changed since its last
//! run ([`FileStamp`]): on macOS a connector's own copy of a database is reported as events on
//! it, which would otherwise run it again and again. Writes to files kept open are reported
//! late, so a watching connector is also looked at every [`SourcesOptions::watch_every`]: it
//! runs then only when its files changed, or its last run did not read everything, so a
//! database that did not change is not copied again.
//!
//! The person's setting of a source is read again before every batch of a run: a pause, an
//! exclusion or a disconnect made while a run goes on counts from the next batch, and nothing
//! of a source disconnected meanwhile is written or saved.
//!
//! Nothing runs while `sources` or `capture` is off in `config.toml`: Knode Service then brings
//! nothing in on its own. Runs happen one at a time, so a space is never written by two
//! connectors at once and locks are always taken in the same order (the space, then the state
//! of the sources).

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use knode_format::Timestamp;
use notify::{RecursiveMode, Watcher as _};

use super::{
    Connector, FetchRequest, FileStamp, LastError, SourceError, SourceSecrets, SourceSetting,
    SourcesOptions, Trigger, Written, provenance, source_setting, source_settings, state_key,
};
use crate::server::{State, last_change, lock_ignoring_poison};

/// What wakes the runtime up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Wake {
    /// Settings changed: read them again.
    Reload,
    /// Run this source of this space now: `<space id>/<source id>`.
    Run(String),
    /// Files a connector watches changed.
    Files(String),
    /// Stop.
    Stop,
}

/// The runtime of a running service.
pub(crate) struct SourcesTask {
    stopping: Arc<AtomicBool>,
    wake: mpsc::Sender<Wake>,
    thread: Option<JoinHandle<()>>,
}

impl SourcesTask {
    pub(crate) fn start(
        state: Arc<State>,
        home: PathBuf,
        options: SourcesOptions,
    ) -> std::io::Result<Self> {
        // Copies of other apps' databases left by a Knode that ended while it read them.
        knode_ingest::sources::sqlite::remove_stale_copies(running);
        let stopping = Arc::new(AtomicBool::new(false));
        let (wake, woken) = mpsc::channel();
        state.set_sources_wake(Some(wake.clone()));
        let thread = {
            let stopping = stopping.clone();
            let wake = wake.clone();
            thread::Builder::new()
                .name("knode-sources".into())
                .spawn(move || {
                    let mut runtime = Runtime {
                        state,
                        home,
                        options,
                        stopping,
                        wake,
                        watchers: BTreeMap::new(),
                        files_changed: BTreeMap::new(),
                        forced: BTreeSet::new(),
                        stamps: BTreeMap::new(),
                        read_all: BTreeSet::new(),
                        last_config_error: None,
                    };
                    runtime.supervise(&woken);
                    runtime.state.registry().set_active(false);
                    runtime.state.set_sources_wake(None);
                })?
        };
        Ok(SourcesTask {
            stopping,
            wake,
            thread: Some(thread),
        })
    }

    /// Stops running connectors and waits for a run in progress to finish.
    pub(crate) fn stop(mut self) {
        self.shutdown();
    }

    fn shutdown(&mut self) {
        self.stopping.store(true, Ordering::SeqCst);
        let _ = self.wake.send(Wake::Stop);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for SourcesTask {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// One connector switched on in one space.
struct Entry {
    space: PathBuf,
    space_id: String,
    connector: Arc<dyn Connector>,
    source_id: String,
    setting: SourceSetting,
}

impl Entry {
    fn key(&self) -> String {
        state_key(&self.space_id, &self.source_id)
    }
}

struct Runtime {
    state: Arc<State>,
    home: PathBuf,
    options: SourcesOptions,
    stopping: Arc<AtomicBool>,
    wake: mpsc::Sender<Wake>,
    /// The file watchers, by source id, with the paths they watch.
    watchers: BTreeMap<String, (Vec<PathBuf>, notify::RecommendedWatcher)>,
    /// When the files of each source last changed, by source id: it runs once they are quiet.
    files_changed: BTreeMap<String, Instant>,
    /// Sources asked to run now.
    forced: BTreeSet<String>,
    /// The files a watching connector watches as they were when it last ran, by
    /// `<space id>/<source id>`: a file event runs it only when they changed since.
    stamps: BTreeMap<String, FileStamp>,
    /// The sources whose last run read everything there was without an error, by
    /// `<space id>/<source id>`: at its interval, such a watching connector runs only when its
    /// files changed since.
    read_all: BTreeSet<String>,
    last_config_error: Option<String>,
}

impl Runtime {
    fn supervise(&mut self, woken: &mpsc::Receiver<Wake>) {
        loop {
            if self.stopping.load(Ordering::SeqCst) {
                return;
            }
            let entries = self.entries();
            self.watch(&entries);
            // Sources whose files changed and have been quiet long enough.
            let quiet = self.options.quiet;
            let files: BTreeSet<String> = self
                .files_changed
                .iter()
                .filter(|(_, changed)| changed.elapsed() >= quiet)
                .map(|(id, _)| id.clone())
                .collect();
            self.files_changed.retain(|id, _| !files.contains(id));
            // The watched files as they are now, by source id, for the sources woken by them.
            let mut now: BTreeMap<String, Option<FileStamp>> = BTreeMap::new();
            for entry in &entries {
                if self.stopping.load(Ordering::SeqCst) {
                    return;
                }
                // An event on files that did not change since the last run (the connector's
                // own read of them, on macOS) runs nothing.
                let changed = files.contains(&entry.source_id) && {
                    let stamp = now
                        .entry(entry.source_id.clone())
                        .or_insert_with(|| stamp(entry.connector.as_ref()));
                    match (stamp, self.stamps.get(&entry.key())) {
                        (Some(now), Some(last)) => now != last,
                        _ => true,
                    }
                };
                if self.due(entry, changed) {
                    let key = entry.key();
                    let forced = self.forced.remove(&key);
                    // Taken before the run, so a change made while it runs runs it again.
                    let stamp = stamp(entry.connector.as_ref());
                    // At its interval, a watching connector whose files did not change since
                    // a run that read everything has nothing new to read: no copy is made.
                    if !forced && !changed && self.unchanged(entry, stamp.as_ref()) {
                        continue;
                    }
                    match stamp {
                        Some(stamp) => self.stamps.insert(key.clone(), stamp),
                        None => self.stamps.remove(&key),
                    };
                    if self.run(entry) {
                        self.read_all.insert(key);
                    } else {
                        self.read_all.remove(&key);
                    }
                }
            }
            let keys: BTreeSet<String> = entries.iter().map(Entry::key).collect();
            self.stamps.retain(|key, _| keys.contains(key));
            self.read_all.retain(|key| keys.contains(key));
            let wait = self.wait(&entries);
            match woken.recv_timeout(wait) {
                Ok(wake) => {
                    self.take(wake);
                    while let Ok(wake) = woken.try_recv() {
                        self.take(wake);
                    }
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return,
            }
        }
    }

    fn take(&mut self, wake: Wake) {
        match wake {
            Wake::Reload => {}
            Wake::Run(key) => {
                self.forced.insert(key);
            }
            Wake::Files(id) => {
                self.files_changed.insert(id, Instant::now());
            }
            Wake::Stop => self.stopping.store(true, Ordering::SeqCst),
        }
    }

    /// Every connector switched on in a space this service knows, when sources are on.
    fn entries(&mut self) -> Vec<Entry> {
        let registry = self.state.registry();
        let config = knode_config::Config::load_from(&self.home.join(knode_config::CONFIG_FILE));
        let config = match config {
            Ok(config) => {
                self.last_config_error = None;
                config
            }
            Err(e) => {
                let e = e.to_string();
                if self.last_config_error.as_ref() != Some(&e) {
                    eprintln!("Knode Service: sources stopped, config.toml cannot be read: {e}");
                }
                self.last_config_error = Some(e);
                registry.set_active(false);
                return Vec::new();
            }
        };
        let active = config.sources_enabled();
        registry.set_active(active);
        if !active {
            return Vec::new();
        }
        let mut paths: Vec<PathBuf> = config
            .known_spaces()
            .into_iter()
            .filter(|p| p.is_absolute() && p.exists())
            .collect();
        paths.extend(self.state.open_paths());
        let mut seen: BTreeSet<String> = BTreeSet::new();
        let mut entries = Vec::new();
        for path in paths {
            let Ok(space) = self.state.space(&path) else {
                continue;
            };
            let space = lock_ignoring_poison(&space);
            let space_id = space.manifest().id.clone();
            if !seen.insert(space_id.clone()) {
                continue;
            }
            let settings = match source_settings(&space) {
                Ok(settings) => settings,
                Err(e) => {
                    eprintln!(
                        "Knode Service: the sources of {} cannot be read: {e}",
                        space.path().display()
                    );
                    continue;
                }
            };
            for (id, setting) in settings {
                if !setting.on {
                    continue;
                }
                if let Some(connector) = registry.connector(&id) {
                    entries.push(Entry {
                        space: space.path().to_path_buf(),
                        space_id: space_id.clone(),
                        connector: connector.clone(),
                        source_id: id,
                        setting,
                    });
                }
            }
        }
        entries
    }

    /// Watches the files of the connectors that run, and stops watching the others.
    fn watch(&mut self, entries: &[Entry]) {
        let mut wanted: BTreeMap<String, Vec<PathBuf>> = BTreeMap::new();
        for entry in entries {
            if let Trigger::Watch { paths, .. } = entry.connector.trigger() {
                wanted.insert(entry.source_id.clone(), paths);
            }
        }
        self.watchers
            .retain(|id, (paths, _)| wanted.get(id) == Some(paths));
        for (id, paths) in wanted {
            if self.watchers.contains_key(&id) {
                continue;
            }
            match watcher(&id, &paths, self.wake.clone()) {
                Ok(watcher) => {
                    self.watchers.insert(id, (paths, watcher));
                }
                Err(e) => eprintln!("Knode Service: cannot watch the files of {id}: {e}"),
            }
        }
    }

    /// Whether the entry should run now: asked to, or its time came, or its files `changed`
    /// and are quiet, unless it waits after an error.
    fn due(&self, entry: &Entry, changed: bool) -> bool {
        let key = entry.key();
        if self.forced.contains(&key) {
            return true;
        }
        let registry = self.state.registry();
        let state = registry.states().get(&key).cloned().unwrap_or_default();
        let now = Timestamp::now();
        if state.failures > 0 && state.next_run.is_some_and(|t| now < t) {
            return false;
        }
        changed || state.next_run.is_none_or(|t| now >= t)
    }

    /// Whether the files the entry watches are as they were before its last run, which read
    /// everything: then its next run is only set later, and nothing is read or copied.
    fn unchanged(&self, entry: &Entry, now: Option<&FileStamp>) -> bool {
        let key = entry.key();
        if !matches!(entry.connector.trigger(), Trigger::Watch { .. })
            || !self.read_all.contains(&key)
        {
            return false;
        }
        let same = matches!((now, self.stamps.get(&key)), (Some(now), Some(last)) if now == last);
        if !same {
            return false;
        }
        let registry = self.state.registry();
        let mut states = registry.states();
        let Some(state) = states.get_mut(&key) else {
            return false;
        };
        // Kept in memory only: the state on disk says when it last really ran.
        state.next_run = Some(later(
            Timestamp::now(),
            interval(&self.options, entry.connector.as_ref()),
        ));
        true
    }

    /// How long to wait before looking again.
    fn wait(&self, entries: &[Entry]) -> Duration {
        let mut wait = self.options.settings_poll;
        let registry = self.state.registry();
        let now = Timestamp::now();
        for entry in entries {
            if let Some(next) = registry.states().get(&entry.key()).and_then(|s| s.next_run) {
                let ms = (next.0 - now.0).max(0) as u64;
                wait = wait.min(Duration::from_millis(ms));
            }
        }
        // Files that changed run once they are quiet.
        let quiet = self.options.quiet;
        for changed in self.files_changed.values() {
            wait = wait.min(quiet.saturating_sub(changed.elapsed()));
        }
        wait.max(Duration::from_millis(10))
    }

    /// Runs one connector for one space and records what happened. Nothing is recorded for a
    /// source disconnected while it ran: its state stays forgotten. Returns whether it read
    /// everything there was, without an error.
    fn run(&self, entry: &Entry) -> bool {
        let registry = self.state.registry();
        let key = entry.key();
        let started = Timestamp::now();
        registry.set_running(&key, true);
        let result = self.batches(entry, &key);
        registry.set_running(&key, false);
        if matches!(result, Ok((_, Ended::Gone))) {
            return false;
        }
        let done = matches!(result, Ok((_, Ended::Done)));
        let interval = interval(&self.options, entry.connector.as_ref());
        // The space is locked while the state is saved, so a disconnect cannot come between
        // the check that the source is still connected and the save.
        let space = match self.state.space(&entry.space) {
            Ok(space) => space,
            Err(e) => {
                eprintln!(
                    "Knode Service: {} cannot be opened: {}",
                    entry.space.display(),
                    e.message
                );
                return false;
            }
        };
        let space = lock_ignoring_poison(&space);
        if matches!(live(&space, entry), Ok(None)) {
            return false;
        }
        let mut states = registry.states();
        let state = states.entry(&key);
        state.last_run = Some(started);
        let now = Timestamp::now();
        match result {
            Ok((written, _)) => {
                state.last_success = Some(now);
                state.last_items = written.items;
                state.last_error = None;
                state.notice = state
                    .cursor
                    .as_ref()
                    .and_then(|c| entry.connector.notice(c));
                state.failures = 0;
                state.next_run = Some(later(now, interval));
            }
            Err((written, error)) => {
                state.last_items = written.items;
                state.failures = state.failures.saturating_add(1);
                let wait = backoff(&self.options, state.failures).max(match error {
                    // A database that changed is not checked again every few seconds.
                    SourceError::NeedsUpdate(_) => interval,
                    _ => Duration::ZERO,
                });
                state.next_run = Some(later(now, wait));
                let message = error.to_string();
                if state
                    .last_error
                    .as_ref()
                    .is_none_or(|e| e.message != message)
                {
                    eprintln!("Knode Service: {} failed: {message}", entry.source_id);
                }
                state.last_error = Some(LastError {
                    kind: error.kind().to_string(),
                    message,
                    at: now,
                });
            }
        }
        let read_all = done
            && !state
                .cursor
                .as_ref()
                .is_some_and(|c| entry.connector.waiting(c));
        if let Err(e) = states.save(&self.home) {
            eprintln!("Knode Service: the state of sources cannot be saved: {e}");
        }
        read_all
    }

    /// Fetches and writes batches until the connector has nothing more, the run is long
    /// enough, or the person paused or disconnected the source. Returns what was written and
    /// how the run ended, or the error that stopped it.
    ///
    /// The setting of the source is read again before each batch, so a pause or an exclusion
    /// made while the run goes on counts from the next batch, and again with the space locked
    /// before a batch is written: a batch fetched before the source was paused, disconnected,
    /// or had one of its chats excluded is not written, and its cursor is not saved.
    fn batches(
        &self,
        entry: &Entry,
        key: &str,
    ) -> Result<(Written, Ended), (Written, SourceError)> {
        let mut written = Written::default();
        entry.connector.check().map_err(|e| (written, e))?;
        let registry = self.state.registry();
        let secrets = SourceSecrets::new(&self.home, &entry.source_id);
        let by = provenance(&entry.source_id);
        for _ in 0..self.options.max_batches.max(1) {
            if self.stopping.load(Ordering::SeqCst) {
                break;
            }
            let setting = {
                let space = self.space(entry).map_err(|e| (written, e))?;
                let space = lock_ignoring_poison(&space);
                match live(&space, entry).map_err(|e| (written, e))? {
                    None => return Ok((written, Ended::Gone)),
                    Some(setting) if !setting.on => return Ok((written, Ended::Paused)),
                    Some(setting) => setting,
                }
            };
            let cursor = registry.states().get(key).and_then(|s| s.cursor.clone());
            let request = FetchRequest {
                space_id: &entry.space_id,
                cursor: cursor.as_ref(),
                history: setting.history,
                since: setting.since,
                excluded: &setting.exclude,
                max_items: self.options.max_items.max(1),
                secrets: &secrets,
            };
            let fetched = entry.connector.fetch(&request).map_err(|e| (written, e))?;
            let space = self.space(entry).map_err(|e| (written, e))?;
            let mut space = lock_ignoring_poison(&space);
            match live(&space, entry).map_err(|e| (written, e))? {
                None => return Ok((written, Ended::Gone)),
                Some(now) if !now.on => return Ok((written, Ended::Paused)),
                // Fetched with fewer exclusions than there are now: fetched again.
                Some(now)
                    if now
                        .exclude
                        .iter()
                        .any(|c| !setting.exclude.iter().any(|e| e.id == c.id)) =>
                {
                    continue;
                }
                Some(_) => {}
            }
            let mut this = Written::default();
            if let Some(batch) = fetched.batch.filter(|b| !b.is_empty()) {
                let mark =
                    last_change(&space).map_err(|e| (written, SourceError::Failed(e.message)))?;
                let result = batch.write(&mut space, &by);
                // What was written before a failure is announced too.
                let _ = self.state.notify_since(&space, mark);
                this = result.map_err(|e| (written, e))?;
                written += this;
            }
            // Saved only now that the batch is in the space, with the space still locked.
            {
                let mut states = registry.states();
                let state = states.entry(key);
                state.cursor = Some(fetched.cursor);
                state.items = state.items.saturating_add(this.items as u64);
                if let Err(e) = states.save(&self.home) {
                    return Err((
                        written,
                        SourceError::Failed(format!("the cursor cannot be saved: {e}")),
                    ));
                }
            }
            if !fetched.more {
                return Ok((written, Ended::Done));
            }
        }
        Ok((written, Ended::More))
    }

    fn space(&self, entry: &Entry) -> Result<Arc<Mutex<knode_format::Space>>, SourceError> {
        self.state
            .space(&entry.space)
            .map_err(|e| SourceError::Failed(e.message))
    }
}

/// How a run that did not fail ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ended {
    /// It read everything there was.
    Done,
    /// It ran long enough, or the service is stopping: there may be more to read.
    More,
    /// The person paused the source while it ran.
    Paused,
    /// The person disconnected the source while it ran (or disconnected and connected it
    /// again, which is another connection with its own state).
    Gone,
}

/// The setting of the entry's source in the space now, or `None` when it is no longer the
/// connection the run started for: disconnected, or connected again since.
fn live(space: &knode_format::Space, entry: &Entry) -> Result<Option<SourceSetting>, SourceError> {
    Ok(source_setting(space, &entry.source_id)
        .map_err(SourceError::Space)?
        .filter(|s| s.connected_at == entry.setting.connected_at))
}

/// The longest time between two runs of the connector.
fn interval(options: &SourcesOptions, connector: &dyn Connector) -> Duration {
    match connector.trigger() {
        // Events on files kept open come late: the interval is the bound.
        Trigger::Watch { every, .. } => every.min(options.watch_every),
        Trigger::Interval(every) => every,
    }
}

/// Whether the process `pid` is running (or cannot be told, so its files are kept).
fn running(pid: u32) -> bool {
    let Ok(pid) = libc::pid_t::try_from(pid) else {
        return true;
    };
    // Signal 0 sends nothing: it only asks whether the process is there.
    if unsafe { libc::kill(pid, 0) } == 0 {
        return true;
    }
    std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
}

/// The files a connector watches as they are now; `None` when it watches none, or they cannot
/// be told.
fn stamp(connector: &dyn Connector) -> Option<FileStamp> {
    match connector.trigger() {
        Trigger::Watch { paths, .. } => FileStamp::of(&paths),
        Trigger::Interval(_) => None,
    }
}

/// The wait after `failures` errors in a row: `backoff` doubled each time, at most
/// `max_backoff`.
fn backoff(options: &SourcesOptions, failures: u32) -> Duration {
    let doubled = options
        .backoff
        .saturating_mul(2u32.saturating_pow(failures.saturating_sub(1).min(20)));
    doubled.min(options.max_backoff)
}

fn later(now: Timestamp, wait: Duration) -> Timestamp {
    Timestamp(
        now.0
            .saturating_add(wait.as_millis().min(i64::MAX as u128) as i64),
    )
}

/// Watches the folders of `paths` (a database and its `-wal` are replaced and written beside
/// each other) and sends [`Wake::Files`] when anything in them changes.
fn watcher(
    id: &str,
    paths: &[PathBuf],
    wake: mpsc::Sender<Wake>,
) -> notify::Result<notify::RecommendedWatcher> {
    let id = id.to_string();
    // Events name files by their real path (`/private/var/...` for `/var/...` on macOS).
    let wanted: Vec<PathBuf> = paths.iter().map(|p| real(p)).collect();
    let wake = Mutex::new(wake);
    let mut watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
        let relevant = match &event {
            Ok(event) => event.paths.iter().any(|p| touches(&wanted, p)),
            Err(_) => true,
        };
        if relevant {
            let _ = lock_ignoring_poison(&wake).send(Wake::Files(id.clone()));
        }
    })?;
    let mut folders: BTreeSet<PathBuf> = BTreeSet::new();
    for path in paths {
        let path = real(path);
        let folder = if path.is_dir() {
            path
        } else {
            path.parent().map(Path::to_path_buf).unwrap_or_default()
        };
        if folder.is_dir() {
            folders.insert(folder);
        }
    }
    for folder in folders {
        watcher.watch(&folder, RecursiveMode::NonRecursive)?;
    }
    Ok(watcher)
}

/// The real path of `path`: itself when it exists, else its folder's real path and its name.
fn real(path: &Path) -> PathBuf {
    if let Ok(path) = std::fs::canonicalize(path) {
        return path;
    }
    match (path.parent().map(std::fs::canonicalize), path.file_name()) {
        (Some(Ok(parent)), Some(name)) => parent.join(name),
        _ => path.to_path_buf(),
    }
}

/// Whether a change to `path` concerns one of `wanted`: the path itself or anything in it, or
/// a file beside it whose name starts with its name (`chat.db-wal`).
fn touches(wanted: &[PathBuf], path: &Path) -> bool {
    wanted.iter().any(|w| {
        if path.starts_with(w) {
            return true;
        }
        match (w.parent(), w.file_name(), path.parent(), path.file_name()) {
            (Some(wp), Some(wn), Some(pp), Some(pn)) => {
                wp == pp && pn.to_string_lossy().starts_with(&*wn.to_string_lossy())
            }
            _ => false,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_doubles_up_to_the_most() {
        let options = SourcesOptions {
            backoff: Duration::from_secs(30),
            max_backoff: Duration::from_secs(100),
            ..SourcesOptions::with_connectors(Vec::new())
        };
        assert_eq!(backoff(&options, 1), Duration::from_secs(30));
        assert_eq!(backoff(&options, 2), Duration::from_secs(60));
        assert_eq!(backoff(&options, 3), Duration::from_secs(100));
        assert_eq!(backoff(&options, 400), Duration::from_secs(100));
    }

    #[test]
    fn a_process_that_ended_is_not_running() {
        assert!(running(std::process::id()));
        let mut child = std::process::Command::new("true").spawn().unwrap();
        let pid = child.id();
        child.wait().unwrap();
        assert!(!running(pid));
    }

    #[test]
    fn a_database_and_its_wal_are_watched() {
        let wanted = vec![PathBuf::from("/x/Messages/chat.db")];
        assert!(touches(&wanted, Path::new("/x/Messages/chat.db")));
        assert!(touches(&wanted, Path::new("/x/Messages/chat.db-wal")));
        assert!(!touches(&wanted, Path::new("/x/Messages/other.db")));
        let folder = vec![PathBuf::from("/x/Mail")];
        assert!(touches(&folder, Path::new("/x/Mail/V10/INBOX.mbox/1.emlx")));
    }
}
