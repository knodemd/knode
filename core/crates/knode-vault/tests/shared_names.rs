//! Links to a name several notes share go to the nearest note.

mod common;

use common::put;
use knode_format::{NodeId, NodeUpdate, Provenance, Space};
use knode_vault::{import, property_keys};

fn me() -> Provenance {
    Provenance::person("Ada")
}

/// Obsidian resolves a link to a name several notes share by the note nearest to the one linking,
/// whatever order the files are imported in, and so does a link written as a partial path.
#[test]
fn links_to_a_shared_name_go_to_the_nearest_note() {
    let dir = tempfile::tempdir().unwrap();
    let vault = dir.path().join("vault");
    put(&vault, "Alpha/Status.md", "Alpha status.\n");
    put(&vault, "Alpha/Plan.md", "See [[Status]].\n");
    put(
        &vault,
        "Beta/Sprint/Plan.md",
        "See [[Status]] and [[app/README]].\n",
    );
    put(&vault, "Beta/Status.md", "Beta status.\n");
    put(&vault, "Other/app/README.md", "The other app.\n");
    put(&vault, "Beta/Sprint/app/README.md", "This app.\n");
    put(
        &vault,
        "Zeta/Note.md",
        "No path of its own picks the oldest: [[Plan]].\n",
    );
    let mut space = Space::create(dir.path().join("Space.knode"), "Space").unwrap();
    import(&mut space, &vault, &me()).unwrap();
    fn by_path(space: &Space, path: &str) -> NodeId {
        space.find_by_property(property_keys::PATH, path).unwrap()[0].id
    }
    fn targets(space: &Space, path: &str) -> Vec<NodeId> {
        let mut to: Vec<_> = space
            .links_from(&by_path(space, path))
            .unwrap()
            .into_iter()
            .filter(|l| l.kind == "mentions")
            .map(|l| l.to)
            .collect();
        to.sort();
        to
    }
    assert_eq!(
        targets(&space, "Alpha/Plan.md"),
        vec![by_path(&space, "Alpha/Status.md")]
    );
    let mut sprint = vec![
        by_path(&space, "Beta/Status.md"),
        by_path(&space, "Beta/Sprint/app/README.md"),
    ];
    sprint.sort();
    assert_eq!(targets(&space, "Beta/Sprint/Plan.md"), sprint);
    // Of the two notes named Plan, Alpha/Plan.md is fewer folders away from Zeta/Note.md.
    assert_eq!(
        targets(&space, "Zeta/Note.md"),
        vec![by_path(&space, "Alpha/Plan.md")]
    );
    // Editing the note keeps the same targets: they are part of what its body means.
    let id = by_path(&space, "Beta/Sprint/Plan.md");
    let body = space.get_node(&id).unwrap().unwrap().body;
    space
        .update_node(
            &id,
            NodeUpdate {
                body: Some(format!("{body}More.\n")),
                ..Default::default()
            },
            &me(),
        )
        .unwrap();
    assert_eq!(targets(&space, "Beta/Sprint/Plan.md"), sprint);
}

/// One file is read like a folder of one: its frontmatter kept as properties rather than text,
/// named by the title given, and importing it again updates the same note.
#[test]
fn a_single_file_keeps_its_frontmatter_and_is_updated_on_reimport() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("notes").join("fichero.md");
    put(
        &dir.path().join("notes"),
        "fichero.md",
        "---\ntitle: Mi nota\ntags: [a]\n---\nCuerpo.\n\n## Sección\n\nTexto.\n",
    );
    let mut space = Space::create(dir.path().join("Space.knode"), "Space").unwrap();
    let report =
        knode_vault::import_file(&mut space, &file, Some("Mi nota".into()), &me()).unwrap();
    assert_eq!(report.created.len(), 1);
    let found = space
        .find_by_property(property_keys::PATH, "fichero.md")
        .unwrap();
    let node = space.get_node(&found[0].id).unwrap().unwrap();
    assert_eq!(node.title, "Mi nota");
    assert!(!node.body.contains("title:"));
    std::fs::write(
        &file,
        "---\ntitle: Mi nota\ntags: [a]\n---\nCuerpo nuevo.\n",
    )
    .unwrap();
    let again = knode_vault::import_file(&mut space, &file, Some("Mi nota".into()), &me()).unwrap();
    assert_eq!((again.created.len(), again.updated.len()), (0, 1));
    assert_eq!(
        space
            .find_by_property(property_keys::PATH, "fichero.md")
            .unwrap()
            .len(),
        1
    );
}
