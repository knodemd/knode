//! Capture inside the service: every Claude Code and Codex conversation becomes a node of the
//! default space, kept current while the agent writes it.
//!
//! The settings come from `config.toml` in the Knode home: `default_space`, `capture` (on unless
//! `false`) and `capture_exclude`. The file is read again from time to time, so changing it,
//! or creating the first space with `knode init`, takes effect without restarting the service.
//!
//! Capture writes through the same per-space mutex as the clients, so its writes never
//! interleave with theirs, and every node it creates or changes is announced with `changed`.
//! Locks are always taken in the order capture, then space.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use knode_format::{ChangeOp, Space};
use knode_ingest::{
    Agent, Capture, CaptureError, CaptureReport, ProjectFilter, WatchOptions, Watcher, discover,
};

use crate::protocol::ChangeKind;
use crate::server::State;

/// The folder in the Knode home where capture keeps its state.
pub const CAPTURE_DIR: &str = "capture";
/// The capture state file in [`CAPTURE_DIR`].
pub const CAPTURE_STATE_FILE: &str = "state.json";

/// How many session files are read under one lock while catching up, so that clients wait at
/// most for a few files.
const CATCH_UP_CHUNK: usize = 8;

/// How the service captures conversations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureOptions {
    /// The session folders to follow, by agent. `None` uses each agent's own folder:
    /// `$CLAUDE_CONFIG_DIR/projects` (by default `~/.claude/projects`) and `$CODEX_HOME/sessions`
    /// (by default `~/.codex/sessions`).
    pub roots: Option<Vec<(Agent, PathBuf)>>,
    /// How changes in the session folders are batched.
    pub watch: WatchOptions,
    /// How often `config.toml` is read again.
    pub settings_poll: Duration,
}

impl Default for CaptureOptions {
    fn default() -> Self {
        CaptureOptions {
            roots: None,
            watch: WatchOptions::default(),
            settings_poll: Duration::from_secs(5),
        }
    }
}

/// What capture should do, from `config.toml`.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Settings {
    /// The space to capture into; `None` when capture is off or there is no default space.
    space: Option<PathBuf>,
    exclude: Vec<PathBuf>,
}

fn read_settings(home: &Path) -> Result<Settings, String> {
    let config = knode_config::Config::load_from(&home.join(knode_config::CONFIG_FILE))
        .map_err(|e| e.to_string())?;
    let space = config
        .default_space
        .clone()
        .filter(|path| config.capture_enabled() && path.is_absolute());
    Ok(Settings {
        space,
        exclude: config.capture_exclude,
    })
}

/// The capture thread of a running service.
pub(crate) struct CaptureTask {
    stopping: Arc<AtomicBool>,
    wake: mpsc::Sender<()>,
    thread: Option<JoinHandle<()>>,
}

impl CaptureTask {
    /// Starts following `config.toml` in `home` and capturing as it says.
    pub(crate) fn start(
        state: Arc<State>,
        home: PathBuf,
        options: CaptureOptions,
    ) -> std::io::Result<Self> {
        let stopping = Arc::new(AtomicBool::new(false));
        let (wake, woken) = mpsc::channel();
        let thread = {
            let stopping = stopping.clone();
            thread::Builder::new()
                .name("knode-capture".into())
                .spawn(move || supervise(&state, &home, &options, &stopping, &woken))?
        };
        Ok(CaptureTask {
            stopping,
            wake,
            thread: Some(thread),
        })
    }

    /// Stops capturing and waits for a sync in progress to finish.
    pub(crate) fn stop(mut self) {
        self.shutdown();
    }

