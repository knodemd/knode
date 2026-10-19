//! Following the session folders while agents write to them.

use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use knode_format::Space;
use notify::{EventKind, RecursiveMode, Watcher as _};

use crate::capture::{Capture, CaptureError, CaptureReport};

/// How a [`Watcher`] batches changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WatchOptions {
    /// A batch is handed over once no file has changed for this long. Default 2 seconds.
    pub quiet: Duration,
    /// A batch is handed over at the latest this long after its first change, even while files
    /// keep changing, so a long agent turn still shows up. Default 30 seconds.
    pub max_delay: Duration,
    /// How often to ask for a full rescan, which finds what events missed and session folders
    /// that did not exist when watching started. `None` never asks. Default 5 minutes.
    pub rescan: Option<Duration>,
}

impl Default for WatchOptions {
    fn default() -> Self {
        Self {
            quiet: Duration::from_secs(2),
            max_delay: Duration::from_secs(30),
            rescan: Some(Duration::from_secs(300)),
        }
    }
}

/// What changed in the session folders.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Changes {
    /// These session files were written, created or removed. Give them to
    /// [`Capture::sync_paths`].
    Paths(Vec<PathBuf>),
    /// Changes may have been missed, or it is time for the periodic rescan. Call
    /// [`Capture::sync_all`].
    Rescan,
}

enum Message {
    Event(notify::Result<notify::Event>),
    Stop,
}

/// Watches the session folders and hands over the changed session files in batches.
///
/// Folders that do not exist are not watched; the periodic [`Changes::Rescan`] finds sessions
/// written there later. Dropping the watcher, or calling [`Watcher::stop`], stops it.
///
/// ```no_run
/// use std::sync::{Arc, Mutex};
///
/// use knode_format::Space;
/// use knode_ingest::{Capture, WatchOptions, Watcher};
///
/// let space = Arc::new(Mutex::new(Space::open("/Users/example/Knode/Personal.knode")?));
/// let capture = Arc::new(Mutex::new(Capture::open("/Users/example/state.json")?));
/// capture.lock().unwrap().sync_all(&mut space.lock().unwrap())?;
/// let watcher = Watcher::capture(capture, space, WatchOptions::default(), |result| {
///     if let Ok(report) = result {
///         for change in &report.changes {
///             println!("{:?} {}", change.op, change.node_id);
///         }
///     }
/// })?;
/// # drop(watcher);
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
pub struct Watcher {
    watcher: Option<notify::RecommendedWatcher>,
    sender: Sender<Message>,
    worker: Option<JoinHandle<()>>,
    watched: Vec<PathBuf>,
}

impl std::fmt::Debug for Watcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Watcher")
            .field("watched", &self.watched)
            .finish_non_exhaustive()
    }
}

impl Watcher {
    /// Watches `roots` recursively and calls `on_changes` from a background thread with each
    /// batch of changed `.jsonl` files, or [`Changes::Rescan`].
    pub fn start<F>(
        roots: &[PathBuf],
        options: WatchOptions,
        on_changes: F,
    ) -> Result<Self, CaptureError>
    where
        F: FnMut(Changes) + Send + 'static,
    {
        let (sender, receiver) = mpsc::channel();
        let events = sender.clone();
        let mut watcher = notify::recommended_watcher(move |event| {
            let _ = events.send(Message::Event(event));
        })?;
        let mut watched = Vec::new();
        for root in roots {
            if root.is_dir() {
                watcher.watch(root, RecursiveMode::Recursive)?;
                watched.push(root.clone());
            }
        }
        let worker = thread::Builder::new()
            .name("knode-capture-watch".into())
            .spawn(move || batch(receiver, options, on_changes))
            .map_err(|error| CaptureError::Watch(notify::Error::io(error)))?;
        Ok(Self {
            watcher: Some(watcher),
            sender,
            worker: Some(worker),
            watched,
        })
    }

    /// Watches the session folders of `capture` and syncs each batch into `space`, calling
    /// `on_report` with the result of every sync. A rescan runs [`Capture::sync_all`]; a batch
    /// of files runs [`Capture::sync_paths`].
    pub fn capture<F>(
        capture: Arc<Mutex<Capture>>,
        space: Arc<Mutex<Space>>,
        options: WatchOptions,
        mut on_report: F,
    ) -> Result<Self, CaptureError>
    where
        F: FnMut(Result<CaptureReport, CaptureError>) + Send + 'static,
    {
        let roots: Vec<PathBuf> = capture
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .roots()
            .iter()
            .map(|(_, root)| root.clone())
            .collect();
        Self::start(&roots, options, move |changes| {
            let mut capture = capture.lock().unwrap_or_else(PoisonError::into_inner);
            let mut space = space.lock().unwrap_or_else(PoisonError::into_inner);
            let result = match changes {
                Changes::Paths(paths) => capture.sync_paths(&mut space, &paths),
                Changes::Rescan => capture.sync_all(&mut space),
            };
            drop(space);
            drop(capture);
            on_report(result);
        })
    }

    /// The folders being watched: the roots that existed when watching started.
    pub fn watched(&self) -> &[PathBuf] {
        &self.watched
    }

    /// Stops watching. A batch being handed over finishes first; changes not yet handed over
    /// are dropped, and the next [`Capture::sync_all`] finds them.
    pub fn stop(mut self) {
        self.shutdown();
    }

    fn shutdown(&mut self) {
        self.watcher.take();
        let _ = self.sender.send(Message::Stop);
        if let Some(worker) = self.worker.take() {
            if worker.thread().id() != thread::current().id() {
                let _ = worker.join();
            }
        }
    }
}

