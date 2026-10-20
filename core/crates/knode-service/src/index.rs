//! Indexing the open spaces in the background, to search them by meaning.
//!
//! One thread embeds the nodes that wait, a batch at a time. The model runs without the space:
//! the thread locks the space to read a batch, unlocks it while the model embeds the batch, and
//! locks it again to store the vectors, so writes never wait for the model. A node that changed
//! meanwhile is skipped and waits again (section 19.4 of the .knode specification).
//!
//! The model is downloaded once into `$KNODE_HOME/models`, in the background, only while
//! `embeddings` is on in `config.toml`, and loaded when there is work or a search. It is let go
//! after a while without either.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant};

use knode_embed::{Downloader, Embedder, Indexer, MANIFEST_FILE, ModelSpec, Progress};
use knode_format::{EmbeddingModel, Space};

use crate::search::{IndexOptions, IndexStatus, Meaning, ModelState, ModelStatus, SpaceIndex};
use crate::server::{State, lock_ignoring_poison};

/// Nodes embedded at once.
const BATCH: usize = Indexer::DEFAULT_BATCH_SIZE;
/// How often the thread looks again when nothing woke it: to resume after a pause, retry a
/// failed download or let the model go.
const TICK: Duration = Duration::from_secs(30);
/// How long to wait before trying a failed download or load again.
const RETRY: Duration = Duration::from_secs(10 * 60);
/// How long the model stays in memory with no indexing and no search.
const IDLE: Duration = Duration::from_secs(10 * 60);
/// How many batches in a row may store nothing before a space is left for the next change:
/// nodes that change faster than they are embedded.
const MAX_IDLE_BATCHES: u32 = 3;

/// The index: what it does and what it knows, shared by the indexing thread and the requests.
pub(crate) struct Engine {
    home: PathBuf,
    spec: ModelSpec,
    options: IndexOptions,
    inner: Mutex<Inner>,
    wake: Condvar,
    /// The loaded model. Loading takes this lock, so the model is loaded once.
    model: Mutex<Option<Arc<Embedder>>>,
}

struct Inner {
    enabled: bool,
    /// Spaces to index.
    dirty: HashSet<PathBuf>,
    /// Index now, even on battery, and retry a failure at once.
    force: bool,
    stop: bool,
    paused: Option<String>,
    model: ModelState,
    downloaded: u64,
    error: Option<String>,
    failed_at: Option<Instant>,
    last_used: Instant,
    spaces: BTreeMap<PathBuf, SpaceIndex>,
}

impl Engine {
    pub(crate) fn new(home: &Path, options: IndexOptions) -> Self {
        let spec = ModelSpec::default_model();
        let engine = Engine {
            home: home.to_path_buf(),
            options,
            inner: Mutex::new(Inner {
                enabled: true,
                dirty: HashSet::new(),
                force: false,
                stop: false,
                paused: None,
                model: ModelState::Missing,
                downloaded: 0,
                error: None,
                failed_at: None,
                last_used: Instant::now(),
                spaces: BTreeMap::new(),
            }),
            wake: Condvar::new(),
            model: Mutex::new(None),
            spec,
        };
        engine.refresh_enabled();
        let on_disk = engine.on_disk();
        engine.inner().model = if on_disk {
            ModelState::Downloaded
        } else {
            ModelState::Missing
        };
        engine
    }

