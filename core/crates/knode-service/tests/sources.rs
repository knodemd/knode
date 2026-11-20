//! Sources in the service, with a fake connector over synthetic messages: the cursor, the
//! history chosen, exclusions, a restart without duplicates, backoff after errors, the schema
//! check, the file watcher and the status `sources.*` reports.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use knode_format::{ActorKind, NewNode, Properties, Provenance, Space, Timestamp};
use knode_service::sources::{
    Batch, Connector, Container, Cursor, FetchRequest, Fetched, History, Permission, SourceError,
    SourceInfo, SourceKind, SourceState, SourceStatus, SourcesOptions, Trigger, Written,
};
use knode_service::{Client, Config, ServiceHandle, start};
use tempfile::TempDir;

const WAIT: Duration = Duration::from_secs(10);
const DAY: i64 = 24 * 3600 * 1000;
const ID: &str = "fake";
const KEY: &str = "fake.id";

/// One synthetic message of the fake service.
#[derive(Debug, Clone)]
struct Message {
    seq: u64,
    chat: &'static str,
    at: Timestamp,
    text: String,
}

/// The fake service and what the connector saw.
#[derive(Debug, Default)]
struct Store {
    messages: Vec<Message>,
    /// How many of the next fetches fail.
    fail: u32,
    /// Whether the schema check fails.
    changed_schema: bool,
    fetches: usize,
    /// When each fetch was made.
    times: Vec<Instant>,
    /// The cursor and the excluded ids of every fetch.
    requests: Vec<(Option<String>, Vec<String>)>,
    /// A synthetic database each fetch reads a copy of, as connectors to local apps do.
    reads: Option<PathBuf>,
    /// How long listing the containers takes, as over the network.
    slow_containers: Duration,
    /// How long each fetch takes, as over the network or a big database.
    slow_fetch: Duration,
    /// Whether it waits for something its watched files do not show.
    waiting: bool,
}

#[derive(Clone)]
struct Fake {
    store: Arc<Mutex<Store>>,
    trigger: Trigger,
}

impl Fake {
    fn store(&self) -> std::sync::MutexGuard<'_, Store> {
        self.store.lock().unwrap()
    }

    fn add(&self, chat: &'static str, at: Timestamp, text: &str) {
        let mut store = self.store();
        let seq = store.messages.len() as u64 + 1;
        store.messages.push(Message {
            seq,
            chat,
            at,
            text: text.to_string(),
        });
    }
}

impl Connector for Fake {
    fn info(&self) -> SourceInfo {
        SourceInfo::connector(ID, "Fake", "Synthetic messages")
            .needing([Permission::FullDiskAccess])
            .excluding("chat")
    }

    fn trigger(&self) -> Trigger {
        self.trigger.clone()
    }

    fn check(&self) -> Result<(), SourceError> {
        if self.store().changed_schema {
            return Err(SourceError::NeedsUpdate("table message is missing".into()));
        }
        Ok(())
    }

    fn containers(&self, _request: &FetchRequest<'_>) -> Result<Vec<Container>, SourceError> {
        let slow = self.store().slow_containers;
        thread::sleep(slow);
        Ok(vec![
            Container::new("family", "Family"),
            Container::new("work", "Work"),
        ])
    }

    fn waiting(&self, _cursor: &Cursor) -> bool {
        self.store().waiting
    }

