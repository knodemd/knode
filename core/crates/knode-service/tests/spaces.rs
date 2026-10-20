//! The name, settings and folders of a space through a real service: the person changes them,
//! agents only read them, and every client hears about each change.

use std::path::PathBuf;
use std::time::Duration;

use knode_format::{GrantLevel, Provenance, actions, setting_keys};
use knode_service::keys::{fingerprint, generate_key};
use knode_service::protocol::codes;
use knode_service::{
    Client, Config, Error, Event, FolderInput, ServiceHandle, SpaceEvent, SpaceEventKind,
    Subscription, start,
};
use serde_json::json;
use tempfile::TempDir;

const WAIT: Duration = Duration::from_secs(5);

struct Fixture {
    dir: TempDir,
    service: ServiceHandle,
    person: Client,
}

fn me() -> Provenance {
    Provenance::person("Ada")
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let service = start(Config::new(dir.path().join("home"))).unwrap();
        let person = Client::connect(service.socket_path()).unwrap();
        let fixture = Fixture {
            dir,
            service,
            person,
        };
        fixture
            .person
            .create_space(fixture.space(), "Work")
            .unwrap();
        fixture
    }

    fn space(&self) -> PathBuf {
        std::fs::canonicalize(self.dir.path())
            .unwrap()
            .join("Work.knode")
    }

    fn folder(&self, name: &str) -> PathBuf {
        let path = std::fs::canonicalize(self.dir.path()).unwrap().join(name);
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    /// An agent that may read the space.
    fn reader(&self) -> Client {
        let key = generate_key().unwrap();
        self.person
            .add_agent("reader", "claude-code", &fingerprint(&key), &me())
            .unwrap();
        self.person
            .set_grant("reader", self.space(), GrantLevel::Read, &me())
            .unwrap();
        Client::connect_as_agent(self.service.socket_path(), &key).unwrap()
    }
}

fn rpc(result: Result<impl std::fmt::Debug, Error>) -> knode_service::RpcError {
    match result {
        Err(Error::Rpc(e)) => e,
        other => panic!("expected an error from the service, got {other:?}"),
    }
}

fn next_space_event(events: &Subscription) -> SpaceEvent {
    loop {
        match events.recv_timeout(WAIT) {
            Some(Event::Space(event)) => return event,
            Some(_) => continue,
            None => panic!("no space event"),
        }
    }
}

#[test]
fn renaming_a_space_changes_the_name_everyone_sees() {
    let f = Fixture::new();
    let other = Client::connect(f.service.socket_path()).unwrap();
    let events = other.subscribe(Some(&f.space())).unwrap();

    let info = f
        .person
        .rename_space(f.space(), " Day job ", &me())
        .unwrap();
    assert_eq!(info.name, "Day job");
    let event = next_space_event(&events);
    assert_eq!(event.what, SpaceEventKind::Renamed);
    assert_eq!(event.name.as_deref(), Some("Day job"));
    assert_eq!(event.space, f.space());
    // The service answers with the new name at once, and the manifest has it.
    assert_eq!(other.open_space(f.space()).unwrap().name, "Day job");
    let manifest = knode_format::Manifest::read(&f.space()).unwrap();
    assert_eq!(manifest.name, "Day job");

    // The same name again changes nothing and says nothing.
    f.person.rename_space(f.space(), "Day job", &me()).unwrap();
    assert!(events.recv_timeout(Duration::from_millis(200)).is_none());

    let error = rpc(f.person.rename_space(f.space(), "  ", &me()));
    assert_eq!(error.code, codes::INVALID);
    let error = rpc(f
        .person
        .rename_space(f.space(), "X", &Provenance::agent("bot")));
    assert_eq!(error.code, codes::FORBIDDEN);
}

#[test]
fn settings_are_read_by_all_and_changed_by_the_person() {
    let f = Fixture::new();
    let events = f.person.subscribe(None).unwrap();
    let settings = f.person.settings(f.space()).unwrap();
    assert_eq!(settings.len(), 1, "only space.id: {settings:?}");
    assert!(settings.contains_key(setting_keys::SPACE_ID));

    let settings = f
        .person
        .set_setting(f.space(), setting_keys::SPACE_ICON, Some("🌱"), &me())
        .unwrap();
    assert_eq!(settings[setting_keys::SPACE_ICON], "🌱");
    let event = next_space_event(&events);
    assert_eq!(event.what, SpaceEventKind::Settings);
    assert_eq!(event.key.as_deref(), Some(setting_keys::SPACE_ICON));

    // Capture off is stored; on is the default, so it is stored as no value.
    f.person
        .set_setting(f.space(), setting_keys::CAPTURE_ENABLED, Some("off"), &me())
        .unwrap();
    let settings = f
        .person
        .set_setting(f.space(), setting_keys::CAPTURE_ENABLED, Some("on"), &me())
        .unwrap();
    assert!(!settings.contains_key(setting_keys::CAPTURE_ENABLED));

    // Every change is in the history of the space, under the key.
    let changes = f
        .person
        .changes_of(f.space(), setting_keys::CAPTURE_ENABLED, 10)
        .unwrap();
    assert_eq!(changes.len(), 2);
    assert!(changes.iter().all(|c| c.action == actions::SETTINGS_SET));

    // Values the format does not allow are refused.
    for (key, value) in [
        (setting_keys::SPACE_ICON, "work"),
        (setting_keys::CAPTURE_ENABLED, "maybe"),
        (setting_keys::SPACE_ID, "x"),
        (setting_keys::MARKDOWN_FOLDER, "/tmp/x"),
        ("theme", "dark"),
    ] {
        let error = rpc(f.person.set_setting(f.space(), key, Some(value), &me()));
        assert_eq!(error.code, codes::INVALID, "{key}");
    }

    // An agent reads the settings, but cannot change them.
    let agent = f.reader();
    let settings = agent.settings(f.space()).unwrap();
    assert_eq!(settings[setting_keys::SPACE_ICON], "🌱");
    let error = rpc(agent.set_setting(f.space(), setting_keys::SPACE_ICON, None, &me()));
    assert_eq!(error.code, codes::FORBIDDEN);
    let error = rpc(agent.rename_space(f.space(), "Mine", &me()));
    assert_eq!(error.code, codes::FORBIDDEN);
}

