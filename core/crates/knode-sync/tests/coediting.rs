//! Editing a node's body together: a person and agents at once, through Loro documents kept in
//! step with SQLite (section 21 of the specification).

use std::fs;
use std::path::PathBuf;

use knode_format::{NewNode, NodeId, NodeUpdate, Provenance, Space, actions};
use knode_sync::loro::{ExportMode, LoroDoc};
use knode_sync::{BODY, BodyEdit, Docs, Error, Splice, Version};
use tempfile::TempDir;

const PARAGRAPH: &str = "The plan is to ship the format first.";

fn ada() -> Provenance {
    Provenance::person("Ada")
}

fn claude() -> Provenance {
    Provenance::agent("claude-code").with_session("s1")
}

fn codex() -> Provenance {
    Provenance::agent("codex").with_session("s2")
}

fn setup(body: &str) -> (TempDir, Space, NodeId) {
    let dir = tempfile::tempdir().unwrap();
    let mut space = Space::create(dir.path().join("Test.knode"), "Test").unwrap();
    let node = space
        .create_node(NewNode::note("Plan", body), &ada())
        .unwrap();
    (dir, space, node.id)
}

fn body(space: &Space, id: &NodeId) -> String {
    space.get_node(id).unwrap().unwrap().body
}

fn updates_dir(space: &Space, id: &NodeId) -> PathBuf {
    space.loro_dir(id).join("updates")
}

