//! Import an Obsidian vault and export it back.
//!
//! "Unchanged" means: every note of the vault is exported to the same path, and its file is the
//! original file with one line, `id: <the node id>`, added as the first line of its frontmatter
//! (or with a frontmatter of just that line added, when it had none), using the file's own line
//! endings. Nothing else is added, removed or reordered. Hidden folders and attachments are not
//! exported.

mod common;

use std::collections::BTreeMap;
use std::path::Path;

use common::{assert_same, put, tree};
use knode_format::{NodeId, NodeUpdate, Provenance, Space};
use knode_vault::{STATE_DIR, export, import, property_keys};

const NOTES: &[(&str, &str)] = &[
    (
        "Welcome.md",
        "# Welcome\n\nStart with [[Plan|the plan]], its [[Plan#Goals]], ![[Diagram.png]] and ![[Old idea]].\nTagged #inbox and #project/active, not #42.\n\n```\n#notatag [[Not a link]]\n```\n",
    ),
    (
        "Projects/Plan.md",
        "---\naliases: [roadmap, \"the plan\"]\ntags:\n  - project\n  - Work\ncreated: 2023-04-01\nstatus: draft # still\n---\n# Plan\n\n## Goals\n\nShip. Back to [[Welcome]].\n",
    ),
    (
        "Projects/Archive/Old idea.md",
        "---\ntags: [archive]\n---\nAn old #idea.\n",
    ),
    (
        "Daily/2026-10-20.md",
        "---\r\nmood: good\r\n---\r\nToday I read [[Café de ideas]].\r\n",
    ),
    ("Café de ideas.md", "No frontmatter, no newline at the end"),
    ("Empty frontmatter.md", "---\n---\nJust text.\n"),
    ("CRLF only.md", "Line one\r\nLine two\r\n"),
    ("Note [draft].md", "A file name with brackets.\n"),
];

fn me() -> Provenance {
    Provenance::person("Ada")
}

fn vault(dir: &Path) {
    for (path, text) in NOTES {
        put(dir, path, text);
    }
    put(dir, ".obsidian/app.json", "{}");
    put(dir, ".obsidian/workspace.md", "Not a note.");
    put(dir, ".trash/Deleted.md", "Deleted note.");
    put(dir, "attachments/Diagram.png", [0x89, b'P', b'N', b'G']);
    put(dir, "Board.canvas", "{}");
}

/// The original file with the id added, as "unchanged" allows.
fn with_id(text: &str, id: &str) -> String {
    if let Some(rest) = text.strip_prefix("---\n") {
        format!("---\nid: {id}\n{rest}")
    } else if let Some(rest) = text.strip_prefix("---\r\n") {
        format!("---\r\nid: {id}\r\n{rest}")
    } else if text.split('\n').next().is_some_and(|l| l.ends_with('\r')) {
        format!("---\r\nid: {id}\r\n---\r\n{text}")
    } else {
        format!("---\nid: {id}\n---\n{text}")
    }
}

/// The id of the node imported from each path.
fn ids(space: &Space) -> BTreeMap<String, NodeId> {
    NOTES
        .iter()
        .map(|(path, _)| {
            let found = space.find_by_property(property_keys::PATH, path).unwrap();
            assert_eq!(found.len(), 1, "{path}");
            (path.to_string(), found[0].id)
        })
        .collect()
}

#[test]
fn imports_notes_folders_tags_and_links() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("Vault");
    vault(&dir);
    let mut space = Space::create(tmp.path().join("V.knode"), "V").unwrap();
    let report = import(&mut space, &dir, &me()).unwrap();
    assert_eq!(report.created.len(), NOTES.len(), "{report:?}");
    assert!(report.skipped.is_empty(), "{report:?}");
    assert_eq!(
        report.attachments,
        ["Board.canvas", "attachments/Diagram.png"]
    );
    assert_eq!(space.node_count().unwrap(), NOTES.len() as u64);

    let ids = ids(&space);
    let node = |path: &str| space.get_node(&ids[path]).unwrap().unwrap();

    let welcome = node("Welcome.md");
    assert_eq!(welcome.title, "Welcome");
    assert_eq!(welcome.body, NOTES[0].1, "the body is kept as written");
    assert_eq!(welcome.tags, ["inbox", "project/active"]);
    let mut targets: Vec<String> = space
        .links_from(&welcome.id)
        .unwrap()
        .into_iter()
        .map(|l| l.to_title)
        .collect();
    targets.sort();
    assert_eq!(targets, ["Old idea", "Plan"]);
    assert_eq!(
        space.unresolved_links(&welcome.id).unwrap(),
        ["Diagram.png"]
    );

    let plan = node("Projects/Plan.md");
    assert_eq!(plan.tags, ["project", "work"]);
    assert_eq!(
        plan.body,
        "# Plan\n\n## Goals\n\nShip. Back to [[Welcome]].\n"
    );
    assert_eq!(space.backlinks(&plan.id).unwrap()[0].from, welcome.id);
    assert_eq!(
        space
            .property(&plan.id, property_keys::FRONTMATTER)
            .unwrap()
            .as_deref(),
        Some(
            "aliases: [roadmap, \"the plan\"]\ntags:\n  - project\n  - Work\ncreated: 2023-04-01\nstatus: draft # still\n"
        )
    );

    assert_eq!(
        node("Projects/Archive/Old idea.md").tags,
        ["archive", "idea"]
    );
    assert_eq!(node("Note [draft].md").title, "Note (draft)");
    assert_eq!(
        node("Daily/2026-10-20.md").body,
        "Today I read [[Café de ideas]].\r\n"
    );
}

