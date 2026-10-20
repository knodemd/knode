//! Changes, undo, restore and purge (schema version 3), through the public API.

use std::path::Path;

use knode_format::{
    ActorKind, ChangeOp, Error, FolderSpec, GrantLevel, ImportedNode, NewNode, NodeId, NodeUpdate,
    Properties, Provenance, SCHEMA_VERSION, Space, Timestamp, actions, folder_kinds, link_kinds,
    setting_keys,
};
use tempfile::TempDir;

fn new_space() -> (TempDir, Space) {
    let dir = tempfile::tempdir().unwrap();
    let space = Space::create(dir.path().join("Test.knode"), "Test").unwrap();
    (dir, space)
}

fn ada() -> Provenance {
    Provenance::person("Ada")
}

fn claude() -> Provenance {
    Provenance::agent("claude-code").with_session("5f1c")
}

fn set_body(body: &str) -> NodeUpdate {
    NodeUpdate {
        body: Some(body.into()),
        ..Default::default()
    }
}

fn rename(title: &str) -> NodeUpdate {
    NodeUpdate {
        title: Some(title.into()),
        ..Default::default()
    }
}

fn last_change(space: &Space) -> i64 {
    space.changes(1, None).unwrap()[0].id
}

fn body(space: &Space, id: &knode_format::NodeId) -> String {
    space.get_node(id).unwrap().unwrap().body
}

// Changes

#[test]
fn every_write_is_one_change() {
    let (_dir, mut space) = new_space();
    let a = space
        .create_node(NewNode::note("A", "one"), &ada())
        .unwrap();
    let b = space
        .create_node(NewNode::note("B", ""), &claude())
        .unwrap();
    space
        .update_node(&a.id, set_body("two"), &claude())
        .unwrap();
    space
        .link(&a.id, &b.id, link_kinds::RELATED, &ada())
        .unwrap();
    // Linking twice and removing a missing link record nothing.
    space
        .link(&a.id, &b.id, link_kinds::RELATED, &ada())
        .unwrap();
    assert!(
        !space
            .unlink_by(&a.id, &b.id, link_kinds::PART_OF, &ada())
            .unwrap()
    );
    // An update that changes nothing records nothing.
    space.update_node(&a.id, set_body("two"), &ada()).unwrap();
    assert!(
        space
            .unlink_by(&a.id, &b.id, link_kinds::RELATED, &claude())
            .unwrap()
    );
    space.delete_node(&b.id, &ada()).unwrap();

    let changes = space.changes(100, None).unwrap();
    let rows: Vec<_> = changes
        .iter()
        .rev()
        .map(|c| (c.action.as_str(), c.subject.clone(), c.changed_by.kind))
        .collect();
    let (a_id, b_id) = (a.id.to_string(), b.id.to_string());
    assert_eq!(
        rows,
        [
            (actions::NODE_CREATE, a_id.clone(), ActorKind::Person),
            (actions::NODE_CREATE, b_id.clone(), ActorKind::Agent),
            (actions::NODE_UPDATE, a_id.clone(), ActorKind::Agent),
            (actions::LINK_CREATE, a_id.clone(), ActorKind::Person),
            (actions::LINK_DELETE, a_id, ActorKind::Agent),
            (actions::NODE_DELETE, b_id, ActorKind::Person),
        ]
    );
    assert!(changes.windows(2).all(|w| w[0].id > w[1].id));
    assert!(
        changes
            .iter()
            .all(|c| c.undoes.is_none() && c.detail == serde_json::json!({}))
    );

    // Every version points at its change, with the same time and author.
    for version in space.history(&a.id).unwrap() {
        let change = space.change(version.change_id.unwrap()).unwrap().unwrap();
        assert_eq!(change.change.changed_at, version.changed_at);
        assert_eq!(change.change.changed_by, version.changed_by);
        assert_eq!(version.restored_from, None);
    }

    // Links made and removed by hand are kept; mentions are not.
    let links = space.link_history(&a.id).unwrap();
    let ops: Vec<_> = links.iter().map(|l| (l.op, l.kind.as_str())).collect();
    assert_eq!(
        ops,
        [(ChangeOp::Create, "related"), (ChangeOp::Delete, "related")]
    );

    // Paging.
    let first = space.changes(2, None).unwrap();
    let next = space.changes(2, Some(first[1].id)).unwrap();
    assert_eq!(next[0].id, changes[2].id);
    assert_eq!(space.changes_of(&b.id.to_string(), 10).unwrap().len(), 2);
    assert!(space.change(9999).unwrap().is_none());
}

