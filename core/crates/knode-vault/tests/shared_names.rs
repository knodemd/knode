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

/// Inside a table, Obsidian escapes a label's `|` as `\|`; the link still goes to its note.
#[test]
fn links_in_tables_with_an_escaped_pipe_find_their_note() {
    let dir = tempfile::tempdir().unwrap();
    let vault = dir.path().join("vault");
    put(&vault, "Start/web/README.md", "The website.\n");
    put(
        &vault,
        "Index.md",
        "| Project | Notes |\n|---|---|\n| Web | [[Start/web/README\\|web]] |\n",
    );
    let mut space = Space::create(dir.path().join("Space.knode"), "Space").unwrap();
    import(&mut space, &vault, &me()).unwrap();
    let id = |path: &str| space.find_by_property(property_keys::PATH, path).unwrap()[0].id;
    let to: Vec<_> = space
        .links_from(&id("Index.md"))
        .unwrap()
        .into_iter()
        .filter(|l| l.kind == "mentions")
        .map(|l| l.to)
        .collect();
    assert_eq!(to, vec![id("Start/web/README.md")]);
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
    let report = knode_vault::import_file(
        &mut space,
        &file,
        "~/notes/fichero.md",
        Some("Mi nota".into()),
        &me(),
    )
    .unwrap();
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
    let again = knode_vault::import_file(
        &mut space,
        &file,
        "~/notes/fichero.md",
        Some("Mi nota".into()),
        &me(),
    )
    .unwrap();
    assert_eq!((again.created.len(), again.updated.len()), (0, 1));
    assert_eq!(
        space
            .find_by_property(property_keys::PATH, "fichero.md")
            .unwrap()
            .len(),
        1
    );
}

/// Two files with the same name in different folders, imported one at a time, are two notes,
/// and importing each again updates its own note in place.
#[test]
fn single_files_with_the_same_name_in_two_folders_are_two_notes() {
    let dir = tempfile::tempdir().unwrap();
    put(dir.path(), "a/README.md", "# A\n\nFirst.\n");
    put(dir.path(), "b/README.md", "# B\n\nSecond.\n");
    let (a, b) = (
        dir.path().join("a/README.md"),
        dir.path().join("b/README.md"),
    );
    let mut space = Space::create(dir.path().join("Space.knode"), "Space").unwrap();
    let first = knode_vault::import_file(&mut space, &a, "~/a/README.md", None, &me()).unwrap();
    let second = knode_vault::import_file(&mut space, &b, "~/b/README.md", None, &me()).unwrap();
    assert_eq!((first.created.len(), second.created.len()), (1, 1));
    assert_eq!(second.updated.len(), 0);
    fn node(space: &Space, key: &str) -> knode_format::Node {
        let found = space.find_by_property(property_keys::FILE, key).unwrap();
        assert_eq!(found.len(), 1, "{key}");
        space.get_node(&found[0].id).unwrap().unwrap()
    }
    let (node_a, node_b) = (node(&space, "~/a/README.md"), node(&space, "~/b/README.md"));
    assert_ne!(node_a.id, node_b.id);
    assert!(node_a.body.contains("First."));
    assert!(node_b.body.contains("Second."));

    std::fs::write(&a, "# A\n\nFirst, changed.\n").unwrap();
    let again = knode_vault::import_file(&mut space, &a, "~/a/README.md", None, &me()).unwrap();
    assert_eq!((again.created.len(), again.updated.len()), (0, 1));
    assert_eq!(node(&space, "~/a/README.md").id, node_a.id);
    assert!(
        node(&space, "~/a/README.md")
            .body
            .contains("First, changed.")
    );
    assert!(node(&space, "~/b/README.md").body.contains("Second."));
    let unchanged = knode_vault::import_file(&mut space, &a, "~/a/README.md", None, &me()).unwrap();
    assert_eq!((unchanged.created.len(), unchanged.updated.len()), (0, 0));
    assert_eq!(space.node_count().unwrap(), 2);
}

/// A folder's `README.md` is never the note a `README.md` imported on its own made, and the
/// other way round: each import keeps updating its own note.
#[test]
fn a_folder_file_and_a_single_file_with_the_same_name_are_two_notes() {
    let dir = tempfile::tempdir().unwrap();
    let vault = dir.path().join("vault");
    put(&vault, "README.md", "Folder readme.\n");
    put(dir.path(), "alone/README.md", "Single readme.\n");
    let single = dir.path().join("alone/README.md");
    let mut space = Space::create(dir.path().join("Space.knode"), "Space").unwrap();

    let one =
        knode_vault::import_file(&mut space, &single, "~/alone/README.md", None, &me()).unwrap();
    assert_eq!(one.created.len(), 1);
    let folder = import(&mut space, &vault, &me()).unwrap();
    assert_eq!((folder.created.len(), folder.updated.len()), (1, 0));
    let folder = import(&mut space, &vault, &me()).unwrap();
    assert_eq!((folder.created.len(), folder.updated.len()), (0, 0));
    let one =
        knode_vault::import_file(&mut space, &single, "~/alone/README.md", None, &me()).unwrap();
    assert_eq!((one.created.len(), one.updated.len()), (0, 0));

    let found = space
        .find_by_property(property_keys::PATH, "README.md")
        .unwrap();
    assert_eq!(found.len(), 2);
    let bodies: Vec<String> = found
        .iter()
        .map(|s| space.get_node(&s.id).unwrap().unwrap().body)
        .collect();
    assert!(
        bodies.iter().any(|b| b.contains("Folder readme.")),
        "{bodies:?}"
    );
    assert!(
        bodies.iter().any(|b| b.contains("Single readme.")),
        "{bodies:?}"
    );
}