    fn fetch(&self, request: &FetchRequest<'_>) -> Result<Fetched, SourceError> {
        let slow = {
            let mut store = self.store();
            store.fetches += 1;
            store.times.push(Instant::now());
            store.requests.push((
                request.cursor.map(|c| c.as_str().to_string()),
                request.excluded.iter().map(|c| c.id.clone()).collect(),
            ));
            store.slow_fetch
        };
        thread::sleep(slow);
        let mut store = self.store();
        if let Some(db) = &store.reads {
            read_like_macos(db);
        }
        if store.fail > 0 {
            store.fail -= 1;
            return Err(SourceError::Failed("the fake service is down".into()));
        }
        let after: u64 = request.cursor.map_or(0, |c| c.as_str().parse().unwrap());
        let newer: Vec<&Message> = store.messages.iter().filter(|m| m.seq > after).collect();
        let Some(last) = newer
            .iter()
            .take(request.max_items)
            .next_back()
            .map(|m| m.seq)
        else {
            return Ok(Fetched::nothing(Cursor::new(after.to_string())));
        };
        let more = newer.len() > request.max_items;
        let items: Vec<Message> = newer
            .into_iter()
            .take(request.max_items)
            .filter(|m| !request.is_excluded(m.chat) && request.wants(m.at))
            .cloned()
            .collect();
        Ok(Fetched::batch(FakeBatch(items), Cursor::new(last.to_string())).with_more(more))
    }
}

/// Reads a copy of the database at `db` and leaves behind the file events macOS reports for
/// such a read, on every system: a file beside it appears and goes (as its `-shm` does, which
/// the app holds mapped and is left alone here), and its metadata is touched (its permissions
/// are set again, which changes neither its size nor its modification time).
fn read_like_macos(db: &Path) {
    drop(knode_service::sources::sqlite::open_copy(db).unwrap());
    let beside = PathBuf::from(format!("{}-read", db.display()));
    fs::write(&beside, b"").unwrap();
    fs::remove_file(&beside).unwrap();
    let permissions = fs::metadata(db).unwrap().permissions();
    fs::set_permissions(db, permissions).unwrap();
}

/// A synthetic database in WAL mode, kept open by the "app", which writes to it with the
/// connection returned.
fn synthetic_db(dir: &Path) -> (PathBuf, rusqlite::Connection) {
    let db = dir.join("Synthetic.sqlite");
    let conn = rusqlite::Connection::open(&db).unwrap();
    conn.execute_batch(
        "PRAGMA journal_mode = WAL;
         CREATE TABLE message (id INTEGER PRIMARY KEY, text TEXT);",
    )
    .unwrap();
    (db, conn)
}

/// Messages to write: one node per message, found again by its id, as importers do.
struct FakeBatch(Vec<Message>);

impl Batch for FakeBatch {
    fn len(&self) -> usize {
        self.0.len()
    }

    fn write(self: Box<Self>, space: &mut Space, by: &Provenance) -> Result<Written, SourceError> {
        let mut written = Written::default();
        for m in self.0 {
            let id = format!("{}/{}", m.chat, m.seq);
            if !space.find_by_property(KEY, &id)?.is_empty() {
                continue;
            }
            let properties = Properties::from([(KEY.to_string(), id)]);
            let node = NewNode::note(format!("{} {}", m.chat, m.seq), m.text);
            space.create_node_with_properties(node, &properties, by)?;
            written.items += 1;
            written.created += 1;
        }
        Ok(written)
    }
}

struct Fixture {
    dir: TempDir,
    fake: Fake,
    service: Option<ServiceHandle>,
    tune: fn(&mut SourcesOptions),
}

impl Fixture {
    fn new() -> Self {
        Self::with_trigger(Trigger::Interval(Duration::from_millis(100)))
    }

    fn with_trigger(trigger: Trigger) -> Self {
        Self::with_options(trigger, |_| {})
    }

    /// With the options of the tests changed by `tune`.
    fn with_options(trigger: Trigger, tune: fn(&mut SourcesOptions)) -> Self {
        let dir = tempfile::tempdir().unwrap();
        drop(Space::create(dir.path().join("Personal.knode"), "Personal").unwrap());
        let fake = Fake {
            store: Arc::new(Mutex::new(Store::default())),
            trigger,
        };
        let mut fixture = Fixture {
            dir,
            fake,
            service: None,
            tune,
        };
        fixture.write_config("");
        fixture.start();
        fixture
    }

