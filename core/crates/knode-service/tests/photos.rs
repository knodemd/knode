//! Photos and media imports over the protocol: a real service, the person and agents as clients.

use std::path::PathBuf;
use std::time::Duration;

use knode_format::{ActorKind, GrantLevel, NewNode, Provenance, kinds};
use knode_service::import::{ImportOutcome, ImportSource};
use knode_service::keys::{fingerprint, generate_key};
use knode_service::photos::{NewPhoto, Photo, PhotoMetadata, PhotoReading, ReadingState};
use knode_service::protocol::codes;
use knode_service::{ChangeKind, Client, Config, Error, Event, ServiceHandle, Subscription, start};
use serde_json::json;
use tempfile::TempDir;

const WAIT: Duration = Duration::from_secs(10);

struct Fixture {
    dir: TempDir,
    _service: ServiceHandle,
    person: Client,
}

fn me() -> Provenance {
    Provenance::person("Ada")
}

fn vision() -> Provenance {
    Provenance::new(ActorKind::System, "photos").with_source("apple-vision")
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let service = start(Config::new(dir.path().join("home"))).unwrap();
        let person = Client::connect(service.socket_path()).unwrap();
        let fixture = Fixture {
            dir,
            _service: service,
            person,
        };
        fixture
            .person
            .create_space(fixture.space(), "Work")
            .unwrap();
        fixture
    }

    fn socket(&self) -> PathBuf {
        self.dir.path().join("home/service.sock")
    }

    fn space(&self) -> PathBuf {
        std::fs::canonicalize(self.dir.path())
            .unwrap()
            .join("Work.knode")
    }

    fn file(&self, name: &str) -> PathBuf {
        let path = std::fs::canonicalize(self.dir.path()).unwrap().join(name);
        std::fs::write(&path, name.as_bytes()).unwrap();
        path
    }

    fn agent(&self, name: &str, level: GrantLevel) -> Client {
        let key = generate_key().unwrap();
        self.person
            .add_agent(name, "claude-code", &fingerprint(&key), &me())
            .unwrap();
        self.person
            .set_grant(name, self.space(), level, &me())
            .unwrap();
        Client::connect_as_agent(self.socket(), &key).unwrap()
    }

    fn photo(&self, name: &str) -> NewPhoto {
        NewPhoto {
            title: String::new(),
            path: self.file(name),
            project: None,
            metadata: PhotoMetadata {
                taken_at: Some(1_795_343_400_000),
                latitude: Some(40.4168),
                longitude: Some(-3.7038),
                place: None,
                camera: None,
                width: Some(800),
                height: Some(600),
            },
        }
    }
}

fn rpc(result: Result<impl std::fmt::Debug, Error>) -> knode_service::RpcError {
    match result {
        Err(Error::Rpc(e)) => e,
        other => panic!("expected an error from the service, got {other:?}"),
    }
}

fn next_change(events: &Subscription) -> knode_service::Change {
    loop {
        match events.recv_timeout(WAIT) {
            Some(Event::Changed(change)) => return change,
            Some(_) => continue,
            None => panic!("no changed notification"),
        }
    }
}

fn reading() -> PhotoReading {
    PhotoReading {
        engine: "Apple Vision".into(),
        text: "Roadmap Q3".into(),
        labels: vec!["whiteboard".into()],
        description: None,
        metadata: PhotoMetadata::default(),
    }
}

#[test]
fn a_photo_goes_from_added_to_read() {
    let f = Fixture::new();
    let space = f.space();
    let project = f
        .person
        .create_node(
            &space,
            NewNode::note("Launch", "").with_kind(kinds::PROJECT),
            &me(),
        )
        .unwrap();
    let other = Client::connect(f.socket()).unwrap();
    let events = other.subscribe(Some(&space)).unwrap();

    let mut new = f.photo("board.jpg");
    new.project = Some(project.id);
    let photo = f.person.create_photo(&space, &new, &me()).unwrap();
    assert_eq!(photo.title, "board");
    assert_eq!(photo.project, Some(project.id));
    assert_eq!(photo.reading, ReadingState::Pending);
    let change = next_change(&events);
    assert_eq!((change.node_id, change.op), (photo.id, ChangeKind::Create));
    assert_eq!(
        f.person.pending_photos(&space).unwrap(),
        vec![photo.clone()]
    );

    let read = f
        .person
        .set_photo_reading(&space, &photo.id, &reading(), &vision())
        .unwrap();
    assert_eq!(read.reading, ReadingState::Done);
    let node = f.person.get_node(&space, &photo.id).unwrap().unwrap();
    assert!(node.body.contains("Roadmap Q3"));
    assert!(node.body.contains("<!-- knode:photo-reading -->"));
    assert!(f.person.pending_photos(&space).unwrap().is_empty());
    let found = f.person.photo(&space, &photo.id).unwrap();
    assert_eq!(found, read);

    let failed = f
        .person
        .fail_photo(&space, &photo.id, "Unreadable", &vision())
        .unwrap();
    assert_eq!(failed.reading, ReadingState::Failed);
    assert_eq!(f.person.failed_photos(&space).unwrap().len(), 1);
    let retried = f.person.retry_photo(&space, &photo.id, &me()).unwrap();
    assert_eq!(retried.reading, ReadingState::Pending);
    assert_eq!(retried.error, None);
}

