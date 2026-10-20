//! The live Markdown folder works both ways through a real service: edits made to its files
//! come back in, announced to subscribers.

use std::fs;
use std::path::Path;
use std::time::{Duration, Instant};

use knode_format::{ActorKind, NewNode, Provenance};
use knode_service::{ChangeKind, Client, Config, DocSplice, Event, start};

const WAIT: Duration = Duration::from_secs(15);

fn ada() -> Provenance {
    Provenance::person("Ada")
}

/// Waits until `check` is true, or fails after [`WAIT`].
fn eventually(what: &str, check: impl Fn() -> bool) {
    let deadline = Instant::now() + WAIT;
    while !check() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn file_with(dir: &Path, name: &str, text: &str) -> bool {
    fs::read_to_string(dir.join(name)).is_ok_and(|t| t.contains(text))
}

#[test]
fn edits_made_in_the_folder_come_back_in() {
    let tmp = tempfile::tempdir().unwrap();
    let service = start(Config::new(tmp.path().join("home"))).unwrap();
    let client = Client::connect(tmp.path().join("home/service.sock")).unwrap();
    let space = client
        .create_space(tmp.path().join("Live.knode"), "Live")
        .unwrap()
        .path;
    let plan = client
        .create_node(&space, NewNode::note("Plan", "Draft."), &ada())
        .unwrap();
    let dir = tmp.path().join("Live notes");
    client
        .set_markdown_folder(&space, Some(&dir), &ada())
        .unwrap();
    assert!(file_with(&dir, "Plan.md", "Draft."));
    let events = client.subscribe(Some(&space)).unwrap();

    // An edit in another editor is one more edit of the node, by the folder's owner.
    let text = fs::read_to_string(dir.join("Plan.md")).unwrap();
    fs::write(
        dir.join("Plan.md"),
        text.replace("Draft.", "Edited in the folder."),
    )
    .unwrap();
    eventually("the edit", || {
        client.get_node(&space, &plan.id).unwrap().unwrap().body == "Edited in the folder."
    });
    let deadline = Instant::now() + WAIT;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        match events.recv_timeout(left) {
            Some(Event::Changed(c)) if c.node_id == plan.id => {
                assert_eq!(c.op, ChangeKind::Update);
                break;
            }
            Some(_) => {}
            None => panic!("no change announced"),
        }
    }
    let history = client.history(&space, &plan.id).unwrap();
    let last = history.iter().max_by_key(|h| h.seq).unwrap();
    assert_eq!(last.changed_by.kind, ActorKind::Person);
    assert!(
        last.changed_by
            .source
            .as_deref()
            .is_some_and(|s| s.ends_with("Live notes/Plan.md"))
    );
    // Written back with its new frontmatter, and not read in again.
    eventually("the frontmatter", || {
        file_with(&dir, "Plan.md", "updated_by:\n  kind: person")
    });
    std::thread::sleep(Duration::from_millis(2500));
    assert_eq!(
        client.history(&space, &plan.id).unwrap().len(),
        history.len()
    );

    // A new file becomes a node.
    fs::write(dir.join("Idea.md"), "A new idea.\n").unwrap();
    eventually("the new node", || {
        !client.find_by_title(&space, "Idea").unwrap().is_empty()
    });
    eventually("the id of the new file", || {
        file_with(&dir, "Idea.md", "id: ")
    });

    drop(events);
    drop(client);
    service.stop().unwrap();
}

#[test]
fn a_folder_edit_merges_with_an_open_editor_through_the_document() {
    let tmp = tempfile::tempdir().unwrap();
    let service = start(Config::new(tmp.path().join("home"))).unwrap();
    let client = Client::connect(tmp.path().join("home/service.sock")).unwrap();
    let space = client
        .create_space(tmp.path().join("Live.knode"), "Live")
        .unwrap()
        .path;
    let plan = client
        .create_node(
            &space,
            NewNode::note("Plan", "First line.\n\nSecond line."),
            &ada(),
        )
        .unwrap();
    // The note is open in an editor before the folder is written, so the file holds the
    // document's version.
    let open = client.open_doc(&space, &plan.id).unwrap();
    let dir = tmp.path().join("Live notes");
    client
        .set_markdown_folder(&space, Some(&dir), &ada())
        .unwrap();
    assert!(file_with(&dir, "Plan.md", "Second line."));
    let events = client.subscribe(Some(&space)).unwrap();

    // The person types in the editor while the file is edited elsewhere.
    let typed = client
        .edit_doc(
            &space,
            &plan.id,
            Some(&open.version),
            &[DocSplice {
                at: 0,
                delete: 0,
                insert: "Today: ".into(),
            }],
            &ada(),
        )
        .unwrap();
    let text = fs::read_to_string(dir.join("Plan.md")).unwrap();
    fs::write(
        dir.join("Plan.md"),
        text.replace("Second line.", "Second line, edited in the folder."),
    )
    .unwrap();

    let merged = "Today: First line.\n\nSecond line, edited in the folder.";
    eventually("the merge", || {
        client.get_node(&space, &plan.id).unwrap().unwrap().body == merged
    });
    // Open editors are told the document's new version.
    let deadline = Instant::now() + WAIT;
    let change = loop {
        let left = deadline.saturating_duration_since(Instant::now());
        match events.recv_timeout(left) {
            Some(Event::Changed(c)) if c.node_id == plan.id && c.version.is_some() => {
                if c.version.as_deref() != Some(typed.version.as_str()) {
                    break c;
                }
            }
            Some(_) => {}
            None => panic!("no change with the document's version"),
        }
    };
    assert_eq!(change.op, ChangeKind::Update);
    assert_eq!(
        change.by.as_ref().map(|by| by.kind),
        Some(ActorKind::Person)
    );
    // The editor goes on from the version it had: the service's document has the folder edit.
    let again = client
        .edit_doc(
            &space,
            &plan.id,
            Some(&typed.version),
            &[DocSplice {
                at: 0,
                delete: 0,
                insert: "Note. ".into(),
            }],
            &ada(),
        )
        .unwrap();
    assert_eq!(
        again.text.as_deref(),
        Some(format!("Note. {merged}").as_str())
    );
    let doc = client.open_doc(&space, &plan.id).unwrap();
    assert_eq!(doc.text, format!("Note. {merged}"));
    assert_eq!(doc.version, again.version);

    drop(events);
    drop(client);
    service.stop().unwrap();
}
