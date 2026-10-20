//! Edit node bodies together: a person and agents edit the same note at once, and their edits
//! merge with [Loro](https://loro.dev) instead of one overwriting another.
//!
//! This crate implements section 21 of the
//! [.knode specification](https://github.com/knodemd/format/blob/main/spec/knode-0.md):
//!
//! - Each node may have one Loro document holding its body as a `LoroText` named `body`, made
//!   the first time the body is edited together, in `sync/nodes/<id>/`: a `snapshot.loro` and
//!   one `updates/<ULID>.loro` file per write, each written to a temporary name and renamed.
//! - `nodes.body` in SQLite stays the source of truth. Every merged edit is written to it as its
//!   own change, with history and provenance, and `loro_docs` keeps the version of the document
//!   the body holds, in the same transaction. A reader that does not use Loro reads SQLite only.
//! - Each Loro peer is recorded in `loro_peers` with the provenance of the edits made as it, so
//!   authorship survives merging.
//!
//! [`Docs`] is the entry point: it keeps the documents a program has open.
//!
//! ```
//! use knode_format::{NewNode, Provenance, Space};
//! use knode_sync::{BodyEdit, Docs};
//!
//! # let dir = tempfile::tempdir().unwrap();
//! let mut space = Space::create(dir.path().join("Example.knode"), "Example")?;
//! let ada = Provenance::person("Ada");
//! let claude = Provenance::agent("claude-code");
//! let note = space.create_node(NewNode::note("Plan", "Ship v0.3."), &ada)?;
//!
//! let mut docs = Docs::new();
//! let seen = docs.open_doc(&mut space, &note.id)?;
//!
//! // Ada types while Claude rewrites the text it read: both edits are kept.
//! docs.apply_local_edit(&mut space, &note.id, BodyEdit::insert(9, " together"), &ada)?;
//! docs.apply_local_edit(
//!     &mut space,
//!     &note.id,
//!     BodyEdit::text("Release v0.3.").based_on(seen.version),
//!     &claude,
//! )?;
//! assert_eq!(space.get_node(&note.id)?.unwrap().body, "Release v0.3 together.");
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

#![warn(missing_docs)]

mod error;
mod files;
mod version;

use std::collections::{BTreeSet, HashMap};
use std::fs;
use std::io;

use knode_format::{Node, NodeId, Provenance, Space};
use loro::{ExportMode, LoroDoc, LoroText, UpdateOptions};

pub use error::{Error, Result};
pub use files::{EXTENSION, SNAPSHOT_FILE, UPDATES_DIR};
/// The Loro crate this one uses, for editors that keep their own copy of a document.
pub use loro;
pub use version::Version;

/// The root container of a node's document: a `LoroText` holding its Markdown body.
pub const BODY: &str = "body";

/// A node's `updates/` folder is compacted when it holds more files than this (21.6), or more
/// bytes than its snapshot.
pub const COMPACT_AFTER_FILES: usize = 100;

/// How long a character diff of a new full text may take before a line diff is used instead.
const DIFF_TIMEOUT_MS: f64 = 50.0;

/// A change to a node's body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BodyEdit {
    /// The version of the document the writer saw, to which the edit applies. Edits made since
    /// are merged with it, not undone. `None` means the current version.
    pub base: Option<Version>,
    /// What changes.
    pub change: BodyChange,
}

/// What a [`BodyEdit`] changes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BodyChange {
    /// Splices made in order, as an editor makes them.
    Splices(Vec<Splice>),
    /// The new full text, when the writer has only that, such as an agent rewriting a note. It
    /// is turned into Loro operations by a diff from the text of the base version.
    Text(String),
}

/// One splice of a text: at `at`, delete `delete` characters and insert `insert`. Positions and
/// lengths count Unicode scalar values (Rust `char`s), not bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Splice {
    /// Where, in characters from the start.
    pub at: usize,
    /// How many characters to delete there.
    pub delete: usize,
    /// What to insert there.
    pub insert: String,
}

impl BodyEdit {
    /// Replaces the whole body with `text`, by its diff from the base version.
    pub fn text(text: impl Into<String>) -> Self {
        BodyEdit {
            base: None,
            change: BodyChange::Text(text.into()),
        }
    }

    /// Applies splices in order.
    pub fn splices(splices: Vec<Splice>) -> Self {
        BodyEdit {
            base: None,
            change: BodyChange::Splices(splices),
        }
    }

