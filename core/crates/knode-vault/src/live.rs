//! A live Markdown folder: a folder kept in step with a space as it changes, both ways.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use knode_format::{ChangeOp, Node, NodeId, NodeUpdate, Provenance, Space};
use knode_sync::{BodyEdit, Docs, Version};
use notify::Watcher as _;
use serde::{Deserialize, Serialize};

use crate::frontmatter::{entries, split};
use crate::import::{Batch, Note, Outcome, find, parse};
use crate::mirror::{Mirror, content_hash, read_id, write_if_changed};
use crate::names::{fold, stem_of, title_from_stem};
use crate::property_keys as keys;
use crate::{Error, ExportReport, ImportReport, Result, STATE_DIR, Skipped};

/// The file, inside [`STATE_DIR`], where a live folder keeps what it last wrote to each file.
pub const LIVE_FILE: &str = "live.json";

/// A folder that mirrors every node of a space as a `.md` file, both ways ([decision 0006]):
/// changes made in Knode are written to the folder, and changes made to the files, in any
/// editor, come back in as edits of their nodes. The space stays the source of truth.
///
/// # From the space to the folder
///
/// The owner of the space tells it what changed with [`record`](Self::record), with the node id
/// and change of every `changed` notification, and calls [`flush`](Self::flush) when
/// [`is_due`](Self::is_due) says the batch has waited long enough. Writes are batched: many
/// changes to a node in a few seconds write its file once.
///
/// - A changed node is written again; so are the nodes whose `[[links]]` point at it, whose
///   bodies change when it is renamed.
/// - A renamed or moved node is written to its new file and its old file is removed.
/// - A deleted node's file is removed.
/// - Files Knode did not write are never changed or removed, and a file is only removed while
///   its frontmatter still has the id of the node Knode wrote there.
/// - A file changed in the folder and not read back yet is never overwritten: its node waits
///   for the next [`pull`](Self::pull).
///
/// [`resync`](Self::resync) writes the whole space again, for when the folder is turned on or
/// changes may have been missed. Files have the layout of [`export`](crate::export); the folder
/// and an export to it are the same thing.
///
/// # From the folder to the space
///
/// [`watch`](Self::watch) watches the folder; [`pull`](Self::pull) reads the files that changed
/// since Knode last wrote or read them, once [`is_pull_due`](Self::is_pull_due) says the folder
/// has been quiet for a moment. Knode tells its own writes apart by the content it wrote, kept in
/// `.knode/live.json`, so they are never read back as edits. Every change is made as the person
/// who owns the folder, with the file's path as its source, and is one more edit in the
/// history of the node.
///
/// - A changed file updates its node: the node with the `id` of its frontmatter, else the node
///   last read from that path. Its frontmatter is read as [`import`](crate::import) reads it,
///   so the frontmatter of a note from another app is kept as it is.
/// - A new file becomes a new node, and gets the node's `id` as the first line of its
///   frontmatter. A copy of a file, with the id of a node whose own file is still there, becomes
///   a new node too.
/// - A renamed or moved file renames or moves its node: the title follows the new file name.
/// - A deleted file never deletes its node. The node gets the property
///   [`markdown.deleted`](crate::property_keys::DELETED) with the path the file had, and is no
///   longer written to the folder, until a person deletes it or removes the property to keep it
///   (which writes its file again). [`deleted_in_folder`](Self::deleted_in_folder) lists them.
/// - When the node also changed in Knode since its file was written, each field (title, kind,
///   tags, body) changed on one side only takes that side. A body changed on both sides is
///   merged with Loro when the node has a Loro document (`knode-sync`); otherwise, and for the
///   other fields, the side changed last wins, and when it is Knode's the file's version is
///   written first, so it stays in the node's history.
///
/// ```
/// use knode_format::{ChangeOp, NewNode, NodeUpdate, Provenance, Space};
/// use knode_vault::LiveFolder;
///
/// # let tmp = tempfile::tempdir().unwrap();
/// let me = Provenance::person("Ángel");
/// let mut space = Space::create(tmp.path().join("Notes.knode"), "Notes")?;
/// let mut folder = LiveFolder::open(tmp.path().join("Notes"))?;
/// folder.resync(&space)?;
///
/// let plan = space.create_node(NewNode::note("Plan", "Draft."), &me)?;
/// folder.record(plan.id, ChangeOp::Create);
/// let rename = NodeUpdate { title: Some("Roadmap".into()), ..Default::default() };
/// space.update_node(&plan.id, rename, &me)?;
/// folder.record(plan.id, ChangeOp::Update);
///
/// // When `folder.is_due()`, or at once:
/// folder.flush(&space)?;
/// let file = tmp.path().join("Notes/Roadmap.md");
/// assert!(!tmp.path().join("Notes/Plan.md").exists());
///
/// // An edit made in another editor comes back in.
/// let text = std::fs::read_to_string(&file)?.replace("Draft.", "Ship it.");
/// std::fs::write(&file, text)?;
/// let report = folder.pull(&mut space, &me)?;
/// assert_eq!(report.updated, ["Roadmap.md"]);
/// assert_eq!(space.get_node(&plan.id)?.unwrap().body, "Ship it.");
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
///
/// [decision 0006]: https://github.com/knodemd/handbook/blob/main/decisions/0006-markdown.md
#[derive(Debug)]
pub struct LiveFolder {
    mirror: Mirror,
    delay: Duration,
    pending: BTreeSet<NodeId>,
    since: Option<Instant>,
    /// What Knode last wrote to, or read from, each file, by path.
    seen: BTreeMap<String, Seen>,
    /// Where changes read from the folder say they came from: the folder, as the owner of the
    /// space records it.
    source_root: String,
    debounce: Duration,
    /// Changes seen in the folder and not read yet.
    signal: Arc<Signal>,
    watcher: Option<FolderWatcher>,
    docs: Docs,
}

