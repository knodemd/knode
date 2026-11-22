//! The calendar connector: the events of the calendars on this Mac.
//!
//! Calendars are reached only through EventKit, which only the Knode app can use, so this
//! connector is run by the app ([`Trigger::App`]): the service lists it in Settings > Sources,
//! keeps its settings in each space (on, the history, the calendars excluded) and its state on
//! this machine, and asks the app to run it by setting its `next_run`. The app then asks which
//! days and calendars a space wants (`calendar.plan`), reads them, and hands the events to the
//! service (`calendar.sync`), which writes them with [`knode_ingest::calendar::sync`]. See
//! `PROTOCOL.md`, "Calendar".

use knode_ingest::calendar::SOURCE_ID;

use super::{
    Connector, Cursor, FetchRequest, Fetched, Permission, SourceError, SourceInfo, Trigger,
};

/// The calendar connector, run by the Knode app.
#[derive(Debug, Clone, Copy, Default)]
pub struct CalendarConnector;

impl Connector for CalendarConnector {
    fn info(&self) -> SourceInfo {
        SourceInfo::connector(
            SOURCE_ID,
            "Calendar",
            "The events of the calendars on this Mac, read by the Knode app",
        )
        .needing([Permission::Calendar])
        .excluding("calendar")
    }

    fn trigger(&self) -> Trigger {
        Trigger::App
    }

    fn fetch(&self, request: &FetchRequest<'_>) -> Result<Fetched, SourceError> {
        // Never called by the service: the app reads the calendars.
        Ok(Fetched::nothing(
            request.cursor.cloned().unwrap_or_else(|| Cursor::new("")),
        ))
    }
}
