//! What the tests of the mail connectors share: secrets in memory, a space, synthetic messages
//! and ways to look at what was written. Every person and address is invented.

use std::collections::HashMap;
use std::io;
use std::sync::Mutex;

use knode_format::{Node, Space, Timestamp};
use knode_ingest::sources::{Container, Cursor, FetchRequest, Fetched, History, Secrets};
use knode_vault::mail::kinds;

use super::provenance;

/// Secrets in memory.
#[derive(Default)]
pub(crate) struct MemSecrets(Mutex<HashMap<String, String>>);

impl Secrets for MemSecrets {
    fn get(&self, name: &str) -> io::Result<Option<String>> {
        Ok(self.0.lock().unwrap().get(name).cloned())
    }
    fn set(&self, name: &str, value: &str) -> io::Result<()> {
        self.0
            .lock()
            .unwrap()
            .insert(name.to_string(), value.to_string());
        Ok(())
    }
    fn delete(&self, name: &str) -> io::Result<bool> {
        Ok(self.0.lock().unwrap().remove(name).is_some())
    }
}

impl MemSecrets {
    pub(crate) fn names(&self) -> Vec<String> {
        let mut names: Vec<String> = self.0.lock().unwrap().keys().cloned().collect();
        names.sort();
        names
    }
}

pub(crate) fn space(dir: &std::path::Path) -> Space {
    Space::create(dir.join("Mail.knode"), "Mail").unwrap()
}

/// A message with this `Message-ID` (none when empty), subject and day of November 2026.
pub(crate) fn message(id: &str, subject: &str, day: u32, body: &str) -> String {
    let mut out = String::new();
    if !id.is_empty() {
        out.push_str(&format!("Message-ID: <{id}>\r\n"));
    }
    out.push_str(&format!(
        "From: Ana Example <ana@example.com>\r\n\
         To: Bo Example <bo@example.org>\r\n\
         Subject: {subject}\r\n\
         Date: Tue, {day} Nov 2026 10:00:00 +0000\r\n\
         Content-Type: text/plain; charset=utf-8\r\n\
         \r\n\
         {body}\r\n"
    ));
    out
}

/// 10:00 UTC on that day of November 2026.
pub(crate) fn day(day: u32) -> Timestamp {
    let date = chrono::NaiveDate::from_ymd_opt(2026, 11, day)
        .unwrap()
        .and_hms_opt(10, 0, 0)
        .unwrap()
        .and_utc();
    Timestamp(date.timestamp_millis())
}

/// The request of a run.
pub(crate) fn request<'a>(
    cursor: Option<&'a Cursor>,
    history: History,
    excluded: &'a [Container],
    secrets: &'a dyn Secrets,
) -> FetchRequest<'a> {
    FetchRequest {
        space_id: "space",
        cursor,
        history,
        since: Timestamp(day(10).0 - 3_600_000),
        excluded,
        max_items: 500,
        secrets,
        media_max_bytes: None,
    }
}

/// Writes what was fetched; returns how many messages were new.
pub(crate) fn write(space: &mut Space, fetched: Fetched, source: &str) -> usize {
    match fetched.batch {
        Some(batch) => batch.write(space, &provenance(source)).unwrap().items,
        None => 0,
    }
}

/// The thread nodes of the space.
pub(crate) fn threads(space: &Space) -> Vec<Node> {
    let mut out: Vec<Node> = space
        .recent(10_000)
        .unwrap()
        .into_iter()
        .filter(|n| n.kind == kinds::THREAD)
        .map(|n| space.get_node(&n.id).unwrap().unwrap())
        .collect();
    out.sort_by(|a, b| a.title.cmp(&b.title));
    out
}

/// The `Message-ID`s written into the space, every section of every thread, sorted.
pub(crate) fn message_ids(space: &Space) -> Vec<String> {
    let mut ids: Vec<String> = threads(space)
        .iter()
        .flat_map(|n| {
            n.body
                .lines()
                .filter_map(|l| l.strip_prefix("<!-- mail "))
                .map(|l| l.split(' ').next().unwrap().to_string())
                .collect::<Vec<_>>()
        })
        .collect();
    ids.sort();
    ids
}
