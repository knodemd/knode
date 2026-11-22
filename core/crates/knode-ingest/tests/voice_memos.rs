//! The Voice Memos connector over a synthetic library written here: a `CloudRecordings.db` with
//! the tables and columns Voice Memos' has (in versions with and without title columns and
//! folders) and small fake audio files. No real Voice Memos library is read.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, UNIX_EPOCH};

use knode_format::{Space, Timestamp};
use knode_ingest::recordings::{self, RecordingKind, SummaryState, Track, TranscriptState};
use knode_ingest::sources::{
    Connector, Container, Cursor, FetchRequest, History, NoSecrets, Permission, SourceError,
    Trigger, Written, provenance, start_of_today,
};
use knode_ingest::voice_memos::{ID, VoiceMemosConnector};
use rusqlite::Connection;
use tempfile::TempDir;

const APPLE_EPOCH: i64 = 978_307_200;
const DAY: i64 = 24 * 3600;

/// The memo table of a recent macOS: both title columns, eviction date and folder.
const FULL: &str = "CREATE TABLE ZCLOUDRECORDING (Z_PK INTEGER PRIMARY KEY, Z_ENT INTEGER, \
     Z_OPT INTEGER, ZFLAGS INTEGER, ZFOLDER INTEGER, ZDATE TIMESTAMP, ZDURATION FLOAT, \
     ZEVICTIONDATE TIMESTAMP, ZLOCALDURATION FLOAT, ZCUSTOMLABEL VARCHAR, \
     ZENCRYPTEDTITLE VARCHAR, ZPATH VARCHAR, ZUNIQUEID VARCHAR);
     CREATE TABLE Z_PRIMARYKEY (Z_ENT INTEGER PRIMARY KEY, Z_NAME VARCHAR, Z_SUPER INTEGER, \
     Z_MAX INTEGER);
     INSERT INTO Z_PRIMARYKEY VALUES (1, 'CloudRecording', 0, 0), (2, 'Folder', 0, 0);
     CREATE TABLE ZFOLDER (Z_PK INTEGER PRIMARY KEY, Z_ENT INTEGER, Z_OPT INTEGER, \
     ZRANK INTEGER, ZENCRYPTEDNAME VARCHAR, ZUUID VARCHAR);";

/// An older memo table: no title column, no eviction date, no folders.
const BARE: &str = "CREATE TABLE ZCLOUDRECORDING (Z_PK INTEGER PRIMARY KEY, Z_ENT INTEGER, \
     Z_OPT INTEGER, ZDATE TIMESTAMP, ZDURATION FLOAT, ZPATH VARCHAR, ZUNIQUEID VARCHAR);";

/// A synthetic `Recordings` folder.
struct Library {
    _dir: TempDir,
    root: PathBuf,
    /// Kept open, as Voice Memos keeps it: what it writes is in the `-wal`.
    db: Connection,
    full: bool,
}

impl Library {
    fn new() -> Library {
        Library::with_schema(FULL, true)
    }

    fn with_schema(schema: &str, full: bool) -> Library {
        let dir = tempfile::tempdir().unwrap();
        let root = dir
            .path()
            .join("Library/Group Containers/group.com.apple.VoiceMemos.shared/Recordings");
        fs::create_dir_all(&root).unwrap();
        let db = Connection::open(root.join("CloudRecordings.db")).unwrap();
        db.execute_batch(&format!("PRAGMA journal_mode = WAL; {schema}"))
            .unwrap();
        Library {
            _dir: dir,
            root,
            db,
            full,
        }
    }

    fn connector(&self) -> VoiceMemosConnector {
        VoiceMemosConnector::new(&self.root)
    }

    fn folder(&self, name: &str, uuid: &str) -> i64 {
        self.db
            .execute(
                "INSERT INTO ZFOLDER (ZENCRYPTEDNAME, ZUUID) VALUES (?1, ?2)",
                [name, uuid],
            )
            .unwrap();
        self.db.last_insert_rowid()
    }