    /// Inserts `text` at character `at`.
    pub fn insert(at: usize, text: impl Into<String>) -> Self {
        Self::splices(vec![Splice {
            at,
            delete: 0,
            insert: text.into(),
        }])
    }

    /// Deletes `len` characters at character `at`.
    pub fn delete(at: usize, len: usize) -> Self {
        Self::splices(vec![Splice {
            at,
            delete: len,
            insert: String::new(),
        }])
    }

    /// Makes the edit apply to `version`, the version the writer saw.
    pub fn based_on(mut self, version: Version) -> Self {
        self.base = Some(version);
        self
    }
}

/// A node's document as a writer opens it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenDoc {
    /// The body, equal to `nodes.body`.
    pub text: String,
    /// The version of the document, to pass back with the next edit and to ask for the updates
    /// made since with [`Docs::export_updates_since`].
    pub version: Version,
}

/// What a write did.
#[derive(Debug, Clone)]
pub struct Written {
    /// The node as it is now in SQLite.
    pub node: Node,
    /// The version of the document the body holds now.
    pub version: Version,
    /// The Loro updates the write made, to send to the other editors of the node, who import
    /// them. Empty when nothing changed.
    pub update: Vec<u8>,
}

/// What a compaction did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Compaction {
    /// The update files folded into the snapshot and removed.
    pub removed: usize,
    /// The size of the new snapshot, in bytes.
    pub snapshot_bytes: u64,
}

/// A node's document, loaded from its files.
struct Loaded {
    doc: LoroDoc,
    /// Update files imported, which a compaction may remove.
    read: BTreeSet<String>,
    /// Update files that could not be imported, such as a partial file left by a crash.
    bad: BTreeSet<String>,
    /// The snapshot file last imported.
    snapshot: Option<files::Stamp>,
}

impl Loaded {
    fn new() -> Self {
        let doc = LoroDoc::new();
        doc.set_record_timestamp(true);
        Loaded {
            doc,
            read: BTreeSet::new(),
            bad: BTreeSet::new(),
            snapshot: None,
        }
    }

    fn text(&self) -> LoroText {
        self.doc.get_text(BODY)
    }

    fn version(&self) -> Version {
        Version::from_loro(&self.doc.oplog_vv())
    }

    /// Imports the snapshot if it changed and the update files not read yet. Files that cannot be
    /// imported are skipped: SQLite stays the source of truth, and 21.5 repairs the document.
    fn refresh(&mut self, dir: &std::path::Path) -> io::Result<()> {
        let snapshot = files::snapshot_path(dir);
        let stamp = files::stamp(&snapshot)?;
        if stamp.is_some() && stamp != self.snapshot {
            match fs::read(&snapshot) {
                Ok(bytes) => {
                    let _ = self.doc.import(&bytes);
                }
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => return Err(e),
            }
            self.snapshot = stamp;
        }
        let updates = files::updates_dir(dir);
        let mut batch = Vec::new();
        for (name, _) in files::update_names(dir)? {
            if self.read.contains(&name) || self.bad.contains(&name) {
                continue;
            }
            match fs::read(updates.join(&name)) {
                Ok(bytes) => batch.push((name, bytes)),
                // Removed by a compaction meanwhile: the snapshot has it.
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => return Err(e),
            }
        }
        if batch.is_empty() {
            return Ok(());
        }
        let bytes: Vec<Vec<u8>> = batch.iter().map(|(_, b)| b.clone()).collect();
        if self.doc.import_batch(&bytes).is_ok() {
            self.read.extend(batch.into_iter().map(|(name, _)| name));
        } else {
            for (name, bytes) in batch {
                if self.doc.import(&bytes).is_ok() {
                    self.read.insert(name);
                } else {
                    self.bad.insert(name);
                }
            }
        }
        // A compaction elsewhere may have folded files in and removed them: re-read the
        // snapshot next time it changes.
        Ok(())
    }
}

/// The Loro documents a program has open, and the Loro peers it edits as.
///
/// One `Docs` belongs to one program run: it makes a new random peer id for each provenance it
/// edits for in each space, as the specification asks, so two programs never share a peer. Other
/// programs may write the same documents at the same time; their update files are read before
/// each operation.
///
/// Every method takes the [`Space`] the node is in. A write is made in two steps: the update file
/// is written and flushed first, then the node is written in one SQLite transaction with its
/// `loro_docs` version (21.4). A crash between the two leaves the document ahead of the body,
/// and the next operation on the node writes the body (21.5).
#[derive(Default)]
pub struct Docs {
    open: HashMap<(String, NodeId), Loaded>,
    peers: HashMap<(String, Provenance), u64>,
}

