//! How much room a space takes, what fills it and its limits ([`knode_ingest::storage`]). The
//! same rules as Knode Service's `storage.*` methods, for when the app writes the space itself.

use knode_ingest::storage as st;

use crate::{KnodeSpace, Provenance, Result};

/// The bytes a space takes, by kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Record)]
pub struct StorageByKind {
    /// The notes: the database and the documents of concurrent editing.
    pub notes: u64,
    /// Attached images.
    pub photos: u64,
    /// Attached videos.
    pub videos: u64,
    /// Attached audio.
    pub audio: u64,
    /// Every other attached file.
    pub other: u64,
    /// Files only deleted nodes use, or none: a purge frees them.
    pub deleted: u64,
}

/// What one source, agent or person added.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct StorageBySource {
    /// Who attached the files.
    pub actor: String,
    /// `person`, `agent` or `system`.
    pub kind: String,
    /// Their bytes.
    pub size: u64,
    /// How many files.
    pub files: u64,
}

/// One of the largest files.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct StorageFile {
    /// Its node.
    pub node: String,
    /// The node's title.
    pub title: String,
    /// The node's kind.
    pub kind: String,
    /// Its name in the node.
    pub name: String,
    /// Its size in bytes.
    pub size: u64,
    /// Its media type.
    pub media_type: String,
    /// Who attached it.
    pub added_by: String,
}

/// The limits of a space.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Record)]
pub struct StorageLimits {
    /// The largest file a space attaches.
    pub max_file: u64,
    /// The largest original kept when the space keeps large originals.
    pub max_original: u64,
    /// Whether videos larger than `max_file` keep their original.
    pub keep_large_originals: bool,
    /// The soft limit, in bytes; 0 never warns.
    pub warn_at: u64,
}

/// How much room a space takes and what fills it.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct StorageSummary {
    /// The bytes of the whole package.
    pub total: u64,
    /// By kind.
    pub kinds: StorageByKind,
    /// How many nodes.
    pub nodes: u64,
    /// How many attached files, once each.
    pub files: u64,
    /// How many photos.
    pub photos: u64,
    /// How many recordings.
    pub recordings: u64,
    /// By who added them, largest first.
    pub sources: Vec<StorageBySource>,
    /// The largest files, largest first.
    pub largest: Vec<StorageFile>,
    /// The limits.
    pub limits: StorageLimits,
    /// Whether the space is past its soft limit.
    pub over: bool,
}

/// What a space would take after adding some bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Record)]
pub struct StorageCheck {
    /// The bytes it takes now.
    pub total: u64,
    /// The bytes after.
    pub after: u64,
    /// The soft limit; 0 never warns.
    pub warn_at: u64,
    /// Whether to ask first.
    pub warn: bool,
}

impl From<st::Limits> for StorageLimits {
    fn from(l: st::Limits) -> Self {
        StorageLimits {
            max_file: l.max_file,
            max_original: l.max_original,
            keep_large_originals: l.keep_large_originals,
            warn_at: l.warn_at,
        }
    }
}

impl From<st::Summary> for StorageSummary {
    fn from(s: st::Summary) -> Self {
        let k = s.kinds;
        StorageSummary {
            total: s.total,
            kinds: StorageByKind {
                notes: k.notes,
                photos: k.photos,
                videos: k.videos,
                audio: k.audio,
                other: k.other,
                deleted: k.deleted,
            },
            nodes: s.nodes as u64,
            files: s.files as u64,
            photos: s.photos as u64,
            recordings: s.recordings as u64,
            sources: s
                .sources
                .into_iter()
                .map(|b| StorageBySource {
                    actor: b.actor,
                    kind: b.kind,
                    size: b.size,
                    files: b.files as u64,
                })
                .collect(),
            largest: s
                .largest
                .into_iter()
                .map(|f| StorageFile {
                    node: f.node.to_string(),
                    title: f.title,
                    kind: f.kind,
                    name: f.name,
                    size: f.size,
                    media_type: f.media_type,
                    added_by: f.added_by,
                })
                .collect(),
            limits: s.limits.into(),
            over: s.over,
        }
    }
}

#[uniffi::export]
impl KnodeSpace {
    /// How much room the space takes, by kind and by source, with its `top` largest files.
    pub fn storage_summary(&self, top: u32) -> Result<StorageSummary> {
        Ok(st::summary(&self.lock(), top as usize)?.into())
    }

    /// What the space would take after adding `size` bytes, and whether to ask first.
    pub fn storage_check(&self, size: u64) -> Result<StorageCheck> {
        let c = st::check(&self.lock(), size)?;
        Ok(StorageCheck {
            total: c.total,
            after: c.after,
            warn_at: c.warn_at,
            warn: c.warn,
        })
    }

    /// Changes the limits of the space; `None` leaves one as it is.
    pub fn set_storage_limits(
        &self,
        warn_at: Option<u64>,
        keep_large_originals: Option<bool>,
        by: Provenance,
    ) -> Result<StorageLimits> {
        Ok(st::set_limits(&mut self.lock(), warn_at, keep_large_originals, &by.into())?.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ActorKind;

    #[test]
    fn summarises_checks_and_sets_limits() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("S.knode").display().to_string();
        let space = KnodeSpace::create(path, "S".into()).unwrap();
        let me = Provenance {
            kind: ActorKind::Person,
            actor: "Ada".into(),
            session: None,
            source: None,
        };
        let node = space
            .create_node("Board".into(), String::new(), me.clone())
            .unwrap();
        let file = dir.path().join("board.png");
        let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
        png.extend_from_slice(&[0u8; 100]);
        std::fs::write(&file, &png).unwrap();
        space
            .attach_file(node.id.clone(), file.display().to_string(), me.clone())
            .unwrap();
        let summary = space.storage_summary(5).unwrap();
        assert_eq!(summary.kinds.photos, png.len() as u64);
        assert_eq!(summary.largest[0].title, "Board");
        assert_eq!(summary.limits.max_file, knode_format::MAX_ATTACHMENT_SIZE);
        assert!(!space.storage_check(10).unwrap().warn);
        let limits = space.set_storage_limits(Some(1), Some(true), me).unwrap();
        assert!(limits.keep_large_originals);
        assert!(space.storage_check(10).unwrap().warn);
        assert!(space.storage_summary(0).unwrap().largest.is_empty());
    }
}
