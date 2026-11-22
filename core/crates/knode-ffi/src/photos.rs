//! Photos added to a space, with what the device reads in them ([`knode_ingest::photos`]). The
//! same rules as Knode Service's `photos.*` methods, for when the app writes the space itself.

use knode_ingest::photos as ph;

use crate::{KnodeSpace, Provenance, Result, parse_id};

/// How far the reading of a photo is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum PhotoReadingState {
    /// Still to be read.
    Pending,
    /// Read.
    Done,
    /// Reading it failed; the photo's `error` says why.
    Failed,
}

impl From<ph::ReadingState> for PhotoReadingState {
    fn from(v: ph::ReadingState) -> Self {
        match v {
            ph::ReadingState::Pending => PhotoReadingState::Pending,
            ph::ReadingState::Done => PhotoReadingState::Done,
            ph::ReadingState::Failed => PhotoReadingState::Failed,
        }
    }
}

/// What the file of a photo says about it. Every part is optional.
#[derive(Debug, Clone, Default, PartialEq, uniffi::Record)]
pub struct PhotoMetadata {
    /// When it was taken, in Unix milliseconds.
    pub taken_at: Option<i64>,
    /// Latitude in degrees, north positive.
    pub latitude: Option<f64>,
    /// Longitude in degrees, east positive.
    pub longitude: Option<f64>,
    /// The name of the place, only when the device could tell it without the network.
    pub place: Option<String>,
    /// The camera.
    pub camera: Option<String>,
    /// Width in pixels.
    pub width: Option<u32>,
    /// Height in pixels.
    pub height: Option<u32>,
}

impl From<PhotoMetadata> for ph::PhotoMetadata {
    fn from(m: PhotoMetadata) -> Self {
        ph::PhotoMetadata {
            taken_at: m.taken_at,
            latitude: m.latitude,
            longitude: m.longitude,
            place: m.place,
            camera: m.camera,
            width: m.width,
            height: m.height,
        }
    }
}

impl From<ph::PhotoMetadata> for PhotoMetadata {
    fn from(m: ph::PhotoMetadata) -> Self {
        PhotoMetadata {
            taken_at: m.taken_at,
            latitude: m.latitude,
            longitude: m.longitude,
            place: m.place,
            camera: m.camera,
            width: m.width,
            height: m.height,
        }
    }
}

/// A photo to add.
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct NewPhoto {
    /// Its title; the file's name without its extension when empty.
    pub title: String,
    /// The file on this machine. It is copied into the space unchanged.
    pub path: String,
    /// The project node it belongs to.
    pub project: Option<String>,
    /// What its file says about it.
    pub metadata: PhotoMetadata,
}

/// A stored photo.
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct Photo {
    /// Its node.
    pub id: String,
    /// Its title.
    pub title: String,
    /// The name of the attached original file.
    pub file: String,
    /// The hash of its file, `sha256:<hex>`.
    pub key: Option<String>,
    /// When and where it was taken, the camera and its size.
    pub metadata: PhotoMetadata,
    /// How far its reading is.
    pub reading: PhotoReadingState,
    /// Why its reading failed.
    pub error: Option<String>,
    /// The project it is part of.
    pub project: Option<String>,
}

impl From<ph::Photo> for Photo {
    fn from(p: ph::Photo) -> Self {
        Photo {
            id: p.id.to_string(),
            title: p.title,
            file: p.file,
            key: p.key,
            metadata: p.metadata.into(),
            reading: p.reading.into(),
            error: p.error,
            project: p.project.map(|id| id.to_string()),
        }
    }
}

/// What the device read in a photo.
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct PhotoReading {
    /// What read it, such as `Apple Vision`.
    pub engine: String,
    /// The text found in it, line by line.
    pub text: String,
    /// What it shows, most likely first.
    pub labels: Vec<String>,
    /// A short description, where an on-device model wrote one.
    pub description: Option<String>,
    /// What its file says about it, added to what the photo already has.
    pub metadata: PhotoMetadata,
}

impl From<PhotoReading> for ph::PhotoReading {
    fn from(r: PhotoReading) -> Self {
        ph::PhotoReading {
            engine: r.engine,
            text: r.text,
            labels: r.labels,
            description: r.description,
            metadata: r.metadata.into(),
        }
    }
}

#[uniffi::export]
impl KnodeSpace {
    /// Adds a photo: its node, with its file copied in unchanged, its properties and a `part-of`
    /// link to its project. The same file the space already holds returns that photo.
    pub fn create_photo(&self, photo: NewPhoto, by: Provenance) -> Result<Photo> {
        let new = ph::NewPhoto {
            title: photo.title,
            path: photo.path.into(),
            project: photo.project.as_deref().map(parse_id).transpose()?,
            metadata: photo.metadata.into(),
        };
        Ok(ph::create(&mut self.lock(), &new, &by.into())?.into())
    }

