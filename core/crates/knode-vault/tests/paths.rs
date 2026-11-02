//! Lists and search say which file a note came from.

mod common;

use common::put;
use knode_format::{Provenance, Space};
use knode_vault::import;

fn me() -> Provenance {
    Provenance::person("Ada")
}

/// Lists and search say which file a note came from, to tell apart notes of the same name.
#[test]
fn summaries_carry_the_imported_path() {
    let dir = tempfile::tempdir().unwrap();
    let vault = dir.path().join("vault");
    put(&vault, "Alpha/README.md", "Alpha readme.\n");
    put(&vault, "Beta/README.md", "Beta readme.\n");
    let mut space = Space::create(dir.path().join("Space.knode"), "Space").unwrap();
    import(&mut space, &vault, &me()).unwrap();
    space
        .create_node(knode_format::NewNode::note("Written here", "readme"), &me())
        .unwrap();
    let mut paths: Vec<_> = space
        .find_by_title("README")
        .unwrap()
        .into_iter()
        .map(|n| n.path)
        .collect();
    paths.sort();
    assert_eq!(
        paths,
        vec![
            Some("Alpha/README.md".to_owned()),
            Some("Beta/README.md".to_owned())
        ]
    );
    let recent = space.recent(10).unwrap();
    let written = recent.iter().find(|n| n.title == "Written here").unwrap();
    assert_eq!(written.path, None);
    assert!(recent.iter().filter(|n| n.path.is_some()).count() == 2);
    let hits = space.search("readme", 10).unwrap();
    assert!(
        hits.iter()
            .any(|h| h.node.path.as_deref() == Some("Beta/README.md"))
    );
}
