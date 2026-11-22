//! The shapes of the `photos.*` methods: photos added to a space, with what the device reads in
//! them ([`knode_ingest::photos`]).
//!
//! Agents may read photos (`photos.get`, `photos.pending`, `photos.failed`) with `read`; adding
//! one names a file on this machine, and readings are written by the app on the device, so every
//! write is for people only. `PROTOCOL.md` describes each method.

use std::path::PathBuf;

use knode_format::{NodeId, Provenance};
pub use knode_ingest::photos::{NewPhoto, Photo, PhotoMetadata, PhotoReading, ReadingState};
use serde::{Deserialize, Serialize};

use crate::protocol::methods;

/// The methods an agent may call with `read` in a space. The others are for people only.
pub(crate) const READS: &[&str] = &[
    methods::PHOTOS_GET,
    methods::PHOTOS_PENDING,
    methods::PHOTOS_FAILED,
];

/// `photos.create`: a photo and the space to store it in.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PhotoCreateParams {
    /// The space.
    pub space: PathBuf,
    /// The photo; its path is absolute.
    #[serde(flatten)]
    pub photo: NewPhoto,
    /// Who adds it.
    pub by: Provenance,
}

/// `photos.reading`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PhotoReadingParams {
    /// The space.
    pub space: PathBuf,
    /// The photo.
    pub id: NodeId,
    /// What the device read in it.
    pub reading: PhotoReading,
    /// Who read it, such as a system provenance naming the engine.
    pub by: Provenance,
}

/// `photos.fail`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PhotoFailParams {
    /// The space.
    pub space: PathBuf,
    /// The photo.
    pub id: NodeId,
    /// Why, for people.
    #[serde(default)]
    pub error: String,
    /// Who says so.
    pub by: Provenance,
}

/// `photos.retry`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PhotoRetryParams {
    /// The space.
    pub space: PathBuf,
    /// The photo.
    pub id: NodeId,
    /// Who asks.
    pub by: Provenance,
}
