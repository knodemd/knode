//! The Photos library connector: the photos and videos of the albums a person picks in the
//! Photos library of this Mac.
//!
//! The library is reached only through PhotoKit, which only the Knode app can use, so this
//! connector is run by the app ([`Trigger::App`]), like the calendar: the service lists it in
//! Settings > Sources, keeps its settings in each space (on, the history, the albums picked) and
//! its state on this machine, and asks the app to run it by setting its `next_run`. The app asks
//! what a space wants (`photos_library.plan`), which assets it has already
//! (`photos_library.known`), exports the new ones and hands each to `photos_library.add`, then
//! ends the run with `photos_library.sync`, which marks the assets that left the albums. Only
//! the albums the person picks are read ([`SourceInfo::picking`]): none until they pick one.
//! See `PROTOCOL.md`, "Photos library".
//!
//! Asset and album identifiers mean something only in the library of one Mac, and a space (with
//! its picks) can be read on several. So this Knode home has a library id of its own
//! ([`library_id`], made once and kept in [`DIR`]): every node the library adds keeps it, the
//! albums the app reports are named `<library id>/<album id>` so picks belong to one Mac, and
//! only the nodes of this library are ever marked. Next to it, one [`Ledger`] per space keeps
//! what the library did there on this Mac: the assets whose node the person deleted, never
//! brought back, and the ones that failed for good, never exported again.

use std::fs;
use std::io;
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};

use knode_ingest::photos_library::{Ledger, SOURCE_ID};

use crate::keys::write_private;

use super::{
    Connector, Cursor, FetchRequest, Fetched, Permission, SourceError, SourceInfo, Trigger,
};

/// The Photos library connector, run by the Knode app.
#[derive(Debug, Clone, Copy, Default)]
pub struct PhotosLibraryConnector;

impl Connector for PhotosLibraryConnector {
    fn info(&self) -> SourceInfo {
        SourceInfo::connector(
            SOURCE_ID,
            "Photos library",
            "The photos and videos of the albums you pick in Photos on this Mac, read by the Knode app",
        )
        .needing([Permission::Photos])
        .picking("album")
    }

    fn trigger(&self) -> Trigger {
        Trigger::App
    }

    fn fetch(&self, request: &FetchRequest<'_>) -> Result<Fetched, SourceError> {
        // Never called by the service: the app reads the library.
        Ok(Fetched::nothing(
            request.cursor.cloned().unwrap_or_else(|| Cursor::new("")),
        ))
    }
}

/// The folder of the Knode home, in [`super::SOURCES_DIR`], with the library id and the ledgers.
pub const DIR: &str = "photos-library";
/// The file in [`DIR`] with the library id of this Knode home.
const LIBRARY_FILE: &str = "library";

fn dir(home: &Path) -> PathBuf {
    home.join(super::SOURCES_DIR).join(DIR)
}

fn make_dir(home: &Path) -> io::Result<PathBuf> {
    let dir = dir(home);
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&dir)?;
    Ok(dir)
}

/// The id of the Photos library of this Knode home: made the first time it is asked for, then
/// kept. Asset and album identifiers are qualified by it.
pub fn library_id(home: &Path) -> io::Result<String> {
    let path = dir(home).join(LIBRARY_FILE);
    if let Ok(text) = fs::read_to_string(&path) {
        let id = text.trim();
        if !id.is_empty() && !id.contains('/') {
            return Ok(id.to_string());
        }
    }
    let id = ulid::Ulid::generate().to_string();
    make_dir(home)?;
    write_private(&path, id.as_bytes())?;
    Ok(id)
}

fn ledger_path(home: &Path, space_id: &str) -> PathBuf {
    dir(home).join(format!("{space_id}.json"))
}

/// The ledger of a space for `library`: empty when there is none, it cannot be read, or it is of
/// another library.
pub fn load_ledger(home: &Path, space_id: &str, library: &str) -> Ledger {
    fs::read(ledger_path(home, space_id))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Ledger>(&bytes).ok())
        .filter(|l| l.library == library)
        .unwrap_or_else(|| Ledger::new(library))
}

/// Saves the ledger of a space, readable only by its owner.
pub fn save_ledger(home: &Path, space_id: &str, ledger: &Ledger) -> io::Result<()> {
    make_dir(home)?;
    let bytes = serde_json::to_vec(ledger).map_err(io::Error::other)?;
    write_private(&ledger_path(home, space_id), &bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_library_id_is_made_once_and_ledgers_are_kept_per_library() {
        let home = tempfile::tempdir().unwrap();
        let id = library_id(home.path()).unwrap();
        assert_eq!(library_id(home.path()).unwrap(), id);
        let mut ledger = Ledger::new(&id);
        ledger.deleted.insert("A".into());
        save_ledger(home.path(), "S", &ledger).unwrap();
        assert_eq!(load_ledger(home.path(), "S", &id), ledger);
        assert!(load_ledger(home.path(), "S", "other").deleted.is_empty());
    }
}