impl std::fmt::Debug for Docs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Docs")
            .field("open", &self.open.len())
            .field("peers", &self.peers.len())
            .finish()
    }
}

impl Docs {
    /// No documents open yet.
    pub fn new() -> Self {
        Self::default()
    }

    /// Opens a node's document for editing together, making it if the node has none yet from
    /// the current body, inserted as `system`/`knode` (21.1), and bringing it in step with
    /// `nodes.body` (21.5).
    pub fn open_doc(&mut self, space: &mut Space, node: &NodeId) -> Result<OpenDoc> {
        self.guard(space, node, |docs, space| {
            docs.prepare(space, node)?;
            let loaded = docs.loaded(space, node);
            Ok(OpenDoc {
                text: loaded.text().to_string(),
                version: loaded.version(),
            })
        })
    }

    /// Whether a node has a document in `sync/`.
    pub fn has_doc(space: &Space, node: &NodeId) -> bool {
        space.loro_dir(node).is_dir()
    }

    /// Edits a node's body as `by`: merges the edit into its document, writes the update file,
    /// then writes the body, history and change in SQLite (21.4).
    ///
    /// Splices apply to the text of the edit's base version. A new full text is turned into
    /// splices by a diff from that text, so edits others made since the base are kept.
    pub fn apply_local_edit(
        &mut self,
        space: &mut Space,
        node: &NodeId,
        edit: BodyEdit,
        by: &Provenance,
    ) -> Result<Written> {
        self.guard(space, node, |docs, space| {
            docs.prepare(space, node)?;
            let current = docs.loaded(space, node).version();
            let base = edit.base.clone().unwrap_or_else(|| current.clone());
            if !current.includes(&base) {
                return Err(Error::UnknownVersion(base.to_json()));
            }
            let before = docs.loaded(space, node).doc.oplog_vv();
            if base == current {
                let peer = docs.peer_for(space, by)?;
                let loaded = docs.loaded(space, node);
                loaded.doc.set_peer_id(peer)?;
                apply_change(&loaded.text(), &edit.change)?;
                loaded.doc.commit();
            } else {
                // The writer saw an older version: edit a fork at it and merge. The fork may go
                // on with this program's peer only if it has every operation made as it.
                let peer = match docs.peer_for(space, by)? {
                    peer if current.count(peer) == base.count(peer) => peer,
                    _ => docs.rotate_peer(space, by)?,
                };
                let loaded = docs.loaded(space, node);
                let base_vv = base.to_loro();
                let fork = loaded.doc.fork_at(&loaded.doc.vv_to_frontiers(&base_vv))?;
                fork.set_record_timestamp(true);
                fork.set_peer_id(peer)?;
                apply_change(&fork.get_text(BODY), &edit.change)?;
                fork.commit();
                let update = fork.export(ExportMode::updates(&base_vv))?;
                loaded.doc.import(&update)?;
            }
            docs.write(space, node, &before, by)
        })
    }

    /// Merges Loro updates made by another editor of the node, such as an open editor with its
    /// own copy of the document, and writes the result as one change.
    ///
    /// Every peer that made the updates must be recorded in `loro_peers` (see
    /// [`new_peer`](Self::new_peer)); the change's provenance is theirs when they are all one,
    /// otherwise `system`/`knode`. Updates already merged change nothing.
    pub fn merge_updates(
        &mut self,
        space: &mut Space,
        node: &NodeId,
        bytes: &[u8],
    ) -> Result<Written> {
        self.guard(space, node, |docs, space| {
            docs.prepare(space, node)?;
            let loaded = docs.loaded(space, node);
            let before = loaded.doc.oplog_vv();
            let status = loaded.doc.import(bytes)?;
            if status.pending.is_some() {
                return Err(Error::MissingHistory);
            }
            let after = loaded.version();
            let by = provenance_of(space, &after, &Version::from_loro(&before))?;
            docs.write(space, node, &before, &by)
        })
    }