#[test]
fn unlink_without_provenance_is_recorded_as_knode() {
    let (_dir, mut space) = new_space();
    let a = space.create_node(NewNode::note("A", ""), &ada()).unwrap();
    let b = space.create_node(NewNode::note("B", ""), &ada()).unwrap();
    space.link(&a.id, &b.id, "part-of", &ada()).unwrap();
    assert!(space.unlink(&a.id, &b.id, "part-of").unwrap());
    let change = &space.changes(1, None).unwrap()[0];
    assert_eq!(change.action, actions::LINK_DELETE);
    assert_eq!(change.changed_by, Provenance::system());
}

#[test]
fn a_delete_keeps_the_links_made_by_hand() {
    let (_dir, mut space) = new_space();
    let a = space
        .create_node(NewNode::note("A", "see [[B]]"), &ada())
        .unwrap();
    let b = space.create_node(NewNode::note("B", ""), &ada()).unwrap();
    let c = space.create_node(NewNode::note("C", ""), &ada()).unwrap();
    space
        .link(&a.id, &b.id, link_kinds::PART_OF, &ada())
        .unwrap();
    space
        .link(&c.id, &b.id, link_kinds::RELATED, &ada())
        .unwrap();
    space.delete_node(&b.id, &ada()).unwrap();
    let detail = space.change(last_change(&space)).unwrap().unwrap();
    assert_eq!(detail.versions.len(), 1);
    assert_eq!(detail.versions[0].op, ChangeOp::Delete);
    let removed: Vec<_> = detail
        .links
        .iter()
        .map(|l| (l.op, l.from, l.kind.as_str()))
        .collect();
    assert_eq!(
        removed,
        [
            (ChangeOp::Delete, a.id, "part-of"),
            (ChangeOp::Delete, c.id, "related"),
        ]
    );
}

#[test]
fn a_rename_is_one_change_with_the_bodies_it_rewrote() {
    let (_dir, mut space) = new_space();
    let target = space.create_node(NewNode::note("Old", ""), &ada()).unwrap();
    let source = space
        .create_node(NewNode::note("S", "see [[Old]]"), &ada())
        .unwrap();
    space
        .update_node(&target.id, rename("New"), &ada())
        .unwrap();
    let detail = space.change(last_change(&space)).unwrap().unwrap();
    let nodes: Vec<_> = detail.versions.iter().map(|v| v.node_id).collect();
    assert_eq!(nodes, [target.id, source.id]);
}

#[test]
fn a_run_of_session_updates_is_one_change() {
    let (_dir, mut space) = new_space();
    let node = space
        .create_node(NewNode::note("Live", "a"), &claude())
        .unwrap();
    for body in ["ab", "abc", "abcd"] {
        space
            .update_node_coalescing(&node.id, set_body(body), &claude())
            .unwrap();
    }
    let changes = space.changes(10, None).unwrap();
    assert_eq!(changes.len(), 2, "the run replaced its change each time");
    let history = space.history(&node.id).unwrap();
    assert_eq!(history.len(), 2);
    assert_eq!(history[1].change_id, Some(changes[0].id));
    assert_eq!(changes[0].action, actions::NODE_UPDATE);

    // Undoing the run puts back the version before it.
    space.undo(changes[0].id, &ada(), false).unwrap();
    assert_eq!(body(&space, &node.id), "a");
    // The undo ends the run: the next update adds a version.
    space
        .update_node_coalescing(&node.id, set_body("x"), &claude())
        .unwrap();
    assert_eq!(space.history(&node.id).unwrap().len(), 4);
}

// Undo