impl Drop for Watcher {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn is_session(path: &Path) -> bool {
    path.extension() == Some(OsStr::new("jsonl"))
}

/// Collects events into batches and hands them over, until told to stop.
fn batch<F>(receiver: mpsc::Receiver<Message>, options: WatchOptions, mut on_changes: F)
where
    F: FnMut(Changes),
{
    let mut pending = BTreeSet::new();
    let mut rescan = false;
    let mut first: Option<Instant> = None;
    let mut last = Instant::now();
    let mut next_rescan = options.rescan.map(|every| Instant::now() + every);
    loop {
        let now = Instant::now();
        let mut deadline = next_rescan;
        if let Some(first) = first {
            let flush = (last + options.quiet).min(first + options.max_delay);
            deadline = Some(deadline.map_or(flush, |d| d.min(flush)));
        }
        let message = match deadline {
            Some(deadline) => receiver.recv_timeout(deadline.saturating_duration_since(now)),
            None => receiver.recv().map_err(|_| RecvTimeoutError::Disconnected),
        };
        match message {
            Ok(Message::Event(Ok(event))) => {
                if event.need_rescan() {
                    rescan = true;
                } else if matches!(
                    event.kind,
                    EventKind::Create(_)
                        | EventKind::Modify(_)
                        | EventKind::Remove(_)
                        | EventKind::Any
                ) {
                    pending.extend(event.paths.into_iter().filter(|p| is_session(p)));
                }
                if rescan || !pending.is_empty() {
                    first.get_or_insert_with(Instant::now);
                    last = Instant::now();
                }
            }
            Ok(Message::Event(Err(_))) => {
                rescan = true;
                first.get_or_insert_with(Instant::now);
                last = Instant::now();
            }
            Ok(Message::Stop) | Err(RecvTimeoutError::Disconnected) => return,
            Err(RecvTimeoutError::Timeout) => {}
        }
        let now = Instant::now();
        let scheduled = next_rescan.is_some_and(|at| now >= at);
        if scheduled {
            rescan = true;
            first.get_or_insert(now);
            next_rescan = options.rescan.map(|every| now + every);
        }
        let Some(started) = first else { continue };
        if scheduled || now >= last + options.quiet || now >= started + options.max_delay {
            if rescan {
                on_changes(Changes::Rescan);
            } else {
                on_changes(Changes::Paths(
                    std::mem::take(&mut pending).into_iter().collect(),
                ));
            }
            pending.clear();
            rescan = false;
            first = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn options() -> WatchOptions {
        WatchOptions {
            quiet: Duration::from_millis(50),
            max_delay: Duration::from_millis(400),
            rescan: None,
        }
    }

    fn event(path: &str) -> Message {
        Message::Event(Ok(notify::Event::new(EventKind::Modify(
            notify::event::ModifyKind::Any,
        ))
        .add_path(PathBuf::from(path))))
    }

    #[test]
    fn batches_session_files_until_quiet() {
        let (sender, receiver) = mpsc::channel();
        let (out, batches) = mpsc::channel();
        let worker =
            thread::spawn(move || batch(receiver, options(), move |c| out.send(c).unwrap()));
        sender.send(event("/r/a.jsonl")).unwrap();
        sender.send(event("/r/b.txt")).unwrap();
        sender.send(event("/r/a.jsonl")).unwrap();
        sender.send(event("/r/c.jsonl")).unwrap();
        let got = batches.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(
            got,
            Changes::Paths(vec!["/r/a.jsonl".into(), "/r/c.jsonl".into()])
        );
        sender.send(Message::Stop).unwrap();
        worker.join().unwrap();
        assert!(batches.try_recv().is_err());
    }

    #[test]
    fn hands_over_a_busy_batch_after_the_maximum_delay() {
        let (sender, receiver) = mpsc::channel();
        let (out, batches) = mpsc::channel();
        let worker =
            thread::spawn(move || batch(receiver, options(), move |c| out.send(c).unwrap()));
        let start = Instant::now();
        // Keep writing faster than `quiet` for longer than `max_delay`.
        while start.elapsed() < Duration::from_millis(700) {
            sender.send(event("/r/a.jsonl")).unwrap();
            thread::sleep(Duration::from_millis(10));
        }
        let got = batches
            .try_recv()
            .expect("a batch before the writes stopped");
        assert_eq!(got, Changes::Paths(vec!["/r/a.jsonl".into()]));
        sender.send(Message::Stop).unwrap();
        worker.join().unwrap();
    }

    #[test]
    fn asks_for_a_rescan_on_errors_and_on_schedule() {
        let (sender, receiver) = mpsc::channel();
        let (out, batches) = mpsc::channel();
        let worker =
            thread::spawn(move || batch(receiver, options(), move |c| out.send(c).unwrap()));
        sender
            .send(Message::Event(Err(notify::Error::generic("lost events"))))
            .unwrap();
        assert_eq!(
            batches.recv_timeout(Duration::from_secs(5)).unwrap(),
            Changes::Rescan
        );
        sender.send(Message::Stop).unwrap();
        worker.join().unwrap();

        let (sender, receiver) = mpsc::channel();
        let (out, batches) = mpsc::channel();
        let scheduled = WatchOptions {
            rescan: Some(Duration::from_millis(100)),
            ..options()
        };
        let worker =
            thread::spawn(move || batch(receiver, scheduled, move |c| out.send(c).unwrap()));
        assert_eq!(
            batches.recv_timeout(Duration::from_secs(5)).unwrap(),
            Changes::Rescan
        );
        sender.send(Message::Stop).unwrap();
        worker.join().unwrap();
    }
}