    /// Lists a memo recorded at `at` (Unix seconds); its file is written only with `write`.
    fn list(&self, uid: &str, at: i64, title: Option<&str>, folder: Option<i64>) -> i64 {
        let path = format!("{uid}.m4a");
        let date = (at - APPLE_EPOCH) as f64;
        if self.full {
            self.db
                .execute(
                    "INSERT INTO ZCLOUDRECORDING (Z_PK, ZDATE, ZDURATION, ZENCRYPTEDTITLE, \
                     ZPATH, ZUNIQUEID, ZFOLDER) VALUES ((SELECT Z_MAX + 1 FROM Z_PRIMARYKEY \
                     WHERE Z_NAME = 'CloudRecording'), ?1, 12.5, ?2, ?3, ?4, ?5)",
                    rusqlite::params![date, title, path, uid, folder],
                )
                .unwrap();
        } else {
            self.db
                .execute(
                    "INSERT INTO ZCLOUDRECORDING (ZDATE, ZDURATION, ZPATH, ZUNIQUEID) \
                     VALUES (?1, 12.5, ?2, ?3)",
                    rusqlite::params![date, path, uid],
                )
                .unwrap();
        }
        let pk = self.db.last_insert_rowid();
        if self.full {
            // Core Data gives each memo the number after the highest it gave, and counts it.
            self.db
                .execute(
                    "UPDATE Z_PRIMARYKEY SET Z_MAX = MAX(Z_MAX, ?1) WHERE Z_NAME = 'CloudRecording'",
                    [pk],
                )
                .unwrap();
        }
        pk
    }

    fn file(&self, uid: &str) -> PathBuf {
        self.root.join(format!("{uid}.m4a"))
    }

    /// Writes a memo's file, last changed a minute ago: done being written.
    fn write(&self, uid: &str) {
        fs::write(self.file(uid), format!("audio {uid}")).unwrap();
        self.touch(uid, now() - 60);
    }

    /// Sets when a memo's file last changed (Unix seconds).
    fn touch(&self, uid: &str, at: i64) {
        let file = fs::File::options()
            .write(true)
            .open(self.file(uid))
            .unwrap();
        file.set_modified(UNIX_EPOCH + Duration::from_secs(at as u64))
            .unwrap();
    }

    /// Removes a memo from the database, as emptying Recently Deleted does.
    fn purge(&self, pk: i64) {
        self.db
            .execute("DELETE FROM ZCLOUDRECORDING WHERE Z_PK = ?1", [pk])
            .unwrap();
    }

    fn restore(&self, pk: i64) {
        self.db
            .execute(
                "UPDATE ZCLOUDRECORDING SET ZEVICTIONDATE = NULL WHERE Z_PK = ?1",
                [pk],
            )
            .unwrap();
    }

    fn add(&self, uid: &str, at: i64, title: Option<&str>) -> i64 {
        let pk = self.list(uid, at, title, None);
        self.write(uid);
        pk
    }

    fn evict(&self, pk: i64) {
        self.db
            .execute(
                "UPDATE ZCLOUDRECORDING SET ZEVICTIONDATE = 800000000 WHERE Z_PK = ?1",
                [pk],
            )
            .unwrap();
    }
}

fn now() -> i64 {
    Timestamp::now().0 / 1000
}

fn long_ago() -> i64 {
    start_of_today().0 / 1000 - 400 * DAY
}

fn space(dir: &Path) -> Space {
    Space::create(dir.join("Personal.knode"), "Personal").unwrap()
}

/// Two runs of the service, as a memo's file is brought in only once it is the same in two
/// runs. The fetches are those of the second run.
fn run(
    connector: &VoiceMemosConnector,
    space: &mut Space,
    cursor: &mut Option<Cursor>,
    history: History,
    excluded: &[Container],
    max_items: usize,
) -> Result<(Written, usize), SourceError> {
    let (mut written, _) = run_once(connector, space, cursor, history, excluded, max_items)?;
    let (second, fetches) = run_once(connector, space, cursor, history, excluded, max_items)?;
    written += second;
    Ok((written, fetches))
}

