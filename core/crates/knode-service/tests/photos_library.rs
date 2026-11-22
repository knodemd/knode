//! The Photos library source and the storage of a space in the service: run by the app, which
//! hands the assets it exported to `photos_library.add`. Synthetic files only; nothing here reads
//! a real Photos library.

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use knode_format::{Provenance, Space};
use knode_service::sources::SourcesOptions;
use knode_service::sources::photos_library::PhotosLibraryConnector;
use knode_service::{Client, Config, ServiceHandle, start};
use serde_json::{Value, json};
use tempfile::TempDir;

struct Fixture {
    dir: TempDir,
    service: Option<ServiceHandle>,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        drop(Space::create(dir.path().join("Personal.knode"), "Personal").unwrap());
        let mut f = Fixture { dir, service: None };
        fs::create_dir_all(f.home()).unwrap();
        let text = format!("default_space = {:?}\n", f.space().to_str().unwrap());
        fs::write(f.home().join("config.toml"), text).unwrap();
        let options = SourcesOptions {
            settings_poll: Duration::from_millis(50),
            ..SourcesOptions::with_connectors(vec![Arc::new(PhotosLibraryConnector)])
        };
        f.service = Some(start(Config::new(f.home()).with_sources(options)).unwrap());
        f
    }

    fn home(&self) -> PathBuf {
        self.dir.path().join("home")
    }

    fn space(&self) -> PathBuf {
        fs::canonicalize(self.dir.path().join("Personal.knode")).unwrap()
    }

    fn file(&self, name: &str, bytes: &[u8]) -> PathBuf {
        let path = fs::canonicalize(self.dir.path()).unwrap().join(name);
        fs::write(&path, bytes).unwrap();
        path
    }

    fn try_call(&self, method: &str, mut params: Value) -> Result<Value, String> {
        params["space"] = json!(self.space());
        Client::connect(self.home().join("service.sock"))
            .unwrap()
            .call(method, params)
            .map_err(|e| e.to_string())
    }

    fn call(&self, method: &str, params: Value) -> Value {
        self.try_call(method, params)
            .unwrap_or_else(|e| panic!("{method}: {e}"))
    }

    fn status(&self) -> Value {
        self.call("sources.list", json!({}))
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["id"] == "photos-library")
            .unwrap()
            .clone()
    }

    fn photo(&self, id: &str, name: &str, bytes: &[u8]) -> Value {
        json!({
            "id": id, "kind": "photo", "path": self.file(name, bytes),
            "created_at": 4_102_444_800_000_i64, "metadata": { "width": 3, "height": 2 },
        })
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        if let Some(service) = self.service.take() {
            let _ = service.stop();
        }
    }
}

fn me() -> Provenance {
    Provenance::person("Example")
}

