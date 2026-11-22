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

use knode_ingest::photos_library::SOURCE_ID;

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
