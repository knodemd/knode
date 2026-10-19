//! A live Markdown folder: a folder kept in step with a space as it changes.

use std::collections::BTreeSet;
use std::path::{Component, Path};
use std::time::{Duration, Instant};

use knode_format::{ChangeOp, NodeId, Space};

use crate::mirror::Mirror;
use crate::{ExportReport, Result};

/// A folder that mirrors every node of a space as a `.md` file, one way: from the space to the
/// folder ([decision 0006]).
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
///
/// [`resync`](Self::resync) writes the whole space again, for when the folder is turned on or
/// changes may have been missed. Files have the layout of [`export`](crate::export); the folder
/// and an export to it are the same thing.
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
/// assert!(tmp.path().join("Notes/Roadmap.md").exists());
/// assert!(!tmp.path().join("Notes/Plan.md").exists());
/// # Ok::<(), knode_vault::Error>(())
/// ```
///
/// [decision 0006]: https://github.com/knodemd/handbook/blob/main/decisions/0006-markdown.md
#[derive(Debug)]
pub struct LiveFolder {
    mirror: Mirror,
    delay: Duration,
    pending: BTreeSet<NodeId>,
    since: Option<Instant>,
}

impl LiveFolder {
    /// How long a batch waits after its first change before it is due: two seconds.
    pub const DEFAULT_DELAY: Duration = Duration::from_secs(2);

    /// Opens the folder `dir`, which need not exist yet. Nothing is written until
    /// [`flush`](Self::flush) or [`resync`](Self::resync).
    pub fn open(dir: impl AsRef<Path>) -> Result<LiveFolder> {
        Ok(LiveFolder {
            mirror: Mirror::load(dir.as_ref())?,
            delay: Self::DEFAULT_DELAY,
            pending: BTreeSet::new(),
            since: None,
        })
    }

    /// Sets how long a batch waits after its first change before it is due.
    pub fn with_delay(mut self, delay: Duration) -> LiveFolder {
        self.delay = delay;
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

    /// Writes the changes recorded since the last flush, due or not.
    pub fn flush(&mut self, space: &Space) -> Result<ExportReport> {
        let pending = std::mem::take(&mut self.pending);
        self.since = None;
        match self.mirror.sync(space, Some(&pending)) {
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
    /// [`export`](crate::export). Clears the batch.
    pub fn resync(&mut self, space: &Space) -> Result<ExportReport> {
        let report = self.mirror.sync(space, None)?;
        self.pending.clear();
        self.since = None;
        Ok(report)
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