/// What a [`LiveFolder::pull`] did. Paths are relative to the folder, with `/` between folders.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PullReport {
    /// The files that became new nodes.
    pub created: Vec<String>,
    /// The files that changed their node.
    pub updated: Vec<String>,
    /// Of those, the files that renamed or moved their node.
    pub renamed: Vec<String>,
    /// Of those, the files whose node had also changed in Knode since the file was written.
    pub merged: Vec<String>,
    /// The files deleted from the folder whose node was marked
    /// [`markdown.deleted`](crate::property_keys::DELETED), waiting for a person.
    pub deleted: Vec<String>,
    /// The nodes of those files, in the same order. Properties are not part of the history of
    /// a space, so these are the changes its log of changes does not have.
    pub marked: Vec<NodeId>,
    /// The files that could not be read in, and the typed links that could not be made.
    pub skipped: Vec<Skipped>,
    /// Every node changed, with how, for the owner of the space to announce.
    pub changes: Vec<(NodeId, ChangeOp)>,
    /// The bodies written through their node's Loro document, in order, for the owner of the
    /// space to send to the node's open editors. Their nodes are in `changes` too.
    #[serde(skip)]
    pub bodies: Vec<BodyWritten>,
}

/// A body a [`LiveFolder::pull`] wrote through its node's Loro document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BodyWritten {
    /// The node.
    pub id: NodeId,
    /// Who wrote it: the owner of the folder.
    pub by: Provenance,
    /// The version of the document the body holds now.
    pub version: Version,
    /// The Loro updates of the write, for editors that keep their own copy of the document.
    pub update: Vec<u8>,
}

impl PullReport {
    /// Whether nothing changed.
    pub fn is_empty(&self) -> bool {
        self.changes.is_empty() && self.skipped.is_empty()
    }
}

impl BodyWritten {
    fn of(id: NodeId, by: &Provenance, written: knode_sync::Written) -> Option<BodyWritten> {
        (!written.update.is_empty()).then(|| BodyWritten {
            id,
            by: by.clone(),
            version: written.version,
            update: written.update,
        })
    }
}

/// What Knode last wrote to, or read from, a file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Seen {
    /// The node.
    id: NodeId,
    /// The hash of the content, see [`content_hash`].
    hash: String,
    /// The size and modification time of the file, in nanoseconds since 1970, once they are old
    /// enough to be trusted to change with the content; otherwise the file is read to compare.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    stat: Option<(u64, u64)>,
    /// The hash of the node's title, kind, tags and body when the file had its content, see
    /// [`fields_hash`]: the version of the node the file was written from or read into.
    node: String,
    /// The version of the node's Loro document its body held then, if it had one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    loro: Option<String>,
}

/// `.knode/live.json`.
#[derive(Debug, Default, Serialize, Deserialize)]
struct SeenFile {
    version: u32,
    files: BTreeMap<String, Seen>,
}

/// Changes the watcher saw.
#[derive(Debug, Default)]
struct Signal {
    dirty: AtomicBool,
    last: Mutex<Option<Instant>>,
}

impl Signal {
    fn mark(&self) {
        *self.last.lock().unwrap_or_else(PoisonError::into_inner) = Some(Instant::now());
        self.dirty.store(true, Ordering::SeqCst);
    }
}

struct FolderWatcher(#[allow(dead_code)] notify::RecommendedWatcher);

impl std::fmt::Debug for FolderWatcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("FolderWatcher")
    }
}

/// How recent a modification time may be and still not be trusted: a file changed again within
/// it may keep the same time.
const RACY: Duration = Duration::from_secs(2);

impl LiveFolder {
    /// How long a batch waits after its first change before it is due: two seconds.
    pub const DEFAULT_DELAY: Duration = Duration::from_secs(2);
    /// How long the folder must be quiet before its changes are read: half a second.
    pub const DEFAULT_DEBOUNCE: Duration = Duration::from_millis(500);

    /// Opens the folder `dir`, which need not exist yet. Nothing is written until
    /// [`flush`](Self::flush) or [`resync`](Self::resync), and nothing read until
    /// [`pull`](Self::pull).
    pub fn open(dir: impl AsRef<Path>) -> Result<LiveFolder> {
        let dir = dir.as_ref();
        let mut mirror = Mirror::load(dir)?;
        mirror.live = true;
        Ok(LiveFolder {
            mirror,
            delay: Self::DEFAULT_DELAY,
            pending: BTreeSet::new(),
            since: None,
            seen: read_seen(dir)?,
            source_root: dir.to_string_lossy().replace('\\', "/"),
            debounce: Self::DEFAULT_DEBOUNCE,
            signal: Arc::new(Signal::default()),
            watcher: None,
            docs: Docs::new(),
        })
    }