#[test]
fn undo_puts_back_the_version_before_as_in_the_specification() {
    let (_dir, mut space) = new_space();
    let node = space
        .create_node(NewNode::note("Beds", "Beds: tomato, basil."), &ada())
        .unwrap();
    space
        .update_node(
            &node.id,
            set_body("Beds: tomato, basil. Add mint."),
            &claude(),
        )
        .unwrap();
    let edit = last_change(&space);
    let undo = space.undo(edit, &ada(), false).unwrap();
    assert_eq!(undo.action, actions::UNDO);
    assert_eq!(undo.undoes, Some(edit));
    assert_eq!(undo.subject, node.id.to_string());
    assert_eq!(undo.changed_by, ada());

    assert_eq!(body(&space, &node.id), "Beds: tomato, basil.");
    let history = space.history(&node.id).unwrap();
    assert_eq!(history.len(), 3);
    assert_eq!(history[2].op, ChangeOp::Update);
    assert_eq!(history[2].change_id, Some(undo.id));
    assert_eq!(history[2].restored_from, Some(history[0].seq));
}

#[test]
fn undo_once_and_undoing_the_undo_redoes() {
    let (_dir, mut space) = new_space();
    let node = space.create_node(NewNode::note("N", "1"), &ada()).unwrap();
    space
        .update_node(&node.id, set_body("2"), &claude())
        .unwrap();
    let edit = last_change(&space);
    let undo = space.undo(edit, &ada(), false).unwrap();
    assert!(matches!(
        space.undo(edit, &ada(), false),
        Err(Error::AlreadyUndone { change, undone_by }) if change == edit && undone_by == undo.id
    ));

    let redo = space.undo(undo.id, &ada(), false).unwrap();
    assert_eq!(redo.undoes, Some(undo.id));
    assert_eq!(body(&space, &node.id), "2");

    // With the undo undone, the change can be undone again: the redo is not later work.
    space.undo(edit, &ada(), false).unwrap();
    assert_eq!(body(&space, &node.id), "1");
}

#[test]
fn undo_refuses_to_overwrite_later_changes_unless_forced() {
    let (_dir, mut space) = new_space();
    let node = space.create_node(NewNode::note("N", "1"), &ada()).unwrap();
    space
        .update_node(&node.id, set_body("2"), &claude())
        .unwrap();
    let edit = last_change(&space);
    space.update_node(&node.id, set_body("3"), &ada()).unwrap();
    let later = last_change(&space);

    match space.undo(edit, &claude(), false) {
        Err(Error::UndoConflict {
            change,
            later: found,
        }) => {
            assert_eq!(change, edit);
            assert_eq!(found, [later]);
        }
        other => panic!("expected a conflict, got {other:?}"),
    }
    assert_eq!(body(&space, &node.id), "3", "a refused undo writes nothing");
    assert_eq!(last_change(&space), later);

    space.undo(edit, &ada(), true).unwrap();
    assert_eq!(body(&space, &node.id), "1");
}

#[test]
fn undoing_a_create_deletes_and_undoing_a_delete_brings_it_back() {
    let (_dir, mut space) = new_space();
    let other = space
        .create_node(NewNode::note("Other", "see [[N]]"), &ada())
        .unwrap();
    let node = space
        .create_node(NewNode::note("N", "body").with_tags(["x"]), &claude())
        .unwrap();
    let created = last_change(&space);
    space
        .link(&node.id, &other.id, link_kinds::PART_OF, &ada())
        .unwrap();
    space.delete_node(&node.id, &ada()).unwrap();
    let deleted = last_change(&space);

    space.undo(deleted, &ada(), false).unwrap();
    let back = space.get_node(&node.id).unwrap().unwrap();
    assert_eq!((back.title.as_str(), back.body.as_str()), ("N", "body"));
    assert_eq!(back.tags, ["x"]);
    assert_eq!(
        back.created_at, node.created_at,
        "it keeps when it was created"
    );
    assert_eq!(back.created_by, claude());
    let kinds: Vec<_> = space
        .backlinks(&other.id)
        .unwrap()
        .into_iter()
        .map(|l| l.kind)
        .collect();
    assert_eq!(kinds, ["part-of"], "the link made by hand comes back");
    assert_eq!(
        space.backlinks(&node.id).unwrap()[0].from,
        other.id,
        "and the mention"
    );
    let history = space.history(&node.id).unwrap();
    assert_eq!(history.last().unwrap().op, ChangeOp::Create);
    assert_eq!(
        history.last().unwrap().restored_from,
        Some(history[history.len() - 2].seq)
    );

    // The create was followed by a link and the delete: undoing it needs force.
    assert!(matches!(
        space.undo(created, &ada(), false),
        Err(Error::UndoConflict { .. })
    ));
    space.undo(created, &ada(), true).unwrap();
    assert!(space.get_node(&node.id).unwrap().is_none());
    assert_eq!(space.unresolved_links(&other.id).unwrap(), ["N"]);
}

