//! [`Client`] methods for `photos.*`.

use std::path::Path;

use knode_format::{NodeId, Provenance};

use super::{Client, absolute, space_path};
use crate::Result;
use crate::photos::*;
use crate::protocol::{NodeParams, SpaceParams, methods};

impl Client {
    /// Adds a photo, copying its file into the space unchanged; the same file the space already
    /// holds returns that photo. People only.
    pub fn create_photo(
        &self,
        space: impl AsRef<Path>,
        photo: &NewPhoto,
        by: &Provenance,
    ) -> Result<Photo> {
        let mut photo = photo.clone();
        photo.path = absolute(&photo.path)?;
        let params = PhotoCreateParams {
            space: space_path(space.as_ref())?,
            photo,
            by: by.clone(),
        };
        self.request(methods::PHOTOS_CREATE, params)
    }

    /// The photos still to read, oldest first.
    pub fn pending_photos(&self, space: impl AsRef<Path>) -> Result<Vec<Photo>> {
        let params = SpaceParams {
            space: space_path(space.as_ref())?,
        };
        self.request(methods::PHOTOS_PENDING, params)
    }

    /// The photos whose reading failed, newest first.
    pub fn failed_photos(&self, space: impl AsRef<Path>) -> Result<Vec<Photo>> {
        let params = SpaceParams {
            space: space_path(space.as_ref())?,
        };
        self.request(methods::PHOTOS_FAILED, params)
    }

    /// A photo.
    pub fn photo(&self, space: impl AsRef<Path>, id: &NodeId) -> Result<Photo> {
        let params = NodeParams {
            space: space_path(space.as_ref())?,
            id: *id,
        };
        self.request(methods::PHOTOS_GET, params)
    }

    /// Writes what the device read in a photo, replacing what it read before. People only.
    pub fn set_photo_reading(
        &self,
        space: impl AsRef<Path>,
        id: &NodeId,
        reading: &PhotoReading,
        by: &Provenance,
    ) -> Result<Photo> {
        let params = PhotoReadingParams {
            space: space_path(space.as_ref())?,
            id: *id,
            reading: reading.clone(),
            by: by.clone(),
        };
        self.request(methods::PHOTOS_READING, params)
    }

    /// Marks the reading of a photo as failed, with why. People only.
    pub fn fail_photo(
        &self,
        space: impl AsRef<Path>,
        id: &NodeId,
        error: &str,
        by: &Provenance,
    ) -> Result<Photo> {
        let params = PhotoFailParams {
            space: space_path(space.as_ref())?,
            id: *id,
            error: error.to_string(),
            by: by.clone(),
        };
        self.request(methods::PHOTOS_FAIL, params)
    }

    /// Puts the reading of a photo back to pending. People only.
    pub fn retry_photo(
        &self,
        space: impl AsRef<Path>,
        id: &NodeId,
        by: &Provenance,
    ) -> Result<Photo> {
        let params = PhotoRetryParams {
            space: space_path(space.as_ref())?,
            id: *id,
            by: by.clone(),
        };
        self.request(methods::PHOTOS_RETRY, params)
    }
}