    /// Sets how long a batch waits after its first change before it is due.
    pub fn with_delay(mut self, delay: Duration) -> LiveFolder {
        self.delay = delay;
        self
    }

    /// Sets how long the folder must be quiet before its changes are read.
    pub fn with_debounce(mut self, debounce: Duration) -> LiveFolder {
        self.debounce = debounce;
        self
    }

    /// Sets how changes read from the folder name it in their source, such as `~/Notes`: the
    /// source of a change is this, `/`, and the path of the file. By default, the folder's path.
    pub fn with_source_root(mut self, root: impl Into<String>) -> LiveFolder {
        self.source_root = root.into().trim_end_matches('/').to_string();
        self
    }

    /// The folder.
    pub fn dir(&self) -> &Path {
        self.mirror.dir()
    }

    /// Records that a node changed. Every kind of change is handled the same way: the node is
    /// read again on the next flush, and its file written, moved or removed.
    pub fn record(&mut self, id: NodeId, op: ChangeOp) {
        let _ = op;
        self.pending.insert(id);
        self.since.get_or_insert_with(Instant::now);
    }

    /// How many nodes changed since the last flush.
    pub fn pending(&self) -> usize {
        self.pending.len()
    }

    /// Whether there are changes and the first of them was recorded at least the delay ago.
    pub fn is_due(&self) -> bool {
        self.is_due_at(Instant::now())
    }

    /// Whether the batch is due at `now`. See [`is_due`](Self::is_due).
    pub fn is_due_at(&self, now: Instant) -> bool {
        self.since
            .is_some_and(|since| now.saturating_duration_since(since) >= self.delay)
    }

    /// Writes the changes recorded since the last flush, due or not. A node whose file changed
    /// in the folder and was not read back yet is not written: it waits for the next flush,
    /// after a [`pull`](Self::pull).
    pub fn flush(&mut self, space: &Space) -> Result<ExportReport> {
        let pending = std::mem::take(&mut self.pending);
        self.since = None;
        match self.sync(space, Some(&pending)) {
            Ok(report) => Ok(report),
            Err(e) => {
                // Keep the batch, to try again.
                self.pending.extend(pending);
                self.since.get_or_insert_with(Instant::now);
                Err(e)
            }
        }
    }

    /// Writes the batch if it is due. Returns `None` when it was not.
    pub fn flush_if_due(&mut self, space: &Space) -> Result<Option<ExportReport>> {
        if self.is_due() {
            self.flush(space).map(Some)
        } else {
            Ok(None)
        }
    }

    /// Writes every node again and removes the files of nodes that are gone, like
    /// [`export`](crate::export). Clears the batch. Files changed in the folder and not read
    /// back yet are left as they are: call [`pull`](Self::pull) first, as
    /// [`start`](Self::start) does.
    pub fn resync(&mut self, space: &Space) -> Result<ExportReport> {
        self.pending.clear();
        self.since = None;
        self.sync(space, None)
    }

    /// Brings the folder and the space in step when the folder is turned on, or when its owner
    /// opens the space: reads every file changed since Knode last wrote it, as
    /// [`pull`](Self::pull) does, then writes the whole space, as [`resync`](Self::resync) does.
    /// So edits made while Knode was not running come in, and are not overwritten.
    pub fn start(&mut self, space: &mut Space, owner: &Provenance) -> Result<PullReport> {
        let report = self.pull(space, owner)?;
        self.resync(space)?;
        Ok(report)
    }

