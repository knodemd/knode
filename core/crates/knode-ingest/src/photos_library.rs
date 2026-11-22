//! Photos and videos of the albums a person picks in the Photos library of their Mac, as media
//! nodes of a space.
//!
//! The library is read only by the Knode app (PhotoKit). For every asset of the albums picked,
//! the app exports the original (a photo's HEIC or JPEG unchanged; a video, or the audio taken
//! from a video larger than a space attaches) and hands it here with [`add`]: a photo becomes a
//! [`photos`](crate::photos) node, a video a [`recordings`](crate::recordings) node, read and
//! transcribed on the device like any other. Each node keeps the asset's `localIdentifier`
//! ([`keys::ID`]), stable for a library on one Mac, so adding the same asset again finds it
//! ([`find`], [`known`]) and nothing is duplicated; the app asks which assets the space has
//! before it exports anything. Two assets of the library with the same content are one photo:
//! the second identifier is kept as an alias ([`alias_key`]).
//!
//! Assets that leave the albums read (deleted from the library, taken out of the album, or an
//! album no longer picked) are marked, never deleted ([`mark`]): their state becomes `removed`
//! and their body starts with [`REMOVED_LINE`]; one that comes back is restored.

use std::collections::HashSet;
use std::path::PathBuf;

use knode_format::{Error, NodeId, NodeUpdate, Provenance, Result, Space};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::media::{Added, AddedFile};
use crate::photos::{self, NewPhoto, PhotoMetadata};
use crate::recordings::{self, AudioInput, NewRecording, RecordingKind, Track};

/// The id of the source.
pub const SOURCE_ID: &str = "photos-library";

/// The properties a node from the Photos library has, next to its photo or recording ones.
pub mod keys {
    /// The `localIdentifier` of the asset in the Photos library.
    pub const ID: &str = "photos_library.id";
    /// `present`, or `removed` once it left the albums read.
    pub const STATE: &str = "photos_library.state";
    /// What every alias property starts with: see [`alias_key`](super::alias_key).
    pub const ALIAS_PREFIX: &str = "photos_library.alias.";
}

/// The space setting that says whether assets kept only in iCloud are downloaded: `on`, or off
/// (the default, also when absent): they wait until the Mac has the original.
pub const ICLOUD_SETTING: &str = "photos_library.icloud";

/// The first line of the body of a node whose asset left the albums read.
pub const REMOVED_LINE: &str = "**No longer in the Photos albums this space reads.**";

const PRESENT: &str = "present";
const REMOVED: &str = "removed";

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
    /// That original, exported too, for a space that keeps large originals.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub original_path: Option<PathBuf>,
}

/// What [`mark`] changed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Marked {
    /// Nodes marked removed.
    pub removed: usize,
    /// Nodes back in the albums read.
    pub restored: usize,
}