#[test]
fn the_app_picks_albums_adds_assets_once_and_marks_the_ones_that_left() {
    let f = Fixture::new();
    let status = f.status();
    assert_eq!(status["permissions"], json!(["photos"]));
    assert_eq!(status["container"], "album");
    assert_eq!(status["picks"], true);
    assert!(f.try_call("photos_library.plan", json!({})).is_err());

    f.call(
        "sources.connect",
        json!({ "source": "photos-library", "history": "everything", "by": me() }),
    );
    // Nothing is read until an album is picked.
    let plan = f.call("photos_library.plan", json!({}));
    assert_eq!(plan["on"], true);
    assert_eq!(plan["albums"], json!([]));
    assert_eq!(plan["elsewhere"], false);
    let lib = plan["library"].as_str().unwrap().to_string();
    assert!(!lib.is_empty());
    assert_eq!(f.call("photos_library.plan", json!({}))["library"], lib);
    assert_eq!(plan["icloud"], false);
    assert_eq!(plan["room"], true);
    assert!(plan.get("since").is_none());

    f.call(
        "photos_library.albums",
        json!({ "albums": [{ "id": "trip", "name": "Trip" }, { "id": "work", "name": "Work" }] }),
    );
    // Albums are named with this Mac's library id: picks belong to one Mac.
    let trip = format!("{lib}/trip");
    f.call(
        "sources.include",
        json!({ "source": "photos-library", "id": trip, "by": me() }),
    );
    // A pick of another Mac is not this Mac's to show or read.
    f.call(
        "sources.include",
        json!({ "source": "photos-library", "id": "OTHERMAC/far", "name": "Far", "by": me() }),
    );
    let containers = f.call("sources.containers", json!({ "source": "photos-library" }));
    assert_eq!(
        containers,
        json!([
            { "id": trip, "name": "Trip", "excluded": false },
            { "id": format!("{lib}/work"), "name": "Work", "excluded": true },
        ])
    );
    let plan = f.call("photos_library.plan", json!({}));
    assert_eq!(plan["albums"], json!([{ "id": "trip", "name": "Trip" }]));

    let a = f.photo("A/L0/001", "IMG_1.HEIC", b"one");
    let added = f.call("photos_library.add", json!({ "asset": a }));
    assert_eq!(added["created"], true);
    assert_eq!(added["node"]["type"], "photo");
    assert_eq!(added["room"], true);
    let again = f.call("photos_library.add", json!({ "asset": a }));
    assert_eq!(again["created"], false);
    assert_eq!(again["node"]["id"], added["node"]["id"]);
    f.call(
        "photos_library.add",
        json!({ "asset": f.photo("B/L0/001", "IMG_2.HEIC", b"two") }),
    );
    let known = f.call(
        "photos_library.known",
        json!({ "ids": ["A/L0/001", "C/L0/001", "B/L0/001"] }),
    );
    assert_eq!(known, json!(["A/L0/001", "B/L0/001"]));

    let synced = f.call(
        "photos_library.sync",
        json!({ "present": ["A/L0/001"], "token": "tok-1", "added": 2, "waiting": 3 }),
    );
    assert_eq!(synced["marked"], json!({ "removed": 1, "restored": 0 }));
    assert_eq!(synced["source"]["last_items"], 2);
    assert!(
        synced["source"]["notice"]
            .as_str()
            .unwrap()
            .contains("3 wait for their original")
    );
    let plan = f.call("photos_library.plan", json!({}));
    assert_eq!(plan["token"], "tok-1");
    assert!(plan["next_run"].as_i64().is_some());

    let space = Space::open(f.space()).unwrap();
    let nodes = space
        .find_by_property("photos_library.state", "removed")
        .unwrap();
    assert_eq!(nodes.len(), 1);
    assert_eq!(nodes[0].title, "IMG_2");
    assert_eq!(nodes[0].kind, "photo");
    let node = space.get_node(&nodes[0].id).unwrap().unwrap();
    assert_eq!(node.created_by.actor, "photos-library");
    assert_eq!(
        space
            .property(&node.id, "photos_library.library")
            .unwrap()
            .as_deref(),
        Some(lib.as_str())
    );
    assert!(!node.body.contains("No longer"), "{}", node.body);
    drop(space);

    // Dropping the album leaves it out; what came from it stays. Only another Mac's pick is
    // left: this Mac reads nothing and says so.
    f.call(
        "sources.exclude",
        json!({ "source": "photos-library", "id": trip, "by": me() }),
    );
    let plan = f.call("photos_library.plan", json!({}));
    assert_eq!(plan["albums"], json!([]));
    assert_eq!(plan["elsewhere"], true);

    let failed = f.call(
        "photos_library.fail",
        json!({ "kind": "permission", "message": "no access" }),
    );
    assert_eq!(failed["state"], "needs-attention");
    assert!(
        f.try_call("photos_library.fail", json!({ "kind": "odd" }))
            .is_err()
    );

    let options = f.call(
        "photos_library.configure",
        json!({ "icloud": true, "by": me() }),
    );
    assert_eq!(options, json!({ "icloud": true, "elsewhere": true }));
    assert_eq!(f.call("photos_library.plan", json!({}))["icloud"], true);
}

