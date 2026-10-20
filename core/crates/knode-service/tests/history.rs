//! History, undo, Markdown export and import, and the live Markdown folder, through a real
//! service.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use knode_format::{NewNode, NodeId, NodeUpdate, Provenance, actions};
use knode_service::protocol::codes;
use knode_service::{ChangeKind, Client, Config, Error, Event, ServiceHandle, Subscription, start};
use serde_json::json;
use tempfile::TempDir;

const WAIT: Duration = Duration::from_secs(10);

struct Fixture {
    dir: TempDir,
    service: Option<ServiceHandle>,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let service = start(Config::new(dir.path().join("home"))).unwrap();
        Fixture {
            dir,
            service: Some(service),
        }
    }

    fn client(&self) -> Client {
        Client::connect(self.dir.path().join("home/service.sock")).unwrap()
    }

    fn path(&self, rel: &str) -> PathBuf {
        self.dir.path().join(rel)
    }

    fn space(&self, client: &Client, name: &str) -> PathBuf {
        client
            .create_space(self.path(&format!("{name}.knode")), name)
            .unwrap()
            .path
    }

    fn restart(&mut self) {
        self.service.take().unwrap().stop().unwrap();
        self.service = Some(start(Config::new(self.dir.path().join("home"))).unwrap());
    }
}

fn ada() -> Provenance {
    Provenance::person("Ada")
}

fn claude() -> Provenance {
    Provenance::agent("claude-code").with_session("5f1c")
}

fn rpc(result: Result<impl std::fmt::Debug, Error>) -> knode_service::RpcError {
    match result {
        Err(Error::Rpc(e)) => e,
        other => panic!("expected an RPC error, got {other:?}"),
    }
}

fn changed(subscription: &Subscription, count: usize) -> Vec<(NodeId, ChangeKind)> {
    let deadline = Instant::now() + WAIT;
    let mut out = Vec::new();
    while out.len() < count {
        let left = deadline.saturating_duration_since(Instant::now());
        match subscription.recv_timeout(left) {
            Some(Event::Changed(c)) => out.push((c.node_id, c.op)),
            Some(other) => panic!("unexpected {other:?}"),
            None => panic!("got {} of {count} changes: {out:?}", out.len()),
        }
    }
    out
}

