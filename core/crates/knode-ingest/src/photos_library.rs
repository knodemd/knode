//! Photos and videos of the albums a person picks in the Photos library of their Mac, as media
//! nodes of a space.
//!
//! The library is read only by the Knode app (PhotoKit). For every asset of the albums picked,
//! the app exports the original (a photo's HEIC or JPEG unchanged; a video, or the audio taken
//! from a video larger than a space attaches) and hands it here with [`add`]: a photo becomes a
//! [`photos`](crate::photos) node, a video a [`recordings`](crate::recordings) node, read and
//! transcribed on the device like any other.
//!
//! Asset identifiers (`localIdentifier`) mean something only in one library on one Mac, and a
//! space can be read by the libraries of several Macs. So every library has its own id (made
//! once per Knode home by Knode Service), each node keeps the library it came from
//! ([`keys::LIBRARY`]) next to the asset's identifier ([`keys::ID`]), and everything here is
//! asked for one library: [`find`], [`known`], [`mark`]. Adding the same asset again finds its
//! node, so nothing is duplicated; the app asks which assets the space has before it exports
//! anything.
//!
//! Two assets with the same content (a photo by its file's hash, a video by the hash of its
//! original, which the app computes while exporting: [`keys::CONTENT`]) are one node: the second
//! identifier is kept as an alias ([`alias_key`]), so a library rebuilt with new identifiers, or
//! the same photo in the library of another Mac, adds nothing twice. Only nodes the library made
//! take aliases: a photo someone else added stays theirs, and the library makes its own node
//! (its file is stored once, as blobs are kept by hash).
//!
//! What the library did on this Mac is kept apart, in a [`Ledger`] Knode Service keeps in the
//! Knode home: the node each asset became, so an asset whose node the person deleted is never
//! brought back (a tombstone), and the assets that can never be added (too large, not
//! supported), so they are not exported again on every run.
//!
//! Assets that leave the albums read (deleted from the library, taken out of the album, or an
//! album no longer picked) are marked, never deleted ([`mark`]): their [`keys::STATE`] becomes
//! `removed`, which the apps show; one that comes back is `present` again. The body is never
//! changed for it, so readings and transcripts that rewrite the body keep the mark.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::PathBuf;

use knode_format::{Error, NodeId, Provenance, Result, Space};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::media::{Added, AddedFile};
use crate::photos::{self, NewPhoto, PhotoMetadata};
use crate::recordings::{self, AudioInput, NewRecording, RecordingKind, Track};

/// The id of the source.
pub const SOURCE_ID: &str = "photos-library";

/// The properties a node from the Photos library has, next to its photo or recording ones.
pub mod keys {
    /// The `localIdentifier` of the asset in the Photos library it came from.
    pub const ID: &str = "photos_library.id";
    /// The id of the library it came from: identifiers mean something only there.
    pub const LIBRARY: &str = "photos_library.library";
    /// `present`, or `removed` once it left the albums read.
    pub const STATE: &str = "photos_library.state";
    /// A video's content: `sha256:` and the hex SHA-256 of its original.
    pub const CONTENT: &str = "photos_library.content";
    /// What every alias property starts with: see [`alias_key`](super::alias_key).
    pub const ALIAS_PREFIX: &str = "photos_library.alias.";
}

/// The space setting that says whether assets kept only in iCloud are downloaded: `on`, or off
/// (the default, also when absent): they wait until the Mac has the original.
pub const ICLOUD_SETTING: &str = "photos_library.icloud";

/// [`keys::STATE`] of a node whose asset is in the albums read.
pub const PRESENT: &str = "present";
/// [`keys::STATE`] of a node whose asset left the albums read.
pub const REMOVED: &str = "removed";

/// What an asset is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AssetKind {
    /// A photo: its original HEIC, JPEG or PNG.
    Photo,
    /// A video: the original, or the audio taken from it when it is larger than a space
    /// attaches.
    Video,
}

