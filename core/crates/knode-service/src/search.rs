//! `search` by words, by meaning or both, and the index that search by meaning reads: the
//! shapes of `search`, `index.status` and `index.run`.
//!
//! Search by meaning needs the `semantic` feature (on by default, on every platform but
//! aarch64 Linux for now). With it, Knode Service downloads a small embedding model once into
//! `$KNODE_HOME/models`, indexes the open spaces in the background and ranks words and meaning
//! together (section 19 of the .knode specification). Without it, or while the model is not
//! ready, `search` is by words and says why.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use knode_format::{HybridHit, SearchMode, Space, rrf_score};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::protocol::{MAX_LIMIT, RpcError, SearchParams};
use crate::server::{State, lock_ignoring_poison, params, to_value};

/// What `search` returns when it is given a `mode`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SearchResults {
    /// The mode the search ran in. It is `words` when meaning was asked for but cannot be
    /// used yet; `note` says why.
    pub mode: SearchMode,
    /// The results, best first.
    pub hits: Vec<HybridHit>,
    /// Why the search ran by words, or that some notes were found by words only because they
    /// are not indexed yet.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// How Knode Service indexes spaces to search them by meaning.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexOptions {
    /// Pause while the machine runs on battery. `index.run` indexes anyway.
    pub pause_on_battery: bool,
    /// How long to wait after a change before indexing, so a burst of writes is indexed once.
    pub debounce: Duration,
    /// Download the model when it is not in `$KNODE_HOME/models` yet. Without it, search by
    /// meaning waits until the model is put there.
    pub download: bool,
}

impl Default for IndexOptions {
    fn default() -> Self {
        IndexOptions {
            pause_on_battery: true,
            debounce: Duration::from_secs(2),
            download: true,
        }
    }
}

/// What the index is doing: the result of `index.status`, and `index` in `service.status`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexStatus {
    /// Whether this build of Knode can search by meaning.
    pub available: bool,
    /// Whether search by meaning is on: `embeddings` in `config.toml`.
    pub enabled: bool,
    /// Whether the service indexes spaces. A service started without indexing (such as one
    /// embedded in tests) searches by words.
    pub running: bool,
    /// The embedding model.
    pub model: ModelStatus,
    /// Why indexing is paused, such as `on battery`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub paused: Option<String>,
    /// Each open space.
    #[serde(default)]
    pub spaces: Vec<SpaceIndex>,
}

/// The embedding model of the service.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelStatus {
    /// Its name, such as `multilingual-e5-small`; empty when this build has none.
    pub name: String,
    /// Where it is.
    pub state: ModelState,
    /// Bytes downloaded so far, while it downloads.
    #[serde(default)]
    pub downloaded: u64,
    /// The size of its files, in bytes.
    #[serde(default)]
    pub size: u64,
    /// What went wrong, when it failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Where the embedding model is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ModelState {
    /// This build of Knode has no search by meaning.
    Unavailable,
    /// Not downloaded yet.
    Missing,
    /// Downloading.
    Downloading,
    /// On disk, not loaded.
    Downloaded,
    /// Loading into memory.
    Loading,
    /// Loaded and in use.
    Ready,
    /// The download or the load failed; `error` says why. It is tried again later.
    Failed,
}

/// How far one open space is indexed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpaceIndex {
    /// The space, as the service opened it.
    pub space: PathBuf,
    /// Its nodes.
    pub nodes: u64,
    /// The nodes that wait to be indexed: new, changed, or not indexed yet.
    pub waiting: u64,
    /// Whether it is being indexed, or will be soon: it changed since it was last indexed.
    pub indexing: bool,
    /// What went wrong the last time it was indexed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub(crate) struct IndexRunParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub space: Option<PathBuf>,
}

/// The index of the service: search by meaning, when it is built in and started.
pub(crate) struct Index {
    #[cfg(semantic)]
    engine: std::sync::OnceLock<Arc<crate::index::Engine>>,
}

/// Stops indexing when dropped or stopped.
pub(crate) struct IndexTask {
    #[cfg(semantic)]
    task: crate::index::Task,
}

impl IndexTask {
    pub(crate) fn stop(self) {
        #[cfg(semantic)]
        self.task.stop();
    }
}

impl Index {
    pub(crate) fn new() -> Self {
        Index {
            #[cfg(semantic)]
            engine: std::sync::OnceLock::new(),
        }
    }

    /// Starts indexing the open spaces in the background. `None` when this build has no search
    /// by meaning.
    #[cfg_attr(not(semantic), allow(unused_variables))]
    pub(crate) fn start(
        &self,
        state: Arc<State>,
        home: &Path,
        options: IndexOptions,
    ) -> Option<IndexTask> {
        #[cfg(semantic)]
        {
            let engine = Arc::new(crate::index::Engine::new(home, options));
            if self.engine.set(engine.clone()).is_err() {
                return None;
            }
            Some(IndexTask {
                task: crate::index::Task::start(state, engine),
            })
        }
        #[cfg(not(semantic))]
        None
    }

    #[cfg(semantic)]
    fn engine(&self) -> Option<&Arc<crate::index::Engine>> {
        self.engine.get()
    }

