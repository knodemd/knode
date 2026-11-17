//! Importing one Markdown file: its identity is where it is, so files with the same name in two
//! folders are two notes, the same file imported again updates its note, and a note an earlier
//! version of Knode made from the file is updated rather than copied.

use std::fs;
use std::path::Path;

use knode_format::{NewNode, Provenance, Space};
use knode_service::import::{ImportResult, prepare, run};
use knode_vault::property_keys;

fn me() -> Provenance {
    Provenance::person("Ada")
}

fn import(space: &mut Space, file: &Path) -> ImportResult {
    let what = prepare(Some(file), None, None).unwrap();
    run(space, &what, &me()).unwrap()
}

fn counts(result: &ImportResult) -> (usize, usize, usize) {
    (result.created, result.updated, result.unchanged)
}

fn put(path: &Path, text: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

#[test]
fn files_with_the_same_name_in_two_folders_are_two_notes() {
    let dir = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(dir.path()).unwrap();
    let (a, b) = (root.join("a/README.md"), root.join("b/README.md"));
    put(&a, "# Readme\n\nProject A.\n");
    put(&b, "# Readme\n\nProject B.\n");
    let mut space = Space::create(root.join("Space.knode"), "Space").unwrap();

    assert_eq!(counts(&import(&mut space, &a)), (1, 0, 0));
    assert_eq!(counts(&import(&mut space, &b)), (1, 0, 0));
    assert_eq!(space.node_count().unwrap(), 2);
    let bodies: Vec<String> = space
        .find_by_title("Readme")
        .unwrap()
        .iter()
        .map(|s| space.get_node(&s.id).unwrap().unwrap().body)
        .collect();
    assert!(
        bodies.iter().any(|b| b.contains("Project A.")),
        "{bodies:?}"
    );
    assert!(
        bodies.iter().any(|b| b.contains("Project B.")),
        "{bodies:?}"
    );

    // The same file again: unchanged, then updated in place once it changes.
    assert_eq!(counts(&import(&mut space, &a)), (0, 0, 1));
    fs::write(&a, "# Readme\n\nProject A, changed.\n").unwrap();
    assert_eq!(counts(&import(&mut space, &a)), (0, 1, 0));
    assert_eq!(space.node_count().unwrap(), 2);
    let key = knode_service::markdown::record_path(&a);
    let found = space.find_by_property(property_keys::FILE, &key).unwrap();
    assert_eq!(found.len(), 1);
    let node = space.get_node(&found[0].id).unwrap().unwrap();
    assert!(node.body.contains("Project A, changed."));
}

/// What the importer before `markdown.file` wrote: a note titled by the first heading (else the
/// file name), the whole file as its body, the file as its source.
fn older_import(space: &mut Space, file: &Path, title: &str, text: &str) -> knode_format::NodeId {
    let by = me().with_source(file.display().to_string());
    space
        .create_node(NewNode::note(title, text), &by)
        .unwrap()
        .id
}

#[test]
fn a_note_an_earlier_version_imported_is_updated_not_copied() {
    let dir = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(dir.path()).unwrap();
    let file = root.join("notes/plan.md");
    let text = "# Plan\n\nThe plan.\n";
    put(&file, text);
    let mut space = Space::create(root.join("Space.knode"), "Space").unwrap();
    let old = older_import(&mut space, &file, "Plan", text);

    let result = import(&mut space, &file);
    assert_eq!(result.created, 0, "{result:?}");
    assert_eq!(space.node_count().unwrap(), 1);
    let key = knode_service::markdown::record_path(&file);
    assert_eq!(
        space.find_by_property(property_keys::FILE, &key).unwrap()[0].id,
        old
    );
    assert_eq!(counts(&import(&mut space, &file)), (0, 0, 1));
}

#[test]
fn a_changed_file_still_finds_the_note_an_earlier_version_made_from_it() {
    let dir = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(dir.path()).unwrap();
    let file = root.join("notes/plan.md");
    put(&file, "# Plan\n\nThe plan, changed since.\n");
    let mut space = Space::create(root.join("Space.knode"), "Space").unwrap();
    // Made from this file by the earlier importer, before the file changed.
    let old = older_import(&mut space, &file, "Plan", "# Plan\n\nThe plan.\n");
    // A note with that title from somewhere else is left alone.
    let other = space
        .create_node(NewNode::note("Plan", "Another plan.\n"), &me())
        .unwrap()
        .id;

    assert_eq!(counts(&import(&mut space, &file)), (0, 1, 0));
    assert_eq!(space.node_count().unwrap(), 2);
    let node = space.get_node(&old).unwrap().unwrap();
    assert!(node.body.contains("The plan, changed since."));
    assert_eq!(
        space.get_node(&other).unwrap().unwrap().body,
        "Another plan.\n"
    );
}

#[test]
fn a_note_with_the_same_title_and_another_body_is_not_adopted() {
    let dir = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(dir.path()).unwrap();
    let file = root.join("plan.md");
    put(&file, "# Plan\n\nMine.\n");
    let mut space = Space::create(root.join("Space.knode"), "Space").unwrap();
    space
        .create_node(NewNode::note("Plan", "Someone else's.\n"), &me())
        .unwrap();
    assert_eq!(counts(&import(&mut space, &file)), (1, 0, 0));
    assert_eq!(space.node_count().unwrap(), 2);
}