    /// Starts watching the folder, creating it if needed. Changes are read by
    /// [`pull`](Self::pull) or [`tick`](Self::tick), once the folder has been quiet for the
    /// debounce delay.
    pub fn watch(&mut self) -> Result<()> {
        let dir = self.dir().to_path_buf();
        fs::create_dir_all(&dir).map_err(crate::io_error(&dir))?;
        let roots: Vec<PathBuf> = [Some(dir.clone()), fs::canonicalize(&dir).ok()]
            .into_iter()
            .flatten()
            .collect();
        let signal = self.signal.clone();
        let watch_error = |e: notify::Error| Error::Io {
            path: dir.clone(),
            source: std::io::Error::other(e.to_string()),
        };
        let mut watcher =
            notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
                let relevant = match &event {
                    Ok(event) => {
                        event.paths.is_empty() || event.paths.iter().any(|p| visible(&roots, p))
                    }
                    Err(_) => true,
                };
                if relevant {
                    signal.mark();
                }
            })
            .map_err(watch_error)?;
        watcher
            .watch(&dir, notify::RecursiveMode::Recursive)
            .map_err(watch_error)?;
        self.watcher = Some(FolderWatcher(watcher));
        Ok(())
    }

    /// Notes that files in the folder may have changed, as the watcher does: the next
    /// [`pull_if_due`](Self::pull_if_due) after the debounce delay reads them.
    pub fn folder_changed(&self) {
        self.signal.mark();
    }

    /// Whether the folder changed and has been quiet for the debounce delay.
    pub fn is_pull_due(&self) -> bool {
        self.is_pull_due_at(Instant::now())
    }

    /// Whether a pull is due at `now`. See [`is_pull_due`](Self::is_pull_due).
    pub fn is_pull_due_at(&self, now: Instant) -> bool {
        if !self.signal.dirty.load(Ordering::SeqCst) {
            return false;
        }
        let last = *self
            .signal
            .last
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        last.is_none_or(|last| now.saturating_duration_since(last) >= self.debounce)
    }

    /// Reads the folder's changes if they are due. Returns `None` when they were not.
    pub fn pull_if_due(
        &mut self,
        space: &mut Space,
        owner: &Provenance,
    ) -> Result<Option<PullReport>> {
        if self.is_pull_due() {
            self.pull(space, owner).map(Some)
        } else {
            Ok(None)
        }
    }

    /// What the owner of the space calls every second or so: reads the folder's changes when
    /// they are due, then writes the batch when it is due. With `all`, does both now. Returns
    /// what came in, for the owner to announce its changes.
    pub fn tick(&mut self, space: &mut Space, owner: &Provenance, all: bool) -> Result<PullReport> {
        let pulled = if all || self.is_pull_due() {
            self.pull(space, owner)
        } else {
            Ok(PullReport::default())
        };
        let flushed = if all {
            self.flush(space).map(Some)
        } else {
            self.flush_if_due(space)
        };
        let report = pulled?;
        flushed?;
        Ok(report)
    }

    /// Like [`start`](Self::start), merging bodies through `docs`, the [`Docs`] the owner of
    /// the space edits them with, so that its open documents stay in step.
    pub fn start_with(
        &mut self,
        space: &mut Space,
        owner: &Provenance,
        docs: &mut Docs,
    ) -> Result<PullReport> {
        self.with_docs(docs, |live| live.start(space, owner))
    }

    /// Like [`tick`](Self::tick), merging bodies through `docs`, as
    /// [`start_with`](Self::start_with) does.
    pub fn tick_with(
        &mut self,
        space: &mut Space,
        owner: &Provenance,
        all: bool,
        docs: &mut Docs,
    ) -> Result<PullReport> {
        self.with_docs(docs, |live| live.tick(space, owner, all))
    }

    /// Like [`pull`](Self::pull), merging bodies through `docs`, as
    /// [`start_with`](Self::start_with) does.
    pub fn pull_with(
        &mut self,
        space: &mut Space,
        owner: &Provenance,
        docs: &mut Docs,
    ) -> Result<PullReport> {
        self.with_docs(docs, |live| live.pull(space, owner))
    }

    /// Runs `f` with `docs` in place of the folder's own documents.
    fn with_docs<T>(&mut self, docs: &mut Docs, f: impl FnOnce(&mut Self) -> T) -> T {
        std::mem::swap(&mut self.docs, docs);
        let result = f(self);
        std::mem::swap(&mut self.docs, docs);
        result
    }

    /// Reads every file of the folder that changed since Knode last wrote or read it into the
    /// space, as `owner` (the person who owns the folder), with the path of the file as the
    /// source. See [`LiveFolder`] for what each change does. The nodes it changes are written
    /// back on the next flush, which brings their frontmatter up to date.
    pub fn pull(&mut self, space: &mut Space, owner: &Provenance) -> Result<PullReport> {
        self.signal.dirty.store(false, Ordering::SeqCst);
        let result = self.pull_now(space, owner);
        if result.is_err() {
            self.signal.mark();
        }
        result
    }

    fn pull_now(&mut self, space: &mut Space, owner: &Provenance) -> Result<PullReport> {
        let dir = self.dir().to_path_buf();
        let mut report = PullReport::default();
        if !dir.is_dir() {
            // A folder that is gone is written again by the next resync; nothing is deleted.
            return Ok(report);
        }
        let mut notes = Vec::new();
        let mut others = Vec::new();
        crate::import::walk(&dir, "", &mut notes, &mut others)?;

        // The files that changed, and the ids of the nodes whose files are still there.
        let mut on_disk: HashSet<String> = HashSet::new();
        let mut changed: Vec<(String, String, Option<Stat>)> = Vec::new();
        for rel in &notes {
            let full = dir.join(rel);
            let stat = stat_of(&full);
            if let Some(seen) = self.seen.get_mut(rel) {
                if seen.stat.is_some() && seen.stat == stat {
                    on_disk.insert(seen.id.to_string());
                    continue;
                }
            }
            let Ok(bytes) = fs::read(&full) else {
                continue;
            };
            let hash = content_hash(&bytes);
            if let Some(seen) = self.seen.get_mut(rel) {
                if seen.hash == hash {
                    seen.stat = trusted(stat);
                    on_disk.insert(seen.id.to_string());
                    continue;
                }
            }
            match String::from_utf8(bytes) {
                Ok(text) => {
                    if let Some(id) = read_id(&full) {
                        on_disk.insert(id);
                    }
                    changed.push((rel.clone(), text, stat));
                }
                Err(_) => report.skipped.push(Skipped {
                    path: rel.clone(),
                    reason: "not UTF-8 text".into(),
                }),
            }
        }

        let mut import = ImportReport::default();
        let mut batch = Batch::new(&dir, &others);
        let mut claimed: HashSet<String> = HashSet::new();
        for (rel, text, stat) in changed {
            let mut note = parse(&rel, &text);
            let by = owner
                .clone()
                .with_source(format!("{}/{rel}", self.source_root));
            // A copy of a file whose node still has its own file there becomes a new node.
            if let Some(id) = note.id.map(|id| id.to_string()) {
                let home = self.mirror.files().get(&id).filter(|p| **p != rel);
                let home_kept = home.is_some_and(|p| read_id(&dir.join(p)).as_ref() == Some(&id));
                if home_kept || !claimed.insert(id) {
                    note.id = None;
                }
            }
            let file_id = note.id;
            let modified = stat.map(|(_, m)| m);
            let mut hook = |space: &mut Space, id: NodeId, rel: &str, note: &mut Note| {
                self.resolve(space, id, rel, note, modified, &by, &mut report)
            };
            let Some((id, outcome)) = batch.note(space, &rel, note, &by, &mut import, &mut hook)?
            else {
                continue;
            };
            let mut text = text;
            if file_id.is_none() && matches!(outcome, Outcome::Created) {
                // The file gets the id of its node, so it stays with it when renamed.
                let with = with_id(&text, &id.to_string());
                let full = dir.join(&rel);
                if fs::read(&full).is_ok_and(|now| now == text.as_bytes()) {
                    write_if_changed(&full, with.as_bytes())?;
                    text = with;
                }
            }
            let stat = stat_of(&dir.join(&rel));
            self.remember(space, &rel, id, &text, stat)?;
            match outcome {
                Outcome::Created => {
                    report.created.push(rel.clone());
                    report.changes.push((id, ChangeOp::Create));
                }
                Outcome::Updated => {
                    report.updated.push(rel.clone());
                    report.changes.push((id, ChangeOp::Update));
                }
                Outcome::Unchanged => {}
            }
            if !matches!(outcome, Outcome::Unchanged) || self.seen_path(&id) != Some(rel.clone()) {
                self.record(id, ChangeOp::Update);
            }
        }
        batch.finish(space, owner, &mut import)?;
        report.skipped.extend(import.skipped);
        report.skipped.extend(import.unlinked);

        // Files gone from the folder, whose node is not in another file. Only a file Knode last
        // saw there counts: once marked, it is forgotten, so a note a person keeps is not marked
        // again before its file is written back.
        let gone: Vec<(String, String)> = self
            .mirror
            .files()
            .iter()
            .filter(|(eid, path)| {
                !on_disk.contains(*eid) && self.seen.contains_key(*path) && !dir.join(path).exists()
            })
            .map(|(eid, path)| (eid.clone(), path.clone()))
            .collect();
        for (eid, path) in gone {
            self.seen.remove(&path);
            let Some(id) = find(space, &eid)? else {
                continue;
            };
            if space.property(&id, keys::DELETED)?.is_some() {
                continue;
            }
            let by = owner
                .clone()
                .with_source(format!("{}/{path}", self.source_root));
            space.set_property(&id, keys::DELETED, &path, &by)?;
            report.deleted.push(path);
            report.marked.push(id);
            report.changes.push((id, ChangeOp::Update));
        }
        self.save_seen()?;
        Ok(report)
    }

    /// The nodes whose file was deleted from the folder, with the path the file had, waiting for
    /// a person to delete them or keep them. See
    /// [`markdown.deleted`](crate::property_keys::DELETED).
    pub fn deleted_in_folder(&self, space: &Space) -> Result<Vec<(NodeId, String)>> {
        let mut out = Vec::new();
        for summary in space.recent(usize::MAX)? {
            if let Some(path) = space.property(&summary.id, keys::DELETED)? {
                out.push((summary.id, path));
            }
        }
        Ok(out)
    }

    /// The path Knode last wrote or read a node's file at.
    fn seen_path(&self, id: &NodeId) -> Option<String> {
        self.mirror
            .files()
            .iter()
            .find(|(eid, _)| **eid == id.to_string())
            .map(|(_, p)| p.clone())
    }

    /// Decides what a changed file does to a node that may also have changed in Knode since the
    /// file was written, and sets the note to it. See [`LiveFolder`].
    #[allow(clippy::too_many_arguments)]
    fn resolve(
        &mut self,
        space: &mut Space,
        id: NodeId,
        rel: &str,
        note: &mut Note,
        modified: Option<u64>,
        by: &Provenance,
        report: &mut PullReport,
    ) -> Result<()> {
        space.remove_property(&id, keys::DELETED)?;
        let current = space
            .get_node(&id)?
            .ok_or(knode_format::Error::NotFound(id))?;
        let export_id = note.id.map(|i| i.to_string()).unwrap_or(id.to_string());

        // A renamed file renames the node, even when its frontmatter has a title.
        let before = self.mirror.files().get(&export_id).cloned();
        if let Some(before) = &before {
            if before != rel {
                if stem_of(before) != stem_of(rel) {
                    note.title = title_from_stem(stem_of(rel));
                }
                report.renamed.push(rel.to_string());
            }
        }

        let seen = self
            .seen
            .get(rel)
            .or_else(|| before.as_ref().and_then(|b| self.seen.get(b)))
            .filter(|s| s.id == id)
            .cloned();
        let file = Fields::of_note(note, &current);
        let now = Fields::of_node(&current);
        let knode_changed = seen
            .as_ref()
            .is_none_or(|s| s.node != fields_hash(&current));
        if !knode_changed || file == now {
            self.keep_doc_in_step(space, &id, &file.body, by, report);
            return Ok(());
        }

        // Both sides may have changed: field by field, from the version the file was written
        // from, when the history still has it.
        let history = space.history(&id)?;
        let base = seen.as_ref().and_then(|s| {
            history
                .iter()
                .map(|h| Fields {
                    title: h.title.clone(),
                    kind: h.kind.clone(),
                    tags: sorted(&h.tags),
                    body: h.body.clone(),
                })
                .rev()
                .find(|f| f.hash() == s.node)
        });
        // The side changed last wins a field both changed.
        let file_newer = modified.is_some_and(|m| (m / 1_000_000) as i64 > current.updated_at.0);
        let mut lost = false;
        let take_title = take(
            &file.title,
            &now.title,
            base.as_ref().map(|b| &b.title),
            file_newer,
            &mut lost,
        );
        let take_kind = take(
            &file.kind,
            &now.kind,
            base.as_ref().map(|b| &b.kind),
            file_newer,
            &mut lost,
        );
        let take_tags = take(
            &file.tags,
            &now.tags,
            base.as_ref().map(|b| &b.tags),
            file_newer,
            &mut lost,
        );
        let lost_before_body = lost;
        let mut body_lost = false;
        let take_body = take(
            &file.body,
            &now.body,
            base.as_ref().map(|b| &b.body),
            file_newer,
            &mut body_lost,
        );
        let body_conflict = file.body != now.body
            && base
                .as_ref()
                .is_none_or(|b| b.body != file.body && b.body != now.body);

        // Merge a body both changed with Loro, when the node has a document and the file says
        // which version it was written from.
        let loro_base = seen
            .as_ref()
            .and_then(|s| s.loro.as_deref())
            .and_then(|v| Version::from_json(v).ok());
        let use_loro = body_conflict && Docs::has_doc(space, &id) && loro_base.is_some();

        let lose_body = body_lost && !use_loro;
        if (lost_before_body || lose_body) && !in_history(&history, &file) {
            // Keep the file's version in the history before Knode's wins.
            space.update_node(
                &id,
                NodeUpdate {
                    title: Some(file.title.clone()),
                    kind: Some(file.kind.clone()),
                    tags: Some(file.tags.clone()),
                    body: (!use_loro).then(|| file.body.clone()),
                },
                by,
            )?;
        }
        let mut body = if take_body {
            file.body.clone()
        } else {
            now.body.clone()
        };
        if use_loro {
            let edit =
                BodyEdit::text(file.body.clone()).based_on(loro_base.expect("checked above"));
            if let Ok(written) = self.docs.apply_local_edit(space, &id, edit, by) {
                body = written.node.body.clone();
                report.bodies.extend(BodyWritten::of(id, by, written));
            }
        }
        note.title = if take_title { file.title } else { now.title };
        note.kind = Some(if take_kind { file.kind } else { now.kind });
        note.tags = if take_tags { file.tags } else { now.tags };
        self.keep_doc_in_step(space, &id, &body, by, report);
        note.body = body;
        report.merged.push(rel.to_string());
        Ok(())
    }

    /// Writes a new body through the node's Loro document, when it has one, so the document
    /// keeps every edit. The note then writes the same body, which changes nothing more.
    fn keep_doc_in_step(
        &mut self,
        space: &mut Space,
        id: &NodeId,
        body: &str,
        by: &Provenance,
        report: &mut PullReport,
    ) {
        if !Docs::has_doc(space, id) {
            return;
        }
        let differs = space
            .get_node(id)
            .ok()
            .flatten()
            .is_some_and(|n| n.body != body);
        if !differs {
            return;
        }
        let edit = BodyEdit::text(body.to_string());
        if let Ok(written) = self.docs.apply_local_edit(space, id, edit, by) {
            report.bodies.extend(BodyWritten::of(*id, by, written));
        }
    }

    /// Remembers that the file `rel` holds `text`, for the node `id` as it is now.
    fn remember(
        &mut self,
        space: &Space,
        rel: &str,
        id: NodeId,
        text: &str,
        stat: Option<(u64, u64)>,
    ) -> Result<()> {
        let Some(node) = space.get_node(&id)? else {
            self.seen.remove(rel);
            return Ok(());
        };
        self.seen.insert(
            rel.to_string(),
            Seen {
                id,
                hash: content_hash(text.as_bytes()),
                stat: trusted(stat),
                node: fields_hash(&node),
                loro: space.loro_version(&id)?,
            },
        );
        Ok(())
    }

    /// Writes the space to the folder, leaving alone the files changed in the folder and not
    /// read back yet, and remembers what it wrote.
    fn sync(&mut self, space: &Space, only: Option<&BTreeSet<NodeId>>) -> Result<ExportReport> {
        self.mirror.protected = self.changed_on_disk();
        let result = self.mirror.sync(space, only);
        self.mirror.protected.clear();
        let report = result?;
        let rendered = std::mem::take(&mut self.mirror.rendered);
        for file in rendered {
            let stat = stat_of(&self.dir().join(&file.path));
            let loro = space.loro_version(&file.id)?;
            self.seen.insert(
                file.path,
                Seen {
                    id: file.id,
                    hash: file.hash,
                    stat: trusted(stat),
                    node: file.fields,
                    loro,
                },
            );
        }
        let blocked = std::mem::take(&mut self.mirror.blocked);
        if !blocked.is_empty() {
            // Read the folder first, then write these nodes.
            for id in blocked {
                self.record(id, ChangeOp::Update);
            }
            self.signal.mark();
        }
        let paths: HashSet<&String> = self.mirror.files().values().collect();
        self.seen.retain(|path, _| paths.contains(path));
        self.save_seen()?;
        Ok(report)
    }

    /// The files, folded, that changed in the folder since Knode last wrote or read them,
    /// removed ones included.
    fn changed_on_disk(&self) -> HashSet<String> {
        let mut out = HashSet::new();
        for (path, seen) in &self.seen {
            let full = self.dir().join(path);
            let stat = stat_of(&full);
            if stat.is_some() && seen.stat == stat {
                continue;
            }
            let same = fs::read(&full).is_ok_and(|bytes| content_hash(&bytes) == seen.hash);
            if !same {
                out.insert(fold(path));
            }
        }
        out
    }

    fn save_seen(&self) -> Result<()> {
        let path = self.dir().join(STATE_DIR).join(LIVE_FILE);
        if self.seen.is_empty() && !path.exists() {
            return Ok(());
        }
        let file = SeenFile {
            version: 1,
            files: self.seen.clone(),
        };
        let mut text = serde_json::to_string_pretty(&file).map_err(|e| Error::Map {
            path: path.clone(),
            message: e.to_string(),
        })?;
        text.push('\n');
        write_if_changed(&path, text.as_bytes())?;
        Ok(())
    }
}

