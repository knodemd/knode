//! Voice Memos, read live from the Voice Memos library of this Mac: [`VoiceMemosConnector`].
//!
//! Voice Memos keeps every recording made on the Mac, and those made on an iPhone and synced
//! through iCloud, in `~/Library/Group Containers/group.com.apple.VoiceMemos.shared/Recordings`:
//! one audio file per memo (`.m4a`, or `.qta` on recent macOS) and `CloudRecordings.db`, a Core
//! Data SQLite database listing them. Reading them needs Full Disk Access.
//!
//! - The database is never opened where it is: it is copied, with its write-ahead log, into a
//!   private temporary folder and only the copy is read
//!   ([`sqlite::open_copy`](crate::sources::sqlite::open_copy)). The copy is checked first for
//!   the columns Knode needs ([`REQUIRED`]); when one is missing, nothing is written and the
//!   error says the connector needs an update. The columns that differ between versions of
//!   macOS (the title, the eviction date, the folder) are read only when they are there.
//! - Every memo becomes one recording of kind
//!   [`VOICE_MEMO_KIND`](crate::recordings::VOICE_MEMO_KIND), stored with
//!   [`recordings::create`]: its title (or `Voice memo, <date> <time>` when it has none), when it
//!   started and ended, and its audio file attached as the `mixed` track. Its key is
//!   `voice-memos:<unique id>`, so a memo read twice (a run cut short, a lost cursor, a rebuilt
//!   database, two Macs writing one space) is stored once. The app transcribes it, and
//!   summarises it when the space asks for summaries, on the device.
//! - Memos in Recently Deleted (with an eviction date) are left out, and looked at again on
//!   every run: one restored comes in then, one removed for good is forgotten.
//! - A memo's audio file is brought in only once it is settled: the same size and modification
//!   time in two runs, and not changed for [`SETTLED_AFTER_MS`]. A memo still being recorded, or
//!   a file iCloud or Voice Memos is still writing, is waited for instead of being attached cut
//!   short; so a memo comes in one run after it is first seen.
//! - The cursor is the number (`Z_PK`) of the last memo read, with the memos waited for (their
//!   audio file is not on this Mac yet, synced from iCloud and still downloading, or not settled)
//!   and those in Recently Deleted. Those are read again on every run; none is dropped, and those
//!   whose file is still not on this Mac after a day are counted in the connector's
//!   [notice](Connector::notice). A memo whose file is larger than a space attaches is left out
//!   and counted in the notice too.
//! - The cursor starts again only for a rebuilt database: one whose Core Data count of the
//!   memo numbers it gave (`Z_PRIMARYKEY.Z_MAX`, which never goes down) is below the cursor.
//!   Removing the newest memo does not start it again. Without that table, a cursor above the
//!   highest number left starts again. Memos read again then are found by their key and stored
//!   once, but a memo whose recording was deleted in the space would come back, which is why
//!   the cursor starts again only then.
//! - The containers are the folders of Voice Memos, when the database has them: a memo in an
//!   excluded folder is never read. Memos in no folder are always read.
//! - With [`History::FromToday`], nothing recorded before the day the connector was connected
//!   is read.

use std::fmt;
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use chrono::{DateTime, Local};
use knode_format::{MAX_ATTACHMENT_SIZE, Provenance, Space, Timestamp};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use crate::recordings::{self, AudioInput, NewRecording, RecordingKind, Track};
use crate::sources::sqlite::{self, DatabaseCopy};
use crate::sources::{
    Batch, Connector, Container, Cursor, FetchRequest, Fetched, Permission, SourceError,
    SourceInfo, Trigger, Written,
};

/// The id of the connector.
pub const ID: &str = "voice-memos";

/// The database of the library.
pub const DATABASE: &str = "CloudRecordings.db";

/// The start of the key of a memo's recording: `voice-memos:<unique id>`.
pub const KEY_PREFIX: &str = "voice-memos:";

/// The table of memos.
const MEMOS: &str = "ZCLOUDRECORDING";

/// The table of folders, when the database has one.
const FOLDERS: &str = "ZFOLDER";

