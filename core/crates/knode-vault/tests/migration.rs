//! Moving an Obsidian vault to Knode: the migration report, importing again over a space with
//! edits made in Knode, an import that stops halfway, and comparing an export with the vault.
//! Synthetic vaults only.

mod common;

use std::fs;
use std::path::Path;

use common::put;
use knode_format::{NodeId, NodeUpdate, Provenance, Space};
use knode_vault::migration::{DELETED_REASON, FileStatus, report};
use knode_vault::{
    Difference, ImportOptions, compare, export, import, import_with, property_keys, setting_keys,
};

fn me() -> Provenance {
    Provenance::person("Ada")
}

fn vault(dir: &Path) {
    put(
        dir,
        "Welcome.md",
        "# Welcome\n\nSee [[Plan]], [[README]], [[Missing note]], ![[picture.png]], ![[shared.png]] and ![[gone.pdf]].\n",
    );
    put(
        dir,
        "Projects/Plan.md",
        "---\nid: 01JB7Q9W8K3M2N4P5R6S7T8V9X\nlinks:\n  related: [01JB7Q2A0B1C2D3E4F5G6H7J8K]\n---\n# Plan\n\nShip it.\n",
    );
    put(dir, "Projects/README.md", "Projects readme.\n");
    put(dir, "Areas/README.md", "Areas readme.\n");
    put(dir, "picture.png", [0x89, b'P', b'N', b'G']);
    put(dir, "assets/shared.png", [1, 2, 3]);
    put(dir, "assets/unused.png", [4, 5, 6]);
    put(dir, "Board.canvas", "{}");
    put(dir, "Upper.MD", "Not read.");
    put(dir, ".obsidian/app.json", "{}");
    put(dir, ".trash/Old.md", "Deleted.");
    put(dir, "Broken.md", [0xff, 0xfe]);
}

fn node(space: &Space, path: &str) -> NodeId {
    let found = space.find_by_property(property_keys::PATH, path).unwrap();
    assert_eq!(found.len(), 1, "{path}");
    found[0].id
}

fn body(space: &Space, path: &str) -> String {
    space.get_node(&node(space, path)).unwrap().unwrap().body
}

fn edit(space: &mut Space, path: &str, text: &str) {
    let id = node(space, path);
    space
        .update_node(
            &id,
            NodeUpdate {
                body: Some(text.into()),
                ..Default::default()
            },
            &me(),
        )
        .unwrap();
}