    fn inner(&self) -> MutexGuard<'_, Inner> {
        lock_ignoring_poison(&self.inner)
    }

    fn model_dir(&self) -> PathBuf {
        self.home.join("models").join(&self.spec.id)
    }

    fn on_disk(&self) -> bool {
        self.model_dir().join(MANIFEST_FILE).is_file()
    }

    /// Reads `embeddings` in `config.toml` again (or `KNODE_EMBEDDINGS`). Turning it off lets
    /// the model go.
    pub(crate) fn refresh_enabled(&self) -> bool {
        let config = knode_config::Config::load_from(&self.home.join(knode_config::CONFIG_FILE));
        let enabled = match config {
            Ok(config) => config.embeddings_enabled_with_env(),
            // A config that cannot be read changes nothing: capture says what is wrong with it.
            Err(_) => self.inner().enabled,
        };
        let was = std::mem::replace(&mut self.inner().enabled, enabled);
        if was && !enabled {
            self.unload();
        } else if !was && enabled {
            let mut inner = self.inner();
            let open: Vec<PathBuf> = inner.spaces.keys().cloned().collect();
            inner.dirty.extend(open);
            drop(inner);
            self.wake.notify_all();
        }
        enabled
    }

    fn unload(&self) {
        let had = lock_ignoring_poison(&self.model).take().is_some();
        let mut inner = self.inner();
        if had || inner.model == ModelState::Ready {
            inner.model = ModelState::Downloaded;
        }
    }

    /// A space was opened or one of its nodes changed.
    pub(crate) fn changed(&self, space: &Path) {
        let mut inner = self.inner();
        inner.dirty.insert(space.to_path_buf());
        inner
            .spaces
            .entry(space.to_path_buf())
            .or_insert_with(|| SpaceIndex {
                space: space.to_path_buf(),
                nodes: 0,
                waiting: 0,
                indexing: false,
                error: None,
            });
        drop(inner);
        self.wake.notify_all();
    }

    /// `index.run`: every open space, now.
    pub(crate) fn run_now(&self) {
        let mut inner = self.inner();
        let open: Vec<PathBuf> = inner.spaces.keys().cloned().collect();
        inner.dirty.extend(open);
        inner.force = true;
        inner.failed_at = None;
        drop(inner);
        self.wake.notify_all();
    }

    pub(crate) fn status(&self) -> IndexStatus {
        let inner = self.inner();
        IndexStatus {
            available: true,
            enabled: inner.enabled,
            running: true,
            model: ModelStatus {
                name: self.spec.id.clone(),
                state: inner.model,
                downloaded: inner.downloaded,
                size: self.spec.download_size(),
                error: inner.error.clone(),
            },
            paused: inner.paused.clone(),
            spaces: inner
                .spaces
                .values()
                .map(|space| {
                    let mut space = space.clone();
                    // A change not looked at yet is indexed soon.
                    space.indexing |= inner.enabled && inner.dirty.contains(&space.space);
                    space
                })
                .collect(),
        }
    }

    /// The loaded model: loads it when it is on disk. `None` when it is not there yet.
    fn embedder(&self) -> Result<Option<Arc<Embedder>>, String> {
        let mut slot = lock_ignoring_poison(&self.model);
        if let Some(embedder) = slot.as_ref() {
            self.inner().last_used = Instant::now();
            return Ok(Some(embedder.clone()));
        }
        if !self.on_disk() {
            return Ok(None);
        }
        self.inner().model = ModelState::Loading;
        match Embedder::load(&self.model_dir()) {
            Ok(embedder) => {
                let embedder = Arc::new(embedder);
                *slot = Some(embedder.clone());
                let mut inner = self.inner();
                inner.model = ModelState::Ready;
                inner.error = None;
                inner.last_used = Instant::now();
                Ok(Some(embedder))
            }
            Err(e) => {
                let message = format!("cannot load the search model: {e}");
                self.failed(&message);
                Err(message)
            }
        }
    }

    fn failed(&self, message: &str) {
        let mut inner = self.inner();
        inner.model = ModelState::Failed;
        inner.error = Some(message.to_string());
        inner.failed_at = Some(Instant::now());
    }

    /// Downloads the model, reporting progress in the status.
    fn download(&self) -> Result<(), String> {
        let total = self.spec.download_size();
        {
            let mut inner = self.inner();
            inner.model = ModelState::Downloading;
            inner.downloaded = 0;
            inner.error = None;
        }
        let sizes: BTreeMap<String, u64> = self
            .spec
            .files
            .iter()
            .map(|f| (f.name.clone(), f.size))
            .collect();
        let mut finished = 0u64;
        let result = Downloader::new().ensure(&self.spec, &self.home.join("models"), |progress| {
            let downloaded = match progress {
                Progress::Started { .. } => finished,
                Progress::Downloading { done, .. } => finished + done,
                Progress::Verified { file } | Progress::Skipped { file } => {
                    finished += sizes.get(file).copied().unwrap_or(0);
                    finished
                }
                Progress::Ready { .. } => total,
            };
            self.inner().downloaded = downloaded.min(total);
        });
        match result {
            Ok(_) => {
                let mut inner = self.inner();
                inner.model = ModelState::Downloaded;
                inner.downloaded = total;
                Ok(())
            }
            Err(e) => {
                let message = format!("cannot download the search model: {e}");
                self.failed(&message);
                Err(message)
            }
        }
    }

    /// The query embedded for a search of `space`, or why meaning cannot be used.
    pub(crate) fn query_vector(&self, space: &Arc<Mutex<Space>>, query: &str) -> Meaning {
        let path = lock_ignoring_poison(space).path().to_path_buf();
        let (enabled, state, downloaded) = {
            let inner = self.inner();
            (inner.enabled, inner.model, inner.downloaded)
        };
        if !enabled {
            return Meaning::Unavailable(
                "Search by meaning is off (embeddings = \"off\" in config.toml), so this search \
                 was by words. `knode index run` turns it on."
                    .into(),
            );
        }
        let embedder = match self.embedder() {
            Ok(Some(embedder)) => embedder,
            Ok(None) => {
                // Not downloaded yet: make sure the download is under way.
                self.changed(&path);
                let total = self.spec.download_size().max(1);
                let progress = if state == ModelState::Downloading {
                    format!(" ({}%)", downloaded * 100 / total)
                } else {
                    String::new()
                };
                return Meaning::Unavailable(format!(
                    "The search model is downloading{progress}, once, so this search was by \
                     words. Search by meaning starts when it is ready."
                ));
            }
            Err(message) => {
                return Meaning::Unavailable(format!("{message}. This search was by words."));
            }
        };
        let in_use = {
            let space = lock_ignoring_poison(space);
            match space.embedding_model() {
                Ok(Some(model)) => space
                    .stale_node_count(model.id)
                    .map(|waiting| (model, waiting))
                    .ok(),
                _ => None,
            }
        };
        let Some((model, waiting)) = in_use else {
            self.changed(&path);
            return Meaning::Unavailable(
                "This space is not indexed for search by meaning yet, so this search was by \
                 words. Indexing runs in the background."
                    .into(),
            );
        };
        if model.spec != embedder.model_spec() {
            self.changed(&path);
            return Meaning::Unavailable(
                "This space is being indexed again with a new search model, so this search was \
                 by words."
                    .into(),
            );
        }
        match embedder.embed_query(query) {
            Ok(vector) => Meaning::Vector(vector, waiting),
            Err(e) => Meaning::Unavailable(format!(
                "The query could not be embedded ({e}), so this search was by words."
            )),
        }
    }

    /// Waits for work. Returns `None` to stop, or the spaces to index and whether it is forced.
    fn next(&self) -> Option<(Vec<PathBuf>, bool)> {
        let mut inner = self.inner();
        loop {
            if inner.stop {
                return None;
            }
            if !inner.dirty.is_empty() {
                break;
            }
            let (guard, _) = self
                .wake
                .wait_timeout(inner, TICK)
                .unwrap_or_else(|e| e.into_inner());
            inner = guard;
            if inner.dirty.is_empty() && inner.last_used.elapsed() > IDLE {
                drop(inner);
                self.unload();
                inner = self.inner();
            }
        }
        // Let a burst of changes settle.
        if !inner.force {
            let until = Instant::now() + self.options.debounce;
            while !inner.stop {
                let left = until.saturating_duration_since(Instant::now());
                if left.is_zero() {
                    break;
                }
                inner = self
                    .wake
                    .wait_timeout(inner, left)
                    .unwrap_or_else(|e| e.into_inner())
                    .0;
            }
        }
        if inner.stop {
            return None;
        }
        let force = std::mem::take(&mut inner.force);
        Some((inner.dirty.iter().cloned().collect(), force))
    }

    /// Why indexing should not run now, if it should not.
    fn hold(&self, force: bool) -> Option<String> {
        if !force && self.options.pause_on_battery && power::on_battery() {
            return Some("on battery".into());
        }
        None
    }

    fn sleep(&self, duration: Duration) {
        let inner = self.inner();
        if inner.stop || inner.force {
            return;
        }
        let _ = self.wake.wait_timeout(inner, duration);
    }

    fn stopping(&self) -> bool {
        self.inner().stop
    }

    /// One round: the model, then each space that waits.
    fn round(&self, state: &State, spaces: Vec<PathBuf>, force: bool) {
        if !self.refresh_enabled() {
            let mut inner = self.inner();
            inner.dirty.clear();
            inner.paused = None;
            return;
        }
        if let Some(reason) = self.hold(force) {
            self.inner().paused = Some(reason);
            self.sleep(TICK);
            return;
        }
        self.inner().paused = None;
        let retry_wait = {
            let inner = self.inner();
            inner
                .failed_at
                .map(|at| RETRY.saturating_sub(at.elapsed()))
                .filter(|left| !left.is_zero())
        };
        if let Some(left) = retry_wait {
            self.sleep(left.min(TICK));
            return;
        }
        if !self.on_disk() {
            if !self.options.download {
                self.sleep(TICK);
                return;
            }
            if self.download().is_err() {
                return;
            }
        }
        let embedder = match self.embedder() {
            Ok(Some(embedder)) => embedder,
            _ => return,
        };
        for path in spaces {
            if self.stopping() {
                return;
            }
            {
                let mut inner = self.inner();
                inner.dirty.remove(&path);
                if let Some(space) = inner.spaces.get_mut(&path) {
                    space.indexing = true;
                }
            }
            if let Err(message) = self.index_space(state, &embedder, &path, force) {
                let mut inner = self.inner();
                if let Some(space) = inner.spaces.get_mut(&path) {
                    space.error = Some(message);
                }
            }
        }
    }

    fn set_space(&self, path: &Path, f: impl FnOnce(&mut SpaceIndex)) {
        let mut inner = self.inner();
        if let Some(space) = inner.spaces.get_mut(path) {
            f(space);
        }
    }

    /// Indexes one space, a batch at a time, without holding it while the model runs.
    fn index_space(
        &self,
        state: &State,
        embedder: &Embedder,
        path: &Path,
        force: bool,
    ) -> Result<(), String> {
        let Some(space) = state.open_space(path) else {
            self.inner().spaces.remove(path);
            return Ok(());
        };
        let indexer = Indexer::new(embedder).with_batch_size(BATCH);
        let model: EmbeddingModel = {
            let mut space = lock_ignoring_poison(&space);
            if !space.vector_search_available() {
                return Err("sqlite-vec is not available, so this space cannot be indexed".into());
            }
            let model = indexer.prepare(&mut space).map_err(|e| e.to_string())?;
            let waiting = space
                .stale_node_count(model.id)
                .map_err(|e| e.to_string())?;
            let nodes = space.node_count().map_err(|e| e.to_string())?;
            self.set_space(path, |s| {
                s.nodes = nodes;
                s.waiting = waiting;
                s.error = None;
            });
            model
        };
        let result = self.batches(&indexer, &space, &model, path, force);
        self.set_space(path, |s| s.indexing = false);
        result
    }

    fn batches(
        &self,
        indexer: &Indexer<'_>,
        space: &Arc<Mutex<Space>>,
        model: &EmbeddingModel,
        path: &Path,
        force: bool,
    ) -> Result<(), String> {
        let mut idle = 0;
        loop {
            if self.stopping() || !self.inner().enabled {
                self.inner().dirty.insert(path.to_path_buf());
                return Ok(());
            }
            if let Some(reason) = self.hold(force) {
                let mut inner = self.inner();
                inner.paused = Some(reason);
                inner.dirty.insert(path.to_path_buf());
                return Ok(());
            }
            let nodes = {
                let space = lock_ignoring_poison(space);
                space
                    .stale_nodes(model.id, BATCH)
                    .map_err(|e| e.to_string())?
            };
            if nodes.is_empty() {
                break;
            }
            self.inner().last_used = Instant::now();
            // The model runs without the space, so writes go on meanwhile.
            let embedded = indexer.embed_nodes(nodes).map_err(|e| e.to_string())?;
            let (stored, waiting, count) = {
                let mut space = lock_ignoring_poison(space);
                let report = indexer
                    .store(&mut space, model, &embedded)
                    .map_err(|e| e.to_string())?;
                (
                    report.stored,
                    space
                        .stale_node_count(model.id)
                        .map_err(|e| e.to_string())?,
                    space.node_count().map_err(|e| e.to_string())?,
                )
            };
            self.set_space(path, |s| {
                s.waiting = waiting;
                s.nodes = count;
            });
            idle = if stored == 0 { idle + 1 } else { 0 };
            if waiting == 0 || idle >= MAX_IDLE_BATCHES {
                break;
            }
        }
        let mut space = lock_ignoring_poison(space);
        indexer
            .finish(&mut space, model)
            .map_err(|e| e.to_string())?;
        Ok(())
    }
}