    fn start(&mut self) {
        let mut options = SourcesOptions {
            settings_poll: Duration::from_millis(50),
            backoff: Duration::from_millis(200),
            max_backoff: Duration::from_millis(800),
            quiet: Duration::from_millis(50),
            max_items: 3,
            ..SourcesOptions::with_connectors(vec![Arc::new(self.fake.clone())])
        };
        (self.tune)(&mut options);
        let config = Config::new(self.home()).with_sources(options);
        self.service = Some(start(config).unwrap());
    }

    fn stop(&mut self) {
        self.service.take().unwrap().stop().unwrap();
    }

    fn home(&self) -> PathBuf {
        self.dir.path().join("home")
    }

    fn space(&self) -> PathBuf {
        fs::canonicalize(self.dir.path().join("Personal.knode")).unwrap()
    }

    fn write_config(&self, extra: &str) {
        fs::create_dir_all(self.home()).unwrap();
        let space = fs::canonicalize(self.dir.path().join("Personal.knode")).unwrap();
        let text = format!("default_space = {:?}\n{extra}", space.to_str().unwrap());
        fs::write(self.home().join("config.toml"), text).unwrap();
    }

    fn client(&self) -> Client {
        Client::connect(self.home().join("service.sock")).unwrap()
    }

    fn status(&self) -> SourceStatus {
        self.client()
            .sources_list(self.space())
            .unwrap()
            .into_iter()
            .find(|s| s.info.id == ID)
            .unwrap()
    }

    /// The ids of the messages written, sorted.
    fn written(&self) -> Vec<String> {
        let client = self.client();
        let mut ids: Vec<String> = client
            .recent(self.space(), 1000)
            .unwrap()
            .into_iter()
            .filter(|n| n.updated_by.actor == ID)
            .map(|n| n.title)
            .collect();
        ids.sort();
        ids
    }

    fn wait_until(&self, what: &str, done: impl Fn(&Fixture) -> bool) {
        let start = Instant::now();
        while !done(self) {
            assert!(start.elapsed() < WAIT, "timed out waiting for {what}");
            thread::sleep(Duration::from_millis(20));
        }
    }
}

fn me() -> Provenance {
    Provenance::person("Example")
}

fn yesterday() -> Timestamp {
    Timestamp(knode_service::sources::start_of_today().0 - DAY / 2)
}

#[test]
fn importers_and_connectors_are_listed_and_nothing_runs_until_connected() {
    let f = Fixture::new();
    f.fake.add("family", Timestamp::now(), "Hello");
    let list = f.client().sources_list(f.space()).unwrap();
    let fake = list.iter().find(|s| s.info.id == ID).unwrap();
    assert_eq!(fake.info.kind, SourceKind::Connector);
    assert_eq!(fake.state, SourceState::Off);
    assert_eq!(fake.info.permissions, vec![Permission::FullDiskAccess]);
    let importers: Vec<&SourceStatus> = list
        .iter()
        .filter(|s| s.info.kind == SourceKind::Importer)
        .collect();
    assert!(importers.iter().any(|s| s.info.id == "whatsapp"));
    assert!(importers.iter().all(|s| s.state == SourceState::Importer));
    // Importers have nothing to connect.
    assert!(
        f.client()
            .sources_connect(f.space(), "pdf", None, &me())
            .is_err()
    );
    thread::sleep(Duration::from_millis(300));
    assert_eq!(f.fake.store().fetches, 0);
    assert!(f.client().sources_status(None).unwrap().is_empty());
}