#[test]
fn the_report_maps_every_file_and_names_what_did_not_come_through() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("Vault");
    vault(&dir);
    let mut space = Space::create(tmp.path().join("V.knode"), "V").unwrap();
    let imported = import(&mut space, &dir, &me()).unwrap();
    let r = report(&space, &dir, &imported).unwrap();

    // Every note maps to its node.
    for path in [
        "Welcome.md",
        "Projects/Plan.md",
        "Projects/README.md",
        "Areas/README.md",
    ] {
        assert_eq!(imported.nodes[path], node(&space, path), "{path}");
        let entry = r.files.iter().find(|f| f.path == path).unwrap();
        assert_eq!(entry.status, FileStatus::Created);
        assert_eq!(entry.node, Some(node(&space, path)));
    }
    let status = |path: &str| {
        let entry = r
            .files
            .iter()
            .find(|f| f.path == path)
            .unwrap_or_else(|| panic!("{path} not in {:#?}", r.files));
        (entry.status, entry.reason.clone().unwrap_or_default())
    };
    assert_eq!(status("picture.png").0, FileStatus::Attached);
    assert_eq!(status("Broken.md").0, FileStatus::Failed);
    let (s, why) = status("assets/unused.png");
    assert_eq!(s, FileStatus::Skipped);
    assert!(why.contains("no note"), "{why}");
    assert!(status("Board.canvas").1.contains("canvas"));
    assert!(status("Upper.MD").1.contains("lowercase"));
    assert!(status(".obsidian").1.contains("hidden folder"));
    assert!(status(".trash").1.contains("hidden folder"));
    assert_eq!(
        (
            r.counts.created,
            r.counts.attached,
            r.counts.failed,
            r.counts.skipped
        ),
        (4, 1, 1, 6)
    );

    // Links and attachments that did not come through.
    let unresolved: Vec<(&str, &str)> = r
        .unresolved_links
        .iter()
        .map(|l| (l.path.as_str(), l.target.as_str()))
        .collect();
    assert_eq!(unresolved, [("Welcome.md", "Missing note")]);
    assert_eq!(r.ambiguous_links.len(), 1, "{:?}", r.ambiguous_links);
    let ambiguous = &r.ambiguous_links[0];
    assert_eq!(ambiguous.target, "README");
    assert_eq!(
        ambiguous.candidates,
        ["Areas/README.md", "Projects/README.md"]
    );
    assert!(ambiguous.resolved.is_some());
    let missing: Vec<(&str, Vec<String>)> = r
        .missing_attachments
        .iter()
        .map(|m| (m.target.as_str(), m.found.clone()))
        .collect();
    assert_eq!(
        missing,
        [
            ("gone.pdf", vec![]),
            ("shared.png", vec!["assets/shared.png".to_string()])
        ]
    );
    assert!(
        r.failed_links
            .iter()
            .any(|l| l.reason.contains("01JB7Q2A0B1C2D3E4F5G6H7J8K"))
    );

    // It reads as JSON and back.
    let json = serde_json::to_string(&r).unwrap();
    assert_eq!(
        serde_json::from_str::<knode_vault::migration::MigrationReport>(&json).unwrap(),
        r
    );

    // Again: everything unchanged, same nodes.
    let again = import(&mut space, &dir, &me()).unwrap();
    let r2 = report(&space, &dir, &again).unwrap();
    assert_eq!(again.nodes, imported.nodes);
    assert_eq!((r2.counts.created, r2.counts.unchanged), (0, 4));
}

#[test]
fn markdown_images_next_to_the_note_are_attached_and_reported_so() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("Vault");
    put(&dir, "a.md", "# A\n\n![[pic.png]]\n");
    put(&dir, "b.md", "# B\n\n![b](pic2.png)\n");
    put(&dir, "pic.png", [1]);
    put(&dir, "pic2.png", [2]);
    let mut space = Space::create(tmp.path().join("V.knode"), "V").unwrap();
    let imported = import(&mut space, &dir, &me()).unwrap();
    let r = report(&space, &dir, &imported).unwrap();
    assert_eq!(
        (r.counts.attached, r.counts.skipped, r.counts.failed),
        (2, 0, 0),
        "{:#?}",
        r.files
    );
    assert!(
        r.missing_attachments.is_empty(),
        "{:?}",
        r.missing_attachments
    );
}

#[test]
fn a_file_named_by_another_path_is_not_blamed_on_failed_files() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("Vault");
    put(&dir, "n.md", "# N\n\n![n](img/pic.png)\n");
    put(&dir, "pic.png", [1]);
    let mut space = Space::create(tmp.path().join("V.knode"), "V").unwrap();
    let imported = import(&mut space, &dir, &me()).unwrap();
    let r = report(&space, &dir, &imported).unwrap();
    assert_eq!(r.counts.failed, 0);
    let skipped = r.files.iter().find(|f| f.path == "pic.png").unwrap();
    assert_eq!(skipped.status, FileStatus::Skipped);
    let skipped = skipped.reason.clone().unwrap();
    assert_eq!(r.missing_attachments.len(), 1);
    let missing = &r.missing_attachments[0];
    assert_eq!(missing.found, ["pic.png"]);
    assert!(!missing.reason.contains("failed"), "{}", missing.reason);
    assert!(
        missing.reason.contains("another path"),
        "{}",
        missing.reason
    );
    assert!(skipped.contains("another path"), "{skipped}");
}