/// The columns every version of Voice Memos has, which the database must have.
pub const REQUIRED: &[(&str, &[&str])] =
    &[(MEMOS, &["Z_PK", "ZDATE", "ZDURATION", "ZPATH", "ZUNIQUEID"])];

/// The columns holding a memo's title, best first; read when the database has them.
const TITLE_COLUMNS: &[&str] = &["ZENCRYPTEDTITLE", "ZCUSTOMLABEL"];

/// The column set when a memo is in Recently Deleted; read when the database has it.
const EVICTION_COLUMN: &str = "ZEVICTIONDATE";

/// The column naming a memo's folder; read when the database has it.
const FOLDER_COLUMN: &str = "ZFOLDER";

/// The columns holding a folder's name, best first.
const FOLDER_NAME_COLUMNS: &[&str] = &["ZENCRYPTEDNAME", "ZNAME"];

/// The column holding a folder's unique id, used as its container id when the database has it.
const FOLDER_ID_COLUMN: &str = "ZUUID";

/// Seconds from 1970-01-01 to 2001-01-01, where Core Data times start.
const APPLE_EPOCH: f64 = 978_307_200.0;

/// The longest time between two runs when no change of the database was seen. The service
/// also runs the connector at this interval while a memo's file is waited for.
const RUN_AT_LEAST_EVERY: Duration = Duration::from_secs(60);

/// After how long a memo whose file is still not on this Mac is reported in the notice.
const LATE_AFTER_MS: i64 = 24 * 3600 * 1000;

/// How long a memo's audio file must not have changed before it is brought in.
pub const SETTLED_AFTER_MS: i64 = 10_000;

/// The name Core Data gives the memo entity in `Z_PRIMARYKEY`.
const MEMO_ENTITY: &str = "CloudRecording";

/// The Voice Memos library of the person running Knode.
pub fn default_library_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .map(|home| {
            PathBuf::from(home)
                .join("Library/Group Containers/group.com.apple.VoiceMemos.shared/Recordings")
        })
}

/// What a person reads when macOS keeps Knode from reading the library.
const NO_ACCESS: &str = "macOS keeps Knode from reading the recordings of Voice Memos. Open \
     System Settings > Privacy & Security > Full Disk Access, turn it on for Knode (or the app \
     that runs Knode Service), then quit and reopen that app. Voice memos come in from then on.";

/// The Voice Memos connector: brings in the memos of Voice Memos on this Mac as recordings,
/// with their audio attached. See the [module](self).
#[derive(Clone)]
pub struct VoiceMemosConnector {
    /// The `Recordings` folder, with the database and the audio files in it.
    dir: PathBuf,
}

impl fmt::Debug for VoiceMemosConnector {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("VoiceMemosConnector")
            .field("dir", &self.dir)
            .finish()
    }
}

impl VoiceMemosConnector {
    /// The connector for the Voice Memos library of the person running Knode.
    pub fn local() -> Option<VoiceMemosConnector> {
        default_library_dir().map(VoiceMemosConnector::new)
    }

    /// The connector for a Voice Memos `Recordings` folder.
    pub fn new(dir: impl Into<PathBuf>) -> VoiceMemosConnector {
        VoiceMemosConnector { dir: dir.into() }
    }

    /// The database.
    pub fn database(&self) -> PathBuf {
        self.dir.join(DATABASE)
    }

