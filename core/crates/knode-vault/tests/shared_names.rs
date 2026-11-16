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

/// Obsidian's "relative path to file" setting writes `[[../folder/note]]`: such a link goes to
/// that file, whether it is imported before or after the note that links to it.
#[test]
fn relative_links_go_to_the_file_they_name() {
    let dir = tempfile::tempdir().unwrap();
    let vault = dir.path().join("vault");
    // `Course` is imported before `Research`: the first link waits for its file.
    put(
        &vault,
        "OFM/Course/Plan.md",
        "See [[../Research/Insights]], [[./Glossary]] and [[../../Index.md|the index]].\n",
    );
    put(&vault, "OFM/Course/Glossary.md", "Words.\n");
    put(&vault, "OFM/Research/Insights.md", "Findings.\n");
    put(
        &vault,
        "Other/Research/Insights.md",
        "Another project's findings.\n",
    );
    put(&vault, "Index.md", "Everything.\n");
    put(
        &vault,
        "OFM/Research/Notes.md",
        "Back to [[../Course/Plan]].\n",
    );
    let mut space = Space::create(dir.path().join("Space.knode"), "Space").unwrap();
    import(&mut space, &vault, &me()).unwrap();
    let id = |path: &str| space.find_by_property(property_keys::PATH, path).unwrap()[0].id;
    let targets = |path: &str| {
        let mut to: Vec<_> = space
            .links_from(&id(path))
            .unwrap()
            .into_iter()
            .filter(|l| l.kind == "mentions")
            .map(|l| l.to)
            .collect();
        to.sort();
        to
    };
    let mut plan = vec![
        id("OFM/Research/Insights.md"),
        id("OFM/Course/Glossary.md"),
        id("Index.md"),
    ];
    plan.sort();
    assert_eq!(targets("OFM/Course/Plan.md"), plan);
    assert_eq!(
        targets("OFM/Research/Notes.md"),
        vec![id("OFM/Course/Plan.md")]
    );
}