#[test]
fn a_file_next_to_the_note_that_could_not_be_attached_points_to_the_failed_files() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("Vault");
    put(&dir, "n.md", "# N\n\n![n](pic.png)\n");
    put(&dir, "pic.png", [1]);
    let mut space = Space::create(tmp.path().join("V.knode"), "V").unwrap();
    let mut imported = import(&mut space, &dir, &me()).unwrap();
    // As if attaching it had failed.
    let id = imported.nodes["n.md"];
    space.detach(&id, "pic.png", &me()).unwrap();
    imported.attached.clear();
    imported.attachments.push("pic.png".into());
    imported.skipped.push(knode_vault::Skipped {
        path: "pic.png".into(),
        reason: "too big".into(),
    });
    let r = report(&space, &dir, &imported).unwrap();
    assert_eq!(r.counts.failed, 1);
    assert_eq!(r.missing_attachments.len(), 1);
    assert!(
        r.missing_attachments[0].reason.contains("failed"),
        "{}",
        r.missing_attachments[0].reason
    );
}

#[test]
fn importing_again_keeps_edits_made_in_knode() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("Vault");
    put(&dir, "Source only.md", "One.\n");
    put(&dir, "Knode only.md", "One.\n");
    put(&dir, "Both.md", "One.\n");
    put(&dir, "Neither.md", "One.\n");
    let mut space = Space::create(tmp.path().join("V.knode"), "V").unwrap();
    import(&mut space, &dir, &me()).unwrap();

    put(&dir, "Source only.md", "Two, from the vault.\n");
    edit(&mut space, "Knode only.md", "Two, from Knode.\n");
    put(&dir, "Both.md", "Two, from the vault.\n");
    edit(&mut space, "Both.md", "Two, from Knode.\n");

    let again = import(&mut space, &dir, &me()).unwrap();
    assert_eq!(again.updated, ["Source only.md"]);
    assert_eq!(again.kept, ["Knode only.md"]);
    assert_eq!(again.edited, ["Both.md"]);
    assert_eq!(again.unchanged, 2, "Neither and Knode only");
    assert!(again.created.is_empty());
    assert_eq!(space.node_count().unwrap(), 4);
    assert_eq!(body(&space, "Source only.md"), "Two, from the vault.\n");
    assert_eq!(body(&space, "Knode only.md"), "Two, from Knode.\n");
    assert_eq!(body(&space, "Both.md"), "Two, from Knode.\n");
    let r = report(&space, &dir, &again).unwrap();
    let both = r.files.iter().find(|f| f.path == "Both.md").unwrap();
    assert_eq!(both.status, FileStatus::Edited);
    assert!(both.reason.as_ref().unwrap().contains("merge by hand"));
    assert_eq!((r.counts.edited, r.counts.kept), (1, 1));

    // Still so the next time: the conflict stays until a person resolves it.
    let third = import(&mut space, &dir, &me()).unwrap();
    assert_eq!(third.edited, ["Both.md"]);
    assert_eq!(body(&space, "Both.md"), "Two, from Knode.\n");

    // Once the person copies the vault's text into Knode, the two agree again: unchanged, and
    // later vault changes come in.
    edit(&mut space, "Both.md", "Two, from the vault.\n");
    let fourth = import(&mut space, &dir, &me()).unwrap();
    assert!(fourth.edited.is_empty(), "{fourth:?}");
    put(&dir, "Both.md", "Three, from the vault.\n");
    let fifth = import(&mut space, &dir, &me()).unwrap();
    assert_eq!(fifth.updated, ["Both.md"]);
    assert_eq!(body(&space, "Both.md"), "Three, from the vault.\n");
}

#[test]
fn a_node_imported_by_an_earlier_version_takes_the_file() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("Vault");
    put(&dir, "Note.md", "One.\n");
    let mut space = Space::create(tmp.path().join("V.knode"), "V").unwrap();
    import(&mut space, &dir, &me()).unwrap();
    let id = node(&space, "Note.md");
    assert!(space.remove_property(&id, property_keys::IMPORTED).unwrap());
    edit(&mut space, "Note.md", "From Knode.\n");
    put(&dir, "Note.md", "From the vault.\n");
    let again = import(&mut space, &dir, &me()).unwrap();
    assert_eq!(again.updated, ["Note.md"]);
    assert_eq!(body(&space, "Note.md"), "From the vault.\n");
    // Knode's version is still in the history.
    let history = space.history(&id).unwrap();
    assert!(history.iter().any(|h| h.body == "From Knode.\n"));
}