    fn snapshot(&self) -> Result<Snapshot, SourceError> {
        match fs::metadata(&self.dir) {
            Ok(meta) if meta.is_dir() => {}
            Ok(_) => return Err(no_library(&self.dir)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Err(no_library(&self.dir)),
            Err(e) if e.kind() == io::ErrorKind::PermissionDenied => return Err(no_access()),
            Err(e) => {
                return Err(SourceError::Failed(format!(
                    "{} could not be read: {e}",
                    self.dir.display()
                )));
            }
        }
        let copy = sqlite::open_copy(&self.database()).map_err(|e| match e {
            SourceError::Permission(..) => no_access(),
            SourceError::Unavailable(_) => SourceError::Unavailable(
                "Voice Memos has no recordings on this Mac yet: open Voice Memos once, and turn \
                 on Voice Memos in iCloud settings to see the memos of your iPhone"
                    .into(),
            ),
            other => other,
        })?;
        Snapshot::new(copy)
    }

    /// The memo's audio file in the library, when it is there with something in it.
    fn audio(&self, path: Option<&str>) -> Option<(PathBuf, fs::Metadata)> {
        let file = audio_path(&self.dir, path?)?;
        let meta = fs::metadata(&file).ok()?;
        (meta.is_file() && meta.len() > 0).then_some((file, meta))
    }

    /// Sorts out one memo read from the database: brought in, waited for, in Recently Deleted,
    /// too large, or left out. `seen` is how it was waited for until now.
    fn take(
        &self,
        request: &FetchRequest<'_>,
        row: MemoRow,
        seen: Option<Waiting>,
        now: i64,
        found: &mut Found,
        too_large: &mut Vec<i64>,
    ) {
        if row.unique_id.trim().is_empty() {
            return;
        }
        if row.evicted {
            if !found.evicted.contains(&row.pk) {
                found.evicted.push(row.pk);
            }
            return;
        }
        if row
            .folder
            .as_deref()
            .is_some_and(|f| request.is_excluded(f))
        {
            return;
        }
        let first_seen = seen.map_or(now, |w| w.1);
        let Some((file, meta)) = self.audio(row.path.as_deref()) else {
            found.waiting.push(Waiting(row.pk, first_seen, 0, 0));
            return;
        };
        if meta.len() > MAX_ATTACHMENT_SIZE {
            if !too_large.contains(&row.pk) {
                too_large.push(row.pk);
            }
            return;
        }
        let size = meta.len();
        let modified = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX));
        let same = seen.is_some_and(|w| w.2 == size && w.3 == modified);
        if same && modified <= now - SETTLED_AFTER_MS {
            found.memos.push(Memo::new(&row, file));
        } else {
            found
                .waiting
                .push(Waiting(row.pk, first_seen, size, modified));
        }
    }
}

/// What one fetch found.
#[derive(Default)]
struct Found {
    memos: Vec<Memo>,
    waiting: Vec<Waiting>,
    evicted: Vec<i64>,
}

fn no_library(dir: &Path) -> SourceError {
    SourceError::Unavailable(format!(
        "Voice Memos has no library on this Mac ({} is missing): open Voice Memos once, and \
         turn on Voice Memos in iCloud settings to see the memos of your iPhone",
        dir.display()
    ))
}

fn no_access() -> SourceError {
    SourceError::Permission(Permission::FullDiskAccess, NO_ACCESS.into())
}

fn failed(error: rusqlite::Error) -> SourceError {
    SourceError::Failed(format!(
        "the Voice Memos database could not be read: {error}"
    ))
}

/// Where the file a memo names is, inside the library: its path relative to it, or, for an
/// absolute path (older versions), the file of that name in it. `None` for a path leaving it.
fn audio_path(dir: &Path, path: &str) -> Option<PathBuf> {
    let path = Path::new(path.trim());
    if path.as_os_str().is_empty() {
        return None;
    }
    if path.is_absolute() {
        return Some(dir.join(path.file_name()?));
    }
    let mut full = dir.to_path_buf();
    for component in path.components() {
        match component {
            Component::Normal(part) => full.push(part),
            Component::CurDir => {}
            _ => return None,
        }
    }
    Some(full)
}

impl Connector for VoiceMemosConnector {
    fn info(&self) -> SourceInfo {
        SourceInfo::connector(
            ID,
            "Voice Memos",
            "The recordings of Voice Memos on this Mac, and those of your iPhone synced through \
             iCloud",
        )
        .needing([Permission::FullDiskAccess])
        .excluding("folder")
    }

    fn trigger(&self) -> Trigger {
        let db = self.database();
        Trigger::Watch {
            paths: vec![db.clone(), sqlite_wal(&db)],
            every: RUN_AT_LEAST_EVERY,
        }
    }

    fn check(&self) -> Result<(), SourceError> {
        self.snapshot().map(|_| ())
    }

    fn waiting(&self, cursor: &Cursor) -> bool {
        // An audio file iCloud downloads after the database lists the memo is not seen as a
        // change of the database.
        !State::read(Some(cursor)).waiting.is_empty()
    }

