//! Importing one Markdown file: its identity is where it is, so files with the same name in two
//! folders are two notes, and the same file imported again updates its note.

use std::fs;
use std::path::Path;

use knode_format::{Provenance, Space};
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
