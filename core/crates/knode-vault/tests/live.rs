//! A live Markdown folder follows a space, one way.

mod common;

use std::fs;

use common::{assert_same, put, tree};
use knode_format::{ChangeOp, NewNode, NodeUpdate, Provenance, Space};
use knode_vault::{LiveFolder, export};

fn me() -> Provenance {
    Provenance::person("Ada")
}

fn rename(title: &str) -> NodeUpdate {
    NodeUpdate {
        title: Some(title.into()),
        ..Default::default()
    }
}

#[test]
fn follows_creates_edits_renames_and_deletes() {
    let tmp = tempfile::tempdir().unwrap();
    let mut space = Space::create(tmp.path().join("S.knode"), "S").unwrap();
    let dir = tmp.path().join("Live");
    let mut live = LiveFolder::open(&dir).unwrap();
    live.resync(&space).unwrap();

    let roadmap = space
        .create_node(NewNode::note("Roadmap", "Ship."), &me())
        .unwrap();
    let plan = space
        .create_node(NewNode::note("Plan", "See [[Roadmap]]."), &me())
        .unwrap();
    live.record(roadmap.id, ChangeOp::Create);
    live.record(plan.id, ChangeOp::Create);
    assert_eq!(live.pending(), 2);
    let report = live.flush(&space).unwrap();
    let mut written = report.written.clone();
    written.sort();
    assert_eq!(written, ["Plan.md", "Roadmap.md"]);
    assert_eq!(live.pending(), 0);
    assert!(
        fs::read_to_string(dir.join("Plan.md"))
            .unwrap()
            .ends_with("See [[Roadmap]].")
    );

    // A user's own file in the folder.
    put(&dir, "Mine.md", "Mine.\n");

    // Renaming moves the file and rewrites the files that link to it, even if only the renamed
    // node is reported.
    space
        .update_node(&roadmap.id, rename("Roadmap 2026"), &me())
        .unwrap();
    live.record(roadmap.id, ChangeOp::Update);
    let report = live.flush(&space).unwrap();
    assert_eq!(report.removed, ["Roadmap.md"]);
    let mut written = report.written.clone();
    written.sort();
    assert_eq!(written, ["Plan.md", "Roadmap 2026.md"]);
    assert!(!dir.join("Roadmap.md").exists());
    assert!(
        fs::read_to_string(dir.join("Plan.md"))
            .unwrap()
            .ends_with("See [[Roadmap 2026]].")
    );

    // An edit writes one file.
    space
        .update_node(
            &plan.id,
            NodeUpdate {
                body: Some("Done.".into()),
                ..Default::default()
            },
            &me(),
        )
        .unwrap();
    live.record(plan.id, ChangeOp::Update);
    let report = live.flush(&space).unwrap();
    assert_eq!(report.written, ["Plan.md"]);
    assert!(report.removed.is_empty());

    // Deleting removes the file.
    space.delete_node(&plan.id, &me()).unwrap();
    live.record(plan.id, ChangeOp::Delete);
    let report = live.flush(&space).unwrap();
    assert_eq!(report.removed, ["Plan.md"]);
    assert!(!dir.join("Plan.md").exists());

    // The user's file is still there, and the folder is what an export writes.
    assert_eq!(fs::read_to_string(dir.join("Mine.md")).unwrap(), "Mine.\n");
    let exported = tmp.path().join("Export");
    export(&space, &exported).unwrap();
    let mut live_files = tree(&dir);
    live_files.remove("Mine.md");
    live_files.remove(".knode/live.json");
    assert_same(&tree(&exported), &live_files);
}

#[test]
fn never_removes_a_file_someone_else_changed() {
    let tmp = tempfile::tempdir().unwrap();
    let mut space = Space::create(tmp.path().join("S.knode"), "S").unwrap();
    let dir = tmp.path().join("Live");
    let mut live = LiveFolder::open(&dir).unwrap();
    let plan = space
        .create_node(NewNode::note("Plan", "Ours."), &me())
        .unwrap();
    live.resync(&space).unwrap();

    // Someone replaces the file with their own, without an id: it becomes a node of its own.
    put(&dir, "Plan.md", "Theirs now.\n");
    let pulled = live.pull(&mut space, &me()).unwrap();
    assert_eq!(pulled.created, ["Plan.md"]);
    let theirs = fs::read_to_string(dir.join("Plan.md")).unwrap();
    assert!(theirs.starts_with("---\nid: ") && theirs.ends_with("---\nTheirs now.\n"));
    space
        .update_node(&plan.id, rename("Roadmap"), &me())
        .unwrap();
    live.record(plan.id, ChangeOp::Update);
    let report = live.flush(&space).unwrap();
    assert!(report.removed.is_empty());
    assert_eq!(fs::read_to_string(dir.join("Plan.md")).unwrap(), theirs);
    assert!(dir.join("Roadmap.md").exists());

    // And a new node named like it does not overwrite it.
    let other = space
        .create_node(NewNode::note("Plan", "Another plan."), &me())
        .unwrap();
    live.record(other.id, ChangeOp::Create);
    let report = live.flush(&space).unwrap();
    assert_eq!(report.written, ["Plan 2.md"]);
    assert_eq!(fs::read_to_string(dir.join("Plan.md")).unwrap(), theirs);
}

#[test]
fn resync_catches_up_with_missed_changes() {
    let tmp = tempfile::tempdir().unwrap();
    let mut space = Space::create(tmp.path().join("S.knode"), "S").unwrap();
    let dir = tmp.path().join("Live");
    let mut live = LiveFolder::open(&dir).unwrap();
    let a = space.create_node(NewNode::note("A", "a"), &me()).unwrap();
    live.resync(&space).unwrap();

    // Changes nobody reported.
    space.update_node(&a.id, rename("B"), &me()).unwrap();
    space.create_node(NewNode::note("C", "c"), &me()).unwrap();
    let report = live.resync(&space).unwrap();
    assert_eq!(report.removed, ["A.md"]);
    let mut written = report.written.clone();
    written.sort();
    assert_eq!(written, ["B.md", "C.md"]);

    // A new LiveFolder on the same folder keeps the names, from .knode/files.json.
    let mut again = LiveFolder::open(&dir).unwrap();
    let report = again.resync(&space).unwrap();
    assert!(report.written.is_empty());
    assert_eq!(report.unchanged, 2);
}