#[test]
fn the_protocol_takes_and_gives_plain_json() {
    let f = Fixture::new();
    let space = f.space();
    let path = f.file("IMG_1.HEIC");
    let value = f
        .person
        .call(
            "photos.create",
            json!({
                "space": space,
                "path": path,
                "metadata": { "taken_at": 1_795_343_400_000_i64, "place": "Madrid" },
                "by": { "kind": "person", "actor": "Ada" },
            }),
        )
        .unwrap();
    assert_eq!(value["title"], "IMG_1");
    assert_eq!(value["file"], "IMG_1.HEIC");
    assert_eq!(value["reading"], "pending");
    assert_eq!(value["metadata"]["place"], "Madrid");
    let photo: Photo = serde_json::from_value(value).unwrap();
    let relative = f.person.call(
        "photos.create",
        json!({ "space": space, "path": "IMG_1.HEIC", "by": { "kind": "person", "actor": "Ada" } }),
    );
    assert_eq!(rpc(relative).code, codes::INVALID_PARAMS);
    let value = f
        .person
        .call(
            "photos.reading",
            json!({
                "space": space, "id": photo.id,
                "reading": { "engine": "Apple Vision", "text": "Hola", "labels": ["document"] },
                "by": { "kind": "system", "actor": "photos", "source": "apple-vision" },
            }),
        )
        .unwrap();
    assert_eq!(value["reading"], "done");
}

#[test]
fn photos_location_is_a_setting() {
    let f = Fixture::new();
    let space = f.space();
    let mut located = f.photo("before.png");
    located.metadata.latitude = Some(40.4);
    located.metadata.longitude = Some(-3.7);
    let before = f.person.create_photo(&space, &located, &me()).unwrap();
    assert_eq!(before.metadata.latitude, Some(40.4));
    let settings = f
        .person
        .set_setting(&space, "photos.location", Some("off"), &me())
        .unwrap();
    assert_eq!(settings["photos.location"], "off");
    assert_eq!(
        rpc(f
            .person
            .set_setting(&space, "photos.location", Some("maybe"), &me()))
        .code,
        codes::INVALID
    );
    let photo = f
        .person
        .create_photo(&space, &f.photo("trip.png"), &me())
        .unwrap();
    assert_eq!(photo.metadata.latitude, None);
    assert_eq!(photo.metadata.width, Some(800));
    // The photo added before lost its location when it was turned off.
    let forgotten = f.person.photo(&space, &before.id).unwrap();
    assert_eq!(
        (forgotten.metadata.latitude, forgotten.metadata.longitude),
        (None, None)
    );
}

#[test]
fn agents_read_photos_but_never_write_them() {
    let f = Fixture::new();
    let space = f.space();
    let photo = f
        .person
        .create_photo(&space, &f.photo("a.jpg"), &me())
        .unwrap();
    let writer = f.agent("writer", GrantLevel::Write);
    assert_eq!(writer.photo(&space, &photo.id).unwrap().id, photo.id);
    assert_eq!(writer.pending_photos(&space).unwrap().len(), 1);
    assert!(writer.failed_photos(&space).unwrap().is_empty());
    assert_eq!(
        rpc(writer.create_photo(&space, &f.photo("b.jpg"), &me())).code,
        codes::FORBIDDEN
    );
    assert_eq!(
        rpc(writer.set_photo_reading(&space, &photo.id, &reading(), &me())).code,
        codes::FORBIDDEN
    );
    assert_eq!(
        rpc(writer.fail_photo(&space, &photo.id, "x", &me())).code,
        codes::FORBIDDEN
    );
    assert_eq!(
        rpc(writer.retry_photo(&space, &photo.id, &me())).code,
        codes::FORBIDDEN
    );
    let none = f.agent("none", GrantLevel::None);
    assert!(none.photo(&space, &photo.id).is_err());
}

#[test]
fn media_files_and_folders_are_imported_once() {
    let f = Fixture::new();
    let space = f.space();
    let folder = std::fs::canonicalize(f.dir.path()).unwrap().join("Camera");
    std::fs::create_dir(&folder).unwrap();
    std::fs::write(folder.join("board.jpg"), b"jpeg").unwrap();
    std::fs::write(folder.join("talk.mov"), b"movie").unwrap();
    std::fs::write(folder.join("memo.m4a"), b"audio").unwrap();
    std::fs::write(folder.join("readme.txt"), b"skip me").unwrap();

    let result = f
        .person
        .import_path(&space, Some(&folder), Some(ImportSource::Media), &me())
        .unwrap();
    assert_eq!(result.source, ImportSource::Media);
    assert_eq!(result.created, 3);
    assert_eq!(result.attached, 3);
    assert_eq!(f.person.pending_photos(&space).unwrap().len(), 1);
    assert_eq!(f.person.pending_recordings(&space).unwrap().len(), 2);

    // One file, found by its extension, already there.
    let again = f
        .person
        .import_path(&space, Some(&folder.join("board.jpg")), None, &me())
        .unwrap();
    assert_eq!(again.source, ImportSource::Media);
    assert_eq!(again.nodes[0].outcome, ImportOutcome::Unchanged);
    assert_eq!(again.created, 0);
}
