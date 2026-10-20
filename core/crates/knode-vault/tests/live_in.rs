//! A live Markdown folder brings the edits made to its files back in.

mod common;

use std::fs;
use std::path::Path;
use std::time::{Duration, SystemTime};

use common::put;
use knode_format::{ActorKind, ChangeOp, NewNode, Node, NodeUpdate, Provenance, Space};
use knode_sync::{BodyEdit, Docs};
use knode_vault::{LiveFolder, property_keys};

fn me() -> Provenance {
    Provenance::person("Ada")
}

fn agent() -> Provenance {
    Provenance::agent("claude-code")
}

struct Setup {
    _tmp: tempfile::TempDir,
    space: Space,
    dir: std::path::PathBuf,
    live: LiveFolder,
}

/// A space with a live folder, written once.
fn setup(nodes: &[(&str, &str)]) -> (Setup, Vec<Node>) {
    let tmp = tempfile::tempdir().unwrap();
    let mut space = Space::create(tmp.path().join("S.knode"), "S").unwrap();
    let made = nodes
        .iter()
        .map(|(title, body)| {
            space
                .create_node(NewNode::note(*title, *body), &agent())
                .unwrap()
        })
        .collect();
    let dir = tmp.path().join("Live");
    let mut live = LiveFolder::open(&dir).unwrap().with_source_root("~/Live");
    live.resync(&space).unwrap();
    (
        Setup {
            _tmp: tmp,
            space,
            dir,
            live,
        },
        made,
    )
}

fn read(dir: &Path, rel: &str) -> String {
    fs::read_to_string(dir.join(rel)).unwrap()
}

/// Edits a file as another editor would: replaces `from` with `to`.
fn edit(dir: &Path, rel: &str, from: &str, to: &str) {
    let text = read(dir, rel);
    assert!(text.contains(from), "{rel} has no {from:?}:\n{text}");
    fs::write(dir.join(rel), text.replacen(from, to, 1)).unwrap();
}

fn set_modified(dir: &Path, rel: &str, when: SystemTime) {
    fs::File::options()
        .write(true)
        .open(dir.join(rel))
        .unwrap()
        .set_modified(when)
        .unwrap();
}

fn body(space: &Space, node: &Node) -> String {
    space.get_node(&node.id).unwrap().unwrap().body
}

fn update_body(space: &mut Space, node: &Node, body: &str) {
    space
        .update_node(
            &node.id,
            NodeUpdate {
                body: Some(body.into()),
                ..Default::default()
            },
            &agent(),
        )
        .unwrap();
}

#[test]
fn an_edit_in_the_folder_is_one_more_edit_with_history() {
    let (mut s, nodes) = setup(&[("Plan", "Draft.")]);
    let plan = &nodes[0];
    edit(&s.dir, "Plan.md", "Draft.", "Ship it.");

    let report = s.live.pull(&mut s.space, &me()).unwrap();
    assert_eq!(report.updated, ["Plan.md"]);
    assert!(report.merged.is_empty());
    assert_eq!(report.changes, [(plan.id, ChangeOp::Update)]);
    assert_eq!(body(&s.space, plan), "Ship it.");

    let history = s.space.history(&plan.id).unwrap();
    assert_eq!(history.len(), 2);
    let last = history.iter().max_by_key(|h| h.seq).unwrap();
    assert_eq!(last.body, "Ship it.");
    assert_eq!(last.changed_by.kind, ActorKind::Person);
    assert_eq!(last.changed_by.actor, "Ada");
    assert_eq!(last.changed_by.source.as_deref(), Some("~/Live/Plan.md"));

    // The node is written back with its new frontmatter, and that write is not read again.
    assert_eq!(s.live.pending(), 1);
    s.live.flush(&s.space).unwrap();
    assert!(read(&s.dir, "Plan.md").contains("updated_by:\n  kind: person\n  actor: Ada\n"));
    let again = s.live.pull(&mut s.space, &me()).unwrap();
    assert!(again.is_empty(), "{again:?}");
    assert_eq!(s.space.history(&plan.id).unwrap().len(), 2);
}