#[test]
fn undoing_link_changes() {
    let (_dir, mut space) = new_space();
    let a = space.create_node(NewNode::note("A", ""), &ada()).unwrap();
    let b = space.create_node(NewNode::note("B", ""), &ada()).unwrap();
    space
        .link(&a.id, &b.id, link_kinds::RELATED, &claude())
        .unwrap();
    let linked = last_change(&space);
    space.undo(linked, &ada(), false).unwrap();
    assert!(space.links_from(&a.id).unwrap().is_empty());

    space
        .link(&a.id, &b.id, link_kinds::PART_OF, &ada())
        .unwrap();
    space
        .unlink_by(&a.id, &b.id, link_kinds::PART_OF, &claude())
        .unwrap();
    let unlinked = last_change(&space);
    let undo = space.undo(unlinked, &ada(), false).unwrap();
    let link = &space.links_from(&a.id).unwrap()[0];
    assert_eq!(link.kind, "part-of");
    assert_eq!(link.created_by, ada(), "made again by whoever undid");
    let detail = space.change(undo.id).unwrap().unwrap();
    assert_eq!(detail.links[0].op, ChangeOp::Create);
}

#[test]
fn undoing_a_rename_puts_back_every_body_once() {
    let (_dir, mut space) = new_space();
    let target = space.create_node(NewNode::note("Old", ""), &ada()).unwrap();
    let source = space
        .create_node(NewNode::note("S", "see [[Old]] and [[Old]]"), &ada())
        .unwrap();
    space
        .update_node(&target.id, rename("New"), &claude())
        .unwrap();
    assert_eq!(body(&space, &source.id), "see [[New]] and [[New]]");
    space.undo(last_change(&space), &ada(), false).unwrap();
    assert_eq!(space.get_node(&target.id).unwrap().unwrap().title, "Old");
    assert_eq!(body(&space, &source.id), "see [[Old]] and [[Old]]");
    assert_eq!(space.backlinks(&target.id).unwrap()[0].from, source.id);
    assert_eq!(
        space
            .change(last_change(&space))
            .unwrap()
            .unwrap()
            .versions
            .len(),
        2
    );
}

#[test]
fn only_node_and_link_changes_are_undone() {
    let (_dir, mut space) = new_space();
    space
        .set_setting(setting_keys::SPACE_ICON, Some("🌱"), &ada())
        .unwrap();
    assert!(matches!(
        space.undo(last_change(&space), &ada(), false),
        Err(Error::CannotUndo { .. })
    ));
    assert!(matches!(
        space.undo(42, &ada(), false),
        Err(Error::ChangeNotFound(42))
    ));
}

// Restore

#[test]
fn restores_an_earlier_version_as_an_update() {
    let (_dir, mut space) = new_space();
    let node = space
        .create_node(NewNode::note("Plan", "v1"), &ada())
        .unwrap();
    let source = space
        .create_node(NewNode::note("S", "[[Plan]]"), &ada())
        .unwrap();
    space
        .update_node(&node.id, set_body("v2"), &claude())
        .unwrap();
    space
        .update_node(&node.id, rename("Plan B"), &claude())
        .unwrap();
    let first = space.history(&node.id).unwrap()[0].seq;

    let restored = space.restore(&node.id, first, &ada()).unwrap();
    assert_eq!(
        (restored.title.as_str(), restored.body.as_str()),
        ("Plan", "v1")
    );
    // A restored title renames: the bodies that link here follow.
    assert_eq!(body(&space, &source.id), "[[Plan]]");
    let change = &space.changes(1, None).unwrap()[0];
    assert_eq!(change.action, actions::NODE_RESTORE);
    let last = space.history(&node.id).unwrap().pop().unwrap();
    assert_eq!(last.restored_from, Some(first));

    // Restoring the version it already has writes nothing.
    let before = last_change(&space);
    space.restore(&node.id, first, &ada()).unwrap();
    assert_eq!(last_change(&space), before);
    // A version of another node is not a version of this one.
    let other = space.history(&source.id).unwrap()[0].seq;
    assert!(matches!(
        space.restore(&node.id, other, &ada()),
        Err(Error::Invalid(_))
    ));
}