    /// The Loro updates of a node's document made since `version`, for an editor that has
    /// `version` to import. [`Version::empty`] gives the whole document.
    pub fn export_updates_since(
        &mut self,
        space: &mut Space,
        node: &NodeId,
        version: &Version,
    ) -> Result<Vec<u8>> {
        self.guard(space, node, |docs, space| {
            docs.prepare(space, node)?;
            let loaded = docs.loaded(space, node);
            Ok(loaded.doc.export(ExportMode::updates(&version.to_loro()))?)
        })
    }

    /// Records a new Loro peer for an editor that keeps its own copy of a document, such as an
    /// open editor in an app, and returns it. The editor makes its edits as this peer, and only
    /// it does, then sends them to [`merge_updates`](Self::merge_updates), which attributes them
    /// to `by`.
    pub fn new_peer(&self, space: &mut Space, by: &Provenance) -> Result<u64> {
        // A new document draws a random peer id.
        let peer = LoroDoc::new().peer_id();
        space.add_loro_peer(peer, by)?;
        Ok(peer)
    }

    /// Brings a node's document in step with a body written without Loro (21.5): makes the
    /// document's text equal to `nodes.body`, by a text update made as the peer of the body's
    /// last writer, and records the version. Nothing is written to `nodes`. Returns whether the
    /// document changed.
    pub fn pull_sqlite_into_doc(&mut self, space: &mut Space, node: &NodeId) -> Result<bool> {
        self.guard(space, node, |docs, space| {
            let current = space.get_node(node)?.ok_or(Error::NotFound(*node))?;
            docs.load(space, node)?;
            docs.pull(space, &current)
        })
    }

    /// Writes a node's document to SQLite when it is ahead of `nodes.body` (21.5), such as after
    /// a crash between writing an update file and the database. The change's provenance is that
    /// of the peers that made the operations the body lacks, if they are all one, otherwise
    /// `system`/`knode`. Returns the node when it was written.
    pub fn write_doc_to_sqlite(
        &mut self,
        space: &mut Space,
        node: &NodeId,
    ) -> Result<Option<Node>> {
        self.guard(space, node, |docs, space| {
            space.get_node(node)?.ok_or(Error::NotFound(*node))?;
            docs.load(space, node)?;
            docs.push(space, node)
        })
    }