#[test]
fn knodes_own_writes_are_not_read_back() {
    let (mut s, nodes) = setup(&[("Plan", "Draft."), ("Notes", "See [[Plan]].")]);
    // Nothing changed in the folder since it was written.
    assert!(s.live.pull(&mut s.space, &me()).unwrap().is_empty());

    // Knode renames a node and edits another: the files it writes, moves and removes are its own.
    s.space
        .update_node(
            &nodes[0].id,
            NodeUpdate {
                title: Some("Roadmap".into()),
                ..Default::default()
            },
            &agent(),
        )
        .unwrap();
    s.live.record(nodes[0].id, ChangeOp::Update);
    s.live.flush(&s.space).unwrap();
    assert!(s.dir.join("Roadmap.md").exists());
    let report = s.live.pull(&mut s.space, &me()).unwrap();
    assert!(report.is_empty(), "{report:?}");

    // A new LiveFolder on the same folder remembers what Knode wrote.
    let mut again = LiveFolder::open(&s.dir).unwrap();
    let report = again.start(&mut s.space, &me()).unwrap();
    assert!(report.is_empty(), "{report:?}");
    assert_eq!(s.space.history(&nodes[0].id).unwrap().len(), 2);
}

#[test]
fn a_file_changed_and_not_read_yet_is_never_overwritten() {
    let (mut s, nodes) = setup(&[("Plan", "Draft.")]);
    let plan = &nodes[0];
    edit(&s.dir, "Plan.md", "Draft.", "Edited in the folder.");
    // Knode changes the node too, and its batch is written before the folder is read.
    s.space
        .set_property(&plan.id, "status", "open", &agent())
        .unwrap();
    s.live.record(plan.id, ChangeOp::Update);
    let report = s.live.flush(&s.space).unwrap();
    assert!(report.written.is_empty());
    assert!(read(&s.dir, "Plan.md").ends_with("Edited in the folder."));
    assert!(
        s.live
            .is_pull_due_at(std::time::Instant::now() + Duration::from_secs(5))
    );
    assert_eq!(s.live.pending(), 1);

    s.live.pull(&mut s.space, &me()).unwrap();
    assert_eq!(body(&s.space, plan), "Edited in the folder.");
    s.live.flush(&s.space).unwrap();
    let text = read(&s.dir, "Plan.md");
    assert!(text.contains("status: open") && text.ends_with("Edited in the folder."));
}

#[test]
fn edits_on_both_sides_to_different_fields_are_both_kept() {
    let (mut s, nodes) = setup(&[("Plan", "Draft.")]);
    let plan = &nodes[0];
    // The folder changes the tags, Knode the body.
    edit(
        &s.dir,
        "Plan.md",
        "kind: note\n",
        "kind: note\ntags: [work]\n",
    );
    update_body(&mut s.space, plan, "Draft two.");

    let report = s.live.pull(&mut s.space, &me()).unwrap();
    assert_eq!(report.merged, ["Plan.md"]);
    let node = s.space.get_node(&plan.id).unwrap().unwrap();
    assert_eq!(node.body, "Draft two.");
    assert_eq!(node.tags, ["work"]);
}