/// What the service does in one run: check, then fetch and write until there is no more.
fn run_once(
    connector: &VoiceMemosConnector,
    space: &mut Space,
    cursor: &mut Option<Cursor>,
    history: History,
    excluded: &[Container],
    max_items: usize,
) -> Result<(Written, usize), SourceError> {
    connector.check()?;
    let mut written = Written::default();
    let mut fetches = 0;
    loop {
        let request = FetchRequest {
            space_id: "space",
            cursor: cursor.as_ref(),
            history,
            since: start_of_today(),
            excluded,
            max_items,
            secrets: &NoSecrets,
        };
        let fetched = connector.fetch(&request)?;
        fetches += 1;
        if let Some(batch) = fetched.batch {
            assert!(batch.len() <= max_items);
            written += batch.write(space, &provenance(ID))?;
        }
        *cursor = Some(fetched.cursor);
        if !fetched.more {
            return Ok((written, fetches));
        }
    }
}

fn titles(space: &Space) -> Vec<String> {
    let mut titles: Vec<String> = space
        .recent(1000)
        .unwrap()
        .into_iter()
        .filter(|n| n.kind == recordings::VOICE_MEMO_KIND)
        .map(|n| n.title)
        .collect();
    titles.sort();
    titles
}

#[test]
fn from_today_brings_only_memos_recorded_since_the_connect_day() {
    let lib = Library::new();
    lib.add("OLD-1", long_ago(), Some("Old idea"));
    let at = now();
    let pk = lib.add("NEW-1", at, Some("  Groceries   list "));
    let dir = tempfile::tempdir().unwrap();
    let mut space = space(dir.path());
    let mut cursor = None;
    let (written, _) = run(
        &lib.connector(),
        &mut space,
        &mut cursor,
        History::FromToday,
        &[],
        10,
    )
    .unwrap();
    assert_eq!(written.created, 1);
    assert_eq!(titles(&space), ["Groceries list"]);
    assert!(cursor.unwrap().as_str().contains(&format!("\"pk\":{pk}")));

    let id = recordings::find_by_key(&space, "voice-memos:NEW-1")
        .unwrap()
        .unwrap();
    let rec = recordings::get(&space, &id).unwrap();
    assert_eq!(rec.kind, RecordingKind::VoiceMemo);
    assert_eq!(rec.started_at, at * 1000);
    assert_eq!(rec.ended_at, at * 1000 + 12_500);
    assert_eq!(rec.transcript, TranscriptState::Pending);
    assert_eq!(rec.summary, SummaryState::Pending);
    assert_eq!(rec.tracks.len(), 1);
    assert_eq!(rec.tracks[0].name, "NEW-1.m4a");
    assert_eq!(rec.tracks[0].track, Track::Mixed);
    let node = space.get_node(&id).unwrap().unwrap();
    assert_eq!(node.created_by.actor, ID);
}

#[test]
fn everything_brings_the_history_and_untitled_memos_are_named_by_date() {
    let lib = Library::new();
    lib.add("OLD-1", long_ago(), Some("Old idea"));
    lib.add("NEW-1", now(), None);
    let dir = tempfile::tempdir().unwrap();
    let mut space = space(dir.path());
    let mut cursor = None;
    run(
        &lib.connector(),
        &mut space,
        &mut cursor,
        History::Everything,
        &[],
        10,
    )
    .unwrap();
    let titles = titles(&space);
    assert_eq!(titles.len(), 2);
    assert!(titles.contains(&"Old idea".to_string()));
    assert!(
        titles.iter().any(|t| t.starts_with("Voice memo, 20")),
        "{titles:?}"
    );
}