/// An asset of the library, exported by the app, to add with [`add`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LibraryAsset {
    /// Its `localIdentifier`.
    pub id: String,
    /// What it is.
    pub kind: AssetKind,
    /// The exported file: the photo, the video, or the audio taken from the video.
    pub path: PathBuf,
    /// Its title: the original file's name without its extension. The file's when empty.
    #[serde(default)]
    pub title: String,
    /// When it was taken, in Unix milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<i64>,
    /// How long a video lasts, in milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<i64>,
    /// What the library says about a photo.
    #[serde(default)]
    pub metadata: PhotoMetadata,
    /// The name of a video's original, when only its audio is in `path`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub original: Option<String>,
    /// A video's content: `sha256:` and the hex SHA-256 of its original, computed by the app
    /// while exporting it (the original may not be what is in `path`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
}

/// What [`mark`] changed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Marked {
    /// Nodes marked removed.
    pub removed: usize,
    /// Nodes back in the albums read.
    pub restored: usize,
}

/// What the Photos library did in one space on this Mac, beyond what the space keeps: Knode
/// Service keeps it in the Knode home, one per space. Losing it means only that assets whose
/// node was deleted, or that failed for good, may be tried again.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ledger {
    /// The library it is about; a ledger of another library is started again.
    #[serde(default)]
    pub library: String,
    /// The node each asset became, by identifier.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub held: BTreeMap<String, NodeId>,
    /// Identifiers of assets whose node the person deleted: never added again (tombstones).
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub deleted: BTreeSet<String>,
    /// Identifiers of assets that can never be added, with why: not exported again.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub failed: BTreeMap<String, String>,
}

impl Ledger {
    /// An empty ledger of `library`.
    pub fn new(library: &str) -> Self {
        Ledger {
            library: library.to_string(),
            ..Ledger::default()
        }
    }

    /// Whether the asset is never added: its node was deleted, or it failed for good.
    pub fn skips(&self, id: &str) -> bool {
        self.deleted.contains(id) || self.failed.contains_key(id)
    }

    /// Remembers that an asset can never be added, and why.
    pub fn fail(&mut self, id: &str, reason: &str) {
        let id = id.trim();
        if !id.is_empty() && !self.held.contains_key(id) && !self.deleted.contains(id) {
            self.failed
                .insert(id.to_string(), reason.trim().to_string());
        }
    }

    /// Brings the ledger up to date with the space: an asset whose node is gone was deleted by
    /// the person (a tombstone), a tombstone whose node is back (an undo) is lifted, and every
    /// node of the library is held (a lost ledger is learned again). Returns how many new
    /// tombstones it made.
    pub fn reconcile(&mut self, space: &Space) -> Result<usize> {
        let library = self.library.clone();
        let mut gone = Vec::new();
        for (id, node) in &self.held {
            if !holds(space, &library, id, node)? {
                gone.push(id.clone());
            }
        }
        for id in &gone {
            self.held.remove(id);
            self.deleted.insert(id.clone());
        }
        let mut back = Vec::new();
        for id in &self.deleted {
            if let Some(node) = find(space, &library, id)? {
                back.push((id.clone(), node));
            }
        }
        for (id, node) in back {
            self.deleted.remove(&id);
            self.held.insert(id, node);
        }
        for summary in space.find_by_property(keys::LIBRARY, &library)? {
            for id in identifiers(&space.properties(&summary.id)?, &library) {
                self.held.entry(id).or_insert(summary.id);
            }
        }
        Ok(gone.len())
    }
}

/// An asset identifier qualified by its library: `<library>/<id>`.
pub fn qualified(library: &str, id: &str) -> String {
    format!("{library}/{id}")
}

/// The property that keeps a second identifier of a node: two assets with the same content are
/// one node. `qualified` is the identifier with its library ([`qualified`]); the key holds a hash
/// of it (property keys cannot hold `/`) and the value the identifier itself.
pub fn alias_key(qualified: &str) -> String {
    let digest = Sha256::digest(qualified.as_bytes());
    let hex: String = digest.iter().take(8).map(|b| format!("{b:02x}")).collect();
    format!("{}{hex}", keys::ALIAS_PREFIX)
}

/// Whether the space downloads assets kept only in iCloud.
pub fn icloud_enabled(space: &Space) -> Result<bool> {
    Ok(space.setting(ICLOUD_SETTING)?.as_deref() == Some("on"))
}

/// Sets whether the space downloads assets kept only in iCloud. Returns whether it changed.
pub fn set_icloud(space: &mut Space, on: bool, by: &Provenance) -> Result<bool> {
    space.set_setting(ICLOUD_SETTING, on.then_some("on"), by)
}

