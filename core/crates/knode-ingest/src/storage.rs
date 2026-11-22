//! How much room a space takes and what fills it, and the limits that keep it from growing by
//! gigabytes without the person noticing (decision 0010, "Large files are planned for").
//!
//! [`summary`] counts every blob once, by the kind of file it is (photos, videos, audio, other),
//! next to the notes (the database and the documents of concurrent editing); says which source
//! added how much (by the provenance of each attachment); and lists the largest files. Files of
//! deleted nodes, and blobs no node uses, are counted apart: a purge frees them.
//!
//! The limits: one file is at most [`MAX_ATTACHMENT_SIZE`], and a video larger than that keeps
//! only the audio taken from it; past a soft limit ([`WARN_AT_SETTING`], 10 GB unless changed)
//! imports ask first ([`check`]) and connectors that add media hold back. [`check`] is cheap, as
//! it runs before every add: it sums the rows of the blobs and the size of the database files,
//! never walking the package. Sizes are computed on the device.

use std::collections::{BTreeMap, HashSet};

use knode_format::{MAX_ATTACHMENT_SIZE, NodeId, Provenance, Result, Space, setting_keys};
use serde::{Deserialize, Serialize};

/// The space setting with the soft limit, in bytes; `0` never warns.
pub const WARN_AT_SETTING: &str = setting_keys::STORAGE_WARN_AT;

/// The soft limit of a space that never chose one: 10 GB.
pub const DEFAULT_WARN_AT: u64 = 10 * 1024 * 1024 * 1024;

/// How many of the largest files [`summary`] lists unless asked for another number.
pub const DEFAULT_TOP: usize = 20;

/// The bytes a space takes, by kind.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ByKind {
    /// The notes: the database (text, history, properties, search index) and `sync/`.
    pub notes: u64,
    /// Attached images.
    pub photos: u64,
    /// Attached videos.
    pub videos: u64,
    /// Attached audio: recordings, voice memos, audio taken from large videos.
    pub audio: u64,
    /// Every other attached file.
    pub other: u64,
    /// Files only deleted nodes use, or none: a purge frees them.
    pub deleted: u64,
}

/// What a source added: the attachments whose provenance names it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BySource {
    /// Who attached them: a source id (`photos-library`, `voice-memos`), an agent or a person.
    pub actor: String,
    /// `person`, `agent` or `system`.
    pub kind: String,
    /// Their bytes, each blob counted once.
    pub size: u64,
    /// How many files.
    pub files: usize,
}

/// One large file: [`Summary::largest`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LargeFile {
    /// Its node.
    pub node: NodeId,
    /// The node's title.
    pub title: String,
    /// The node's kind, such as `photo` or `recording`.
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Limits {
    /// The largest file a space attaches.
    pub max_file: u64,
    /// The soft limit, in bytes; `0` never warns.
    pub warn_at: u64,
}

/// How much room a space takes and what fills it: `storage.summary`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Summary {
    /// The bytes of the whole package on disk.
    pub total: u64,
    /// By kind.
    pub kinds: ByKind,
    /// How many nodes, attached files (once each) and media nodes.
    pub nodes: usize,
    pub files: usize,
    pub photos: usize,
    pub recordings: usize,
    /// By who added them, largest first.
    pub sources: Vec<BySource>,
    /// The largest attached files of nodes that are not deleted, largest first.
    pub largest: Vec<LargeFile>,
    /// The limits.
    pub limits: Limits,
    /// Whether the space is past its soft limit.
    pub over: bool,
}

/// What a space would take after adding `size` bytes: `storage.check`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Check {
    /// The bytes it takes now.
    pub total: u64,
    /// The bytes it would take after.
    pub after: u64,
    /// The soft limit; `0` never warns.
    pub warn_at: u64,
    /// Whether to ask first: `after` is past the soft limit.
    pub warn: bool,
}

/// The limits of the space.
pub fn limits(space: &Space) -> Result<Limits> {
    Ok(Limits {
        max_file: MAX_ATTACHMENT_SIZE,
        warn_at: warn_at(space)?,
    })
}