    fn notice(&self, cursor: &Cursor) -> Option<String> {
        let state = State::read(Some(cursor));
        let late_since = Timestamp::now().0 - LATE_AFTER_MS;
        let late = state
            .waiting
            .iter()
            .filter(|w| w.2 == 0 && w.1 < late_since)
            .count();
        let mut notes = Vec::new();
        match late {
            0 => {}
            1 => notes.push(
                "1 voice memo has had no audio on this Mac for more than a day, so it is not in \
                 the space yet: it is still in iCloud. It comes in once Voice Memos downloads \
                 it, for example when you play it in Voice Memos."
                    .to_string(),
            ),
            n => notes.push(format!(
                "{n} voice memos have had no audio on this Mac for more than a day, so they are \
                 not in the space yet: they are still in iCloud. They come in once Voice Memos \
                 downloads them, for example when you play them in Voice Memos."
            )),
        }
        let max = MAX_ATTACHMENT_SIZE / (1024 * 1024);
        match state.too_large.len() {
            0 => {}
            1 => notes.push(format!(
                "1 voice memo is larger than {max} MB, the most a space attaches, so it was \
                 left out."
            )),
            n => notes.push(format!(
                "{n} voice memos are larger than {max} MB, the most a space attaches, so they \
                 were left out."
            )),
        }
        (!notes.is_empty()).then(|| notes.join(" "))
    }

    fn containers(&self, _request: &FetchRequest<'_>) -> Result<Vec<Container>, SourceError> {
        let snapshot = self.snapshot()?;
        let mut folders = snapshot.folders().map_err(failed)?;
        folders.sort_by(|a, b| (&a.name, &a.id).cmp(&(&b.name, &b.id)));
        Ok(folders)
    }

    fn fetch(&self, request: &FetchRequest<'_>) -> Result<Fetched, SourceError> {
        let snapshot = self.snapshot()?;
        let newest = snapshot.last_pk().map_err(failed)?;
        let given = snapshot.given_pk().map_err(failed)?.unwrap_or(0);
        let mut state = State::read(request.cursor);
        // A rebuilt database: its numbers start again. What was read before is found again by
        // its key and stored once.
        if state.pk > newest.max(given) {
            state = State::default();
        }
        let now = Timestamp::now().0;
        let max = request.max_items.max(1);
        let mut found = Found::default();
        let mut more = false;

        // The memos waited for, first. One that left the database or is in a folder now
        // excluded is not waited for any more.
        for w in std::mem::take(&mut state.waiting) {
            if found.memos.len() >= max {
                found.waiting.push(w);
                more = true;
                continue;
            }
            if let Some(row) = snapshot.memo(w.0).map_err(failed)? {
                self.take(request, row, Some(w), now, &mut found, &mut state.too_large);
            }
        }

        // Then those in Recently Deleted, which come in once restored.
        for pk in std::mem::take(&mut state.evicted) {
            if found.memos.len() >= max {
                found.evicted.push(pk);
                more = true;
                continue;
            }
            if let Some(row) = snapshot.memo(pk).map_err(failed)? {
                self.take(request, row, None, now, &mut found, &mut state.too_large);
            }
        }

        // Then what is new after the cursor.
        let mut last = state.pk;
        if !more {
            for row in snapshot.memos_after(state.pk).map_err(failed)? {
                if found.memos.len() >= max {
                    more = true;
                    break;
                }
                last = row.pk;
                if !request.wants(Timestamp(row.started_at)) {
                    continue;
                }
                self.take(request, row, None, now, &mut found, &mut state.too_large);
            }
            if !more {
                last = last.max(newest);
            }
        }
        state.pk = last;
        state.waiting = found.waiting;
        state.evicted = found.evicted;
        let cursor = state.write();
        if found.memos.is_empty() {
            return Ok(Fetched::nothing(cursor).with_more(more));
        }
        Ok(Fetched::batch(VoiceMemosBatch { memos: found.memos }, cursor).with_more(more))
    }
}

fn sqlite_wal(db: &Path) -> PathBuf {
    let mut name = db.as_os_str().to_os_string();
    name.push("-wal");
    PathBuf::from(name)
}