#[test]
fn recovers_a_deleted_node_with_its_links() {
    let (_dir, mut space) = new_space();
    let node = space
        .create_node(NewNode::note("N", "text"), &ada())
        .unwrap();
    let a = space
        .create_node(NewNode::note("A", "see [[N]]"), &ada())
        .unwrap();
    let b = space.create_node(NewNode::note("B", ""), &ada()).unwrap();
    space
        .link(&node.id, &a.id, link_kinds::DERIVED_FROM, &ada())
        .unwrap();
    space
        .link(&b.id, &node.id, link_kinds::RELATED, &ada())
        .unwrap();
    space.delete_node(&node.id, &claude()).unwrap();
    // Another change since the delete does not stop a restore.
    space
        .update_node(&a.id, set_body("see [[N]] again"), &ada())
        .unwrap();

    let delete_row = space.history(&node.id).unwrap().pop().unwrap();
    assert_eq!(delete_row.op, ChangeOp::Delete);
    let back = space.restore(&node.id, delete_row.seq, &ada()).unwrap();
    assert_eq!(back.id, node.id);
    assert_eq!(back.body, "text");
    let mut kinds: Vec<_> = space
        .links_from(&node.id)
        .unwrap()
        .into_iter()
        .chain(space.backlinks(&node.id).unwrap())
        .map(|l| l.kind)
        .collect();
    kinds.sort();
    assert_eq!(kinds, ["derived-from", "mentions", "related"]);
    let change = space.change(last_change(&space)).unwrap().unwrap();
    assert_eq!(change.versions[0].op, ChangeOp::Create);
    assert_eq!(change.links.len(), 2);
}

// Purge

#[test]
fn purge_erases_a_node_and_its_versions() {
    let (_dir, mut space) = new_space();
    let node = space
        .create_node(NewNode::note("Secret", "password"), &ada())
        .unwrap();
    let other = space
        .create_node(NewNode::note("O", "[[Secret]]"), &ada())
        .unwrap();
    space
        .link(&other.id, &node.id, link_kinds::RELATED, &ada())
        .unwrap();
    space
        .update_node(&node.id, set_body("password 2"), &ada())
        .unwrap();
    let edit = last_change(&space);

    assert!(matches!(
        space.purge(&node.id, &claude()),
        Err(Error::PersonOnly(_))
    ));
    assert!(matches!(
        space.purge(&node.id, &Provenance::system()),
        Err(Error::PersonOnly(_))
    ));
    let purge = space.purge(&node.id, &ada()).unwrap();
    assert_eq!(purge.action, actions::NODE_PURGE);
    assert_eq!(purge.detail, serde_json::json!({}));
    assert!(space.get_node(&node.id).unwrap().is_none());
    assert!(space.history(&node.id).unwrap().is_empty());
    assert!(space.link_history(&node.id).unwrap().is_empty());
    assert_eq!(space.unresolved_links(&other.id).unwrap(), ["Secret"]);
    // The changes stay, without content.
    assert_eq!(space.changes_of(&node.id.to_string(), 10).unwrap().len(), 3);
    assert!(space.change(edit).unwrap().unwrap().versions.is_empty());

    assert!(matches!(
        space.undo(edit, &ada(), true),
        Err(Error::CannotUndo { .. })
    ));
    assert!(space.restore(&node.id, 1, &ada()).is_err());
    assert!(matches!(
        space.purge(&node.id, &ada()),
        Err(Error::NotFound(_))
    ));

    // Purging a deleted node erases its history too.
    let gone = space
        .create_node(NewNode::note("Gone", ""), &ada())
        .unwrap();
    space.delete_node(&gone.id, &ada()).unwrap();
    space.purge(&gone.id, &ada()).unwrap();
    assert!(space.history(&gone.id).unwrap().is_empty());
}