/// The fields of a node a file can change, as compared between the file, the node and the
/// version the file was written from.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Fields {
    title: String,
    kind: String,
    tags: Vec<String>,
    body: String,
}

impl Fields {
    fn of_node(node: &Node) -> Fields {
        Fields {
            title: node.title.clone(),
            kind: node.kind.clone(),
            tags: sorted(&node.tags),
            body: node.body.clone(),
        }
    }

    fn hash(&self) -> String {
        let mut text = String::new();
        for part in [
            &self.title,
            &self.kind,
            &self.tags.join("\u{1f}"),
            &self.body,
        ] {
            text.push_str(part);
            text.push('\0');
        }
        content_hash(text.as_bytes())
    }

    fn of_note(note: &Note, node: &Node) -> Fields {
        Fields {
            title: note.title.clone(),
            kind: note.kind.clone().unwrap_or_else(|| node.kind.clone()),
            tags: sorted(&note.tags),
            body: note.body.clone(),
        }
    }
}

/// Whether a field takes the file's value: when only the file changed it, or both did and the
/// file changed last. Sets `lost` when both changed it and Knode's value wins.
fn take<T: PartialEq>(
    file: &T,
    now: &T,
    base: Option<&T>,
    file_newer: bool,
    lost: &mut bool,
) -> bool {
    if file == now {
        return false;
    }
    match base {
        Some(base) if base == file => false,
        Some(base) if base == now => true,
        _ => {
            *lost |= !file_newer;
            file_newer
        }
    }
}