/// A memo waited for: its number (`Z_PK`), when it was first seen (milliseconds since 1970),
/// and the size and modification time (milliseconds since 1970) its file had then, both 0 while
/// it has none. Cursors written before the last two were kept have only the first two.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
struct Waiting(i64, i64, #[serde(default)] u64, #[serde(default)] i64);

/// The cursor: the last memo read, the memos waited for, those in Recently Deleted, and those
/// left out as too large.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
struct State {
    pk: i64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    waiting: Vec<Waiting>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    evicted: Vec<i64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    too_large: Vec<i64>,
}

impl State {
    fn read(cursor: Option<&Cursor>) -> State {
        cursor
            .and_then(|c| serde_json::from_str(c.as_str()).ok())
            .unwrap_or_default()
    }

    fn write(&self) -> Cursor {
        Cursor::new(serde_json::to_string(self).unwrap_or_default())
    }
}

/// A memo as the database lists it.
#[derive(Debug, Clone)]
struct MemoRow {
    pk: i64,
    unique_id: String,
    /// Unix milliseconds.
    started_at: i64,
    duration_ms: i64,
    path: Option<String>,
    title: Option<String>,
    evicted: bool,
    /// The container id of its folder.
    folder: Option<String>,
}

/// A checked copy of the database, removed when dropped.
struct Snapshot {
    copy: DatabaseCopy,
    /// The SQL giving a memo's title.
    title: String,
    /// The SQL saying whether a memo is in Recently Deleted.
    evicted: String,
    /// The SQL giving a memo's folder's container id, and the join it needs.
    folder: String,
    folder_join: String,
    /// The SQL listing the folders (id, name), when the database has them.
    folders: Option<String>,
}

fn columns(conn: &Connection, table: &str) -> rusqlite::Result<Vec<String>> {
    let mut stmt = conn.prepare("SELECT name FROM pragma_table_info(?1)")?;
    let rows = stmt.query_map([table], |row| row.get::<_, String>(0))?;
    rows.collect()
}

fn has(columns: &[String], name: &str) -> bool {
    columns.iter().any(|c| c.eq_ignore_ascii_case(name))
}

impl Snapshot {
    fn new(copy: DatabaseCopy) -> Result<Snapshot, SourceError> {
        sqlite::check_schema(copy.conn(), REQUIRED)?;
        let memo_columns = columns(copy.conn(), MEMOS).map_err(failed)?;
        let titles: Vec<String> = TITLE_COLUMNS
            .iter()
            .filter(|c| has(&memo_columns, c))
            .map(|c| format!("NULLIF(TRIM(m.{c}), '')"))
            .collect();
        let title = match titles.len() {
            0 => "NULL".to_string(),
            1 => titles[0].clone(),
            _ => format!("COALESCE({})", titles.join(", ")),
        };
        let evicted = if has(&memo_columns, EVICTION_COLUMN) {
            format!("m.{EVICTION_COLUMN} IS NOT NULL")
        } else {
            "0".to_string()
        };
        let folder_columns = columns(copy.conn(), FOLDERS).map_err(failed)?;
        let folder_name = FOLDER_NAME_COLUMNS.iter().find(|c| has(&folder_columns, c));
        let usable = has(&memo_columns, FOLDER_COLUMN) && has(&folder_columns, "Z_PK");
        let (folder, folder_join, folders) = match folder_name.filter(|_| usable) {
            Some(name) => {
                let id = if has(&folder_columns, FOLDER_ID_COLUMN) {
                    format!("COALESCE(NULLIF(f.{FOLDER_ID_COLUMN}, ''), 'pk:' || f.Z_PK)")
                } else {
                    "'pk:' || f.Z_PK".to_string()
                };
                (
                    format!("CASE WHEN f.Z_PK IS NULL THEN NULL ELSE {id} END"),
                    format!("LEFT JOIN {FOLDERS} f ON f.Z_PK = m.{FOLDER_COLUMN}"),
                    Some(format!(
                        "SELECT {id}, COALESCE(NULLIF(TRIM(f.{name}), ''), 'Folder') \
                         FROM {FOLDERS} f ORDER BY f.Z_PK"
                    )),
                )
            }
            None => ("NULL".to_string(), String::new(), None),
        };
        Ok(Snapshot {
            copy,
            title,
            evicted,
            folder,
            folder_join,
            folders,
        })
    }

    fn conn(&self) -> &Connection {
        self.copy.conn()
    }

    fn last_pk(&self) -> rusqlite::Result<i64> {
        self.conn().query_row(
            &format!("SELECT IFNULL(MAX(Z_PK), 0) FROM {MEMOS}"),
            [],
            |r| r.get(0),
        )
    }

    /// The highest memo number Core Data has given in this database (`Z_PRIMARYKEY.Z_MAX`),
    /// which never goes down when memos are removed; `None` without that table.
    fn given_pk(&self) -> rusqlite::Result<Option<i64>> {
        let keys = columns(self.conn(), "Z_PRIMARYKEY")?;
        if !has(&keys, "Z_MAX") || !has(&keys, "Z_NAME") {
            return Ok(None);
        }
        // The entity by name, or else by the entity number the memos carry.
        let by_ent = if has(&keys, "Z_ENT") && has(&columns(self.conn(), MEMOS)?, "Z_ENT") {
            format!(" OR Z_ENT IN (SELECT DISTINCT Z_ENT FROM {MEMOS})")
        } else {
            String::new()
        };
        let sql = format!(
            "SELECT Z_MAX FROM Z_PRIMARYKEY WHERE Z_NAME = ?1{by_ent} \
             ORDER BY Z_NAME = ?1 DESC LIMIT 1"
        );
        let mut stmt = self.conn().prepare(&sql)?;
        let mut rows = stmt.query([MEMO_ENTITY])?;
        match rows.next()? {
            Some(row) => row.get::<_, Option<i64>>(0),
            None => Ok(None),
        }
    }

    fn folders(&self) -> rusqlite::Result<Vec<Container>> {
        let Some(sql) = &self.folders else {
            return Ok(Vec::new());
        };
        let mut stmt = self.conn().prepare(sql)?;
        let rows = stmt.query_map([], |r| {
            Ok(Container::new(
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
            ))
        })?;
        rows.collect()
    }

    fn select(&self, filter: &str) -> String {
        format!(
            "SELECT m.Z_PK, m.ZUNIQUEID, m.ZDATE, m.ZDURATION, m.ZPATH, {}, {}, {} \
             FROM {MEMOS} m {} WHERE {filter} ORDER BY m.Z_PK",
            self.title, self.evicted, self.folder, self.folder_join
        )
    }

    fn rows(&self, sql: &str, value: i64) -> rusqlite::Result<Vec<MemoRow>> {
        let mut stmt = self.conn().prepare(sql)?;
        let rows = stmt.query_map([value], |r| {
            let date: Option<f64> = r.get(2)?;
            let duration: Option<f64> = r.get(3)?;
            Ok(MemoRow {
                pk: r.get(0)?,
                unique_id: r.get::<_, Option<String>>(1)?.unwrap_or_default(),
                started_at: unix_millis(date.unwrap_or(0.0)),
                duration_ms: (duration.unwrap_or(0.0).max(0.0) * 1000.0).round() as i64,
                path: r.get(4)?,
                title: r.get(5)?,
                evicted: r.get::<_, Option<bool>>(6)?.unwrap_or(false),
                folder: r.get(7)?,
            })
        })?;
        rows.collect()
    }

    /// The memos after number `after`, in order.
    fn memos_after(&self, after: i64) -> rusqlite::Result<Vec<MemoRow>> {
        self.rows(&self.select("m.Z_PK > ?1"), after)
    }

    /// The memo numbered `pk`, if the database still has it.
    fn memo(&self, pk: i64) -> rusqlite::Result<Option<MemoRow>> {
        Ok(self.rows(&self.select("m.Z_PK = ?1"), pk)?.pop())
    }
}

/// A Core Data time (seconds since 2001-01-01) in milliseconds since 1970.
fn unix_millis(date: f64) -> i64 {
    ((date + APPLE_EPOCH) * 1000.0).round() as i64
}

/// The title of a memo without one: `Voice memo, YYYY-MM-DD HH:MM`, in the Mac's time zone.
fn default_title(started_at: i64) -> String {
    let at = DateTime::from_timestamp_millis(started_at)
        .unwrap_or_default()
        .with_timezone(&Local);
    format!("Voice memo, {}", at.format("%Y-%m-%d %H:%M"))
}

/// A memo to store.
#[derive(Debug, Clone)]
struct Memo {
    title: String,
    started_at: i64,
    ended_at: i64,
    audio: PathBuf,
    key: String,
}

impl Memo {
    fn new(row: &MemoRow, audio: PathBuf) -> Memo {
        let title = row
            .title
            .as_deref()
            .map(|t| t.split_whitespace().collect::<Vec<_>>().join(" "))
            .filter(|t| !t.is_empty())
            .unwrap_or_else(|| default_title(row.started_at));
        Memo {
            title,
            started_at: row.started_at,
            ended_at: row.started_at + row.duration_ms,
            audio,
            key: format!("{KEY_PREFIX}{}", row.unique_id.trim()),
        }
    }
}

/// Memos read from Voice Memos, stored as recordings.
struct VoiceMemosBatch {
    memos: Vec<Memo>,
}

impl Batch for VoiceMemosBatch {
    fn len(&self) -> usize {
        self.memos.len()
    }

    fn write(self: Box<Self>, space: &mut Space, by: &Provenance) -> Result<Written, SourceError> {
        let summaries = recordings::summaries_enabled(space)?;
        let mut written = Written::default();
        for memo in self.memos {
            if recordings::find_by_key(space, &memo.key)?.is_some() {
                continue;
            }
            let new = NewRecording {
                kind: RecordingKind::VoiceMemo,
                title: memo.title,
                started_at: memo.started_at,
                ended_at: memo.ended_at,
                project: None,
                audio: vec![AudioInput {
                    path: memo.audio,
                    track: Track::Mixed,
                }],
                key: Some(memo.key),
                app: None,
                summaries,
            };
            recordings::create(space, &new, by)?;
            written.items += 1;
            written.created += 1;
        }
        Ok(written)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_core_data_times() {
        // 2026-11-22 10:30:00 UTC.
        assert_eq!(unix_millis(817_036_200.0), 1_795_343_400_000);
        assert_eq!(unix_millis(0.25), 978_307_200_250);
    }

    #[test]
    fn audio_stays_in_the_library() {
        let dir = Path::new("/Library/Recordings");
        assert_eq!(
            audio_path(dir, "20261122 103000-AB.m4a"),
            Some(dir.join("20261122 103000-AB.m4a"))
        );
        assert_eq!(
            audio_path(dir, "/Users/x/old/Recordings/memo.m4a"),
            Some(dir.join("memo.m4a"))
        );
        assert_eq!(audio_path(dir, "../CloudRecordings.db"), None);
        assert_eq!(audio_path(dir, "  "), None);
    }

    #[test]
    fn cursors_keep_the_memos_waited_for() {
        let state = State {
            pk: 7,
            waiting: vec![Waiting(5, 1000, 12, 900), Waiting(6, 1000, 0, 0)],
            evicted: vec![4],
            too_large: vec![3],
        };
        assert_eq!(State::read(Some(&state.write())), state);
        assert_eq!(
            state.write().as_str(),
            r#"{"pk":7,"waiting":[[5,1000,12,900],[6,1000,0,0]],"evicted":[4],"too_large":[3]}"#
        );
        // The format before the evicted memos and the file of those waited for were kept.
        assert_eq!(
            State::read(Some(&Cursor::new(
                r#"{"pk":7,"waiting":[[5,1000]],"too_large":[3]}"#
            ))),
            State {
                pk: 7,
                waiting: vec![Waiting(5, 1000, 0, 0)],
                evicted: vec![],
                too_large: vec![3],
            }
        );
        assert_eq!(State::read(Some(&Cursor::new("junk"))), State::default());
        assert_eq!(State::read(None), State::default());
        assert_eq!(State::default().write().as_str(), r#"{"pk":0}"#);
    }

    #[test]
    fn memos_without_a_title_are_named_by_their_date() {
        let title = default_title(1_795_343_400_000);
        assert!(title.starts_with("Voice memo, 2026-11-2"), "{title}");
    }
}