/// The node of the space that holds the asset `id` of `library`, if any.
pub fn find(space: &Space, library: &str, id: &str) -> Result<Option<NodeId>> {
    for found in space.find_by_property(keys::ID, id)? {
        if space.property(&found.id, keys::LIBRARY)?.as_deref() == Some(library) {
            return Ok(Some(found.id));
        }
    }
    let q = qualified(library, id);
    Ok(space
        .find_by_property(&alias_key(&q), &q)?
        .into_iter()
        .next()
        .map(|s| s.id))
}

/// Whether `node` still holds the asset `id` of `library`.
fn holds(space: &Space, library: &str, id: &str, node: &NodeId) -> Result<bool> {
    if space.property(node, keys::ID)?.as_deref() == Some(id)
        && space.property(node, keys::LIBRARY)?.as_deref() == Some(library)
    {
        return Ok(true);
    }
    let q = qualified(library, id);
    Ok(space.property(node, &alias_key(&q))?.as_deref() == Some(q.as_str()))
}

/// The identifiers of `library` a node of that library holds: its own and its aliases.
fn identifiers(props: &BTreeMap<String, String>, library: &str) -> Vec<String> {
    let mut out = Vec::new();
    if props.get(keys::LIBRARY).map(String::as_str) != Some(library) {
        return out;
    }
    if let Some(id) = props.get(keys::ID) {
        out.push(id.clone());
    }
    let prefix = format!("{library}/");
    out.extend(
        props
            .iter()
            .filter(|(k, _)| k.starts_with(keys::ALIAS_PREFIX))
            .filter_map(|(_, v)| v.strip_prefix(&prefix).map(str::to_string)),
    );
    out
}

/// The identifiers of `ids` the app need not export, in the order given: the ones the space
/// holds for `library`, and the ones the ledger skips (deleted by the person, or failed for
/// good).
pub fn known(space: &Space, library: &str, ids: &[String], ledger: &Ledger) -> Result<Vec<String>> {
    let mut out = Vec::new();
    for id in ids {
        if ledger.skips(id) || find(space, library, id)?.is_some() {
            out.push(id.clone());
        }
    }
    Ok(out)
}

/// Whether a node was made by the Photos library (of any Mac).
fn from_library(space: &Space, node: &NodeId) -> Result<bool> {
    Ok(space.property(node, keys::ID)?.is_some())
}

/// Adds an asset of `library`: a photo node or a recording node, with its exported file
/// attached, found again by its identifier, and held in the ledger. An asset the space holds
/// already returns its node unchanged, with `created` false; so does one with the content of a
/// node the library made, which takes its identifier as an alias. An asset whose node the
/// person deleted is refused.
pub fn add(
    space: &mut Space,
    library: &str,
    asset: &LibraryAsset,
    ledger: &mut Ledger,
    by: &Provenance,
) -> Result<AddedFile> {
    let id = asset.id.trim();
    if id.is_empty() {
        return Err(Error::Invalid("the asset has no identifier".into()));
    }
    if library.trim().is_empty() {
        return Err(Error::Invalid("the library has no id".into()));
    }
    // Held before, but its node is gone: the person deleted it since the ledger last looked.
    if let Some(node) = ledger.held.get(id).copied()
        && !holds(space, library, id, &node)?
    {
        ledger.held.remove(id);
        ledger.deleted.insert(id.to_string());
    }
    if ledger.deleted.contains(id) {
        return Err(Error::Invalid(format!(
            "the asset {id} was deleted from the space, and is not added again"
        )));
    }
    if let Some(node) = find(space, library, id)? {
        ledger.held.insert(id.to_string(), node);
        return Ok(AddedFile {
            node: node_of(space, &node)?,
            created: false,
        });
    }
    let same = match asset.kind {
        AssetKind::Photo => {
            let key = crate::media::file_key(&asset.path)?;
            let mut same = None;
            for found in space.find_by_property(photos::keys::KEY, &key)? {
                if found.kind == photos::PHOTO_KIND && from_library(space, &found.id)? {
                    same = Some(found.id);
                    break;
                }
            }
            same.ok_or(key)
        }
        AssetKind::Video => {
            let content = asset
                .content
                .as_deref()
                .map(str::trim)
                .filter(|c| !c.is_empty());
            let mut same = None;
            if let Some(content) = content {
                for found in space.find_by_property(keys::CONTENT, content)? {
                    if from_library(space, &found.id)? {
                        same = Some(found.id);
                        break;
                    }
                }
            }
            same.ok_or_else(String::new)
        }
    };
    let (node, created) = match same {
        Ok(node) => {
            let q = qualified(library, id);
            space.set_property(&node, &alias_key(&q), &q, by)?;
            (node, false)
        }
        Err(key) => (create(space, library, id, asset, &key, by)?, true),
    };
    ledger.held.insert(id.to_string(), node);
    ledger.failed.remove(id);
    Ok(AddedFile {
        node: node_of(space, &node)?,
        created,
    })
}