/// A hash of the fields of a node a file can change: its title, kind, tags and body.
pub(crate) fn fields_hash(node: &Node) -> String {
    Fields::of_node(node).hash()
}

fn sorted(tags: &[String]) -> Vec<String> {
    let set: BTreeSet<&String> = tags.iter().collect();
    set.into_iter().cloned().collect()
}

/// Whether the history of the node already has this version.
fn in_history(history: &[knode_format::HistoryEntry], fields: &Fields) -> bool {
    history.iter().any(|h| {
        h.title == fields.title
            && h.kind == fields.kind
            && h.body == fields.body
            && sorted(&h.tags) == fields.tags
    })
}

/// The size of a file and its modification time, in nanoseconds since 1970.
type Stat = (u64, u64);

/// The size and modification time of a file, in nanoseconds since 1970.
fn stat_of(path: &Path) -> Option<Stat> {
    let meta = fs::metadata(path).ok()?;
    let modified = meta.modified().ok()?.duration_since(UNIX_EPOCH).ok()?;
    Some((meta.len(), u64::try_from(modified.as_nanos()).ok()?))
}

/// A stat to compare with later, unless the file was modified too recently for its time to be
/// trusted.
fn trusted(stat: Option<(u64, u64)>) -> Option<(u64, u64)> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()?
        .as_nanos();
    let (_, modified) = stat?;
    (u128::from(modified) + RACY.as_nanos() <= now).then_some(stat?)
}

