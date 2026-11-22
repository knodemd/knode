//! Photos, videos and audio files added to a space: each becomes one node, with the original file
//! attached unchanged. A photo is a [`photos`](crate::photos) node; audio and video are a
//! [`recordings`](crate::recordings) node of kind `recording` (source `file`), which the app
//! transcribes on the device like any recording. The same file added twice (by hash) is one node.

use std::fs;
use std::io::Read;
use std::path::Path;
use std::time::UNIX_EPOCH;

use knode_format::{Error, MAX_ATTACHMENT_SIZE, NodeId, Provenance, Result, Space, setting_keys};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::photos::{self, NewPhoto, Photo};
use crate::recordings::{self, AudioInput, NewRecording, Recording, RecordingKind, Track};

/// The space setting that says whether videos larger than a space attaches keep their original:
/// `on`, or `off` (the default).
pub const KEEP_LARGE_ORIGINALS_SETTING: &str = setting_keys::MEDIA_KEEP_LARGE_ORIGINALS;

/// Whether the space keeps the original of a video larger than [`MAX_ATTACHMENT_SIZE`], next to
/// the audio taken from it: its `media.keep_large_originals` setting is `on`. Off by default, so
/// a space stays quick to copy, back up and sync.
pub fn keep_large_originals(space: &Space) -> Result<bool> {
    Ok(space.setting(KEEP_LARGE_ORIGINALS_SETTING)?.as_deref() == Some("on"))
}

/// Sets whether the space keeps large originals. Returns whether it changed.
pub fn set_keep_large_originals(space: &mut Space, on: bool, by: &Provenance) -> Result<bool> {
    let value = if on { Some("on") } else { None };
    space.set_setting(KEEP_LARGE_ORIGINALS_SETTING, value, by)
}

/// What a media file is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MediaKind {
    /// A photo: HEIC, JPEG or PNG.
    Photo,
    /// An audio file.
    Audio,
    /// A video, whose audio is transcribed.
    Video,
}

/// The extensions of photos Knode adds.
pub const PHOTO_EXTENSIONS: &[&str] = &["heic", "heif", "jpg", "jpeg", "png"];
/// The extensions of audio files Knode adds.
pub const AUDIO_EXTENSIONS: &[&str] = &[
    "m4a", "mp3", "wav", "aac", "aif", "aiff", "caf", "flac", "qta",
];
/// The extensions of videos Knode adds.
pub const VIDEO_EXTENSIONS: &[&str] = &["mov", "mp4", "m4v"];

/// What the file at `path` is, by its extension; `None` when it is not media Knode adds.
pub fn kind_of(path: &Path) -> Option<MediaKind> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    let ext = ext.as_str();
    if PHOTO_EXTENSIONS.contains(&ext) {
        Some(MediaKind::Photo)
    } else if AUDIO_EXTENSIONS.contains(&ext) {
        Some(MediaKind::Audio)
    } else if VIDEO_EXTENSIONS.contains(&ext) {
        Some(MediaKind::Video)
    } else {
        None
    }
}

/// The hash of a file's content, `sha256:<hex>`: the key that finds a media node again.
pub fn file_key(path: &Path) -> Result<String> {
    let mut file = fs::File::open(path)
        .map_err(|e| Error::Invalid(format!("cannot read {}: {e}", path.display())))?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 1 << 16];
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hasher.update(&buffer[..n]);
    }
    let digest = hasher.finalize();
    let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    Ok(format!("sha256:{hex}"))
}

/// Fails unless `path` is a file a space can attach: it exists, is a file and is at most
/// [`MAX_ATTACHMENT_SIZE`].
pub fn check_file(path: &Path) -> Result<()> {
    let metadata = fs::metadata(path)
        .map_err(|e| Error::Invalid(format!("cannot read {}: {e}", path.display())))?;
    if !metadata.is_file() {
        return Err(Error::Invalid(format!("{} is not a file", path.display())));
    }
    if metadata.len() > MAX_ATTACHMENT_SIZE {
        return Err(too_large(path));
    }
    Ok(())
}

fn too_large(path: &Path) -> Error {
    Error::Invalid(format!(
        "{} is larger than {} MB, the most a space attaches",
        path.display(),
        MAX_ATTACHMENT_SIZE / (1024 * 1024)
    ))
}

pub(crate) fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

pub(crate) fn stem(path: &Path) -> String {
    path.file_stem()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| file_name(path))
}

/// The node a media file became.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Added {
    /// A photo.
    Photo(Photo),
    /// An audio file or a video.
    Recording(Recording),
}

impl Added {
    /// Its node.
    pub fn id(&self) -> NodeId {
        match self {
            Added::Photo(p) => p.id,
            Added::Recording(r) => r.id,
        }
    }

    /// Its title.
    pub fn title(&self) -> &str {
        match self {
            Added::Photo(p) => &p.title,
            Added::Recording(r) => &r.title,
        }
    }
}