/// The property that keeps a second identifier of a node: two assets of the library with the
/// same content are one photo. Its key holds a hash of the identifier (property keys cannot
/// hold `/`) and its value the identifier.
pub fn alias_key(id: &str) -> String {
    let digest = Sha256::digest(id.as_bytes());
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

/// The node of the space that holds the asset `id`, if any.
pub fn find(space: &Space, id: &str) -> Result<Option<NodeId>> {
    if let Some(found) = space.find_by_property(keys::ID, id)?.into_iter().next() {
        return Ok(Some(found.id));
    }
    Ok(space
        .find_by_property(&alias_key(id), id)?
        .into_iter()
        .next()
        .map(|s| s.id))
}

/// The identifiers of `ids` the space already holds, in the order given.
pub fn known(space: &Space, ids: &[String]) -> Result<Vec<String>> {
    let mut out = Vec::new();
    for id in ids {
        if find(space, id)?.is_some() {
            out.push(id.clone());
        }
    }
    Ok(out)
}

/// Adds an asset: a photo node or a recording node, with its exported file attached, found
/// again by its identifier. An asset the space holds already returns its node unchanged, with
/// `created` false.
pub fn add(space: &mut Space, asset: &LibraryAsset, by: &Provenance) -> Result<AddedFile> {
    let id = asset.id.trim();
    if id.is_empty() {
        return Err(Error::Invalid("the asset has no identifier".into()));
    }
    if let Some(node) = find(space, id)? {
        return Ok(AddedFile {
            node: node_of(space, &node)?,
            created: false,
        });
    }
    let (node, created) = match asset.kind {
        AssetKind::Photo => {
            let key = crate::media::file_key(&asset.path)?;
            let existed = photos::find_by_key(space, &key)?.is_some();
            let photo = photos::create(
                space,
                &NewPhoto {
                    title: asset.title.clone(),
                    path: asset.path.clone(),
                    project: None,
                    metadata: asset.metadata.clone(),
                },
                by,
            )?;
            (Added::Photo(photo), !existed)
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
                    key: Some(format!("{SOURCE_ID}:{id}")),
                    app: None,
                    summaries: recordings::summaries_enabled(space)?,
                    original: asset.original.clone(),
                    original_path: asset.original_path.clone(),
                },
                by,
            )?;
            (Added::Recording(recording), true)
        }
    };
    let node_id = node.id();
    let props = space.properties(&node_id)?;
    match props.get(keys::ID) {
        Some(first) if first != id => {
            space.set_property(&node_id, &alias_key(id), id, by)?;
        }
        Some(_) => {}
        None => {
            space.set_property(&node_id, keys::STATE, PRESENT, by)?;
            space.set_property(&node_id, keys::ID, id, by)?;
        }
    }
    Ok(AddedFile {
        node: node_of(space, &node_id)?,
        created,
    })
}

fn node_of(space: &Space, id: &NodeId) -> Result<Added> {
    match photos::get(space, id) {
        Ok(photo) => Ok(Added::Photo(photo)),
        Err(Error::Invalid(_)) => Ok(Added::Recording(recordings::get(space, id)?)),
        Err(e) => Err(e),
    }
}

/// Marks the nodes from the library whose assets are not among `present` (every identifier in
/// the albums read now) as removed, and restores the ones marked before that are back. Nothing
/// is deleted.
pub fn mark(space: &mut Space, present: &[String], by: &Provenance) -> Result<Marked> {
    let present: HashSet<&str> = present.iter().map(String::as_str).collect();
    let mut marked = Marked::default();
    for state in [PRESENT, REMOVED] {
        for summary in space.find_by_property(keys::STATE, state)? {
            let props = space.properties(&summary.id)?;
            let inside = props
                .iter()
                .filter(|(k, _)| k.as_str() == keys::ID || k.starts_with(keys::ALIAS_PREFIX))
                .any(|(_, v)| present.contains(v.as_str()));
            match (state, inside) {
                (PRESENT, false) => {
                    set_removed(space, &summary.id, true, by)?;
                    marked.removed += 1;
                }
                (REMOVED, true) => {
                    set_removed(space, &summary.id, false, by)?;
                    marked.restored += 1;
                }
                _ => {}
            }
        }
    }
    Ok(marked)
}

fn set_removed(space: &mut Space, id: &NodeId, removed: bool, by: &Provenance) -> Result<()> {
    let body = space.get_node(id)?.ok_or(Error::NotFound(*id))?.body;
    let prefix = format!("{REMOVED_LINE}\n\n");
    let new = match (removed, body.strip_prefix(&prefix)) {
        (true, None) => Some(format!("{prefix}{body}")),
        (false, Some(rest)) => Some(rest.to_string()),
        _ => None,
    };
    if let Some(body) = new {
        space.update_node(
            id,
            NodeUpdate {
                body: Some(body),
                ..Default::default()
            },
            by,
        )?;
    }
    space.set_property(id, keys::STATE, if removed { REMOVED } else { PRESENT }, by)?;
    Ok(())
}

#[cfg(test)]
mod tests;