#[test]
fn a_body_edited_on_both_sides_goes_to_the_last_writer_and_keeps_the_other_in_history() {
    let (mut s, nodes) = setup(&[("Plan", "Draft."), ("Ideas", "Some.")]);
    let (plan, ideas) = (&nodes[0], &nodes[1]);

    // The folder edited Plan first, Knode last: Knode's body wins, the file's is in history.
    edit(&s.dir, "Plan.md", "Draft.", "Folder draft.");
    set_modified(
        &s.dir,
        "Plan.md",
        SystemTime::now() - Duration::from_secs(60),
    );
    update_body(&mut s.space, plan, "Knode draft.");
    // Knode edited Ideas first, the folder last: the file's body wins.
    update_body(&mut s.space, ideas, "Knode ideas.");
    edit(&s.dir, "Ideas.md", "Some.", "Folder ideas.");
    set_modified(
        &s.dir,
        "Ideas.md",
        SystemTime::now() + Duration::from_secs(60),
    );

    let mut report = s.live.pull(&mut s.space, &me()).unwrap();
    report.merged.sort();
    assert_eq!(report.merged, ["Ideas.md", "Plan.md"]);

    assert_eq!(body(&s.space, plan), "Knode draft.");
    let bodies: Vec<String> = s
        .space
        .history(&plan.id)
        .unwrap()
        .into_iter()
        .map(|h| h.body)
        .collect();
    assert!(bodies.contains(&"Folder draft.".to_string()), "{bodies:?}");

    assert_eq!(body(&s.space, ideas), "Folder ideas.");
    let bodies: Vec<String> = s
        .space
        .history(&ideas.id)
        .unwrap()
        .into_iter()
        .map(|h| h.body)
        .collect();
    assert!(bodies.contains(&"Knode ideas.".to_string()), "{bodies:?}");

    // Both files are written again with the bodies that won.
    s.live.flush(&s.space).unwrap();
    assert!(read(&s.dir, "Plan.md").ends_with("Knode draft."));
    assert!(read(&s.dir, "Ideas.md").ends_with("Folder ideas."));
}

#[test]
fn a_body_edited_on_both_sides_merges_through_loro() {
    let tmp = tempfile::tempdir().unwrap();
    let mut space = Space::create(tmp.path().join("S.knode"), "S").unwrap();
    let plan = space
        .create_node(
            NewNode::note("Plan", "Line one.\nLine two.\nLine three.\n"),
            &agent(),
        )
        .unwrap();
    let mut docs = Docs::new();
    docs.open_doc(&mut space, &plan.id).unwrap();
    let dir = tmp.path().join("Live");
    let mut live = LiveFolder::open(&dir).unwrap();
    live.resync(&space).unwrap();

    // An agent edits the end in Knode, a person the start in the folder.
    let at = "Line one.\nLine two.\nLine three.\n".chars().count();
    docs.apply_local_edit(
        &mut space,
        &plan.id,
        BodyEdit::insert(at, "Line four.\n"),
        &agent(),
    )
    .unwrap();
    edit(&dir, "Plan.md", "Line one.", "Line 1.");

    let report = live.pull(&mut space, &me()).unwrap();
    assert_eq!(report.merged, ["Plan.md"]);
    assert_eq!(
        body(&space, &plan),
        "Line 1.\nLine two.\nLine three.\nLine four.\n"
    );
    live.flush(&space).unwrap();
    assert!(read(&dir, "Plan.md").ends_with("Line 1.\nLine two.\nLine three.\nLine four.\n"));
}

#[test]
fn a_new_file_becomes_a_node_and_gets_its_id() {
    let (mut s, _) = setup(&[("Plan", "Draft.")]);
    put(&s.dir, "Projects/Idea.md", "Just an [[Plan|idea]].\n");

    let report = s.live.pull(&mut s.space, &me()).unwrap();
    assert_eq!(report.created, ["Projects/Idea.md"]);
    let (id, op) = report.changes[0];
    assert_eq!(op, ChangeOp::Create);
    let node = s.space.get_node(&id).unwrap().unwrap();
    assert_eq!(node.title, "Idea");
    assert_eq!(node.body, "Just an [[Plan|idea]].\n");
    assert_eq!(
        node.created_by.source.as_deref(),
        Some("~/Live/Projects/Idea.md")
    );
    assert_eq!(s.space.links_from(&id).unwrap().len(), 1);
    assert_eq!(
        read(&s.dir, "Projects/Idea.md"),
        format!("---\nid: {id}\n---\nJust an [[Plan|idea]].\n")
    );

    // Written back as it is, and never read twice.
    let flushed = s.live.flush(&s.space).unwrap();
    assert!(flushed.written.is_empty(), "{flushed:?}");
    assert!(s.live.pull(&mut s.space, &me()).unwrap().is_empty());
    assert_eq!(s.space.node_count().unwrap(), 2);
}