/// What [`add_file`] did.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AddedFile {
    /// The node.
    pub node: Added,
    /// Whether it is new; false when the space already held the same file.
    pub created: bool,
}

/// Adds a photo, an audio file or a video to the space, from what the core can tell without the
/// app: a photo without metadata (the app reads it with the photo), a recording titled after the
/// file, started when the file was last changed, of unknown length. A file larger than a space
/// attaches is refused: the Mac app takes larger videos and keeps their audio.
pub fn add_file(
    space: &mut Space,
    path: &Path,
    project: Option<NodeId>,
    by: &Provenance,
) -> Result<AddedFile> {
    let kind = kind_of(path).ok_or_else(|| {
        Error::Invalid(format!(
            "{} is not a photo (HEIC, JPEG, PNG), an audio file or a video (MOV, MP4)",
            path.display()
        ))
    })?;
    let metadata = fs::metadata(path)
        .map_err(|e| Error::Invalid(format!("cannot read {}: {e}", path.display())))?;
    if metadata.is_file() && metadata.len() > MAX_ATTACHMENT_SIZE {
        let hint = match kind {
            MediaKind::Photo => "",
            _ => ": add it in the Knode app for Mac, which keeps its audio for the transcript",
        };
        return Err(Error::Invalid(format!("{}{hint}", too_large(path))));
    }
    check_file(path)?;
    let key = file_key(path)?;
    match kind {
        MediaKind::Photo => {
            let created = photos::find_by_key(space, &key)?.is_none();
            let photo = photos::create(
                space,
                &NewPhoto {
                    title: String::new(),
                    path: path.to_path_buf(),
                    project,
                    metadata: Default::default(),
                },
                by,
            )?;
            Ok(AddedFile {
                node: Added::Photo(photo),
                created,
            })
        }
        MediaKind::Audio | MediaKind::Video => {
            let created = recordings::find_by_key(space, &key)?.is_none();
            let started_at = metadata
                .modified()
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.as_millis() as i64)
                .unwrap_or_else(|| chrono::Utc::now().timestamp_millis());
            let recording = recordings::create(
                space,
                &NewRecording {
                    kind: RecordingKind::Recording,
                    title: stem(path),
                    started_at,
                    ended_at: started_at,
                    project,
                    audio: vec![AudioInput {
                        path: path.to_path_buf(),
                        track: Track::Mixed,
                    }],
                    key: Some(key),
                    app: None,
                    summaries: recordings::summaries_enabled(space)?,
                    original: None,
                    original_path: None,
                },
                by,
            )?;
            Ok(AddedFile {
                node: Added::Recording(recording),
                created,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use knode_format::Provenance;

    #[test]
    fn kinds_by_extension() {
        assert_eq!(kind_of(Path::new("a/IMG_1.HEIC")), Some(MediaKind::Photo));
        assert_eq!(kind_of(Path::new("talk.mp4")), Some(MediaKind::Video));
        assert_eq!(kind_of(Path::new("memo.m4a")), Some(MediaKind::Audio));
        assert_eq!(kind_of(Path::new("notes.md")), None);
        assert_eq!(kind_of(Path::new("noext")), None);
    }

    #[test]
    fn the_same_file_is_added_once() {
        let dir = tempfile::tempdir().unwrap();
        let mut space = Space::create(dir.path().join("M.knode"), "M").unwrap();
        let by = Provenance::person("Ada");
        let photo = dir.path().join("board.jpg");
        fs::write(&photo, b"jpeg bytes").unwrap();
        let first = add_file(&mut space, &photo, None, &by).unwrap();
        assert!(first.created);
        let copy = dir.path().join("copy.jpg");
        fs::copy(&photo, &copy).unwrap();
        let again = add_file(&mut space, &copy, None, &by).unwrap();
        assert!(!again.created);
        assert_eq!(again.node.id(), first.node.id());

        let video = dir.path().join("talk.mov");
        fs::write(&video, b"movie").unwrap();
        let added = add_file(&mut space, &video, None, &by).unwrap();
        let Added::Recording(recording) = &added.node else {
            panic!("a video is a recording");
        };
        assert_eq!(recording.kind, RecordingKind::Recording);
        assert_eq!(recording.source, recordings::Source::File);
        assert_eq!(recording.tracks[0].name, "talk.mov");
        assert_eq!(recording.title, "talk");
        assert!(recording.key.as_deref().unwrap().starts_with("sha256:"));
        let body = space.get_node(&recording.id).unwrap().unwrap().body;
        assert!(body.starts_with("Recorded on "), "{body}");
        assert!(!add_file(&mut space, &video, None, &by).unwrap().created);
        assert_eq!(recordings::pending(&space).unwrap().len(), 1);

        let text = dir.path().join("a.txt");
        fs::write(&text, b"x").unwrap();
        assert!(add_file(&mut space, &text, None, &by).is_err());
    }

    #[test]
    fn keys_are_sha256_of_the_content() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.png");
        fs::write(&path, b"abc").unwrap();
        assert_eq!(
            file_key(&path).unwrap(),
            "sha256:ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