#[test]
fn folders_are_listed_added_changed_and_removed() {
    let f = Fixture::new();
    let events = f.person.subscribe(Some(&f.space())).unwrap();
    let project = f.folder("code/app");
    assert!(f.person.folders(f.space()).unwrap().is_empty());

    let input = FolderInput {
        id: None,
        kind: "project".into(),
        path: format!("{}/./", project.display()),
        capture: true,
        rules: json!({ "exclude": ["vendor/**"] }),
    };
    let added = f.person.set_folder(f.space(), &input, &me()).unwrap();
    assert_eq!(added.folder.kind, "project");
    assert_eq!(added.folder.path, project.to_string_lossy());
    assert_eq!(added.local.as_deref(), Some(project.as_path()));
    assert!(!added.missing);
    let event = next_space_event(&events);
    assert_eq!(event.what, SpaceEventKind::Folders);
    assert_eq!(event.folder.as_deref(), Some(added.folder.id.as_str()));

    // The same kind and path changes that folder; capture off for it.
    let off = FolderInput {
        capture: false,
        ..input.clone()
    };
    let changed = f.person.set_folder(f.space(), &off, &me()).unwrap();
    assert_eq!(changed.folder.id, added.folder.id);
    assert!(!changed.folder.capture);
    next_space_event(&events);

    // A folder in the home folder is recorded from ~, and a missing one is kept.
    let home = std::env::var("HOME").unwrap();
    let notes = FolderInput {
        id: None,
        kind: "notes".into(),
        path: "~/knode-test-folder-that-does-not-exist".into(),
        capture: true,
        rules: json!({}),
    };
    let missing = f.person.set_folder(f.space(), &notes, &me()).unwrap();
    assert_eq!(
        missing.folder.path,
        "~/knode-test-folder-that-does-not-exist"
    );
    assert_eq!(
        missing.local,
        Some(PathBuf::from(&home).join("knode-test-folder-that-does-not-exist"))
    );
    assert!(missing.missing);
    next_space_event(&events);

    let listed = f.person.folders(f.space()).unwrap();
    assert_eq!(listed.len(), 2);
    let history = f
        .person
        .changes_of(f.space(), &added.folder.id, 10)
        .unwrap();
    assert_eq!(history.len(), 2);
    assert!(history.iter().all(|c| c.action == actions::FOLDER_SET));

    // What the format does not allow is refused.
    let bad = [
        FolderInput {
            kind: "music".into(),
            ..input.clone()
        },
        FolderInput {
            path: "relative/path".into(),
            ..input.clone()
        },
        FolderInput {
            kind: "agent".into(),
            rules: json!({}),
            ..input.clone()
        },
        FolderInput {
            rules: json!({ "exclude": "vendor" }),
            ..input.clone()
        },
        FolderInput {
            path: f.space().join("inside").display().to_string(),
            ..input.clone()
        },
    ];
    for input in bad {
        let error = rpc(f.person.set_folder(f.space(), &input, &me()));
        assert_eq!(error.code, codes::INVALID, "{input:?}");
    }
    let agent_folder = FolderInput {
        kind: "agent".into(),
        rules: json!({ "agent": "codex" }),
        ..input.clone()
    };
    f.person
        .set_folder(f.space(), &agent_folder, &me())
        .unwrap();
    next_space_event(&events);

    // An agent lists the folders, but cannot change them.
    let agent = f.reader();
    assert_eq!(agent.folders(f.space()).unwrap().len(), 3);
    let error = rpc(agent.set_folder(f.space(), &input, &me()));
    assert_eq!(error.code, codes::FORBIDDEN);
    let error = rpc(agent.remove_folder(f.space(), &added.folder.id, &me()));
    assert_eq!(error.code, codes::FORBIDDEN);

    assert!(
        f.person
            .remove_folder(f.space(), &added.folder.id, &me())
            .unwrap()
    );
    let event = next_space_event(&events);
    assert_eq!(event.folder.as_deref(), Some(added.folder.id.as_str()));
    assert!(
        !f.person
            .remove_folder(f.space(), &added.folder.id, &me())
            .unwrap()
    );
    assert_eq!(f.person.folders(f.space()).unwrap().len(), 2);
}

#[test]
fn agents_hear_about_spaces_they_may_read() {
    let f = Fixture::new();
    let agent = f.reader();
    let events = agent.subscribe(None).unwrap();
    f.person
        .set_setting(f.space(), setting_keys::SPACE_ICON, Some("🧭"), &me())
        .unwrap();
    let event = next_space_event(&events);
    assert_eq!(event.key.as_deref(), Some(setting_keys::SPACE_ICON));

    // Without a grant, the space is invisible: no events either.
    f.person
        .set_grant("reader", f.space(), GrantLevel::None, &me())
        .unwrap();
    f.person.rename_space(f.space(), "Hidden", &me()).unwrap();
    assert!(events.recv_timeout(Duration::from_millis(300)).is_none());
}