    /// The photos still to read, oldest first.
    pub fn pending_photos(&self) -> Result<Vec<Photo>> {
        Ok(ph::pending(&self.lock())?
            .into_iter()
            .map(Photo::from)
            .collect())
    }

    /// The photos whose reading failed, newest first.
    pub fn failed_photos(&self) -> Result<Vec<Photo>> {
        Ok(ph::failed(&self.lock())?
            .into_iter()
            .map(Photo::from)
            .collect())
    }

    /// A photo, read from its node.
    pub fn photo(&self, id: String) -> Result<Photo> {
        let id = parse_id(&id)?;
        Ok(ph::get(&self.lock(), &id)?.into())
    }

    /// Writes what the device read in a photo, replacing what it read before.
    pub fn set_photo_reading(
        &self,
        id: String,
        reading: PhotoReading,
        by: Provenance,
    ) -> Result<Photo> {
        let id = parse_id(&id)?;
        Ok(ph::set_reading(&mut self.lock(), &id, &reading.into(), &by.into())?.into())
    }

    /// Marks the reading of a photo as failed, with why.
    pub fn fail_photo(&self, id: String, error: String, by: Provenance) -> Result<Photo> {
        let id = parse_id(&id)?;
        Ok(ph::set_failed(&mut self.lock(), &id, &error, &by.into())?.into())
    }

    /// Puts the reading of a photo back to pending.
    pub fn retry_photo(&self, id: String, by: Provenance) -> Result<Photo> {
        let id = parse_id(&id)?;
        Ok(ph::retry(&mut self.lock(), &id, &by.into())?.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ActorKind, KnodeError};

    fn by(kind: ActorKind, actor: &str, source: Option<&str>) -> Provenance {
        Provenance {
            kind,
            actor: actor.into(),
            session: None,
            source: source.map(Into::into),
        }
    }

    #[test]
    fn adds_reads_fails_and_retries() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("P.knode").display().to_string();
        let space = KnodeSpace::create(path, "P".into()).unwrap();
        let me = by(ActorKind::Person, "Ada", None);
        let file = dir.path().join("board.png");
        std::fs::write(&file, b"png").unwrap();
        let new = NewPhoto {
            title: String::new(),
            path: file.display().to_string(),
            project: None,
            metadata: PhotoMetadata {
                taken_at: Some(1_795_343_400_000),
                width: Some(2),
                height: Some(1),
                ..Default::default()
            },
        };
        let photo = space.create_photo(new.clone(), me.clone()).unwrap();
        assert_eq!(photo.title, "board");
        assert_eq!(photo.reading, PhotoReadingState::Pending);
        assert_eq!(photo.metadata.width, Some(2));
        assert_eq!(space.create_photo(new, me.clone()).unwrap(), photo);
        assert_eq!(space.pending_photos().unwrap(), vec![photo.clone()]);

        let vision = by(ActorKind::System, "photos", Some("apple-vision"));
        let read = space
            .set_photo_reading(
                photo.id.clone(),
                PhotoReading {
                    engine: "Apple Vision".into(),
                    text: "Hola".into(),
                    labels: vec!["document".into()],
                    description: None,
                    metadata: PhotoMetadata {
                        latitude: Some(40.0),
                        longitude: Some(-3.0),
                        ..Default::default()
                    },
                },
                vision.clone(),
            )
            .unwrap();
        assert_eq!(read.reading, PhotoReadingState::Done);
        assert_eq!(read.metadata.latitude, Some(40.0));
        let body = space.get_node(photo.id.clone()).unwrap().unwrap().body;
        assert!(body.contains("## Text in the photo\n\nHola"));
        assert_eq!(space.photo(photo.id.clone()).unwrap(), read);

        let failed = space
            .fail_photo(photo.id.clone(), "Unreadable".into(), vision)
            .unwrap();
        assert_eq!(failed.reading, PhotoReadingState::Failed);
        assert_eq!(space.failed_photos().unwrap().len(), 1);
        let retried = space.retry_photo(photo.id.clone(), me.clone()).unwrap();
        assert_eq!(retried.reading, PhotoReadingState::Pending);
        assert!(space.failed_photos().unwrap().is_empty());

        let note = space.create_node("Note".into(), String::new(), me).unwrap();
        assert!(matches!(
            space.photo(note.id),
            Err(KnodeError::Invalid { .. })
        ));
    }
}