    fn shutdown(&mut self) {
        self.stopping.store(true, Ordering::SeqCst);
        let _ = self.wake.send(());
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for CaptureTask {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Reads the settings, starts and stops capture as they change, until told to stop.
fn supervise(
    state: &Arc<State>,
    home: &Path,
    options: &CaptureOptions,
    stopping: &AtomicBool,
    woken: &mpsc::Receiver<()>,
) {
    // The settings capture runs with, and its watcher while it runs.
    let mut current: Option<Settings> = None;
    let mut watcher: Option<Watcher> = None;
    let mut last_error: Option<String> = None;
    loop {
        match read_settings(home) {
            Ok(settings) => {
                let changed = current.as_ref() != Some(&settings);
                // A space that could not be opened is tried again at each poll.
                let retry = settings.space.is_some() && watcher.is_none();
                if changed || retry {
                    if let Some(old) = watcher.take() {
                        old.stop();
                        state.set_capturing(None);
                        eprintln!("Knode Service: capture stopped");
                    }
                    current = Some(settings.clone());
                    if let Some(space) = &settings.space {
                        match start(state, home, options, space, &settings.exclude, stopping) {
                            Ok(started) => {
                                watcher = started;
                                last_error = None;
                            }
                            Err(error) => {
                                state.set_capturing(None);
                                if last_error.as_ref() != Some(&error) {
                                    eprintln!("Knode Service: capture cannot start: {error}");
                                }
                                last_error = Some(error);
                            }
                        }
                    }
                }
            }
            Err(error) => {
                // Keep capturing with the settings already read.
                if last_error.as_ref() != Some(&error) {
                    eprintln!("Knode Service: {error}");
                }
                last_error = Some(error);
            }
        }
        if stopping.load(Ordering::SeqCst) {
            break;
        }
        match woken.recv_timeout(options.settings_poll) {
            Err(RecvTimeoutError::Timeout) => {}
            Ok(()) | Err(RecvTimeoutError::Disconnected) => break,
        }
        if stopping.load(Ordering::SeqCst) {
            break;
        }
    }
    if let Some(watcher) = watcher {
        watcher.stop();
    }
    state.set_capturing(None);
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Opens the space in the service, catches up with every session file and starts the watcher.
/// Returns `None` when the service started stopping meanwhile.
fn start(
    state: &Arc<State>,
    home: &Path,
    options: &CaptureOptions,
    path: &Path,
    exclude: &[PathBuf],
    stopping: &AtomicBool,
) -> Result<Option<Watcher>, String> {
    if !path.exists() {
        return Err(format!(
            "the default space {} does not exist",
            path.display()
        ));
    }
    let space: Arc<Mutex<Space>> = state.space(path).map_err(|e| e.message)?;
    let key = lock(&space).path().to_path_buf();
    let state_file = home.join(CAPTURE_DIR).join(CAPTURE_STATE_FILE);
    let mut capture = Capture::open(state_file)
        .map_err(|e| e.to_string())?
        .with_filter(ProjectFilter::excluding(exclude.iter().cloned()));
    if let Some(roots) = &options.roots {
        capture = capture.with_roots(roots.iter().cloned());
    }
    let roots = capture.roots().to_vec();
    let capture = Arc::new(Mutex::new(capture));
    state.set_capturing(Some(key.clone()));
    eprintln!(
        "Knode Service: capturing conversations into {}",
        key.display()
    );

    // Watch first, so nothing written while catching up is missed: the watcher's syncs wait
    // for the capture lock.
    let watcher = {
        let state = state.clone();
        let key = key.clone();
        Watcher::capture(
            capture.clone(),
            space.clone(),
            options.watch,
            move |result| {
                report(&state, &key, result);
            },
        )
        .map_err(|e| e.to_string())?
    };

    // Catch up a few files at a time, newest first, so clients never wait long and the latest
    // conversations show up first.
    let mut files = Vec::new();
    for (agent, root) in &roots {
        files.extend(discover(*agent, root).map_err(|e| e.to_string())?);
    }
    files.sort_by_key(|f| std::cmp::Reverse(f.modified));
    let paths: Vec<PathBuf> = files.into_iter().map(|f| f.path).collect();
    for chunk in paths.chunks(CATCH_UP_CHUNK) {
        if stopping.load(Ordering::SeqCst) {
            return Ok(None);
        }
        let result = {
            let mut capture = lock(&capture);
            let mut space = lock(&space);
            capture.sync_paths(&mut space, chunk)
        };
        report(state, &key, result);
    }
    // Then once more over everything, which also forgets files that are gone.
    if stopping.load(Ordering::SeqCst) {
        return Ok(None);
    }
    let result = {
        let mut capture = lock(&capture);
        let mut space = lock(&space);
        capture.sync_all(&mut space)
    };
    report(state, &key, result);
    Ok(Some(watcher))
}

/// Announces what a sync wrote, and logs what went wrong.
fn report(state: &State, space: &Path, result: Result<CaptureReport, CaptureError>) {
    match result {
        Ok(report) => {
            for change in &report.changes {
                let op = match change.op {
                    ChangeOp::Create => ChangeKind::Create,
                    ChangeOp::Update => ChangeKind::Update,
                    ChangeOp::Delete => ChangeKind::Delete,
                };
                state.notify_path(space, change.node_id, op);
            }
            for (path, error) in &report.failed {
                eprintln!(
                    "Knode Service: capture could not read {}: {error}",
                    path.display()
                );
            }
        }
        Err(error) => eprintln!("Knode Service: {error}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_follow_the_config_file() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        let off = Settings {
            space: None,
            exclude: Vec::new(),
        };
        assert_eq!(read_settings(home).unwrap(), off);

        let config = home.join("config.toml");
        std::fs::write(
            &config,
            "default_space = \"/s/Work.knode\"\ncapture_exclude = [\"/p\"]\n",
        )
        .unwrap();
        let on = read_settings(home).unwrap();
        assert_eq!(on.space, Some(PathBuf::from("/s/Work.knode")));
        assert_eq!(on.exclude, vec![PathBuf::from("/p")]);

        std::fs::write(
            &config,
            "default_space = \"/s/Work.knode\"\ncapture = false\n",
        )
        .unwrap();
        assert_eq!(read_settings(home).unwrap().space, None);

        std::fs::write(&config, "default_space = \"relative.knode\"\n").unwrap();
        assert_eq!(read_settings(home).unwrap().space, None);

        std::fs::write(&config, "default_space = 3\n").unwrap();
        assert!(read_settings(home).is_err());
    }
}