#[test]
fn from_today_reads_only_what_is_new_and_the_cursor_moves_on() {
    let f = Fixture::new();
    f.fake.add("family", yesterday(), "Old");
    f.fake.add("family", Timestamp::now(), "One");
    f.fake.add("work", Timestamp::now(), "Two");
    f.fake.add("work", Timestamp::now(), "Three");
    let connected = f
        .client()
        .sources_connect(f.space(), ID, None, &me())
        .unwrap();
    assert_eq!(
        connected.setting.as_ref().unwrap().history,
        History::FromToday
    );
    f.wait_until("three messages", |f| f.written().len() == 3);
    assert_eq!(f.written(), vec!["family 2", "work 3", "work 4"]);
    f.wait_until("the status", |f| f.status().items == 3);
    let status = f.status();
    assert!(matches!(
        status.state,
        SourceState::Idle | SourceState::Running
    ));
    assert!(status.last_success.is_some());
    // Four messages, three a batch: the second batch started from the first's cursor.
    let requests = f.fake.store().requests.clone();
    assert_eq!(requests[0].0, None);
    assert_eq!(requests[1].0.as_deref(), Some("3"));

    f.fake.add("family", Timestamp::now(), "Four");
    f.wait_until("the new message", |f| f.written().len() == 4);
    assert_eq!(f.status().items, 4);
    // Every node names the source.
    let client = f.client();
    let node = client.recent(f.space(), 1).unwrap().remove(0);
    let node = client.get_node(f.space(), &node.id).unwrap().unwrap();
    assert_eq!(node.created_by.kind, ActorKind::System);
    assert_eq!(node.created_by.actor, ID);
}

#[test]
fn everything_reads_the_history_too() {
    let f = Fixture::new();
    f.fake.add("family", Timestamp(0), "Very old");
    f.fake.add("family", Timestamp::now(), "New");
    f.client()
        .sources_connect(f.space(), ID, Some(History::Everything), &me())
        .unwrap();
    f.wait_until("both messages", |f| f.written().len() == 2);
    // The history is chosen once.
    let again = f
        .client()
        .sources_connect(f.space(), ID, Some(History::FromToday), &me());
    assert!(again.is_err());
}

#[test]
fn excluded_chats_are_never_read() {
    let f = Fixture::new();
    f.client()
        .sources_connect(f.space(), ID, Some(History::Everything), &me())
        .unwrap();
    f.client()
        .sources_exclude(f.space(), ID, "family", Some("Family"), &me())
        .unwrap();
    f.fake.add("family", Timestamp::now(), "Private");
    f.fake.add("work", Timestamp::now(), "Hello");
    f.wait_until("the work message", |f| f.written() == vec!["work 2"]);
    let containers = f.client().sources_containers(f.space(), ID).unwrap();
    let family = containers
        .iter()
        .find(|c| c.container.id == "family")
        .unwrap();
    assert!(family.excluded);
    assert!(
        !containers
            .iter()
            .find(|c| c.container.id == "work")
            .unwrap()
            .excluded
    );
    let last = f.fake.store().requests.last().cloned().unwrap();
    assert_eq!(last.1, vec!["family".to_string()]);
    let setting = f
        .client()
        .sources_include(f.space(), ID, "family", &me())
        .unwrap()
        .setting
        .unwrap();
    assert!(setting.exclude.is_empty());
    f.fake.add("family", Timestamp::now(), "Back");
    f.wait_until("the family message", |f| f.written().len() == 2);
    assert_eq!(f.written(), vec!["family 3", "work 2"]);
}