#[test]
fn an_import_that_stopped_halfway_finishes_when_run_again() {
    let tmp = tempfile::tempdir().unwrap();
    let full = tmp.path().join("Vault");
    put(&full, "A.md", "Links to [[B]].\n");
    put(
        &full,
        "B.md",
        "---\nid: 01JB7Q9W8K3M2N4P5R6S7T8V9X\nlinks:\n  related: [01JB7Q2A0B1C2D3E4F5G6H7J8K]\n---\nB text.\n",
    );
    put(
        &full,
        "Folder/C.md",
        "---\nid: 01JB7Q2A0B1C2D3E4F5G6H7J8K\n---\nC text, ![[c.png]].\n",
    );
    put(&full, "Folder/c.png", [7, 7]);
    put(&full, "D.md", [0xff]);

    // The first run read only part of the vault, as if it stopped after A.
    let partial = tmp.path().join("Partial");
    put(&partial, "A.md", "Links to [[B]].\n");
    let mut space = Space::create(tmp.path().join("V.knode"), "V").unwrap();
    let first = import(&mut space, &partial, &me()).unwrap();
    assert_eq!(first.created, ["A.md"]);

    // Run again on the whole vault: A is found, the rest written, the links made.
    let second = import(&mut space, &full, &me()).unwrap();
    assert_eq!(second.created, ["B.md", "Folder/C.md"]);
    assert_eq!(second.unchanged, 1);
    assert_eq!(second.links, 1);
    assert_eq!(second.attached, ["Folder/c.png"]);
    assert_eq!(second.skipped.len(), 1, "D.md is not UTF-8");
    assert_eq!(space.node_count().unwrap(), 3);
    let a = node(&space, "A.md");
    assert_eq!(space.links_from(&a).unwrap().len(), 1, "A mentions B now");

    // The file that failed is fixed, and the import retried: only D is new.
    put(&full, "D.md", "Fixed.\n");
    let third = import(&mut space, &full, &me()).unwrap();
    assert_eq!(third.created, ["D.md"]);
    assert_eq!(third.unchanged, 3);
    assert!(third.skipped.is_empty());
    assert_eq!(third.links, 1, "the typed link already exists");
    assert_eq!(space.node_count().unwrap(), 4);
}

#[test]
fn a_note_deleted_in_knode_is_not_imported_again() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("Vault");
    put(&dir, "Keep.md", "Kept.\n");
    put(&dir, "Gone.md", "Deleted in Knode.\n");
    put(
        &dir,
        "With id.md",
        "---\nid: 01JB7Q9W8K3M2N4P5R6S7T8V9X\n---\nAn exported note.\n",
    );
    let mut space = Space::create(tmp.path().join("V.knode"), "V").unwrap();
    let first = import(&mut space, &dir, &me()).unwrap();
    assert_eq!(first.created.len(), 3);
    assert!(
        space
            .setting(setting_keys::IMPORTED_PATHS)
            .unwrap()
            .unwrap()
            .contains("Gone.md")
    );
    let gone = node(&space, "Gone.md");
    let with_id = node(&space, "With id.md");
    space.delete_node(&gone, &me()).unwrap();
    space.delete_node(&with_id, &me()).unwrap();

    // Imported again: both stay deleted, and the report says so.
    let again = import(&mut space, &dir, &me()).unwrap();
    assert!(again.created.is_empty(), "{again:?}");
    assert_eq!(again.unchanged, 1);
    assert_eq!(again.deleted.len(), 2);
    assert_eq!(again.deleted["Gone.md"], gone);
    assert_eq!(again.deleted["With id.md"], with_id);
    assert_eq!(space.node_count().unwrap(), 1);
    let r = report(&space, &dir, &again).unwrap();
    let entry = r.files.iter().find(|f| f.path == "Gone.md").unwrap();
    assert_eq!(entry.status, FileStatus::Skipped);
    assert_eq!(entry.reason.as_deref(), Some(DELETED_REASON));
    assert_eq!(entry.node, Some(gone));
    assert_eq!(r.counts.deleted, 2);

    // Still so when the file changed since.
    put(&dir, "Gone.md", "Changed in the vault.\n");
    let third = import(&mut space, &dir, &me()).unwrap();
    assert_eq!(third.deleted.len(), 2);
    assert_eq!(space.node_count().unwrap(), 1);

    // A new file is imported as always.
    put(&dir, "New.md", "New.\n");
    let fourth = import(&mut space, &dir, &me()).unwrap();
    assert_eq!(fourth.created, ["New.md"]);

    // Asked for, they come back as notes, once.
    let options = ImportOptions {
        include_deleted: true,
    };
    let back = import_with(&mut space, &dir, &me(), &options).unwrap();
    let mut created = back.created.clone();
    created.sort();
    assert_eq!(created, ["Gone.md", "With id.md"]);
    assert!(back.deleted.is_empty());
    assert_eq!(space.node_count().unwrap(), 4);
    assert_eq!(body(&space, "Gone.md"), "Changed in the vault.\n");
    let after = import(&mut space, &dir, &me()).unwrap();
    assert!(
        after.created.is_empty() && after.deleted.is_empty(),
        "{after:?}"
    );
    assert_eq!(space.node_count().unwrap(), 4);
}

