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
    put(&vault, "Alpha/Estado.md", "Alpha status.\n");
    put(&vault, "Alpha/Plan.md", "See [[Estado]].\n");
    put(
        &vault,
        "Basket/Season/Plan.md",
        "See [[Estado]] and [[app/README]].\n",
    );
    put(&vault, "Basket/Estado.md", "Basket status.\n");
    put(&vault, "Labs/app/README.md", "The other app.\n");
    put(&vault, "Basket/Season/app/README.md", "This app.\n");
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
        vec![by_path(&space, "Alpha/Estado.md")]
    );
    let mut season = vec![
        by_path(&space, "Basket/Estado.md"),
        by_path(&space, "Basket/Season/app/README.md"),
    ];
    season.sort();
    assert_eq!(targets(&space, "Basket/Season/Plan.md"), season);
    // Of the two notes named Plan, Alpha/Plan.md is fewer folders away from Zeta/Note.md.
    assert_eq!(
        targets(&space, "Zeta/Note.md"),
        vec![by_path(&space, "Alpha/Plan.md")]
    );
    // Editing the note keeps the same targets: they are part of what its body means.
    let id = by_path(&space, "Basket/Season/Plan.md");
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
    assert_eq!(targets(&space, "Basket/Season/Plan.md"), season);
}