/// Whether a path the watcher reports is a file of the folder, not one inside a hidden folder
/// such as `.knode/` or a temporary file.
fn visible(roots: &[PathBuf], path: &Path) -> bool {
    let Some(rest) = roots.iter().find_map(|root| path.strip_prefix(root).ok()) else {
        return true;
    };
    !rest.components().any(|c| match c {
        Component::Normal(name) => name.to_string_lossy().starts_with('.'),
        _ => false,
    })
}

/// `text` with `id` as the id of its frontmatter: the first line of the frontmatter, replacing
/// the id it had, or a new frontmatter.
fn with_id(text: &str, id: &str) -> String {
    let parts = split(text);
    let nl = parts.newline;
    let Some(fm) = parts.frontmatter else {
        return format!("---{nl}id: {id}{nl}---{nl}{text}");
    };
    let start = fm.as_ptr() as usize - text.as_ptr() as usize;
    let first = entries(fm).into_iter().next();
    match first.filter(|e| e.key.as_deref() == Some("id")) {
        Some(entry) => {
            let line = entry
                .text
                .split_inclusive('\n')
                .next()
                .unwrap_or(entry.text);
            let end = start + line.len();
            format!("{}id: {id}{nl}{}", &text[..start], &text[end..])
        }
        None => format!("{}id: {id}{nl}{}", &text[..start], &text[start..]),
    }
}