// Migration from schema version 2

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        if entry.file_type().unwrap().is_file() {
            std::fs::copy(entry.path(), to.join(entry.file_name())).unwrap();
        }
    }
}

#[test]
fn opens_a_schema_2_space_and_makes_every_version_a_change() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("Example.knode");
    copy_dir(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/example-v2.knode"),
        &path,
    );
    let version = |path: &Path| -> u32 {
        rusqlite::Connection::open(path.join("knode.db"))
            .unwrap()
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap()
    };
    assert_eq!(version(&path), 2);

    let mut space = Space::open(&path).unwrap();
    assert_eq!(version(&path), SCHEMA_VERSION);
    assert_eq!(space.node_count().unwrap(), 5);

    // Six versions, six changes, numbered like the versions.
    let changes = space.changes(100, None).unwrap();
    assert_eq!(changes.len(), 6);
    let mut versions = Vec::new();
    for change in &changes {
        let detail = space.change(change.id).unwrap().unwrap();
        assert_eq!(detail.versions.len(), 1);
        let version = &detail.versions[0];
        assert_eq!(version.seq, change.id);
        assert_eq!(version.change_id, Some(change.id));
        assert_eq!(change.changed_at, version.changed_at);
        assert_eq!(change.changed_by, version.changed_by);
        assert_eq!(change.subject, version.node_id.to_string());
        versions.push((change.action.clone(), version.op));
    }
    assert_eq!(
        versions[0],
        (actions::NODE_UPDATE.to_string(), ChangeOp::Update)
    );
    assert!(
        versions[1..]
            .iter()
            .all(|(a, op)| a == actions::NODE_CREATE && *op == ChangeOp::Create)
    );
    // No agents and no grants: nothing changes meaning.
    assert!(space.agents().unwrap().is_empty());
    assert!(space.folders().unwrap().is_empty());

    // The update by codex can be undone, and new changes follow the old numbers.
    let update = &changes[0];
    let node = space.change(update.id).unwrap().unwrap().versions[0].node_id;
    let undo = space
        .undo(update.id, &Provenance::person("Ada"), false)
        .unwrap();
    assert_eq!(undo.id, 7);
    assert!(!body(&space, &node).contains("calendar"));
}

// Settings, folders, agents and grants (groundwork)

#[test]
fn settings_are_changes_made_by_people() {
    let (_dir, mut space) = new_space();
    assert_eq!(space.setting(setting_keys::SPACE_ICON).unwrap(), None);
    assert!(
        space
            .set_setting(setting_keys::SPACE_ICON, Some("🌱"), &ada())
            .unwrap()
    );
    assert!(
        !space
            .set_setting(setting_keys::SPACE_ICON, Some("🌱"), &ada())
            .unwrap()
    );
    assert_eq!(
        space.setting(setting_keys::SPACE_ICON).unwrap().as_deref(),
        Some("🌱")
    );
    let change = &space.changes(1, None).unwrap()[0];
    assert_eq!(change.action, actions::SETTINGS_SET);
    assert_eq!(change.subject, setting_keys::SPACE_ICON);
    assert_eq!(change.detail, serde_json::json!({ "value": "🌱" }));
    assert!(
        space
            .set_setting(setting_keys::SPACE_ICON, None, &ada())
            .unwrap()
    );
    assert_eq!(space.setting(setting_keys::SPACE_ICON).unwrap(), None);
    space
        .set_setting(setting_keys::CAPTURE_ENABLED, Some("off"), &ada())
        .unwrap();
    let all = space.settings().unwrap();
    assert_eq!(all.len(), 2, "{all:?}");
    assert_eq!(all[setting_keys::CAPTURE_ENABLED], "off");
    assert_eq!(all[setting_keys::SPACE_ID], space.manifest().id);

    assert!(matches!(
        space.set_setting(setting_keys::CAPTURE_ENABLED, Some("off"), &claude()),
        Err(Error::PersonOnly(_))
    ));
    assert!(
        space
            .set_setting(setting_keys::SPACE_ID, Some("x"), &ada())
            .is_err()
    );
}