#[test]
fn a_rerun_or_a_lost_cursor_adds_nothing_twice() {
    let lib = Library::new();
    lib.add("A", now(), Some("One"));
    lib.add("B", now(), Some("Two"));
    let connector = lib.connector();
    let dir = tempfile::tempdir().unwrap();
    let mut space = space(dir.path());
    let mut cursor = None;
    run(
        &connector,
        &mut space,
        &mut cursor,
        History::Everything,
        &[],
        10,
    )
    .unwrap();
    let (again, _) = run(
        &connector,
        &mut space,
        &mut cursor,
        History::Everything,
        &[],
        10,
    )
    .unwrap();
    assert_eq!(again, Written::default());
    let mut lost = None;
    let (lost_run, _) = run(
        &connector,
        &mut space,
        &mut lost,
        History::Everything,
        &[],
        10,
    )
    .unwrap();
    assert_eq!(lost_run.created, 0);
    assert_eq!(titles(&space), ["One", "Two"]);

    // A new memo after that comes in alone.
    lib.add("C", now(), Some("Three"));
    let (next, _) = run(
        &connector,
        &mut space,
        &mut cursor,
        History::Everything,
        &[],
        10,
    )
    .unwrap();
    assert_eq!(next.created, 1);
    assert_eq!(titles(&space), ["One", "Three", "Two"]);
}

#[test]
fn memos_come_in_batches() {
    let lib = Library::new();
    for i in 0..5 {
        lib.add(&format!("M{i}"), now(), Some(&format!("Memo {i}")));
    }
    let dir = tempfile::tempdir().unwrap();
    let mut space = space(dir.path());
    let mut cursor = None;
    let (written, fetches) = run(
        &lib.connector(),
        &mut space,
        &mut cursor,
        History::Everything,
        &[],
        2,
    )
    .unwrap();
    assert_eq!(written.created, 5);
    assert_eq!(fetches, 3);
    assert_eq!(titles(&space).len(), 5);
}

#[test]
fn memos_in_recently_deleted_are_left_out() {
    let lib = Library::new();
    let gone = lib.add("GONE", now(), Some("Deleted"));
    lib.evict(gone);
    lib.add("KEPT", now(), Some("Kept"));
    let dir = tempfile::tempdir().unwrap();
    let mut space = space(dir.path());
    let mut cursor = None;
    run(
        &lib.connector(),
        &mut space,
        &mut cursor,
        History::Everything,
        &[],
        10,
    )
    .unwrap();
    assert_eq!(titles(&space), ["Kept"]);
}

#[test]
fn a_memo_still_in_icloud_is_waited_for_then_brought_in() {
    let lib = Library::new();
    lib.list("CLOUD", now(), Some("From the iPhone"), None);
    lib.add("HERE", now(), Some("On the Mac"));
    let connector = lib.connector();
    let dir = tempfile::tempdir().unwrap();
    let mut space = space(dir.path());
    let mut cursor = None;
    run(
        &connector,
        &mut space,
        &mut cursor,
        History::Everything,
        &[],
        10,
    )
    .unwrap();
    assert_eq!(titles(&space), ["On the Mac"]);
    let waiting = cursor.clone().unwrap();
    assert!(connector.waiting(&waiting));
    // Not a day yet: nothing to report.
    assert_eq!(connector.notice(&waiting), None);

    // Still not there: still waited for.
    run(
        &connector,
        &mut space,
        &mut cursor,
        History::Everything,
        &[],
        10,
    )
    .unwrap();
    assert!(connector.waiting(cursor.as_ref().unwrap()));

    lib.write("CLOUD");
    let (written, _) = run(
        &connector,
        &mut space,
        &mut cursor,
        History::Everything,
        &[],
        10,
    )
    .unwrap();
    assert_eq!(written.created, 1);
    assert_eq!(titles(&space), ["From the iPhone", "On the Mac"]);
    assert!(!connector.waiting(cursor.as_ref().unwrap()));
}