    /// A space was opened or one of its nodes changed: index it soon.
    #[cfg_attr(not(semantic), allow(unused_variables))]
    pub(crate) fn changed(&self, space: &Path) {
        #[cfg(semantic)]
        if let Some(engine) = self.engine() {
            engine.changed(space);
        }
    }

    /// What the index is doing, from what it last saw. Never waits for a space.
    pub(crate) fn status(&self) -> IndexStatus {
        #[cfg(semantic)]
        if let Some(engine) = self.engine() {
            return engine.status();
        }
        IndexStatus {
            available: cfg!(semantic),
            enabled: false,
            running: false,
            model: ModelStatus {
                name: String::new(),
                state: ModelState::Unavailable,
                downloaded: 0,
                size: 0,
                error: None,
            },
            paused: None,
            spaces: Vec::new(),
        }
    }

    /// Reads `config.toml` again, then answers [`Index::status`].
    fn refreshed_status(&self) -> IndexStatus {
        #[cfg(semantic)]
        if let Some(engine) = self.engine() {
            engine.refresh_enabled();
        }
        self.status()
    }

    /// Indexes every open space now, even on battery, and retries a failed download.
    fn run(&self) {
        #[cfg(semantic)]
        if let Some(engine) = self.engine() {
            engine.run_now();
        }
    }

    /// The query embedded by the model the space is indexed with, or why there is none.
    #[cfg_attr(not(semantic), allow(unused_variables))]
    fn query_vector(&self, space: &Arc<Mutex<Space>>, query: &str) -> Meaning {
        #[cfg(semantic)]
        if let Some(engine) = self.engine() {
            return engine.query_vector(space, query);
        }
        Meaning::Unavailable(if cfg!(semantic) {
            "Search by meaning is not running in this Knode Service, so this search was by words."
                .into()
        } else {
            "This build of Knode searches by words only: search by meaning is not available on \
             this platform yet."
                .into()
        })
    }
}

/// The query vector for a search by meaning, or why there is none.
pub(crate) enum Meaning {
    /// The query vector, and how many nodes are not indexed yet.
    #[cfg_attr(not(semantic), allow(dead_code))]
    Vector(Vec<f32>, u64),
    /// Why meaning cannot be used for this search.
    Unavailable(String),
}

/// `search`: words, meaning or both. Without `mode` it answers the bare list of hits that
/// clients before `mode` expect, ranked as `hybrid`.
pub(crate) fn search(state: &State, raw: Value) -> Result<Value, RpcError> {
    let p: SearchParams = params(crate::protocol::methods::SEARCH, raw)?;
    let limit = p.limit.min(MAX_LIMIT);
    let requested = p.mode.unwrap_or_default();
    let space = state.space(&p.space)?;
    let meaning = match requested {
        SearchMode::Words => None,
        SearchMode::Meaning | SearchMode::Hybrid => {
            Some(state.index.query_vector(&space, &p.query))
        }
    };
    let space = lock_ignoring_poison(&space);
    let results = match meaning {
        Some(Meaning::Vector(vector, waiting)) => {
            let found = space.hybrid_search(&p.query, Some(&vector), requested, limit)?;
            let note = (waiting > 0).then(|| {
                format!(
                    "{} not indexed yet, so {} found by words only.",
                    count(waiting, "note is", "notes are"),
                    if waiting == 1 { "it is" } else { "they are" }
                )
            });
            SearchResults {
                mode: found.mode,
                hits: found.hits,
                note,
            }
        }
        other => SearchResults {
            mode: SearchMode::Words,
            hits: by_words(&space, &p.query, limit)?,
            note: match other {
                Some(Meaning::Unavailable(why)) => Some(why),
                _ => None,
            },
        },
    };
    if p.mode.is_some() {
        to_value(results)
    } else {
        to_value(results.hits)
    }
}

/// A search by words alone, as hybrid hits: every one ranked by words only.
fn by_words(space: &Space, query: &str, limit: usize) -> knode_format::Result<Vec<HybridHit>> {
    Ok(space
        .search(query, limit)?
        .into_iter()
        .enumerate()
        .map(|(index, hit)| {
            let rank = Some(index as u32 + 1);
            HybridHit {
                node: hit.node,
                snippet: hit.snippet,
                score: rrf_score(rank, None),
                word_rank: rank,
                meaning_rank: None,
            }
        })
        .collect())
}

/// `index.status`.
pub(crate) fn index_status(state: &State) -> Result<Value, RpcError> {
    to_value(state.index.refreshed_status())
}

/// `index.run`: opens the space when one is given, then indexes every open space now.
pub(crate) fn index_run(state: &State, raw: Value) -> Result<Value, RpcError> {
    let p: IndexRunParams = params(crate::protocol::methods::INDEX_RUN, raw)?;
    if let Some(space) = &p.space {
        state.space(space)?;
    }
    state.index.refreshed_status();
    state.index.run();
    to_value(state.index.status())
}

pub(crate) fn count(n: u64, one: &str, many: &str) -> String {
    if n == 1 {
        format!("1 {one}")
    } else {
        format!("{n} {many}")
    }
}