#[test]
fn a_file_renamed_without_an_id_becomes_a_new_note() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("Vault");
    put(&dir, "Old name.md", "Text.\n");
    let mut space = Space::create(tmp.path().join("V.knode"), "V").unwrap();
    import(&mut space, &dir, &me()).unwrap();
    fs::rename(dir.join("Old name.md"), dir.join("New name.md")).unwrap();
    let again = import(&mut space, &dir, &me()).unwrap();
    assert_eq!(again.created, ["New name.md"]);
    assert_eq!(space.node_count().unwrap(), 2, "the old node stays");
}

#[test]
fn an_export_compared_with_the_vault_shows_only_what_changed() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("Vault");
    vault(&dir);
    fs::remove_file(dir.join("Broken.md")).unwrap();
    let mut space = Space::create(tmp.path().join("V.knode"), "V").unwrap();
    import(&mut space, &dir, &me()).unwrap();
    let out = tmp.path().join("Export");
    export(&space, &out).unwrap();

    let c = compare(&dir, &out).unwrap();
    // Notes: the same text, Knode's id added to the frontmatter.
    assert_eq!(c.notes.len(), 4);
    for note in &c.notes {
        assert_eq!(note.difference, Difference::Metadata, "{note:?}");
        if note.path == "Projects/Plan.md" {
            // It had its own id; its typed link to a note that is not there was not made.
            assert!(note.added_keys.is_empty(), "{note:?}");
            assert_eq!(note.removed_keys, ["links"], "{note:?}");
        } else {
            assert_eq!(note.added_keys, ["id"], "{note:?}");
            assert!(note.removed_keys.is_empty(), "{note:?}");
        }
    }
    let file = |path: &str| c.files.iter().find(|f| f.path == path).unwrap().difference;
    assert_eq!(file("picture.png"), Difference::Identical);
    assert_eq!(file("assets/shared.png"), Difference::Missing);
    assert_eq!(file("Board.canvas"), Difference::Missing);
    assert!(c.excluded.iter().any(|e| e.path == ".obsidian"));
    assert!(c.added.is_empty(), "{:?}", c.added);
    assert!(!c.text_preserved());

    // An edit in Knode and a note made there show up.
    edit(
        &mut space,
        "Projects/README.md",
        "Projects readme.\nMore.\n",
    );
    space
        .create_node(knode_format::NewNode::note("Made in Knode", "New."), &me())
        .unwrap();
    export(&space, &out).unwrap();
    let c = compare(&dir, &out).unwrap();
    let readme = c
        .notes
        .iter()
        .find(|n| n.path == "Projects/README.md")
        .unwrap();
    assert_eq!(readme.difference, Difference::Changed);
    assert_eq!(readme.first_changed_line, Some(2));
    assert_eq!(c.added, ["Made in Knode.md"]);
    assert_eq!(c.counts.changed, 1);
}