    /// Folds a node's update files into a full snapshot (21.6): writes `snapshot.loro` from the
    /// document, then removes the update files it read, and only those. Holds the space's write
    /// lock meanwhile (see [`Space::loro_lock`]).
    pub fn compact(&mut self, space: &Space, node: &NodeId) -> Result<Compaction> {
        let dir = space.loro_dir(node);
        // Under the write lock, after reading the files again: a snapshot written by another
        // program meanwhile would otherwise be replaced by one that lacks its edits.
        let _lock = space.loro_lock()?;
        self.load(space, node)?;
        let loaded = self.loaded(space, node);
        let snapshot = loaded.doc.export(ExportMode::Snapshot)?;
        files::write_atomically(&dir, SNAPSHOT_FILE, &snapshot)?;
        loaded.snapshot = files::stamp(&files::snapshot_path(&dir))?;
        let updates = files::updates_dir(&dir);
        let mut removed = 0;
        for name in std::mem::take(&mut loaded.read) {
            match fs::remove_file(updates.join(&name)) {
                Ok(()) => removed += 1,
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
        }
        Ok(Compaction {
            removed,
            snapshot_bytes: snapshot.len() as u64,
        })
    }

    /// Whether a node's `updates/` folder should be compacted: more than
    /// [`COMPACT_AFTER_FILES`] files, or more bytes than its snapshot.
    pub fn needs_compaction(space: &Space, node: &NodeId) -> Result<bool> {
        let dir = space.loro_dir(node);
        let updates = files::update_names(&dir)?;
        if updates.is_empty() {
            return Ok(false);
        }
        let snapshot = files::stamp(&files::snapshot_path(&dir))?.map_or(0, |(len, _)| len);
        let bytes: u64 = updates.iter().map(|(_, len)| len).sum();
        Ok(updates.len() > COMPACT_AFTER_FILES || bytes > snapshot)
    }

    /// Forgets a node's document loaded in memory. Its files stay.
    pub fn close(&mut self, space: &Space, node: &NodeId) {
        self.open.remove(&key(space, node));
    }

    /// Removes a node's document, files and all, when the node is purged (21.6). A deleted node
    /// keeps its document, so an undo or a restore finds it.
    pub fn remove_doc(&mut self, space: &Space, node: &NodeId) -> Result<()> {
        self.close(space, node);
        match fs::remove_dir_all(space.loro_dir(node)) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e.into()),
            _ => Ok(()),
        }
    }

    /// Runs `f`, and on failure forgets the node's document in memory, which may hold edits that
    /// were not written, so the next operation loads it again from its files.
    fn guard<T>(
        &mut self,
        space: &mut Space,
        node: &NodeId,
        f: impl FnOnce(&mut Self, &mut Space) -> Result<T>,
    ) -> Result<T> {
        let result = f(self, space);
        if let Err(e) = &result {
            self.close(space, node);
            if !matches!(
                e,
                Error::NotFound(_) | Error::Invalid(_) | Error::UnknownVersion(_)
            ) {
                // Edits made as this run's peers may be lost with the document: never reuse
                // their ids.
                let id = space.manifest().id.clone();
                self.peers.retain(|(space, _), _| *space != id);
            }
        }
        result
    }

    fn loaded(&mut self, space: &Space, node: &NodeId) -> &mut Loaded {
        self.open
            .get_mut(&key(space, node))
            .expect("the document is loaded")
    }

    fn load(&mut self, space: &Space, node: &NodeId) -> Result<()> {
        let loaded = self
            .open
            .entry(key(space, node))
            .or_insert_with(Loaded::new);
        loaded.refresh(&space.loro_dir(node))?;
        Ok(())
    }

    /// The peer this program edits as for `by`, recorded the first time.
    fn peer_for(&mut self, space: &mut Space, by: &Provenance) -> Result<u64> {
        let key = (space.manifest().id.clone(), by.clone());
        if let Some(peer) = self.peers.get(&key) {
            return Ok(*peer);
        }
        self.rotate_peer(space, by)
    }

    /// A new peer this program edits as for `by` from now on.
    fn rotate_peer(&mut self, space: &mut Space, by: &Provenance) -> Result<u64> {
        let peer = self.new_peer(space, by)?;
        self.peers
            .insert((space.manifest().id.clone(), by.clone()), peer);
        Ok(peer)
    }

    /// Loads a node's document, makes it if needed, and brings it in step with SQLite (21.5).
    fn prepare(&mut self, space: &mut Space, node: &NodeId) -> Result<()> {
        let current = space.get_node(node)?.ok_or(Error::NotFound(*node))?;
        self.load(space, node)?;
        let version = self.loaded(space, node).version();
        if version == Version::empty() && space.loro_version(node)?.is_none() {
            // The first document of the node starts from its body, written by Knode (21.1).
            let created = Node {
                updated_by: Provenance::system(),
                ..current
            };
            self.pull(space, &created)?;
            return Ok(());
        }
        let row = match space.loro_version(node)? {
            Some(row) => Some(Version::from_json(&row)?),
            None => None,
        };
        match row {
            Some(row) if version.includes(&row) => {
                if version != row {
                    self.push(space, node)?;
                } else if self.loaded(space, node).text().to_string() != current.body {
                    self.pull(space, &current)?;
                }
            }
            _ => {
                self.pull(space, &current)?;
            }
        }
        // Another program may have written meanwhile; a document ahead of the body is written.
        self.push(space, node)?;
        Ok(())
    }

    /// Makes the document's text equal to the node's body, as the peer of its last writer.
    ///
    /// Done under the space's write lock, after reading the files again: two programs pulling
    /// the same body at once would each insert it.
    fn pull(&mut self, space: &mut Space, node: &Node) -> Result<bool> {
        let id = node.id;
        let peer = self.peer_for(space, &node.updated_by)?;
        let changed = {
            let _lock = space.loro_lock()?;
            self.load(space, &id)?;
            let body = space.get_node(&id)?.ok_or(Error::NotFound(id))?.body;
            let row = match space.loro_version(&id)? {
                Some(row) => Some(Version::from_json(&row)?),
                None => None,
            };
            let loaded = self.loaded(space, &id);
            let version = loaded.version();
            if let Some(row) = row.filter(|row| version.includes(row)) {
                // Read again, the document has every edit the body holds: it was behind only
                // because another program wrote meanwhile. Pulling a body that is behind it
                // would undo edits.
                if version != row || loaded.text().to_string() == body {
                    return Ok(false);
                }
            }
            let before = loaded.doc.oplog_vv();
            if loaded.text().to_string() != body {
                loaded.doc.set_peer_id(peer)?;
                set_text(&loaded.text(), &body)?;
                loaded.doc.commit();
            }
            let changed = loaded.doc.oplog_vv() != before;
            if changed {
                let update = loaded.doc.export(ExportMode::updates(&before))?;
                let name = files::write_update(&space.loro_dir(&id), &update)?;
                loaded.read.insert(name);
            }
            changed
        };
        let version = self.loaded(space, &id).version();
        space.set_loro_version(&id, &version.to_json())?;
        Ok(changed)
    }

    /// Writes the document's text to the node when the document is ahead of `loro_docs`.
    fn push(&mut self, space: &mut Space, node: &NodeId) -> Result<Option<Node>> {
        let row = match space.loro_version(node)? {
            Some(row) => Version::from_json(&row)?,
            None => return Ok(None),
        };
        let loaded = self.loaded(space, node);
        let version = loaded.version();
        if version == row || !version.includes(&row) {
            return Ok(None);
        }
        let text = loaded.text().to_string();
        let by = provenance_of(space, &version, &row)?;
        Ok(Some(space.write_loro_body(
            node,
            &text,
            &version.to_json(),
            &by,
        )?))
    }

    /// Writes what the document gained since `before`: the update file, then the node and its
    /// version (21.4); then compacts if the updates have grown.
    fn write(
        &mut self,
        space: &mut Space,
        node: &NodeId,
        before: &loro::VersionVector,
        by: &Provenance,
    ) -> Result<Written> {
        let dir = space.loro_dir(node);
        let loaded = self.loaded(space, node);
        let version = loaded.version();
        if version == Version::from_loro(before) {
            let current = space.get_node(node)?.ok_or(Error::NotFound(*node))?;
            return Ok(Written {
                node: current,
                version,
                update: Vec::new(),
            });
        }
        let update = loaded.doc.export(ExportMode::updates(before))?;
        let name = files::write_update(&dir, &update)?;
        loaded.read.insert(name);
        let text = loaded.text().to_string();
        let written = space.write_loro_body(node, &text, &version.to_json(), by)?;
        if Self::needs_compaction(space, node)? {
            self.compact(space, node)?;
        }
        Ok(Written {
            node: written,
            version,
            update,
        })
    }
}