#[test]
fn folders_are_added_changed_and_removed() {
    let (_dir, mut space) = new_space();
    let spec = FolderSpec {
        id: None,
        kind: folder_kinds::PROJECT.into(),
        path: "~/Developer/knode".into(),
        capture: true,
        rules: serde_json::json!({ "exclude": ["node_modules/**"] }),
    };
    let folder = space.set_folder(&spec, &ada()).unwrap();
    assert_eq!(folder.id.len(), 26);
    assert!(folder.capture);
    assert_eq!(folder.created_by, ada());

    // The same kind and path changes the same folder.
    let off = space
        .set_folder(
            &FolderSpec {
                capture: false,
                ..spec.clone()
            },
            &ada(),
        )
        .unwrap();
    assert_eq!(off.id, folder.id);
    assert!(!off.capture);
    assert_eq!(space.folders().unwrap().len(), 1);

    assert!(matches!(
        space.set_folder(&spec, &claude()),
        Err(Error::PersonOnly(_))
    ));
    assert!(space.remove_folder(&folder.id, &ada()).unwrap());
    assert!(!space.remove_folder(&folder.id, &ada()).unwrap());
    let actions: Vec<_> = space
        .changes(10, None)
        .unwrap()
        .into_iter()
        .map(|c| c.action)
        .collect();
    assert_eq!(actions, ["folder.remove", "folder.set", "folder.set"]);
}

#[test]
fn agents_keys_and_grants() {
    let (_dir, mut space) = new_space();
    let agent = space
        .add_agent(None, "claude-code", "claude-code", &ada())
        .unwrap();
    // Adding the same agent again, as when it is copied to a space, changes nothing.
    let again = space
        .add_agent(Some(&agent.id), "claude-code", "claude-code", &ada())
        .unwrap();
    assert_eq!(again, agent);
    assert!(
        space
            .add_agent(None, "claude-code", "claude-code", &ada())
            .is_err()
    );
    assert!(matches!(
        space.add_agent(None, "codex", "codex", &claude()),
        Err(Error::PersonOnly(_))
    ));

    let fingerprint = "sha256:2d8e7ee0d1fd2f7fa95d62fff7405c54a6c86833c447fe84c53f2de800dfb3a1";
    assert!(space.add_agent_key(&agent.id, fingerprint, &ada()).unwrap());
    assert!(!space.add_agent_key(&agent.id, fingerprint, &ada()).unwrap());
    assert!(
        space
            .add_agent_key(&agent.id, "knk_secret", &ada())
            .is_err()
    );
    assert_eq!(space.agent_keys(&agent.id).unwrap()[0].revoked_at, None);

    assert_eq!(space.grant(&agent.id).unwrap(), GrantLevel::None);
    assert!(
        space
            .set_grant(&agent.id, GrantLevel::Read, &ada())
            .unwrap()
    );
    assert!(
        !space
            .set_grant(&agent.id, GrantLevel::Read, &ada())
            .unwrap()
    );
    assert_eq!(space.grant(&agent.id).unwrap(), GrantLevel::Read);
    let change = &space.changes(1, None).unwrap()[0];
    assert_eq!(change.action, actions::GRANT_SET);
    assert_eq!(change.subject, agent.id);
    assert_eq!(
        change.detail,
        serde_json::json!({ "level": "read", "previous": "none" })
    );
    assert!(matches!(
        space.set_grant(&agent.id, GrantLevel::Write, &claude()),
        Err(Error::PersonOnly(_))
    ));

    assert!(space.revoke_agent_key(fingerprint, &ada()).unwrap());
    assert!(!space.revoke_agent_key(fingerprint, &ada()).unwrap());
    assert!(space.agent_keys(&agent.id).unwrap()[0].revoked_at.is_some());
    // A revoked key never becomes valid again.
    assert!(!space.add_agent_key(&agent.id, fingerprint, &ada()).unwrap());
    let actions: Vec<_> = space
        .changes(10, None)
        .unwrap()
        .into_iter()
        .rev()
        .map(|c| c.action)
        .collect();
    assert_eq!(
        actions,
        [
            "agent.add",
            "agent.key.add",
            "grant.set",
            "agent.key.revoke"
        ]
    );
}