/// Waits until `check` is true, or fails after [`WAIT`].
fn eventually(what: &str, check: impl Fn() -> bool) {
    let deadline = Instant::now() + WAIT;
    while !check() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn body(text: &str) -> NodeUpdate {
    NodeUpdate {
        body: Some(text.into()),
        ..Default::default()
    }
}

#[test]
fn changes_undo_restore_and_purge() {
    let fixture = Fixture::new();
    let client = fixture.client();
    let space = fixture.space(&client, "History");
    let node = client
        .create_node(&space, NewNode::note("Plan", "one"), &ada())
        .unwrap();
    client
        .update_node(&space, &node.id, body("two"), &claude())
        .unwrap();

    // The changes of the node and of the space, newest first.
    let of_node = client.changes_of(&space, &node.id.to_string(), 10).unwrap();
    let actions: Vec<&str> = of_node.iter().map(|c| c.action.as_str()).collect();
    assert_eq!(actions, [actions::NODE_UPDATE, actions::NODE_CREATE]);
    assert_eq!(of_node[0].changed_by, claude());
    let page = client.changes(&space, 1, Some(of_node[0].id)).unwrap();
    assert_eq!(page[0].id, of_node[1].id);
    let detail = client.change(&space, of_node[0].id).unwrap().unwrap();
    assert_eq!(detail.versions[0].body, "two");
    assert_eq!(client.change(&space, 9999).unwrap(), None);
    let both = client.call(
        "changes",
        json!({ "space": space, "subject": node.id.to_string(), "before": 3 }),
    );
    assert_eq!(rpc(both).code, codes::INVALID_PARAMS);

    // Undo, announced to subscribers; once only.
    let listener = fixture.client();
    let events = listener.subscribe(Some(&space)).unwrap();
    let undo = client.undo(&space, of_node[0].id, false, &ada()).unwrap();
    assert_eq!(undo.undoes, Some(of_node[0].id));
    assert_eq!(changed(&events, 1), [(node.id, ChangeKind::Update)]);
    assert_eq!(
        client.get_node(&space, &node.id).unwrap().unwrap().body,
        "one"
    );
    let again = rpc(client.undo(&space, of_node[0].id, false, &ada()));
    assert_eq!(
        (again.code, again.kind()),
        (codes::ALREADY_UNDONE, Some("already_undone"))
    );
    assert_eq!(again.data.unwrap()["undone_by"], undo.id);

    // Undoing the create would overwrite the later changes: refused unless forced.
    let conflict = rpc(client.undo(&space, of_node[1].id, false, &claude()));
    assert_eq!(
        (conflict.code, conflict.kind()),
        (codes::UNDO_CONFLICT, Some("undo_conflict"))
    );
    assert!(
        !conflict.data.unwrap()["later"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let settings = client.call(
        "undo",
        json!({ "space": space, "change": 424242, "by": { "kind": "person", "actor": "Ada" } }),
    );
    assert_eq!(rpc(settings).kind(), Some("not_found"));

    // Restore the second version, then delete and recover the node.
    let history = client.history(&space, &node.id).unwrap();
    let restored = client
        .restore(&space, &node.id, history[1].seq, &ada())
        .unwrap();
    assert_eq!(restored.body, "two");
    assert_eq!(changed(&events, 1), [(node.id, ChangeKind::Update)]);
    client.delete_node(&space, &node.id, &ada()).unwrap();
    changed(&events, 1);
    let deleted = client.history(&space, &node.id).unwrap().pop().unwrap();
    client
        .restore(&space, &node.id, deleted.seq, &ada())
        .unwrap();
    assert_eq!(changed(&events, 1), [(node.id, ChangeKind::Create)]);

    // Only a person purges.
    let refused = rpc(client.purge(&space, &node.id, &claude()));
    assert_eq!(
        (refused.code, refused.kind()),
        (codes::FORBIDDEN, Some("forbidden"))
    );
    let purge = client.purge(&space, &node.id, &ada()).unwrap();
    assert_eq!(purge.action, actions::NODE_PURGE);
    assert_eq!(changed(&events, 1), [(node.id, ChangeKind::Delete)]);
    assert!(client.history(&space, &node.id).unwrap().is_empty());
}

#[test]
fn removing_a_link_records_who_removed_it() {
    let fixture = Fixture::new();
    let client = fixture.client();
    let space = fixture.space(&client, "Links");
    let a = client
        .create_node(&space, NewNode::note("A", ""), &ada())
        .unwrap();
    let b = client
        .create_node(&space, NewNode::note("B", ""), &ada())
        .unwrap();
    client
        .link(&space, &a.id, &b.id, "related", &ada())
        .unwrap();
    assert!(
        client
            .unlink(&space, &a.id, &b.id, "related", &claude())
            .unwrap()
    );
    let last = &client.changes(&space, 1, None).unwrap()[0];
    assert_eq!(last.action, actions::LINK_DELETE);
    assert_eq!(last.changed_by, claude());

    // A client older than v0.2 sends no provenance: the change is Knode's own.
    client
        .link(&space, &a.id, &b.id, "related", &ada())
        .unwrap();
    client
        .call(
            "link.delete",
            json!({ "space": space, "from": a.id, "to": b.id, "kind": "related" }),
        )
        .unwrap();
    let last = &client.changes(&space, 1, None).unwrap()[0];
    assert_eq!(last.changed_by, Provenance::system());
}

#[test]
fn exports_and_imports_markdown() {
    let fixture = Fixture::new();
    let client = fixture.client();
    let space = fixture.space(&client, "Source");
    let plan = client
        .create_node(&space, NewNode::note("Plan", "See [[Goal]]."), &claude())
        .unwrap();
    client
        .create_node(&space, NewNode::note("Goal", "Ship."), &ada())
        .unwrap();
    let dir = fixture.path("export");
    let report = client.export_markdown(&space, &dir).unwrap();
    assert_eq!(report.files, 2);
    assert!(dir.join("Plan.md").exists());
    let again = client.export_markdown(&space, &dir).unwrap();
    assert_eq!((again.written.len(), again.unchanged), (0, 2));

    let copy = fixture.space(&client, "Copy");
    let listener = fixture.client();
    let events = listener.subscribe(Some(&copy)).unwrap();
    let imported = client.import_markdown(&copy, &dir, &ada()).unwrap();
    assert_eq!(imported.created.len(), 2);
    let seen = changed(&events, 2);
    assert!(seen.contains(&(plan.id, ChangeKind::Create)), "{seen:?}");
    let node = client.get_node(&copy, &plan.id).unwrap().unwrap();
    assert_eq!(node.created_by, claude());
    assert_eq!(node.created_at, plan.created_at);

    let relative = client.call(
        "vault.export",
        json!({ "space": space, "dir": "relative/folder" }),
    );
    assert_eq!(rpc(relative).code, codes::INVALID_PARAMS);
    fs::write(fixture.path("file"), "").unwrap();
    let not_a_folder = rpc(client.import_markdown(&copy, fixture.path("file"), &ada()));
    assert_eq!(
        (not_a_folder.code, not_a_folder.kind()),
        (codes::FOLDER, Some("folder"))
    );
}

fn file_with(dir: &Path, name: &str, text: &str) -> bool {
    fs::read_to_string(dir.join(name)).is_ok_and(|t| t.contains(text))
}

#[test]
fn keeps_the_live_markdown_folder_in_step() {
    let mut fixture = Fixture::new();
    let client = fixture.client();
    let space = fixture.space(&client, "Live");
    let early = client
        .create_node(&space, NewNode::note("Early", "Before the folder."), &ada())
        .unwrap();
    assert_eq!(client.markdown_folder(&space).unwrap().folder, None);

    // Only a person turns it on; turning it on writes the whole space.
    let dir = fixture.path("Live notes");
    let refused = rpc(client.set_markdown_folder(&space, Some(&dir), &claude()));
    assert_eq!(refused.kind(), Some("forbidden"));
    let inside = rpc(client.set_markdown_folder(&space, Some(&space.join("x")), &ada()));
    assert_eq!(inside.kind(), Some("invalid"));
    let folder = client
        .set_markdown_folder(&space, Some(&dir), &ada())
        .unwrap();
    assert_eq!(folder.path.as_deref(), Some(dir.as_path()));
    assert_eq!(folder.warning, None);
    assert_eq!(client.markdown_folder(&space).unwrap(), folder);
    assert!(file_with(&dir, "Early.md", "Before the folder."));

    // Changes are written within a few seconds.
    client
        .create_node(&space, NewNode::note("Later", "Written live."), &claude())
        .unwrap();
    eventually("Later.md", || file_with(&dir, "Later.md", "Written live."));
    client
        .update_node(
            &space,
            &early.id,
            NodeUpdate {
                title: Some("Renamed".into()),
                ..Default::default()
            },
            &ada(),
        )
        .unwrap();
    eventually("the rename", || {
        file_with(&dir, "Renamed.md", "Before") && !dir.join("Early.md").exists()
    });

    // What is left is written when the service stops, and the folder catches up with
    // changes made while it was not running when the space opens again.
    client
        .update_node(&space, &early.id, body("At shutdown."), &ada())
        .unwrap();
    drop(client);
    fixture.restart();
    assert!(file_with(&dir, "Renamed.md", "At shutdown."));
    {
        let mut direct = knode_format::Space::open(&space).unwrap();
        direct
            .create_node(NewNode::note("Offline", "Made directly."), &ada())
            .unwrap();
    }
    let client = fixture.client();
    client.open_space(&space).unwrap();
    assert!(file_with(&dir, "Offline.md", "Made directly."));

    // Turned off, the folder stays as it is.
    let off = client.set_markdown_folder(&space, None, &ada()).unwrap();
    assert_eq!(off.folder, None);
    client
        .create_node(&space, NewNode::note("After", "Not written."), &ada())
        .unwrap();
    std::thread::sleep(Duration::from_millis(3500));
    assert!(!dir.join("After.md").exists());
    assert!(dir.join("Offline.md").exists());
}

#[test]
fn warns_about_a_live_folder_in_a_cloud_folder() {
    let fixture = Fixture::new();
    let client = fixture.client();
    let space = fixture.space(&client, "Cloud");
    let dir = fixture.path("Dropbox/Notes");
    let folder = client
        .set_markdown_folder(&space, Some(&dir), &ada())
        .unwrap();
    assert!(folder.warning.unwrap().contains("Dropbox"));
}
