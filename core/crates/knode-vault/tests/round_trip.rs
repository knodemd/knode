//! Export a space, import it into another one, export again: the same files, byte for byte.

mod common;

use std::fs;

use common::{assert_same, put, tree};
use knode_format::{NewNode, NodeUpdate, Properties, Provenance, Space, link_kinds};
use knode_vault::{export, import, property_keys};

fn me() -> Provenance {
    Provenance::person("Ada")
}

/// A space with what a real one has: kinds, tags, properties, typed links, `[[links]]` by title,
/// by id and to nothing yet, titles that need care as file names, and two notes with one title.
fn sample(dir: &std::path::Path) -> Space {
    let mut space = Space::create(dir.join("Sample.knode"), "Sample").unwrap();
    let me = me();
    let roadmap = space
        .create_node(
            NewNode::note("Roadmap", "# Roadmap\n\n- [x] v0.1\n- [ ] v0.2\n").with_tags(["plan"]),
            &me,
        )
        .unwrap();
    let decision = space
        .create_node(
            NewNode::note(
                "Use SQLite",
                "We chose **SQLite**, see [[Roadmap#v0.2|the roadmap]] and [[Nowhere yet]].\n",
            )
            .with_kind("decision")
            .with_tags(["format", "storage/sqlite"]),
            &me,
        )
        .unwrap();
    // Written by a person, changed by an agent in a session.
    space
        .update_node(
            &roadmap.id,
            NodeUpdate {
                body: Some("# Roadmap\n\n- [x] v0.1\n- [ ] v0.2\n- [ ] v0.3\n".into()),
                ..Default::default()
            },
            &Provenance::agent("claude-code")
                .with_session("8f14e45f")
                .with_source("~/code/knode"),
        )
        .unwrap();
    let conversation = space
        .create_node_with_properties(
            NewNode::note("Session: format talk", "> user: what about \"quotes\"?\n")
                .with_kind("conversation"),
            &Properties::from([
                ("conversation.agent".into(), "claude-code".into()),
                ("conversation.session".into(), "8f14e45f".into()),
                ("note".into(), "true".into()),
                ("odd".into(), "a: b # c".into()),
            ]),
            &me,
        )
        .unwrap();
    space
        .create_node(
            NewNode::note(
                "Plan",
                "First plan, by id: [[".to_string() + &roadmap.id.to_string() + "]].",
            ),
            &me,
        )
        .unwrap();
    space
        .create_node(NewNode::note("plan", "Second plan.\r\nWith CRLF.\r\n"), &me)
        .unwrap();
    space
        .create_node(NewNode::note(".hidden / odd: title?", ""), &me)
        .unwrap();
    space
        .create_node(
            NewNode::note("Café de ideas", "Ñandú, 漢字 and ![[Roadmap]].").with_kind("idea"),
            &me,
        )
        .unwrap();
    space
        .create_node(
            NewNode::note("2026", "A title that looks like a number."),
            &me,
        )
        .unwrap();
    space
        .link(
            &decision.id,
            &conversation.id,
            link_kinds::DERIVED_FROM,
            &me,
        )
        .unwrap();
    space
        .link(&decision.id, &roadmap.id, link_kinds::PART_OF, &me)
        .unwrap();
    space
        .link(&roadmap.id, &decision.id, link_kinds::RELATED, &me)
        .unwrap();
    space
}