#[test]
fn memos_waited_for_more_than_a_day_are_reported() {
    let lib = Library::new();
    let connector = lib.connector();
    let old = Timestamp::now().0 - 2 * DAY * 1000;
    let cursor = Cursor::new(format!(r#"{{"pk":3,"waiting":[[2,{old}],[3,{old}]]}}"#));
    assert!(connector.waiting(&cursor));
    let notice = connector.notice(&cursor).unwrap();
    assert!(notice.starts_with("2 voice memos"), "{notice}");
    assert!(notice.contains("iCloud"), "{notice}");
}

#[test]
fn excluded_folders_are_never_read() {
    let lib = Library::new();
    let work = lib.folder("Work", "F-WORK");
    let home = lib.folder("Home", "F-HOME");
    lib.list("W", now(), Some("Work memo"), Some(work));
    lib.write("W");
    lib.list("H", now(), Some("Home memo"), Some(home));
    lib.write("H");
    lib.add("N", now(), Some("No folder"));
    let connector = lib.connector();
    let dir = tempfile::tempdir().unwrap();
    let mut space = space(dir.path());
    let request = FetchRequest {
        space_id: "space",
        cursor: None,
        history: History::Everything,
        since: start_of_today(),
        excluded: &[],
        max_items: 10,
        secrets: &NoSecrets,
    };
    assert_eq!(
        connector.containers(&request).unwrap(),
        [
            Container::new("F-HOME", "Home"),
            Container::new("F-WORK", "Work")
        ]
    );
    let mut cursor = None;
    run(
        &connector,
        &mut space,
        &mut cursor,
        History::Everything,
        &[Container::new("F-WORK", "Work")],
        10,
    )
    .unwrap();
    assert_eq!(titles(&space), ["Home memo", "No folder"]);
}

#[test]
fn a_library_without_title_columns_or_folders_still_reads() {
    let lib = Library::with_schema(BARE, false);
    lib.add("BARE-1", now(), None);
    let connector = lib.connector();
    let dir = tempfile::tempdir().unwrap();
    let mut space = space(dir.path());
    let mut cursor = None;
    let (written, _) = run(
        &connector,
        &mut space,
        &mut cursor,
        History::Everything,
        &[],
        10,
    )
    .unwrap();
    assert_eq!(written.created, 1);
    let titles = titles(&space);
    assert!(titles[0].starts_with("Voice memo, "), "{titles:?}");
    let request = FetchRequest {
        space_id: "space",
        cursor: None,
        history: History::Everything,
        since: start_of_today(),
        excluded: &[],
        max_items: 10,
        secrets: &NoSecrets,
    };
    assert!(connector.containers(&request).unwrap().is_empty());
}

#[test]
fn summaries_follow_the_space_setting() {
    let lib = Library::new();
    lib.add("S", now(), Some("Quiet"));
    let dir = tempfile::tempdir().unwrap();
    let mut space = space(dir.path());
    space
        .set_setting(
            recordings::SUMMARIES_SETTING,
            Some("off"),
            &knode_format::Provenance::person("Ada"),
        )
        .unwrap();
    let mut cursor = None;
    run(
        &lib.connector(),
        &mut space,
        &mut cursor,
        History::Everything,
        &[],
        10,
    )
    .unwrap();
    let id = recordings::find_by_key(&space, "voice-memos:S")
        .unwrap()
        .unwrap();
    assert_eq!(
        recordings::get(&space, &id).unwrap().summary,
        SummaryState::Off
    );
}

#[test]
fn a_changed_database_writes_nothing_and_needs_an_update() {
    let lib = Library::with_schema(
        "CREATE TABLE ZCLOUDRECORDING (Z_PK INTEGER PRIMARY KEY, ZDATE TIMESTAMP, \
         ZDURATION FLOAT, ZUNIQUEID VARCHAR);",
        false,
    );
    let err = lib.connector().check().unwrap_err();
    assert_eq!(err.kind(), "needs-update");
    assert!(err.to_string().contains("ZCLOUDRECORDING.ZPATH"), "{err}");
}

#[test]
fn without_a_library_it_is_unavailable() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("Recordings");
    let connector = VoiceMemosConnector::new(&missing);
    let err = connector.check().unwrap_err();
    assert_eq!(err.kind(), "unavailable");
    assert!(err.to_string().contains("Voice Memos"), "{err}");
    // A folder without its database.
    fs::create_dir_all(&missing).unwrap();
    assert_eq!(connector.check().unwrap_err().kind(), "unavailable");

    let info = connector.info();
    assert_eq!(info.id, "voice-memos");
    assert_eq!(info.name, "Voice Memos");
    assert_eq!(info.permissions, vec![Permission::FullDiskAccess]);
    assert_eq!(info.container.as_deref(), Some("folder"));
    match connector.trigger() {
        Trigger::Watch { paths, .. } => {
            assert!(paths[0].ends_with("Recordings/CloudRecordings.db"));
            assert!(paths[1].ends_with("Recordings/CloudRecordings.db-wal"));
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn without_full_disk_access_it_says_how_to_allow_it() {
    let lib = Library::new();
    let db = lib.root.join("CloudRecordings.db");
    use std::os::unix::fs::PermissionsExt;
    let mut perms = fs::metadata(&db).unwrap().permissions();
    perms.set_mode(0o000);
    fs::set_permissions(&db, perms.clone()).unwrap();
    let refused = fs::read(&db).is_err();
    let result = lib.connector().check();
    perms.set_mode(0o644);
    fs::set_permissions(&db, perms).unwrap();
    if !refused {
        // Running as a user every file is open to: nothing to test.
        return;
    }
    let err = result.unwrap_err();
    assert!(matches!(
        err,
        SourceError::Permission(Permission::FullDiskAccess, _)
    ));
    assert!(err.to_string().contains("Full Disk Access"), "{err}");
}

#[test]
fn a_rebuilt_database_starts_the_cursor_again_without_duplicates() {
    let lib = Library::new();
    lib.add("A", now(), Some("One"));
    let connector = lib.connector();
    let dir = tempfile::tempdir().unwrap();
    let mut space = space(dir.path());
    // A cursor from a database with more memos than this one has now.
    let mut cursor = Some(Cursor::new(r#"{"pk":99}"#));
    run(
        &connector,
        &mut space,
        &mut cursor,
        History::Everything,
        &[],
        10,
    )
    .unwrap();
    assert_eq!(titles(&space), ["One"]);
    run(
        &connector,
        &mut space,
        &mut cursor,
        History::Everything,
        &[],
        10,
    )
    .unwrap();
    assert_eq!(titles(&space), ["One"]);
}

#[test]
fn purging_the_newest_memo_does_not_bring_back_memos_deleted_in_the_space() {
    let lib = Library::new();
    lib.add("A", now(), Some("One"));
    let newest = lib.add("B", now(), Some("Two"));
    let connector = lib.connector();
    let dir = tempfile::tempdir().unwrap();
    let mut space = space(dir.path());
    let mut cursor = None;
    let everything = History::Everything;
    run(&connector, &mut space, &mut cursor, everything, &[], 10).unwrap();
    assert_eq!(titles(&space), ["One", "Two"]);
    // The person deletes "One" in the space, and empties Recently Deleted of "Two".
    let one = recordings::find_by_key(&space, "voice-memos:A")
        .unwrap()
        .unwrap();
    space
        .delete_node(&one, &knode_format::Provenance::person("Ada"))
        .unwrap();
    lib.purge(newest);
    let (written, _) = run(&connector, &mut space, &mut cursor, everything, &[], 10).unwrap();
    assert_eq!(written.created, 0);
    assert_eq!(titles(&space), ["Two"]);
    // A memo recorded after that still comes in.
    lib.add("C", now(), Some("Three"));
    run(&connector, &mut space, &mut cursor, everything, &[], 10).unwrap();
    assert_eq!(titles(&space), ["Three", "Two"]);
}

#[test]
fn a_memo_restored_from_recently_deleted_comes_in() {
    let lib = Library::new();
    let gone = lib.add("GONE", now(), Some("Restored"));
    lib.evict(gone);
    let purged = lib.add("PURGED", now(), Some("Purged"));
    lib.evict(purged);
    lib.add("KEPT", now(), Some("Kept"));
    let connector = lib.connector();
    let dir = tempfile::tempdir().unwrap();
    let mut space = space(dir.path());
    let mut cursor = None;
    let everything = History::Everything;
    run(&connector, &mut space, &mut cursor, everything, &[], 10).unwrap();
    assert_eq!(titles(&space), ["Kept"]);

    lib.restore(gone);
    lib.purge(purged);
    let (written, _) = run(&connector, &mut space, &mut cursor, everything, &[], 10).unwrap();
    assert_eq!(written.created, 1);
    assert_eq!(titles(&space), ["Kept", "Restored"]);
    // Nothing is looked at again: the restored memo is in, the purged one is gone.
    let state = cursor.unwrap();
    assert!(!state.as_str().contains("evicted"), "{}", state.as_str());
    assert!(!connector.waiting(&state));
}

#[test]
fn a_file_still_being_written_is_brought_in_once_it_settles() {
    let lib = Library::new();
    lib.list("REC", now(), Some("Still recording"), None);
    fs::write(lib.file("REC"), "partial").unwrap();
    let connector = lib.connector();
    let dir = tempfile::tempdir().unwrap();
    let mut space = space(dir.path());
    let mut cursor = None;
    let everything = History::Everything;
    // Just written: not brought in, however many runs see it.
    run(&connector, &mut space, &mut cursor, everything, &[], 10).unwrap();
    assert!(titles(&space).is_empty());
    assert!(connector.waiting(cursor.as_ref().unwrap()));

    // Quiet for a while, but larger than in the run before: still waited for.
    fs::write(lib.file("REC"), "partial and the rest").unwrap();
    lib.touch("REC", now() - 60);
    run_once(&connector, &mut space, &mut cursor, everything, &[], 10).unwrap();
    assert!(titles(&space).is_empty());

    // The same in two runs: brought in, whole.
    let (written, _) = run_once(&connector, &mut space, &mut cursor, everything, &[], 10).unwrap();
    assert_eq!(written.created, 1);
    let id = recordings::find_by_key(&space, "voice-memos:REC")
        .unwrap()
        .unwrap();
    let rec = recordings::get(&space, &id).unwrap();
    assert_eq!(rec.tracks.len(), 1);
    assert!(!connector.waiting(cursor.as_ref().unwrap()));
    // Nothing is reported as late: it was never in iCloud.
    assert_eq!(connector.notice(cursor.as_ref().unwrap()), None);
}

#[test]
fn cursors_of_the_earlier_format_still_read() {
    let lib = Library::new();
    lib.list("CLOUD", now(), Some("From the iPhone"), None);
    lib.write("CLOUD");
    let connector = lib.connector();
    let dir = tempfile::tempdir().unwrap();
    let mut space = space(dir.path());
    let mut cursor = Some(Cursor::new(format!(
        r#"{{"pk":1,"waiting":[[1,{}]],"too_large":[7]}}"#,
        Timestamp::now().0
    )));
    assert!(connector.waiting(cursor.as_ref().unwrap()));
    let (written, _) = run(
        &connector,
        &mut space,
        &mut cursor,
        History::Everything,
        &[],
        10,
    )
    .unwrap();
    assert_eq!(written.created, 1);
    assert_eq!(titles(&space), ["From the iPhone"]);
    assert!(cursor.unwrap().as_str().contains("\"too_large\":[7]"));
}