/// Makes the node of a new asset, owned by the library. `key` is a photo's file key.
fn create(
    space: &mut Space,
    library: &str,
    id: &str,
    asset: &LibraryAsset,
    key: &str,
    by: &Provenance,
) -> Result<NodeId> {
    let node = match asset.kind {
        AssetKind::Photo => {
            photos::create_separate(
                space,
                &NewPhoto {
                    title: asset.title.clone(),
                    path: asset.path.clone(),
                    project: None,
                    metadata: asset.metadata.clone(),
                },
                key,
                by,
            )?
            .id
        }
        AssetKind::Video => {
            let started_at = asset
                .created_at
                .unwrap_or_else(|| chrono::Utc::now().timestamp_millis());
            let title = match asset.title.trim() {
                "" => crate::media::stem(&asset.path),
                t => t.to_string(),
            };
            let recording = recordings::create(
                space,
                &NewRecording {
                    kind: RecordingKind::Recording,
                    title,
                    started_at,
                    ended_at: started_at + asset.duration_ms.unwrap_or(0).max(0),
                    project: None,
                    audio: vec![AudioInput {
                        path: asset.path.clone(),
                        track: Track::Mixed,
                    }],
                    key: Some(format!("{SOURCE_ID}:{}", qualified(library, id))),
                    app: None,
                    summaries: recordings::summaries_enabled(space)?,
                    original: asset.original.clone(),
                },
                by,
            )?;
            if let Some(content) = asset
                .content
                .as_deref()
                .map(str::trim)
                .filter(|c| !c.is_empty())
            {
                space.set_property(&recording.id, keys::CONTENT, content, by)?;
            }
            recording.id
        }
    };
    space.set_property(&node, keys::STATE, PRESENT, by)?;
    space.set_property(&node, keys::LIBRARY, library, by)?;
    space.set_property(&node, keys::ID, id, by)?;
    Ok(node)
}

fn node_of(space: &Space, id: &NodeId) -> Result<Added> {
    match photos::get(space, id) {
        Ok(photo) => Ok(Added::Photo(photo)),
        Err(Error::Invalid(_)) => Ok(Added::Recording(recordings::get(space, id)?)),
        Err(e) => Err(e),
    }
}

/// Marks the nodes of `library` whose assets are not among `present` (every identifier in the
/// albums read now, in that library) as removed, and restores the ones marked before that are
/// back. Nodes of other libraries, and other nodes, are never touched; nothing is deleted, and
/// only the [`keys::STATE`] property changes.
pub fn mark(
    space: &mut Space,
    library: &str,
    present: &[String],
    by: &Provenance,
) -> Result<Marked> {
    let present: HashSet<&str> = present.iter().map(String::as_str).collect();
    let mut marked = Marked::default();
    for summary in space.find_by_property(keys::LIBRARY, library)? {
        let props = space.properties(&summary.id)?;
        let inside = identifiers(&props, library)
            .iter()
            .any(|id| present.contains(id.as_str()));
        let state = props.get(keys::STATE).map(String::as_str);
        match (state, inside) {
            (Some(REMOVED), true) => {
                space.set_property(&summary.id, keys::STATE, PRESENT, by)?;
                marked.restored += 1;
            }
            (Some(REMOVED), false) | (_, true) => {}
            (_, false) => {
                space.set_property(&summary.id, keys::STATE, REMOVED, by)?;
                marked.removed += 1;
            }
        }
    }
    Ok(marked)
}

#[cfg(test)]
mod tests;