#[test]
fn from_today_leaves_older_assets_out_and_a_paused_source_adds_nothing() {
    let f = Fixture::new();
    f.call(
        "sources.connect",
        json!({ "source": "photos-library", "history": "from-today", "by": me() }),
    );
    let plan = f.call("photos_library.plan", json!({}));
    let since = plan["since"].as_i64().unwrap();
    let mut old = f.photo("OLD", "old.jpg", b"old");
    old["created_at"] = json!(since - 1);
    let err = f
        .try_call("photos_library.add", json!({ "asset": old }))
        .unwrap_err();
    assert!(err.contains("before the day"), "{err}");
    let relative = json!({ "id": "R", "kind": "photo", "path": "relative.jpg" });
    assert!(
        f.try_call("photos_library.add", json!({ "asset": relative }))
            .is_err()
    );

    f.call(
        "sources.pause",
        json!({ "source": "photos-library", "by": me() }),
    );
    let new = f.photo("NEW", "new.jpg", b"new");
    assert!(
        f.try_call("photos_library.add", json!({ "asset": new }))
            .unwrap_err()
            .contains("paused")
    );
    let synced = f.call("photos_library.sync", json!({ "present": [] }));
    assert_eq!(synced["marked"], json!({ "removed": 0, "restored": 0 }));
}

#[test]
fn deleted_nodes_and_failures_for_good_are_never_brought_back() {
    let f = Fixture::new();
    f.call(
        "sources.connect",
        json!({ "source": "photos-library", "history": "everything", "by": me() }),
    );
    let a = f.photo("A", "a.jpg", b"a");
    let added = f.call("photos_library.add", json!({ "asset": a }));
    let id: knode_format::NodeId = added["node"]["id"].as_str().unwrap().parse().unwrap();
    let mut space = Space::open(f.space()).unwrap();
    space.delete_node(&id, &me()).unwrap();
    drop(space);

    // The end of the run sees the node gone: the asset is never exported or added again.
    let synced = f.call(
        "photos_library.sync",
        json!({ "present": ["A", "V"], "failed": [{ "id": "V", "reason": "audio too long" }] }),
    );
    assert!(
        synced["source"]["notice"]
            .as_str()
            .unwrap()
            .contains("1 cannot be added"),
        "{synced}"
    );
    let known = f.call("photos_library.known", json!({ "ids": ["A", "V", "W"] }));
    assert_eq!(known, json!(["A", "V"]));
    let err = f
        .try_call("photos_library.add", json!({ "asset": a }))
        .unwrap_err();
    assert!(err.contains("deleted"), "{err}");
    let space = Space::open(f.space()).unwrap();
    assert!(
        space
            .find_by_property("photos_library.id", "A")
            .unwrap()
            .is_empty()
    );
}

#[test]
fn run_now_asks_the_app() {
    let f = Fixture::new();
    f.call(
        "sources.connect",
        json!({ "source": "photos-library", "by": me() }),
    );
    let before = f.call("photos_library.plan", json!({}));
    assert!(before.get("next_run").is_none());
    f.call("sources.run", json!({ "source": "photos-library" }));
    assert!(
        f.call("photos_library.plan", json!({}))["next_run"]
            .as_i64()
            .is_some()
    );
}

#[test]
fn storage_summary_check_and_limits() {
    let f = Fixture::new();
    let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
    png.extend_from_slice(&[0u8; 400]);
    let path = f.file("board.png", &png);
    f.call(
        "photos.create",
        json!({ "title": "", "path": path, "by": me() }),
    );
    let summary = f.call("storage.summary", json!({ "top": 5 }));
    assert_eq!(summary["kinds"]["photos"], png.len());
    assert_eq!(summary["photos"], 1);
    assert_eq!(summary["largest"][0]["name"], "board.png");
    assert_eq!(summary["largest"][0]["kind"], "photo");
    assert_eq!(summary["sources"][0]["actor"], "Example");
    assert_eq!(summary["limits"]["max_file"], 100 * 1024 * 1024);
    assert_eq!(summary["over"], false);

    assert_eq!(f.call("storage.check", json!({ "size": 1 }))["warn"], false);
    let limits = f.call("storage.configure", json!({ "warn_at": 1, "by": me() }));
    assert_eq!(limits["warn_at"], 1);
    assert!(limits.get("keep_large_originals").is_none());
    assert_eq!(f.call("storage.check", json!({ "size": 1 }))["warn"], true);
    assert_eq!(f.call("storage.summary", json!({}))["over"], true);
    assert!(
        f.try_call("storage.configure", json!({ "warn_at": 0 }))
            .is_err()
    );
}