#[test]
fn import_then_export_gives_the_vault_back_with_ids() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("Vault");
    vault(&dir);
    let mut space = Space::create(tmp.path().join("V.knode"), "V").unwrap();
    import(&mut space, &dir, &me()).unwrap();
    let ids = ids(&space);

    let out = tmp.path().join("Out");
    export(&space, &out).unwrap();
    let mut exported = tree(&out);
    assert!(
        exported
            .remove(&format!("{STATE_DIR}/files.json"))
            .is_some()
    );
    let expected: BTreeMap<String, Vec<u8>> = NOTES
        .iter()
        .map(|(path, text)| {
            let id = ids[*path].to_string();
            (path.to_string(), with_id(text, &id).into_bytes())
        })
        .collect();
    assert_same(&expected, &exported);
}

#[test]
fn importing_again_never_duplicates() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("Vault");
    vault(&dir);
    let mut space = Space::create(tmp.path().join("V.knode"), "V").unwrap();
    import(&mut space, &dir, &me()).unwrap();

    // The same vault again: matched by path.
    let again = import(&mut space, &dir, &me()).unwrap();
    assert_eq!(
        (again.created.len(), again.updated.len(), again.unchanged),
        (0, 0, NOTES.len())
    );

    // The exported vault, with ids: matched by id, and exported again unchanged.
    let out = tmp.path().join("Out");
    export(&space, &out).unwrap();
    let first = tree(&out);
    let back = import(&mut space, &out, &me()).unwrap();
    assert_eq!(back.created.len(), 0, "{back:?}");
    assert_eq!(space.node_count().unwrap(), NOTES.len() as u64);
    export(&space, &out).unwrap();
    assert_same(&first, &tree(&out));

    // Into a new space: the same files again.
    let mut copy = Space::create(tmp.path().join("C.knode"), "C").unwrap();
    import(&mut copy, &out, &me()).unwrap();
    let copy_out = tmp.path().join("CopyOut");
    export(&copy, &copy_out).unwrap();
    assert_same(&first, &tree(&copy_out));
}

#[test]
fn changes_made_in_knode_show_in_the_frontmatter() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("Vault");
    vault(&dir);
    let mut space = Space::create(tmp.path().join("V.knode"), "V").unwrap();
    import(&mut space, &dir, &me()).unwrap();
    let ids = ids(&space);

    space
        .update_node(
            &ids["Projects/Plan.md"],
            NodeUpdate {
                tags: Some(vec!["project".into(), "work".into(), "urgent".into()]),
                ..Default::default()
            },
            &me(),
        )
        .unwrap();
    space
        .update_node(
            &ids["Welcome.md"],
            NodeUpdate {
                kind: Some("idea".into()),
                ..Default::default()
            },
            &me(),
        )
        .unwrap();
    space
        .update_node(
            &ids["Café de ideas.md"],
            NodeUpdate {
                title: Some("Ideas/Café".into()),
                ..Default::default()
            },
            &me(),
        )
        .unwrap();

    let out = tmp.path().join("Out");
    export(&space, &out).unwrap();
    let read = |path: &str| std::fs::read_to_string(out.join(path)).unwrap();
    assert_eq!(
        read("Projects/Plan.md"),
        format!(
            "---\nid: {}\naliases: [roadmap, \"the plan\"]\ntags: [project, urgent, work]\ncreated: 2023-04-01\nstatus: draft # still\n---\n# Plan\n\n## Goals\n\nShip. Back to [[Welcome]].\n",
            ids["Projects/Plan.md"]
        )
    );
    assert!(read("Welcome.md").starts_with(&format!(
        "---\nid: {}\nkind: idea\n---\n# Welcome",
        ids["Welcome.md"]
    )));
    assert!(!out.join("Café de ideas.md").exists());
    assert_eq!(
        read("Ideas-Café.md"),
        format!(
            "---\nid: {}\ntitle: Ideas/Café\n---\nNo frontmatter, no newline at the end",
            ids["Café de ideas.md"]
        )
    );
    // The link to it was rewritten to the new title.
    assert!(read("Daily/2026-10-20.md").contains("[[Ideas/Café]]"));

    // And they come back in.
    let mut copy = Space::create(tmp.path().join("C.knode"), "C").unwrap();
    import(&mut copy, &out, &me()).unwrap();
    let welcome = &copy.find_by_title("Welcome").unwrap()[0];
    assert_eq!(welcome.kind, "idea");
    let plan = copy.find_by_title("Plan").unwrap()[0].id;
    assert_eq!(
        copy.get_node(&plan).unwrap().unwrap().tags,
        ["project", "urgent", "work"]
    );
    assert_eq!(copy.find_by_title("Ideas/Café").unwrap().len(), 1);
}