/// The indexing thread.
pub(crate) struct Task {
    engine: Arc<Engine>,
    thread: Option<thread::JoinHandle<()>>,
}

impl Task {
    pub(crate) fn start(state: Arc<State>, engine: Arc<Engine>) -> Task {
        let worker = engine.clone();
        let thread = thread::Builder::new()
            .name("knode-index".into())
            .spawn(move || {
                while let Some((spaces, force)) = worker.next() {
                    worker.round(&state, spaces, force);
                }
            })
            .map_err(|e| eprintln!("Knode Service: cannot start indexing: {e}"))
            .ok();
        Task { engine, thread }
    }

    /// Stops indexing: the batch being embedded is dropped, and indexed again next time.
    pub(crate) fn stop(mut self) {
        self.shutdown();
    }

    fn shutdown(&mut self) {
        self.engine.inner().stop = true;
        self.engine.wake.notify_all();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for Task {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Whether the machine runs on battery.
mod power {
    use std::sync::Mutex;
    use std::time::{Duration, Instant};

    /// How long an answer is kept: asking the system is not free.
    const FRESH: Duration = Duration::from_secs(60);

    static CACHE: Mutex<Option<(Instant, bool)>> = Mutex::new(None);

    pub(super) fn on_battery() -> bool {
        let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((at, value)) = *cache {
            if at.elapsed() < FRESH {
                return value;
            }
        }
        let value = ask();
        *cache = Some((Instant::now(), value));
        value
    }

    #[cfg(target_os = "macos")]
    fn ask() -> bool {
        // `pmset -g ps` starts with "Now drawing from 'Battery Power'" or "'AC Power'".
        std::process::Command::new("/usr/bin/pmset")
            .args(["-g", "ps"])
            .output()
            .ok()
            .is_some_and(|out| String::from_utf8_lossy(&out.stdout).contains("'Battery Power'"))
    }

    #[cfg(target_os = "linux")]
    fn ask() -> bool {
        // On battery: a battery discharges and no mains supply is online.
        let Ok(entries) = std::fs::read_dir("/sys/class/power_supply") else {
            return false;
        };
        let read = |dir: &std::path::Path, file: &str| {
            std::fs::read_to_string(dir.join(file))
                .map(|s| s.trim().to_string())
                .unwrap_or_default()
        };
        let mut discharging = false;
        for entry in entries.flatten() {
            let dir = entry.path();
            match read(&dir, "type").as_str() {
                "Mains" if read(&dir, "online") == "1" => return false,
                "Battery" if read(&dir, "status") == "Discharging" => discharging = true,
                _ => {}
            }
        }
        discharging
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    fn ask() -> bool {
        false
    }
}