#[test]
fn a_copied_file_becomes_a_node_of_its_own() {
    let (mut s, nodes) = setup(&[("Plan", "Draft.")]);
    fs::copy(s.dir.join("Plan.md"), s.dir.join("Plan copy.md")).unwrap();

    let report = s.live.pull(&mut s.space, &me()).unwrap();
    assert_eq!(report.created, ["Plan copy.md"]);
    let (copy, _) = report.changes[0];
    assert_ne!(copy, nodes[0].id);
    assert!(read(&s.dir, "Plan copy.md").starts_with(&format!("---\nid: {copy}\n")));
    assert_eq!(
        s.space.get_node(&nodes[0].id).unwrap().unwrap().title,
        "Plan"
    );
    s.live.flush(&s.space).unwrap();
    assert!(s.live.pull(&mut s.space, &me()).unwrap().is_empty());
}

#[test]
fn a_deleted_file_marks_its_node_and_never_deletes_it() {
    let (mut s, nodes) = setup(&[("Plan", "Draft."), ("Other", "Kept.")]);
    let plan = &nodes[0];
    fs::remove_file(s.dir.join("Plan.md")).unwrap();

    let report = s.live.pull(&mut s.space, &me()).unwrap();
    assert_eq!(report.deleted, ["Plan.md"]);
    assert_eq!(report.changes, [(plan.id, ChangeOp::Update)]);
    assert!(s.space.get_node(&plan.id).unwrap().is_some());
    assert_eq!(
        s.space
            .property(&plan.id, property_keys::DELETED)
            .unwrap()
            .as_deref(),
        Some("Plan.md")
    );
    assert_eq!(
        s.live.deleted_in_folder(&s.space).unwrap(),
        [(plan.id, "Plan.md".to_string())]
    );

    // The node is not written back while it waits, even when it changes.
    update_body(&mut s.space, plan, "Changed meanwhile.");
    s.live.record(plan.id, ChangeOp::Update);
    s.live.resync(&s.space).unwrap();
    assert!(!s.dir.join("Plan.md").exists());
    assert!(s.live.pull(&mut s.space, &me()).unwrap().is_empty());

    // Keeping it writes its file again. A pull before the flush does not mark it again.
    s.space
        .remove_property(&plan.id, property_keys::DELETED)
        .unwrap();
    s.live.record(plan.id, ChangeOp::Update);
    assert!(s.live.pull(&mut s.space, &me()).unwrap().is_empty());
    assert_eq!(
        s.space.property(&plan.id, property_keys::DELETED).unwrap(),
        None
    );
    s.live.flush(&s.space).unwrap();
    assert!(read(&s.dir, "Plan.md").ends_with("Changed meanwhile."));
    assert!(s.live.pull(&mut s.space, &me()).unwrap().is_empty());
}

#[test]
fn a_renamed_or_moved_file_renames_or_moves_its_node() {
    let (mut s, nodes) = setup(&[("Plan", "Draft."), ("Notes", "See [[Plan]].")]);
    let plan = &nodes[0];
    fs::rename(s.dir.join("Plan.md"), s.dir.join("Big plan.md")).unwrap();

    let report = s.live.pull(&mut s.space, &me()).unwrap();
    assert_eq!(report.renamed, ["Big plan.md"]);
    assert!(report.deleted.is_empty());
    assert_eq!(
        s.space.get_node(&plan.id).unwrap().unwrap().title,
        "Big plan"
    );
    s.live.flush(&s.space).unwrap();
    assert!(!s.dir.join("Plan.md").exists());
    assert!(read(&s.dir, "Notes.md").ends_with("See [[Big plan]]."));

    // Moving it into a folder keeps the title.
    fs::create_dir_all(s.dir.join("Work")).unwrap();
    fs::rename(s.dir.join("Big plan.md"), s.dir.join("Work/Big plan.md")).unwrap();
    s.live.pull(&mut s.space, &me()).unwrap();
    s.live.flush(&s.space).unwrap();
    assert_eq!(
        s.space.get_node(&plan.id).unwrap().unwrap().title,
        "Big plan"
    );
    assert!(s.dir.join("Work/Big plan.md").exists());
    assert!(!s.dir.join("Big plan.md").exists());
    assert!(s.live.pull(&mut s.space, &me()).unwrap().is_empty());
    assert_eq!(s.space.node_count().unwrap(), 2);
}