#[test]
fn export_import_export_is_byte_identical() {
    let tmp = tempfile::tempdir().unwrap();
    let original = sample(tmp.path());
    let first = tmp.path().join("first");
    let report = export(&original, &first).unwrap();
    assert_eq!(report.files, 8);
    assert_eq!(report.written.len(), 8);

    // Imported by someone else, into another space.
    let mut copy = Space::create(tmp.path().join("Copy.knode"), "Copy").unwrap();
    let grace = Provenance::person("Grace");
    let imported = import(&mut copy, &first, &grace).unwrap();
    assert_eq!(imported.created.len(), 8, "{imported:?}");
    assert!(imported.skipped.is_empty(), "{imported:?}");
    assert_eq!(imported.links, 3);
    assert_eq!(copy.node_count().unwrap(), 8);

    let second = tmp.path().join("second");
    export(&copy, &second).unwrap();
    assert_same(&tree(&first), &tree(&second));

    // Every node keeps its id, dates and provenance; the history says who imported it.
    for summary in original.recent(100).unwrap() {
        let before = original.get_node(&summary.id).unwrap().unwrap();
        let after = copy.get_node(&summary.id).unwrap().unwrap();
        assert_eq!(
            (
                &after.created_at,
                &after.updated_at,
                &after.created_by,
                &after.updated_by
            ),
            (
                &before.created_at,
                &before.updated_at,
                &before.created_by,
                &before.updated_by
            ),
            "{}",
            before.title
        );
        assert_eq!(
            (&after.title, &after.body, &after.kind, &after.tags),
            (&before.title, &before.body, &before.kind, &before.tags)
        );
        assert!(
            !copy.properties(&summary.id).unwrap().keys().any(|k| [
                property_keys::ID,
                property_keys::CREATED,
                property_keys::UPDATED,
                property_keys::WRITTEN
            ]
            .contains(&k.as_str())),
            "no workaround properties"
        );
        let history = copy.history(&summary.id).unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].changed_by, grace);
    }
    let roadmap = original.find_by_title("Roadmap").unwrap()[0].id;
    let node = copy.get_node(&roadmap).unwrap().unwrap();
    assert_eq!(node.created_by, me());
    assert_eq!(node.updated_by.session.as_deref(), Some("8f14e45f"));
    assert_eq!(node.updated_by.source.as_deref(), Some("~/code/knode"));

    // The copy has the same links, by its own ids.
    let decision = &copy.find_by_title("Use SQLite").unwrap()[0];
    let mut kinds: Vec<String> = copy
        .links_from(&decision.id)
        .unwrap()
        .into_iter()
        .map(|l| format!("{} {}", l.kind, l.to_title))
        .collect();
    kinds.sort();
    assert_eq!(
        kinds,
        [
            "derived-from Session: format talk",
            "mentions Roadmap",
            "part-of Roadmap"
        ]
    );
    assert_eq!(
        copy.unresolved_links(&decision.id).unwrap(),
        ["Nowhere yet"]
    );
}

#[test]
fn files_look_as_documented() {
    let tmp = tempfile::tempdir().unwrap();
    let space = sample(tmp.path());
    let dir = tmp.path().join("out");
    export(&space, &dir).unwrap();
    let files = tree(&dir);
    let names: Vec<&str> = files.keys().map(String::as_str).collect();
    assert_eq!(
        names,
        [
            "-hidden - odd- title-.md",
            ".knode/files.json",
            "2026.md",
            "Café de ideas.md",
            "Plan.md",
            "Roadmap.md",
            "Session- format talk.md",
            "Use SQLite.md",
            "plan 2.md",
        ]
    );
    let decision = space.find_by_title("Use SQLite").unwrap()[0].id;
    let roadmap = space.find_by_title("Roadmap").unwrap()[0].id;
    let session = space.find_by_title("Session: format talk").unwrap()[0].id;
    let node = space.get_node(&decision).unwrap().unwrap();
    let mut links = [roadmap.to_string(), session.to_string()];
    links.sort();
    let expected = format!(
        "---\nid: {decision}\nkind: decision\ntags: [format, storage/sqlite]\ncreated: {}\ncreated_by:\n  kind: person\n  actor: Ada\nupdated: {}\nlinks:\n  derived-from: [{session}]\n  part-of: [{roadmap}]\n---\n{}",
        node.created_at, node.updated_at, node.body
    );
    assert_eq!(String::from_utf8_lossy(&files["Use SQLite.md"]), expected);

    let text = String::from_utf8_lossy(&files["Session- format talk.md"]).into_owned();
    assert!(text.contains("title: \"Session: format talk\"\n"), "{text}");
    assert!(text.contains("properties:\n  conversation.agent: claude-code\n  conversation.session: 8f14e45f\n  note: \"true\"\n  odd: \"a: b # c\"\n"), "{text}");
    let text = String::from_utf8_lossy(&files["Roadmap.md"]).into_owned();
    assert!(
        text.contains("created_by:\n  kind: person\n  actor: Ada\nupdated: ")
            && text.contains("updated_by:\n  kind: agent\n  actor: claude-code\n  session: 8f14e45f\n  source: ~/code/knode\n"),
        "{text}"
    );
    let text = String::from_utf8_lossy(&files["2026.md"]).into_owned();
    assert!(!text.contains("title:"), "{text}");
    let text = String::from_utf8_lossy(&files["plan 2.md"]).into_owned();
    assert!(text.contains("title: plan\n"), "{text}");
}