// Imports

fn imported(id: NodeId, title: &str, body: &str) -> ImportedNode {
    ImportedNode {
        id,
        node: NewNode::note(title, body).with_tags(["Plan"]),
        created_at: Timestamp(1_700_000_000_000),
        updated_at: Timestamp(1_700_000_500_000),
        created_by: claude(),
        updated_by: Provenance::person("Grace").with_source("~/notes"),
    }
}

#[test]
fn imports_a_node_keeping_its_id_dates_and_provenance() {
    let (_dir, mut space) = new_space();
    let waiting = space
        .create_node(NewNode::note("Waiting", "see [[Plan]]"), &ada())
        .unwrap();
    let id = NodeId::parse("01JB7Q9W8K3M2N4P5R6S7T8V9X").unwrap();
    let props = Properties::from([("source.url".to_string(), "https://x.y".to_string())]);
    let importer = Provenance::person("Ada").with_source("~/export");
    let node = space
        .import_node(
            imported(id, "Plan", "Ship. See [[Waiting]]."),
            &props,
            &importer,
        )
        .unwrap();
    assert_eq!(node.id, id);
    assert_eq!(node.tags, ["plan"]);
    assert_eq!(node.created_at, Timestamp(1_700_000_000_000));
    assert_eq!(node.updated_at, Timestamp(1_700_000_500_000));
    assert_eq!(node.created_by, claude());
    assert_eq!(node.updated_by.actor, "Grace");
    assert_eq!(space.properties(&id).unwrap(), props);

    // The body's links are read, and links waiting for its title resolve.
    assert_eq!(space.links_from(&id).unwrap()[0].to, waiting.id);
    assert_eq!(space.links_from(&waiting.id).unwrap()[0].to, id);

    // The history and the change are those of the import, made now by the importer.
    let change = space.changes(1, None).unwrap().remove(0);
    assert_eq!(change.action, actions::NODE_CREATE);
    assert_eq!(change.subject, id.to_string());
    assert_eq!(change.changed_by, importer);
    assert!(change.changed_at > node.updated_at);
    let history = space.history(&id).unwrap();
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].op, ChangeOp::Create);
    assert_eq!(history[0].changed_by, importer);
    assert_eq!(history[0].change_id, Some(change.id));

    // Undoing the import removes the node.
    space.undo(change.id, &ada(), false).unwrap();
    assert!(space.get_node(&id).unwrap().is_none());
}

#[test]
fn imports_refuse_ids_in_use_and_dates_out_of_order() {
    let (_dir, mut space) = new_space();
    let node = space.create_node(NewNode::note("A", ""), &ada()).unwrap();
    let before = last_change(&space);
    let err = space
        .import_node(imported(node.id, "B", ""), &Properties::new(), &ada())
        .unwrap_err();
    assert!(matches!(err, Error::Invalid(_)), "{err}");

    let id = NodeId::parse("01JB7Q9W8K3M2N4P5R6S7T8V9X").unwrap();
    let mut backwards = imported(id, "B", "");
    backwards.updated_at = Timestamp(backwards.created_at.0 - 1);
    assert!(matches!(
        space.import_node(backwards, &Properties::new(), &ada()),
        Err(Error::Invalid(_))
    ));
    let mut bad_kind = imported(id, "B", "");
    bad_kind.node.kind = "Not A Kind".into();
    assert!(
        space
            .import_node(bad_kind, &Properties::new(), &ada())
            .is_err()
    );
    assert_eq!(
        last_change(&space),
        before,
        "a refused import writes nothing"
    );
    assert!(space.get_node(&id).unwrap().is_none());

    // An id only the history has, such as a node deleted here, is created again.
    space.delete_node(&node.id, &ada()).unwrap();
    let back = space
        .import_node(imported(node.id, "A", "again"), &Properties::new(), &ada())
        .unwrap();
    assert_eq!(back.body, "again");
    let ops: Vec<ChangeOp> = space
        .history(&node.id)
        .unwrap()
        .into_iter()
        .map(|h| h.op)
        .collect();
    assert_eq!(ops, [ChangeOp::Create, ChangeOp::Delete, ChangeOp::Create]);
}