fn update_files(space: &Space, id: &NodeId) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(updates_dir(space, id))
        .map(|d| {
            d.map(|e| e.unwrap().file_name().into_string().unwrap())
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

fn row(space: &Space, id: &NodeId) -> Option<Version> {
    space
        .loro_version(id)
        .unwrap()
        .map(|v| Version::from_json(&v).unwrap())
}

/// An editor with its own copy of the document, such as an open editor in an app.
fn replica(docs: &mut Docs, space: &mut Space, id: &NodeId, by: &Provenance) -> LoroDoc {
    let doc = LoroDoc::new();
    doc.import(
        &docs
            .export_updates_since(space, id, &Version::empty())
            .unwrap(),
    )
    .unwrap();
    doc.set_peer_id(docs.new_peer(space, by).unwrap()).unwrap();
    doc
}

fn text(doc: &LoroDoc) -> String {
    doc.get_text(BODY).to_string()
}

#[test]
fn the_first_open_makes_the_document_from_the_body() {
    let (_dir, mut space, id) = setup(PARAGRAPH);
    let mut docs = Docs::new();
    assert!(!Docs::has_doc(&space, &id));

    let open = docs.open_doc(&mut space, &id).unwrap();
    assert_eq!(open.text, PARAGRAPH);
    assert!(Docs::has_doc(&space, &id));
    assert_eq!(row(&space, &id), Some(open.version.clone()));

    // Inserted by Knode, so the text written before has no other author; nothing is recorded.
    let (peer, _) = open.version.peers().next().unwrap();
    assert_eq!(space.loro_peer(peer).unwrap(), Some(Provenance::system()));
    assert_eq!(space.history(&id).unwrap().len(), 1);
    assert_eq!(space.get_node(&id).unwrap().unwrap().updated_by, ada());
    assert_eq!(update_files(&space, &id).len(), 1);

    // Opening again changes nothing.
    assert_eq!(docs.open_doc(&mut space, &id).unwrap(), open);
    assert_eq!(Docs::new().open_doc(&mut space, &id).unwrap(), open);
    assert_eq!(update_files(&space, &id).len(), 1);
}

#[test]
fn a_person_and_two_agents_editing_one_paragraph_converge() {
    let (_dir, mut space, id) = setup(PARAGRAPH);
    let mut docs = Docs::new();
    let seen = docs.open_doc(&mut space, &id).unwrap().version;

    // Ada types in her editor; Claude and Codex each rewrite the paragraph they read.
    let ada_edit = PARAGRAPH.find("ship").unwrap();
    docs.apply_local_edit(
        &mut space,
        &id,
        BodyEdit::insert(ada_edit, "finally ").based_on(seen.clone()),
        &ada(),
    )
    .unwrap();
    docs.apply_local_edit(
        &mut space,
        &id,
        BodyEdit::text("The plan is to ship the format and the service first.")
            .based_on(seen.clone()),
        &claude(),
    )
    .unwrap();
    let last = docs
        .apply_local_edit(
            &mut space,
            &id,
            BodyEdit::text("The plan is to ship the format first, then the apps.").based_on(seen),
            &codex(),
        )
        .unwrap();

    let merged = "The plan is to finally ship the format and the service first, then the apps.";
    assert_eq!(body(&space, &id), merged);
    assert_eq!(last.node.body, merged);
    assert_eq!(row(&space, &id), Some(last.version));

    // Each write is its own change, by its writer, and history keeps every version.
    let history = space.history(&id).unwrap();
    let by: Vec<Provenance> = history.iter().map(|h| h.changed_by.clone()).collect();
    assert_eq!(by, vec![ada(), ada(), claude(), codex()]);
    assert_eq!(
        history[2].body,
        "The plan is to finally ship the format and the service first."
    );
    let changes = space.changes_of(&id.to_string(), 10).unwrap();
    assert_eq!(changes.len(), 4);
    assert!(
        changes[..3]
            .iter()
            .all(|c| c.action == actions::NODE_UPDATE)
    );
}

#[test]
fn editors_with_their_own_copies_converge_in_any_order() {
    let (_dir, mut space, id) = setup(PARAGRAPH);
    let mut docs = Docs::new();
    docs.open_doc(&mut space, &id).unwrap();
    let editors = [ada(), claude(), codex()];
    let replicas: Vec<LoroDoc> = editors
        .iter()
        .map(|by| replica(&mut docs, &mut space, &id, by))
        .collect();

    // All three edit the same words at once, before seeing each other.
    replicas[0]
        .get_text(BODY)
        .splice(12, 2, "is, for now, to")
        .unwrap();
    replicas[1]
        .get_text(BODY)
        .update(
            "The plan is to ship the file format first.",
            Default::default(),
        )
        .unwrap();
    replicas[2]
        .get_text(BODY)
        .insert(PARAGRAPH.len() - 1, " of all")
        .unwrap();
    let sent: Vec<Vec<u8>> = replicas
        .iter()
        .map(|r| {
            r.commit();
            r.export(ExportMode::all_updates()).unwrap()
        })
        .collect();

    // The server merges them in one order, and each editor imports the others' in another.
    for (bytes, by) in sent.iter().zip(&editors).rev() {
        let written = docs.merge_updates(&mut space, &id, bytes).unwrap();
        assert_eq!(&written.node.updated_by, by);
    }
    for (i, replica) in replicas.iter().enumerate() {
        for (j, bytes) in sent.iter().enumerate() {
            if i != j {
                replica.import(bytes).unwrap();
            }
        }
    }
    let merged = body(&space, &id);
    for replica in &replicas {
        assert_eq!(text(replica), merged);
    }
    assert!(merged.contains("is, for now, to"), "{merged}");
    assert!(merged.contains("file format"), "{merged}");
    assert!(merged.contains("first of all."), "{merged}");

    // Merging the same updates again changes nothing.
    let history = space.history(&id).unwrap().len();
    let again = docs.merge_updates(&mut space, &id, &sent[0]).unwrap();
    assert!(again.update.is_empty());
    assert_eq!(space.history(&id).unwrap().len(), history);
}

#[test]
fn editors_catch_up_with_updates_since_their_version() {
    let (_dir, mut space, id) = setup(PARAGRAPH);
    let mut docs = Docs::new();
    docs.open_doc(&mut space, &id).unwrap();
    let editor = replica(&mut docs, &mut space, &id, &ada());
    let seen = Version::from_json(&space.loro_version(&id).unwrap().unwrap()).unwrap();

    let written = docs
        .apply_local_edit(&mut space, &id, BodyEdit::insert(0, "Today: "), &claude())
        .unwrap();
    let since = docs.export_updates_since(&mut space, &id, &seen).unwrap();
    editor.import(&since).unwrap();
    assert_eq!(text(&editor), body(&space, &id));
    // The update a write returns is the same edit, for editors that are up to date.
    let other = replica(&mut docs, &mut space, &id, &codex());
    other.import(&written.update).unwrap();
    assert_eq!(text(&other), body(&space, &id));
}

#[test]
fn updates_from_an_unknown_peer_are_refused() {
    let (_dir, mut space, id) = setup(PARAGRAPH);
    let mut docs = Docs::new();
    docs.open_doc(&mut space, &id).unwrap();
    let stranger = LoroDoc::new();
    stranger
        .import(
            &docs
                .export_updates_since(&mut space, &id, &Version::empty())
                .unwrap(),
        )
        .unwrap();
    stranger.get_text(BODY).insert(0, "Hijacked. ").unwrap();
    stranger.commit();
    let files = update_files(&space, &id);
    let bytes = stranger.export(ExportMode::all_updates()).unwrap();
    assert!(matches!(
        docs.merge_updates(&mut space, &id, &bytes),
        Err(Error::UnknownPeer(_))
    ));
    assert_eq!(body(&space, &id), PARAGRAPH);
    assert_eq!(update_files(&space, &id), files);
    // The document in memory was dropped with the refused edits.
    assert_eq!(docs.open_doc(&mut space, &id).unwrap().text, PARAGRAPH);
}

#[test]
fn splices_are_checked() {
    let (_dir, mut space, id) = setup("héllo");
    let mut docs = Docs::new();
    // Positions count characters, not bytes.
    let written = docs
        .apply_local_edit(
            &mut space,
            &id,
            BodyEdit::splices(vec![
                Splice {
                    at: 1,
                    delete: 1,
                    insert: "e".into(),
                },
                Splice {
                    at: 5,
                    delete: 0,
                    insert: "!".into(),
                },
            ]),
            &ada(),
        )
        .unwrap();
    assert_eq!(written.node.body, "hello!");
    assert!(matches!(
        docs.apply_local_edit(&mut space, &id, BodyEdit::delete(3, 9), &ada()),
        Err(Error::Invalid(_))
    ));
    let unknown = Version::from_json(r#"{"1":5}"#).unwrap();
    assert!(matches!(
        docs.apply_local_edit(
            &mut space,
            &id,
            BodyEdit::text("x").based_on(unknown),
            &ada()
        ),
        Err(Error::UnknownVersion(_))
    ));
    assert_eq!(body(&space, &id), "hello!");
}

#[test]
fn sqlite_stays_readable_without_loro() {
    let (dir, mut space, id) = setup(PARAGRAPH);
    let mut docs = Docs::new();
    docs.apply_local_edit(&mut space, &id, BodyEdit::insert(0, "Now: "), &claude())
        .unwrap();
    let expected = format!("Now: {PARAGRAPH}");
    drop(space);

    // A reader that does not use Loro sees every merged edit, searchable.
    let space = Space::open(dir.path().join("Test.knode")).unwrap();
    assert_eq!(body(&space, &id), expected);
    assert_eq!(space.search("now", 5).unwrap()[0].node.id, id);

    // Even with sync/ gone, and a writer with Loro starts again from the body.
    fs::remove_dir_all(dir.path().join("Test.knode/sync")).unwrap();
    let mut space = Space::open(dir.path().join("Test.knode")).unwrap();
    assert_eq!(body(&space, &id), expected);
    let open = Docs::new().open_doc(&mut space, &id).unwrap();
    assert_eq!(open.text, expected);
    assert_eq!(row(&space, &id), Some(open.version));
}

#[test]
fn a_body_written_without_loro_is_pulled_into_the_document() {
    let (_dir, mut space, id) = setup(PARAGRAPH);
    let mut docs = Docs::new();
    let seen = docs.open_doc(&mut space, &id).unwrap().version;
    space
        .update_node(
            &id,
            NodeUpdate {
                body: Some("Written by an older Knode.".into()),
                ..Default::default()
            },
            &codex(),
        )
        .unwrap();
    assert_eq!(row(&space, &id), None);
    let history = space.history(&id).unwrap().len();

    let open = docs.open_doc(&mut space, &id).unwrap();
    assert_eq!(open.text, "Written by an older Knode.");
    assert_eq!(row(&space, &id), Some(open.version.clone()));
    assert_eq!(space.history(&id).unwrap().len(), history);
    // Made as the peer of the body's writer.
    let peers = open.version.peers_ahead_of(&seen);
    assert_eq!(peers.len(), 1);
    assert_eq!(space.loro_peer(peers[0]).unwrap(), Some(codex()));

    // An edit based on the version before is merged with the body written without Loro.
    docs.apply_local_edit(
        &mut space,
        &id,
        BodyEdit::text(format!("{PARAGRAPH} Agreed.")).based_on(seen),
        &ada(),
    )
    .unwrap();
    assert_eq!(body(&space, &id), "Written by an older Knode. Agreed.");

    // An undo writes without Loro too, and the next edit starts from it.
    let last = space.changes_of(&id.to_string(), 1).unwrap()[0].id;
    space.undo(last, &ada(), false).unwrap();
    assert!(!docs.pull_sqlite_into_doc(&mut space, &id).unwrap() || row(&space, &id).is_some());
    assert_eq!(
        docs.open_doc(&mut space, &id).unwrap().text,
        "Written by an older Knode."
    );
}

#[test]
fn a_crash_before_sqlite_is_written_is_recovered() {
    let (_dir, mut space, id) = setup(PARAGRAPH);
    let mut docs = Docs::new();
    docs.open_doc(&mut space, &id).unwrap();
    // A writer wrote its update file and crashed before its transaction.
    let crashed = replica(&mut docs, &mut space, &id, &claude());
    crashed.get_text(BODY).insert(0, "Saved: ").unwrap();
    crashed.commit();
    let bytes = crashed
        .export(ExportMode::updates(&crashed.oplog_vv()))
        .unwrap();
    assert!(bytes.len() < 1000);
    let all = crashed.export(ExportMode::all_updates()).unwrap();
    fs::write(
        updates_dir(&space, &id).join("01M5D1QX7W3E9Y2A6K0B8C4F5H.loro"),
        &all,
    )
    .unwrap();
    assert_eq!(body(&space, &id), PARAGRAPH);

    // The next writer finds the document ahead and writes it, as the crashed writer.
    let open = Docs::new().open_doc(&mut space, &id).unwrap();
    assert_eq!(open.text, format!("Saved: {PARAGRAPH}"));
    let node = space.get_node(&id).unwrap().unwrap();
    assert_eq!(node.body, open.text);
    assert_eq!(node.updated_by, claude());
    assert_eq!(row(&space, &id), Some(open.version));
}

#[test]
fn partial_and_unknown_files_are_ignored() {
    let (_dir, mut space, id) = setup(PARAGRAPH);
    let mut docs = Docs::new();
    let real = docs
        .apply_local_edit(&mut space, &id, BodyEdit::insert(0, "A. "), &ada())
        .unwrap()
        .update;
    let expected = body(&space, &id);
    let updates = updates_dir(&space, &id);
    // A file still being written, a truncated update with a valid name, and other names.
    fs::write(updates.join(".01M5D1R2N0V5T8M3G7J1P9X6QD.loro"), b"partial").unwrap();
    fs::write(
        updates.join("01M5D1R2N0V5T8M3G7J1P9X6QE.loro"),
        &real[..real.len() / 2],
    )
    .unwrap();
    fs::write(updates.join("notes.txt"), b"hello").unwrap();
    fs::write(space.loro_dir(&id).join("README"), b"hello").unwrap();

    let mut fresh = Docs::new();
    assert_eq!(fresh.open_doc(&mut space, &id).unwrap().text, expected);
    fresh
        .apply_local_edit(&mut space, &id, BodyEdit::insert(0, "B. "), &ada())
        .unwrap();
    assert_eq!(body(&space, &id), format!("B. {expected}"));
    // Compaction removes only the update files it read.
    fresh.compact(&space, &id).unwrap();
    let left = update_files(&space, &id);
    assert!(left.contains(&".01M5D1R2N0V5T8M3G7J1P9X6QD.loro".to_string()));
    assert!(left.contains(&"01M5D1R2N0V5T8M3G7J1P9X6QE.loro".to_string()));
    assert!(left.contains(&"notes.txt".to_string()));
    assert_eq!(
        Docs::new().open_doc(&mut space, &id).unwrap().text,
        format!("B. {expected}")
    );
}

#[test]
fn a_lost_update_file_is_repaired_from_sqlite() {
    let (_dir, mut space, id) = setup(PARAGRAPH);
    let mut docs = Docs::new();
    docs.open_doc(&mut space, &id).unwrap();
    // Lose the compacted document and the edit's update file.
    docs.apply_local_edit(&mut space, &id, BodyEdit::insert(0, "Kept: "), &ada())
        .unwrap();
    fs::remove_dir_all(space.loro_dir(&id)).unwrap();
    let open = Docs::new().open_doc(&mut space, &id).unwrap();
    assert_eq!(open.text, format!("Kept: {PARAGRAPH}"));
    assert_eq!(body(&space, &id), open.text);
    assert_eq!(row(&space, &id), Some(open.version));
}

#[test]
fn compaction_keeps_the_content_and_history() {
    let (_dir, mut space, id) = setup(PARAGRAPH);
    let mut docs = Docs::new();
    let seen = docs.open_doc(&mut space, &id).unwrap().version;
    for i in 0..250 {
        docs.apply_local_edit(
            &mut space,
            &id,
            BodyEdit::insert(0, format!("{i} ")),
            &ada(),
        )
        .unwrap();
    }
    let expected = body(&space, &id);
    assert!(space.loro_dir(&id).join("snapshot.loro").is_file());
    assert!(update_files(&space, &id).len() <= 101);
    assert!(!Docs::needs_compaction(&space, &id).unwrap());

    let compaction = docs.compact(&space, &id).unwrap();
    assert!(compaction.snapshot_bytes > 0);
    assert!(update_files(&space, &id).is_empty());

    // A full snapshot: a new program reads the same text and version, and an editor at an old
    // version still merges.
    let mut fresh = Docs::new();
    let open = fresh.open_doc(&mut space, &id).unwrap();
    assert_eq!(open.text, expected);
    assert_eq!(row(&space, &id), Some(open.version));
    fresh
        .apply_local_edit(
            &mut space,
            &id,
            BodyEdit::text(format!("{PARAGRAPH} End.")).based_on(seen),
            &claude(),
        )
        .unwrap();
    assert_eq!(body(&space, &id), format!("{expected} End."));
    assert_eq!(space.history(&id).unwrap().len(), 252);
}

#[test]
fn a_purged_node_loses_its_document() {
    let (_dir, mut space, id) = setup(PARAGRAPH);
    let mut docs = Docs::new();
    docs.open_doc(&mut space, &id).unwrap();
    space.delete_node(&id, &ada()).unwrap();
    assert!(Docs::has_doc(&space, &id));
    assert!(matches!(
        docs.open_doc(&mut space, &id),
        Err(Error::NotFound(_))
    ));
    docs.remove_doc(&space, &id).unwrap();
    assert!(!Docs::has_doc(&space, &id));
}

#[test]
fn three_programs_writing_at_once_lose_nothing() {
    let (dir, space, id) = setup(PARAGRAPH);
    drop(space);
    let path = dir.path().join("Test.knode");
    let writers: Vec<_> = [("ada", ada()), ("claude", claude()), ("codex", codex())]
        .into_iter()
        .map(|(name, by)| {
            let path = path.clone();
            std::thread::spawn(move || {
                let mut space = Space::open(&path).unwrap();
                let mut docs = Docs::new();
                for i in 0..25 {
                    // Each edit applies to the version it saw, whatever others wrote since.
                    let open = docs.open_doc(&mut space, &id).unwrap();
                    let end = open.text.chars().count();
                    docs.apply_local_edit(
                        &mut space,
                        &id,
                        BodyEdit::insert(end, format!(" [{name} {i}]")).based_on(open.version),
                        &by,
                    )
                    .unwrap();
                }
            })
        })
        .collect();
    for writer in writers {
        writer.join().unwrap();
    }
    let mut space = Space::open(&path).unwrap();
    let text = Docs::new().open_doc(&mut space, &id).unwrap().text;
    assert_eq!(body(&space, &id), text);
    for name in ["ada", "claude", "codex"] {
        for i in 0..25 {
            assert!(
                text.contains(&format!("[{name} {i}]")),
                "{name} {i}: {text}"
            );
        }
    }
}