#[test]
fn importing_an_export_again_updates_instead_of_duplicating() {
    let tmp = tempfile::tempdir().unwrap();
    let mut space = sample(tmp.path());
    let dir = tmp.path().join("out");
    export(&space, &dir).unwrap();

    // Into the space it came from: nothing changes.
    let report = import(&mut space, &dir, &me()).unwrap();
    assert_eq!(report.created.len(), 0);
    assert_eq!(
        report.updated.len(),
        8,
        "the path is recorded the first time"
    );
    assert_eq!(space.node_count().unwrap(), 8);
    let report = import(&mut space, &dir, &me()).unwrap();
    assert_eq!(
        (report.created.len(), report.updated.len(), report.unchanged),
        (0, 0, 8)
    );
    let before = tree(&dir);
    export(&space, &dir).unwrap();
    assert_same(&before, &tree(&dir));

    // An edit in the folder comes in as an edit of the same node.
    let path = dir.join("Roadmap.md");
    let text = fs::read_to_string(&path).unwrap();
    fs::write(&path, text.replace("- [ ] v0.2", "- [x] v0.2")).unwrap();
    let report = import(&mut space, &dir, &me()).unwrap();
    assert_eq!(report.updated, ["Roadmap.md"]);
    let roadmap = &space.find_by_title("Roadmap").unwrap()[0];
    assert!(
        space
            .get_node(&roadmap.id)
            .unwrap()
            .unwrap()
            .body
            .contains("- [x] v0.2")
    );

    // Into a copy, twice: the second time matches by the ids in the files.
    let mut copy = Space::create(tmp.path().join("Copy.knode"), "Copy").unwrap();
    import(&mut copy, &dir, &me()).unwrap();
    let again = import(&mut copy, &dir, &me()).unwrap();
    assert_eq!(
        (again.created.len(), again.updated.len(), again.unchanged),
        (0, 0, 8)
    );
    assert_eq!(copy.node_count().unwrap(), 8);

    // And the copy's export, imported into the original, updates the original nodes.
    let copy_dir = tmp.path().join("copy");
    export(&copy, &copy_dir).unwrap();
    let back = import(&mut space, &copy_dir, &me()).unwrap();
    assert_eq!(back.created.len(), 0);
    assert_eq!(space.node_count().unwrap(), 8);
}

#[test]
fn a_copy_edited_later_shows_its_own_update_time() {
    let tmp = tempfile::tempdir().unwrap();
    let space = sample(tmp.path());
    let first = tmp.path().join("first");
    export(&space, &first).unwrap();
    let mut copy = Space::create(tmp.path().join("Copy.knode"), "Copy").unwrap();
    import(&mut copy, &first, &me()).unwrap();

    let roadmap = copy.find_by_title("Roadmap").unwrap()[0].id;
    std::thread::sleep(std::time::Duration::from_millis(5));
    let node = copy
        .update_node(
            &roadmap,
            NodeUpdate {
                body: Some("Changed in the copy.".into()),
                ..Default::default()
            },
            &Provenance::person("Grace"),
        )
        .unwrap();
    let second = tmp.path().join("second");
    export(&copy, &second).unwrap();
    let text = fs::read_to_string(second.join("Roadmap.md")).unwrap();
    assert!(
        text.contains(&format!("updated: {}\n", node.updated_at)),
        "{text}"
    );
    assert!(
        text.contains("updated_by:\n  kind: person\n  actor: Grace\n"),
        "{text}"
    );
    // The id stays the one of the original space.
    let original = space.find_by_title("Roadmap").unwrap()[0].id;
    assert_eq!(roadmap, original);
    assert!(
        text.starts_with(&format!("---\nid: {original}\n")),
        "{text}"
    );
}