fn key(space: &Space, node: &NodeId) -> (String, NodeId) {
    (space.manifest().id.clone(), *node)
}

/// The provenance of the operations in `version` and not in `since`: their peers' if they are
/// all one, otherwise `system`/`knode`. Fails if a peer is not recorded.
fn provenance_of(space: &Space, version: &Version, since: &Version) -> Result<Provenance> {
    let mut found: Option<Provenance> = None;
    let mut mixed = false;
    for peer in version.peers_ahead_of(since) {
        let by = space.loro_peer(peer)?.ok_or(Error::UnknownPeer(peer))?;
        match &found {
            Some(other) if *other != by => mixed = true,
            Some(_) => {}
            None => found = Some(by),
        }
    }
    Ok(match found {
        Some(by) if !mixed => by,
        _ => Provenance::system(),
    })
}

fn apply_change(text: &LoroText, change: &BodyChange) -> Result<()> {
    match change {
        BodyChange::Text(new) => set_text(text, new),
        BodyChange::Splices(splices) => {
            for splice in splices {
                let len = text.len_unicode();
                if splice.at > len || splice.delete > len - splice.at {
                    return Err(Error::Invalid(format!(
                        "a splice at {} deleting {} is past the end of a text of {len} characters",
                        splice.at, splice.delete
                    )));
                }
                if splice.delete > 0 || !splice.insert.is_empty() {
                    text.splice(splice.at, splice.delete, &splice.insert)?;
                }
            }
            Ok(())
        }
    }
}

/// Turns `text` into `new` by the Loro operations of a character diff, or of a line diff when
/// the character diff takes too long.
fn set_text(text: &LoroText, new: &str) -> Result<()> {
    if text.to_string() == new {
        return Ok(());
    }
    let options = UpdateOptions {
        timeout_ms: Some(DIFF_TIMEOUT_MS),
        use_refined_diff: true,
    };
    if text.update(new, options).is_err() {
        // What the character diff applied before it stopped is diffed again, by lines.
        text.update_by_line(new, UpdateOptions::default())
            .map_err(|e| Error::Loro(e.to_string()))?;
    }
    if text.to_string() != new {
        text.splice(0, text.len_unicode(), new)?;
    }
    Ok(())
}
