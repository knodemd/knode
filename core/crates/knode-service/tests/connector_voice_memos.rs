//! The Voice Memos connector in a running service, over a synthetic library written here: a
//! `CloudRecordings.db` with the columns Voice Memos' has and a small fake audio file. No real
//! Voice Memos library is read.

use std::fs;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use knode_format::{Provenance, Space, Timestamp};
use knode_ingest::voice_memos::{ID, VoiceMemosConnector};
use knode_service::recordings::{RecordingKind, Track, TranscriptState};
use knode_service::sources::{History, SourcesOptions};
use knode_service::{Client, Config, start};
use rusqlite::Connection;

const APPLE_EPOCH: i64 = 978_307_200;

#[test]
fn the_service_brings_in_a_voice_memo_as_a_pending_recording() {
    let dir = tempfile::tempdir().unwrap();
    let library = dir.path().join("Recordings");
    fs::create_dir_all(&library).unwrap();
    let db = Connection::open(library.join("CloudRecordings.db")).unwrap();
    db.execute_batch(
        "PRAGMA journal_mode = WAL;
         CREATE TABLE ZCLOUDRECORDING (Z_PK INTEGER PRIMARY KEY, Z_ENT INTEGER, Z_OPT INTEGER, \
         ZFOLDER INTEGER, ZDATE TIMESTAMP, ZDURATION FLOAT, ZEVICTIONDATE TIMESTAMP, \
         ZCUSTOMLABEL VARCHAR, ZENCRYPTEDTITLE VARCHAR, ZPATH VARCHAR, ZUNIQUEID VARCHAR);",
    )
    .unwrap();
    let date = (Timestamp::now().0 / 1000 - APPLE_EPOCH) as f64;
    db.execute(
        "INSERT INTO ZCLOUDRECORDING (ZDATE, ZDURATION, ZENCRYPTEDTITLE, ZPATH, ZUNIQUEID) \
         VALUES (?1, 30.0, 'Idea for the talk', 'MEMO-1.m4a', 'MEMO-1')",
        [date],
    )
    .unwrap();
    fs::write(library.join("MEMO-1.m4a"), b"fake audio").unwrap();
    let connector = VoiceMemosConnector::new(&library);

    drop(Space::create(dir.path().join("Personal.knode"), "Personal").unwrap());
    let home = dir.path().join("home");
    fs::create_dir_all(&home).unwrap();
    let space_path = fs::canonicalize(dir.path().join("Personal.knode")).unwrap();
    fs::write(
        home.join("config.toml"),
        format!("default_space = {:?}\n", space_path.to_str().unwrap()),
    )
    .unwrap();
    let options = SourcesOptions {
        settings_poll: Duration::from_millis(50),
        quiet: Duration::from_millis(100),
        ..SourcesOptions::with_connectors(vec![Arc::new(connector)])
    };
    let service = start(Config::new(&home).with_sources(options)).unwrap();
    let client = Client::connect(home.join("service.sock")).unwrap();
    let listed = client.sources_list(&space_path).unwrap();
    let source = listed.iter().find(|s| s.info.id == ID).unwrap();
    assert_eq!(source.info.name, "Voice Memos");
    client
        .sources_connect(
            &space_path,
            ID,
            Some(History::FromToday),
            &Provenance::person("Example"),
        )
        .unwrap();

    let start = Instant::now();
    let pending = loop {
        let pending = client.pending_recordings(&space_path).unwrap();
        if !pending.is_empty() {
            break pending;
        }
        assert!(
            start.elapsed() < Duration::from_secs(20),
            "timed out waiting for the memo"
        );
        thread::sleep(Duration::from_millis(50));
    };
    assert_eq!(pending.len(), 1);
    let memo = &pending[0];
    assert_eq!(memo.kind, RecordingKind::VoiceMemo);
    assert_eq!(memo.title, "Idea for the talk");
    assert_eq!(memo.transcript, TranscriptState::Pending);
    assert_eq!(memo.key.as_deref(), Some("voice-memos:MEMO-1"));
    assert_eq!(memo.duration_ms, 30_000);
    assert_eq!(memo.tracks.len(), 1);
    assert_eq!(memo.tracks[0].track, Track::Mixed);

    let status = client
        .sources_list(&space_path)
        .unwrap()
        .into_iter()
        .find(|s| s.info.id == ID)
        .unwrap();
    assert!(status.last_error.is_none(), "{:?}", status.last_error);
    service.stop().unwrap();
}