/// The soft limit of the space, in bytes; `0` never warns.
pub fn warn_at(space: &Space) -> Result<u64> {
    Ok(space
        .setting(WARN_AT_SETTING)?
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(DEFAULT_WARN_AT))
}

/// Changes the soft limit a space chooses; `None` leaves it as it is. Returns the limits after
/// the change.
pub fn set_limits(space: &mut Space, warn_at: Option<u64>, by: &Provenance) -> Result<Limits> {
    if let Some(bytes) = warn_at {
        let value = (bytes != DEFAULT_WARN_AT).then(|| bytes.to_string());
        space.set_setting(WARN_AT_SETTING, value.as_deref(), by)?;
    }
    limits(space)
}

/// What the space would take after adding `size` bytes, and whether to ask first. The bytes it
/// takes now are its blobs (by their rows) and its database files: what grows with media. The
/// rest of the package (`sync/`) is left out, so nothing is walked.
pub fn check(space: &Space, size: u64) -> Result<Check> {
    let total = space.blob_bytes()?.saturating_add(space.database_bytes());
    let warn_at = warn_at(space)?;
    let after = total.saturating_add(size);
    Ok(Check {
        total,
        after,
        warn_at,
        warn: warn_at > 0 && after > warn_at,
    })
}

/// How much room the space takes and what fills it, with its `top` largest files.
pub fn summary(space: &Space, top: usize) -> Result<Summary> {
    let usage = space.disk_usage()?;
    let files = space.stored_files()?;
    let mut kinds = ByKind {
        notes: usage.database + usage.sync,
        ..ByKind::default()
    };
    let mut counted: HashSet<&str> = HashSet::new();
    let mut sources: BTreeMap<(String, String), BySource> = BTreeMap::new();
    let mut largest = Vec::new();
    let mut photos = HashSet::new();
    let mut recordings = HashSet::new();
    // Live files first: a blob a live node and a deleted one share counts as live.
    for file in files.iter().filter(|f| f.node_kind.is_some()) {
        let kind = file.node_kind.as_deref().unwrap_or_default();
        if kind == crate::photos::PHOTO_KIND {
            photos.insert(file.node_id);
        } else if kind.parse::<crate::recordings::RecordingKind>().is_ok() {
            recordings.insert(file.node_id);
        }
        if largest.len() < top {
            largest.push(LargeFile {
                node: file.node_id,
                title: file.node_title.clone().unwrap_or_default(),
                kind: kind.to_string(),
                name: file.name.clone(),
                size: file.size,
                media_type: file.media_type.clone(),
                added_by: file.created_by.actor.clone(),
            });
        }
        if !counted.insert(&file.hash) {
            continue;
        }
        let bucket = match file.media_type.split('/').next().unwrap_or_default() {
            "image" => &mut kinds.photos,
            "video" => &mut kinds.videos,
            "audio" => &mut kinds.audio,
            _ => &mut kinds.other,
        };
        *bucket += file.size;
        let by = &file.created_by;
        let entry = sources
            .entry((by.kind.as_str().to_string(), by.actor.clone()))
            .or_insert_with(|| BySource {
                actor: by.actor.clone(),
                kind: by.kind.as_str().to_string(),
                size: 0,
                files: 0,
            });
        entry.size += file.size;
        entry.files += 1;
    }
    let live = counted.len();
    for blob in space.stored_blobs()? {
        if !counted.contains(blob.hash.as_str()) {
            kinds.deleted += blob.size;
        }
    }
    let mut sources: Vec<BySource> = sources.into_values().collect();
    sources.sort_by(|a, b| b.size.cmp(&a.size).then_with(|| a.actor.cmp(&b.actor)));
    let limits = limits(space)?;
    Ok(Summary {
        total: usage.total,
        kinds,
        nodes: space.node_count()? as usize,
        files: live,
        photos: photos.len(),
        recordings: recordings.len(),
        sources,
        largest,
        over: limits.warn_at > 0 && usage.total > limits.warn_at,
        limits,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use knode_format::NewNode;

    #[test]
    fn counts_by_kind_source_and_size() {
        let dir = tempfile::tempdir().unwrap();
        let mut space = Space::create(dir.path().join("S.knode"), "S").unwrap();
        let ada = Provenance::person("Ada");
        let library = crate::sources::provenance("photos-library");
        let note = space
            .create_node(NewNode::note("Trip", "text"), &ada)
            .unwrap();
        // A PNG header, so the media type is an image.
        let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
        png.extend_from_slice(&[0u8; 500]);
        space
            .attach(&note.id, "board.png", &png[..], &library)
            .unwrap();
        space
            .attach(&note.id, "notes.txt", &b"abc"[..], &ada)
            .unwrap();
        // The same content twice is one blob.
        space.attach(&note.id, "copy.png", &png[..], &ada).unwrap();
        let gone = space.create_node(NewNode::note("Old", ""), &ada).unwrap();
        space
            .attach(&gone.id, "old.bin", &[7u8; 50][..], &ada)
            .unwrap();
        space.delete_node(&gone.id, &ada).unwrap();

        let s = summary(&space, 2).unwrap();
        assert_eq!(s.kinds.photos, png.len() as u64);
        assert_eq!(s.kinds.other, 3);
        assert_eq!(s.kinds.deleted, 50);
        assert!(s.kinds.notes > 0);
        assert_eq!(s.files, 2);
        assert_eq!(s.largest.len(), 2);
        assert_eq!(s.largest[0].size, png.len() as u64);
        assert_eq!(s.largest[0].title, "Trip");
        assert_eq!(s.sources[0].actor, "photos-library");
        assert_eq!(s.sources[0].files, 1);
        assert!(s.total >= s.kinds.notes + s.kinds.photos);
        assert_eq!(s.limits.max_file, MAX_ATTACHMENT_SIZE);
        assert_eq!(s.limits.warn_at, DEFAULT_WARN_AT);
        assert!(!s.over);
    }

    #[test]
    fn check_sums_rows_without_walking_the_package() {
        let dir = tempfile::tempdir().unwrap();
        let mut space = Space::create(dir.path().join("S.knode"), "S").unwrap();
        let ada = Provenance::person("Ada");
        let note = space.create_node(NewNode::note("A", ""), &ada).unwrap();
        space
            .attach(&note.id, "a.bin", &[1u8; 500][..], &ada)
            .unwrap();
        // A file of the package no row knows of: only a walk would count it.
        std::fs::write(space.path().join("stray.bin"), vec![0u8; 4_000_000]).unwrap();
        let c = check(&space, 0).unwrap();
        assert!(c.total >= 500 + space.database_bytes());
        assert!(c.total < 4_000_000, "check walked the package: {}", c.total);
    }

    #[test]
    fn limits_and_checks() {
        let dir = tempfile::tempdir().unwrap();
        let mut space = Space::create(dir.path().join("S.knode"), "S").unwrap();
        let ada = Provenance::person("Ada");
        let c = check(&space, 1024).unwrap();
        assert!(!c.warn);
        assert_eq!(c.after, c.total + 1024);

        let l = set_limits(&mut space, Some(1), &ada).unwrap();
        assert_eq!(l.warn_at, 1);
        // Settings grow the database: the limit is set from what the space takes after them.
        let now = check(&space, 0).unwrap().total;
        set_limits(&mut space, Some(now + 100_000), &ada).unwrap();
        assert!(check(&space, 200_000).unwrap().warn);
        assert!(!check(&space, 1).unwrap().warn);

        let l = set_limits(&mut space, Some(0), &ada).unwrap();
        assert_eq!(l.warn_at, 0);
        assert!(!check(&space, u64::MAX).unwrap().warn);
        set_limits(&mut space, Some(DEFAULT_WARN_AT), &ada).unwrap();
        assert_eq!(space.setting(WARN_AT_SETTING).unwrap(), None);
        assert_eq!(summary(&space, 5).unwrap().limits.warn_at, DEFAULT_WARN_AT);
    }
}