#[test]
fn obsidian_style_edits_keep_their_frontmatter() {
    let (mut s, _) = setup(&[]);
    put(
        &s.dir,
        "Daily.md",
        "---\naliases: [today]\ntags: [journal]\ncssclasses: wide\n---\nWoke up. #morning\n",
    );
    let report = s.live.pull(&mut s.space, &me()).unwrap();
    let (id, _) = report.changes[0];
    s.live.flush(&s.space).unwrap();
    let written = read(&s.dir, "Daily.md");
    assert_eq!(
        written,
        format!(
            "---\nid: {id}\naliases: [today]\ntags: [journal]\ncssclasses: wide\n---\nWoke up. #morning\n"
        )
    );

    // Obsidian edits the body and adds to the frontmatter.
    edit(
        &s.dir,
        "Daily.md",
        "cssclasses: wide\n",
        "cssclasses: wide\nmood: good\n",
    );
    edit(&s.dir, "Daily.md", "Woke up.", "Woke up early.");
    let report = s.live.pull(&mut s.space, &me()).unwrap();
    assert_eq!(report.updated, ["Daily.md"]);
    let node = s.space.get_node(&id).unwrap().unwrap();
    assert_eq!(node.body, "Woke up early. #morning\n");
    assert_eq!(node.tags, ["journal", "morning"]);
    let before = read(&s.dir, "Daily.md");
    let flushed = s.live.flush(&s.space).unwrap();
    assert!(flushed.written.is_empty(), "{flushed:?}");
    assert_eq!(read(&s.dir, "Daily.md"), before);
    assert!(before.contains("mood: good\n") && before.contains("aliases: [today]\n"));
    assert!(s.live.pull(&mut s.space, &me()).unwrap().is_empty());
}

#[test]
fn edits_made_while_knode_was_not_running_come_in_at_start() {
    let (mut s, nodes) = setup(&[("Plan", "Draft.")]);
    drop(s.live);
    edit(&s.dir, "Plan.md", "Draft.", "Edited offline.");

    let mut live = LiveFolder::open(&s.dir).unwrap();
    let report = live.start(&mut s.space, &me()).unwrap();
    assert_eq!(report.updated, ["Plan.md"]);
    assert_eq!(body(&s.space, &nodes[0]), "Edited offline.");
    assert!(read(&s.dir, "Plan.md").ends_with("Edited offline."));
}

#[test]
fn the_watcher_sees_changes_in_the_folder() {
    let (mut s, nodes) = setup(&[("Plan", "Draft.")]);
    let mut live = LiveFolder::open(&s.dir)
        .unwrap()
        .with_debounce(Duration::from_millis(50));
    live.watch().unwrap();
    edit(&s.dir, "Plan.md", "Draft.", "Seen.");
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while !live.is_pull_due() {
        assert!(std::time::Instant::now() < deadline, "no event");
        std::thread::sleep(Duration::from_millis(20));
    }
    let report = live.tick(&mut s.space, &me(), false).unwrap();
    assert_eq!(report.updated, ["Plan.md"]);
    assert_eq!(body(&s.space, &nodes[0]), "Seen.");
    drop(s.live);
}