fn read_seen(dir: &Path) -> Result<BTreeMap<String, Seen>> {
    let path = dir.join(STATE_DIR).join(LIVE_FILE);
    match fs::read_to_string(&path) {
        // A file that cannot be read is as if there were none: every file is compared with its
        // node again, and nothing is lost.
        Ok(text) => Ok(serde_json::from_str::<SeenFile>(&text)
            .map(|f| f.files)
            .unwrap_or_default()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(BTreeMap::new()),
        Err(e) => Err(crate::io_error(&path)(e)),
    }
}

/// A warning for a live folder inside a folder that a sync service such as iCloud Drive,
/// Dropbox, OneDrive or Google Drive keeps in step, or `None`. Two programs syncing the same
/// files can make conflicted copies; a space syncs on its own.
pub fn cloud_folder_warning(dir: impl AsRef<Path>) -> Option<String> {
    let parts: Vec<String> = dir
        .as_ref()
        .components()
        .filter_map(|c| match c {
            Component::Normal(name) => Some(name.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect();
    let service = parts.iter().enumerate().find_map(|(i, part)| {
        let next = parts.get(i + 1).map(String::as_str).unwrap_or("");
        if part == "Mobile Documents" || part == "iCloud Drive" {
            Some("iCloud Drive")
        } else if part == "CloudStorage" {
            Some(match next {
                n if n.starts_with("Dropbox") => "Dropbox",
                n if n.starts_with("OneDrive") => "OneDrive",
                n if n.starts_with("GoogleDrive") => "Google Drive",
                n if n.starts_with("iCloud") => "iCloud Drive",
                _ => "a cloud storage service",
            })
        } else if part == "Dropbox" || part.starts_with("Dropbox (") {
            Some("Dropbox")
        } else if part.starts_with("OneDrive") {
            Some("OneDrive")
        } else if part == "Google Drive" || part == "My Drive" {
            Some("Google Drive")
        } else {
            None
        }
    })?;
    Some(format!(
        "{} is inside {service}. Two programs syncing the same files can make conflicted \
         copies; a folder outside {service} is safer.",
        dir.as_ref().display()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn warns_about_cloud_folders() {
        for dir in [
            "/Users/a/Library/Mobile Documents/com~apple~CloudDocs/Notes",
            "/Users/a/Library/CloudStorage/Dropbox/Notes",
            "/Users/a/Dropbox/Notes",
            "/Users/a/Library/CloudStorage/OneDrive-Personal/Notes",
            "/Users/a/Library/CloudStorage/GoogleDrive-a@b.c/My Drive/Notes",
        ] {
            assert!(cloud_folder_warning(dir).is_some(), "{dir}");
        }
        assert!(cloud_folder_warning("/Users/a/Knode/Notes").is_none());
        assert!(
            cloud_folder_warning("/Users/a/Dropbox/Notes")
                .unwrap()
                .contains("Dropbox")
        );
    }

    #[test]
    fn batches_wait_for_the_delay() {
        let tmp = std::env::temp_dir().join("knode-vault-live-batch");
        let mut folder = LiveFolder::open(&tmp)
            .unwrap()
            .with_delay(Duration::from_secs(5));
        assert!(!folder.is_due());
        let id = NodeId::parse("01JABCDEFGHJKMNPQRSTVWXYZ0").unwrap();
        folder.record(id, ChangeOp::Update);
        folder.record(id, ChangeOp::Update);
        assert_eq!(folder.pending(), 1);
        let start = folder.since.unwrap();
        assert!(!folder.is_due_at(start + Duration::from_secs(4)));
        assert!(folder.is_due_at(start + Duration::from_secs(5)));
    }
}