#[test]
fn copies_made_by_earlier_versions_keep_the_ids_of_their_files() {
    // Earlier versions gave imported nodes ids of their own and kept the file's id and dates
    // in properties. Those are still read.
    let tmp = tempfile::tempdir().unwrap();
    let mut copy = Space::create(tmp.path().join("Old.knode"), "Old").unwrap();
    let original = "01JB7Q9W8K3M2N4P5R6S7T8V9X";
    let node = copy
        .create_node_with_properties(
            NewNode::note("Plan", "Kept."),
            &Properties::from([
                (property_keys::ID.to_string(), original.to_string()),
                (
                    property_keys::CREATED.to_string(),
                    "2026-01-02T03:04:05.006Z".to_string(),
                ),
                (
                    property_keys::UPDATED.to_string(),
                    "2026-02-03T04:05:06.007Z".to_string(),
                ),
            ]),
            &me(),
        )
        .unwrap();
    copy.set_property(
        &node.id,
        property_keys::WRITTEN,
        &node.updated_at.to_string(),
        &me(),
    )
    .unwrap();
    let dir = tmp.path().join("out");
    export(&copy, &dir).unwrap();
    let text = fs::read_to_string(dir.join("Plan.md")).unwrap();
    assert!(
        text.starts_with(&format!("---\nid: {original}\n")),
        "{text}"
    );
    assert!(
        text.contains("created: 2026-01-02T03:04:05.006Z\n"),
        "{text}"
    );
    assert!(
        text.contains("updated: 2026-02-03T04:05:06.007Z\n"),
        "{text}"
    );

    // Importing the file again finds the node by the old id instead of making another one.
    let report = import(&mut copy, &dir, &me()).unwrap();
    assert!(report.created.is_empty(), "{report:?}");
    assert_eq!(copy.node_count().unwrap(), 1);
}

#[test]
fn export_keeps_names_and_never_touches_other_files() {
    let tmp = tempfile::tempdir().unwrap();
    let mut space = Space::create(tmp.path().join("S.knode"), "S").unwrap();
    let dir = tmp.path().join("out");
    // A file someone else wrote, where Knode would write "Plan.md".
    put(&dir, "Plan.md", "My own plan.\n");
    put(&dir, "notes.txt", "Not Markdown.\n");
    let first = space
        .create_node(NewNode::note("Plan", "One."), &me())
        .unwrap();
    let report = export(&space, &dir).unwrap();
    assert_eq!(report.written, ["Plan 2.md"]);
    assert_eq!(
        fs::read_to_string(dir.join("Plan.md")).unwrap(),
        "My own plan.\n"
    );

    // A second "Plan" gets the next name; the first keeps its file.
    space
        .create_node(NewNode::note("Plan", "Two."), &me())
        .unwrap();
    let report = export(&space, &dir).unwrap();
    assert_eq!(report.written, ["Plan 3.md"]);
    assert_eq!(report.unchanged, 1);

    // Deleting a node removes only its own file.
    space.delete_node(&first.id, &me()).unwrap();
    let report = export(&space, &dir).unwrap();
    assert_eq!(report.removed, ["Plan 2.md"]);
    assert!(dir.join("Plan.md").exists());
    assert!(dir.join("notes.txt").exists());
    assert!(dir.join("Plan 3.md").exists());
}