#[test]
fn a_restart_writes_nothing_twice() {
    let mut f = Fixture::new();
    for text in ["a", "b", "c", "d", "e"] {
        f.fake.add("work", Timestamp::now(), text);
    }
    f.client()
        .sources_connect(f.space(), ID, None, &me())
        .unwrap();
    f.wait_until("five messages", |f| f.written().len() == 5);
    f.stop();
    let state = fs::read_to_string(f.home().join("sources/state.json")).unwrap();
    assert!(state.contains("\"cursor\": \"5\""), "{state}");
    assert!(
        !state.contains("work"),
        "the state holds no content: {state}"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(f.home().join("sources/state.json"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    // Started again, it goes on from the cursor.
    f.fake.store().requests.clear();
    f.start();
    f.fake.add("work", Timestamp::now(), "f");
    f.wait_until("the sixth message", |f| f.written().len() == 6);
    assert_eq!(f.fake.store().requests[0].0.as_deref(), Some("5"));
    f.stop();

    // Even without the cursor, the importer finds what it wrote: nothing is written twice.
    fs::remove_file(f.home().join("sources/state.json")).unwrap();
    f.fake.store().requests.clear();
    f.start();
    f.wait_until("a run from the start", |f| {
        f.fake
            .store()
            .requests
            .first()
            .is_some_and(|r| r.0.is_none())
            && f.status().items == 0
            && f.status().last_success.is_some()
    });
    assert_eq!(f.written().len(), 6);
}

#[test]
fn errors_back_off_and_are_reported() {
    let f = Fixture::with_trigger(Trigger::Interval(Duration::from_millis(50)));
    f.fake.store().fail = 3;
    f.fake.add("work", Timestamp::now(), "Hello");
    f.client()
        .sources_connect(f.space(), ID, None, &me())
        .unwrap();
    f.wait_until("a failure", |f| f.status().state == SourceState::Failing);
    let status = f.status();
    let error = status.last_error.unwrap();
    assert_eq!(error.kind, "failed");
    assert!(error.message.contains("down"));
    assert!(status.next_run.unwrap().0 - error.at.0 >= 150);
    f.wait_until("the recovery", |f| f.written().len() == 1);
    f.wait_until("the status", |f| f.status().state != SourceState::Failing);
    let status = f.status();
    assert!(status.last_error.is_none());
    // Three failures in a row waited 200, 400 and 800 ms before the fourth fetch, where the
    // interval alone would have waited 50 ms each time.
    let times = f.fake.store().times.clone();
    let waited = times[3] - times[0];
    assert!(waited >= Duration::from_millis(1300), "{waited:?}");
    assert!(times[2] - times[1] > times[1] - times[0]);
}

#[test]
fn a_changed_schema_writes_nothing_and_says_so() {
    let f = Fixture::new();
    f.fake.store().changed_schema = true;
    f.fake.add("work", Timestamp::now(), "Hello");
    f.client()
        .sources_connect(f.space(), ID, None, &me())
        .unwrap();
    f.wait_until("the check", |f| {
        f.status().state == SourceState::NeedsUpdate
    });
    assert!(
        f.status()
            .last_error
            .unwrap()
            .message
            .contains("needs an update")
    );
    assert_eq!(f.fake.store().fetches, 0);
    assert!(f.written().is_empty());
    assert!(f.client().sources_containers(f.space(), ID).is_err());
}

#[test]
fn paused_and_stopped_sources_do_not_run() {
    let f = Fixture::new();
    f.client()
        .sources_connect(f.space(), ID, None, &me())
        .unwrap();
    f.client()
        .sources_set_on(f.space(), ID, false, &me())
        .unwrap();
    let fetches = f.fake.store().fetches;
    f.fake.add("work", Timestamp::now(), "Hello");
    thread::sleep(Duration::from_millis(400));
    assert_eq!(f.status().state, SourceState::Paused);
    assert!(f.fake.store().fetches <= fetches + 1);
    assert!(f.written().is_empty());

    // Off in config.toml: nothing runs, even switched on.
    f.write_config("sources = false\n");
    thread::sleep(Duration::from_millis(200));
    f.client()
        .sources_set_on(f.space(), ID, true, &me())
        .unwrap();
    f.wait_until("stopped", |f| f.status().state == SourceState::Stopped);
    thread::sleep(Duration::from_millis(300));
    assert!(f.written().is_empty());
    // capture = false stops sources too.
    f.write_config("capture = false\n");
    thread::sleep(Duration::from_millis(300));
    assert!(f.written().is_empty());

    f.write_config("");
    f.wait_until("the message", |f| f.written().len() == 1);
    let status = f.client().sources_status(None).unwrap();
    assert_eq!(status.len(), 1);
    assert_eq!(status[0].sources[0].info.id, ID);

    assert!(f.client().sources_disconnect(f.space(), ID, &me()).unwrap());
    assert_eq!(f.status().state, SourceState::Off);
    assert_eq!(f.status().items, 0);
    // What it brought in stays.
    assert_eq!(f.written().len(), 1);
}

#[test]
fn watched_files_run_the_connector() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("Synthetic.sqlite");
    fs::write(&db, b"").unwrap();
    let f = Fixture::with_trigger(Trigger::Watch {
        paths: vec![db.clone()],
        every: Duration::from_secs(3600),
    });
    f.client()
        .sources_connect(f.space(), ID, None, &me())
        .unwrap();
    f.wait_until("the first run", |f| f.status().last_success.is_some());
    let first = f.fake.store().fetches;
    f.fake.add("work", Timestamp::now(), "Hello");
    // Give the watcher time to start, then write the "database" and its -wal.
    thread::sleep(Duration::from_millis(200));
    fs::write(Path::new(&format!("{}-wal", db.display())), b"x").unwrap();
    f.wait_until("a run after the change", |f| f.written().len() == 1);
    assert!(f.fake.store().fetches > first);
}

#[test]
fn agents_cannot_manage_sources() {
    let f = Fixture::new();
    let by = Provenance::agent("claude-code");
    assert!(
        f.client()
            .sources_connect(f.space(), ID, None, &by)
            .is_err()
    );
}

#[test]
fn a_connectors_own_read_of_its_files_does_not_run_it_again() {
    let dir = tempfile::tempdir().unwrap();
    let (db, app) = synthetic_db(dir.path());
    let f = Fixture::with_trigger(Trigger::Watch {
        paths: vec![db.clone()],
        every: Duration::from_secs(3600),
    });
    f.fake.store().reads = Some(db.clone());
    f.client()
        .sources_connect(f.space(), ID, None, &me())
        .unwrap();
    f.wait_until("the first run", |f| f.status().last_success.is_some());
    // The events of its read come and are quiet many times over: nothing runs.
    thread::sleep(Duration::from_millis(1500));
    assert_eq!(f.fake.store().fetches, 1);

    // The app writes to its database and closes it (macOS reports writes to a file kept open
    // only late): that runs it.
    f.fake.add("work", Timestamp::now(), "Hello");
    app.execute("INSERT INTO message (text) VALUES ('Hello')", [])
        .unwrap();
    drop(app);
    f.wait_until("a run after the write", |f| f.written().len() == 1);
    // And its own read after that runs nothing again. Linux can report the app's write and
    // close as separate, late events, so wait until the runs settle before checking.
    let mut after = f.fake.store().fetches;
    let settle = std::time::Instant::now();
    loop {
        thread::sleep(Duration::from_millis(1000));
        let now = f.fake.store().fetches;
        if now == after || settle.elapsed() > Duration::from_secs(10) {
            break;
        }
        after = now;
    }
    thread::sleep(Duration::from_millis(1500));
    assert_eq!(f.fake.store().fetches, after);
}

#[test]
fn a_watching_connector_runs_at_its_interval_when_events_come_late() {
    let dir = tempfile::tempdir().unwrap();
    let (db, app) = synthetic_db(dir.path());
    let f = Fixture::with_options(
        Trigger::Watch {
            paths: vec![db],
            // Longer than the service allows: it runs at least every `watch_every`.
            every: Duration::from_secs(3600),
        },
        |o| {
            o.watch_every = Duration::from_millis(300);
            // File events never run it here: only the interval does.
            o.quiet = Duration::from_secs(3600);
        },
    );
    f.client()
        .sources_connect(f.space(), ID, None, &me())
        .unwrap();
    f.wait_until("the first run", |f| f.status().last_success.is_some());
    // Something new, written by the app to the database it keeps open; its events are
    // left out.
    f.fake.add("work", Timestamp::now(), "Hello");
    app.execute("INSERT INTO message (text) VALUES ('Hello')", [])
        .unwrap();
    f.wait_until("a run at the interval", |f| f.written().len() == 1);
    let times = f.fake.store().times.clone();
    assert!(times.len() >= 2);
    assert!(times[1] - times[0] >= Duration::from_millis(250));
}

#[test]
fn a_source_connected_paused_reads_nothing_until_resumed() {
    let f = Fixture::new();
    f.fake.add("family", Timestamp::now(), "Private");
    f.fake.add("work", Timestamp::now(), "Hello");
    let connected = f
        .client()
        .sources_connect_paused(f.space(), ID, Some(History::Everything), &me())
        .unwrap();
    assert_eq!(connected.state, SourceState::Paused);
    let setting = connected.setting.unwrap();
    assert!(!setting.on);
    assert_eq!(setting.history, History::Everything);
    // The containers can be listed and excluded before anything is read.
    let containers = f.client().sources_containers(f.space(), ID).unwrap();
    assert_eq!(containers.len(), 2);
    f.client()
        .sources_exclude(f.space(), ID, "family", Some("Family"), &me())
        .unwrap();
    thread::sleep(Duration::from_millis(400));
    assert_eq!(f.fake.store().fetches, 0);
    assert!(f.written().is_empty());

    f.client()
        .sources_set_on(f.space(), ID, true, &me())
        .unwrap();
    f.wait_until("the message", |f| f.written().len() == 1);
    assert_eq!(f.written(), vec!["work 2"]);
    // The first fetch already left the excluded chat out.
    assert_eq!(f.fake.store().requests[0].1, vec!["family".to_string()]);
}

#[test]
fn listing_containers_slowly_does_not_hold_up_the_space() {
    let f = Fixture::new();
    f.fake.store().slow_containers = Duration::from_secs(2);
    f.client()
        .sources_connect_paused(f.space(), ID, None, &me())
        .unwrap();
    let listing = {
        let client = f.client();
        let space = f.space();
        thread::spawn(move || client.sources_containers(space, ID).unwrap())
    };
    thread::sleep(Duration::from_millis(200));
    // Other calls on the space go on while the connector lists its chats.
    let started = Instant::now();
    f.client()
        .create_node(f.space(), NewNode::note("Meanwhile", ""), &me())
        .unwrap();
    assert_eq!(f.status().state, SourceState::Paused);
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "the space waited {:?}",
        started.elapsed()
    );
    assert_eq!(listing.join().unwrap().len(), 2);
}

/// A connector whose every fetch takes 150 ms, with 30 messages to read three at a time, and
/// connected with everything: a run of ten batches.
fn slow_run(chat_of: fn(u64) -> &'static str) -> Fixture {
    let f = Fixture::with_options(Trigger::Interval(Duration::from_secs(3600)), |o| {
        o.max_batches = 100;
    });
    f.fake.store().slow_fetch = Duration::from_millis(150);
    for i in 1..=30 {
        f.fake.add(chat_of(i), Timestamp::now(), "Hello");
    }
    f.client()
        .sources_connect(f.space(), ID, Some(History::Everything), &me())
        .unwrap();
    f.wait_until("the first fetch", |f| f.fake.store().fetches >= 1);
    f
}

#[test]
fn a_pause_and_an_exclusion_reach_a_run_in_progress() {
    let f = slow_run(|_| "family");
    f.client()
        .sources_set_on(f.space(), ID, false, &me())
        .unwrap();
    f.client()
        .sources_exclude(f.space(), ID, "family", Some("Family"), &me())
        .unwrap();
    let at_pause = f.fake.store().fetches;
    thread::sleep(Duration::from_millis(1200));
    let after = f.fake.store().fetches;
    let written = f.written().len();
    assert_eq!(at_pause, 1);
    assert_eq!(after, at_pause, "fetches after the pause");
    assert!(written <= 3, "written after the pause: {written}");
    assert_eq!(f.status().state, SourceState::Paused);
}

#[test]
fn an_exclusion_counts_from_the_next_batch_of_a_run() {
    let f = slow_run(|i| if i % 2 == 0 { "family" } else { "work" });
    f.client()
        .sources_exclude(f.space(), ID, "family", Some("Family"), &me())
        .unwrap();
    f.wait_until("the run", |f| {
        f.status().last_success.is_some() && f.status().state != SourceState::Running
    });
    let requests = f.fake.store().requests.clone();
    assert!(
        requests[1..]
            .iter()
            .all(|(_, excluded)| excluded == &vec!["family".to_string()]),
        "{requests:?}"
    );
    // The batch fetched before the exclusion was fetched again without the chat.
    assert!(
        f.written().iter().all(|t| t.starts_with("work")),
        "{:?}",
        f.written()
    );
    assert_eq!(f.written().len(), 15);
}

#[test]
fn a_disconnect_during_a_run_does_not_bring_the_source_back() {
    let f = slow_run(|_| "family");
    assert!(f.client().sources_disconnect(f.space(), ID, &me()).unwrap());
    let at_disconnect = f.fake.store().fetches;
    thread::sleep(Duration::from_millis(1200));
    assert_eq!(f.fake.store().fetches, at_disconnect);
    assert!(f.written().len() <= 3, "{:?}", f.written());
    let status = f.status();
    assert_eq!(status.state, SourceState::Off);
    assert_eq!(status.items, 0);
    assert!(status.last_run.is_none());
    // Nothing of it is saved again.
    let state = fs::read_to_string(f.home().join("sources/state.json")).unwrap_or_default();
    assert!(!state.contains(ID), "{state}");
}

#[test]
fn connecting_and_pausing_at_once_reads_nothing() {
    let f = Fixture::new();
    f.fake.store().slow_fetch = Duration::from_millis(150);
    for i in 1..=6 {
        f.fake
            .add("family", Timestamp::now(), &format!("Message {i}"));
    }
    let client = f.client();
    client
        .sources_connect(f.space(), ID, Some(History::Everything), &me())
        .unwrap();
    client.sources_set_on(f.space(), ID, false, &me()).unwrap();
    client
        .sources_exclude(f.space(), ID, "family", Some("Family"), &me())
        .unwrap();
    thread::sleep(Duration::from_millis(600));
    // A fetch already under way when the pause came is not written.
    assert!(f.written().is_empty(), "{:?}", f.written());
    assert!(f.fake.store().fetches <= 1);
}

#[test]
fn a_watching_connector_copies_nothing_at_its_interval_when_its_files_did_not_change() {
    let dir = tempfile::tempdir().unwrap();
    let (db, app) = synthetic_db(dir.path());
    let f = Fixture::with_options(
        Trigger::Watch {
            paths: vec![db.clone()],
            every: Duration::from_secs(3600),
        },
        |o| {
            o.watch_every = Duration::from_millis(200);
            // File events never run it here: only the interval does.
            o.quiet = Duration::from_secs(3600);
        },
    );
    f.fake.store().reads = Some(db);
    f.client()
        .sources_connect(f.space(), ID, None, &me())
        .unwrap();
    f.wait_until("the first run", |f| f.status().last_success.is_some());
    // Many intervals, and the files did not change: no run, no copy.
    thread::sleep(Duration::from_millis(1200));
    assert_eq!(f.fake.store().fetches, 1);
    // Asked to, it runs anyway.
    f.client().sources_run(f.space(), ID).unwrap();
    f.wait_until("the run asked for", |f| f.fake.store().fetches == 2);
    // While it waits for something its files do not show, it runs at every interval.
    f.fake.store().waiting = true;
    f.client().sources_run(f.space(), ID).unwrap();
    f.wait_until("runs at the interval", |f| f.fake.store().fetches >= 5);
    f.fake.store().waiting = false;
    f.wait_until("a run that waits for nothing", |f| {
        let fetches = f.fake.store().fetches;
        thread::sleep(Duration::from_millis(600));
        f.fake.store().fetches == fetches
    });
    // The app writes: the next interval runs it.
    f.fake.add("work", Timestamp::now(), "Hello");
    app.execute("INSERT INTO message (text) VALUES ('Hello')", [])
        .unwrap();
    f.wait_until("a run after the write", |f| f.written().len() == 1);
}
