//! Creates real spaces in temporary folders and works with them through the public API.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use knode_format::{
    ActorKind, ChangeOp, Error, Manifest, NewNode, NodeUpdate, Properties, Provenance,
    SCHEMA_VERSION, Space, kinds, link_kinds, property_keys,
};
use tempfile::TempDir;

fn new_space() -> (TempDir, Space) {
    let dir = tempfile::tempdir().unwrap();
    let space = Space::create(dir.path().join("Test.knode"), "Test").unwrap();
    (dir, space)
}

fn me() -> Provenance {
    Provenance::person("Ángel")
}

fn claude() -> Provenance {
    Provenance::agent("claude-code")
        .with_session("session-1")
        .with_source("~/Developer/Knode/knode")
}

fn rename(title: &str) -> NodeUpdate {
    NodeUpdate {
        title: Some(title.into()),
        ..Default::default()
    }
}

fn set_body(body: &str) -> NodeUpdate {
    NodeUpdate {
        body: Some(body.into()),
        ..Default::default()
    }
}

fn db(path: &Path) -> rusqlite::Connection {
    rusqlite::Connection::open(path.join("knode.db")).unwrap()
}

// Create and open

#[test]
fn creates_the_package() {
    let (dir, space) = new_space();
    let path = dir.path().join("Test.knode");
    assert_eq!(space.path(), path);
    assert!(path.join("manifest.json").is_file());
    assert!(path.join("knode.db").is_file());
    assert!(path.join("blobs").is_dir());
    assert!(path.join("sync").is_dir());

    let manifest = Manifest::read(&path).unwrap();
    assert_eq!(manifest.format, 0);
    assert_eq!(manifest.name, "Test");
    assert_eq!(manifest.id.len(), 26);
    assert!(manifest.created_at.ends_with('Z'));
    assert_eq!(&manifest, space.manifest());

    let conn = db(&path);
    let mode: String = conn
        .query_row("PRAGMA journal_mode", [], |r| r.get(0))
        .unwrap();
    assert_eq!(mode, "wal");
    let version: u32 = conn
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(version, SCHEMA_VERSION);
}

#[test]
fn refuses_to_create_over_something_or_without_the_extension() {
    let (dir, _space) = new_space();
    assert!(matches!(
        Space::create(dir.path().join("Test.knode"), "Again"),
        Err(Error::AlreadyExists(_))
    ));
    assert!(matches!(
        Space::create(dir.path().join("Test"), "Test"),
        Err(Error::Invalid(_))
    ));
    assert!(matches!(
        Space::create(dir.path().join("Empty.knode"), "  "),
        Err(Error::Invalid(_))
    ));
}

#[test]
fn reopens_a_space_with_its_nodes() {
    let (dir, mut space) = new_space();
    let node = space
        .create_node(NewNode::note("Kept", "Still here."), &me())
        .unwrap();
    let id = space.manifest().id.clone();
    drop(space);

    let space = Space::open(dir.path().join("Test.knode")).unwrap();
    assert_eq!(space.manifest().id, id);
    assert_eq!(space.get_node(&node.id).unwrap().unwrap(), node);
    assert_eq!(space.node_count().unwrap(), 1);
}

#[test]
fn renames_the_space_in_the_manifest() {
    let (dir, mut space) = new_space();
    space.rename("Work").unwrap();
    drop(space);
    let space = Space::open(dir.path().join("Test.knode")).unwrap();
    assert_eq!(space.manifest().name, "Work");
}

#[test]
fn refuses_what_is_not_a_space() {
    let dir = tempfile::tempdir().unwrap();
    let empty = dir.path().join("Empty.knode");
    fs::create_dir(&empty).unwrap();
    assert!(matches!(Space::open(&empty), Err(Error::NotASpace(_))));
    assert!(matches!(
        Space::open(dir.path().join("Missing.knode")),
        Err(Error::NotASpace(_))
    ));
}

#[test]
fn refuses_a_newer_format_or_schema() {
    let (dir, space) = new_space();
    let path: PathBuf = space.path().to_path_buf();
    drop(space);

    let manifest_path = path.join("manifest.json");
    let original = fs::read_to_string(&manifest_path).unwrap();
    fs::write(
        &manifest_path,
        original.replace("\"format\": 0", "\"format\": 1"),
    )
    .unwrap();
    assert!(matches!(
        Space::open(&path),
        Err(Error::UnsupportedFormat {
            found: 1,
            supported: 0
        })
    ));
    fs::write(&manifest_path, &original).unwrap();

    db(&path)
        .pragma_update(None, "user_version", SCHEMA_VERSION + 1)
        .unwrap();
    assert!(matches!(Space::open(&path), Err(Error::NewerSchema { .. })));
    drop(dir);
}

#[test]
fn refuses_a_database_from_another_space() {
    let (dir, space) = new_space();
    let path = space.path().to_path_buf();
    drop(space);
    let other = Space::create(dir.path().join("Other.knode"), "Other").unwrap();
    drop(other);
    fs::copy(
        dir.path().join("Other.knode/manifest.json"),
        path.join("manifest.json"),
    )
    .unwrap();
    assert!(matches!(
        Space::open(&path),
        Err(Error::MismatchedSpace { .. })
    ));
}

#[test]
fn keeps_unknown_manifest_fields() {
    let (_dir, mut space) = new_space();
    let path = space.path().join("manifest.json");
    let mut value: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    value["icon"] = "brain".into();
    fs::write(&path, value.to_string()).unwrap();
    let path_dir = space.path().to_path_buf();
    drop(space);
    space = Space::open(&path_dir).unwrap();
    space.rename("Renamed").unwrap();
    let written = fs::read_to_string(&path).unwrap();
    assert!(written.contains("\"icon\": \"brain\""));
}

// Nodes and provenance

#[test]
fn creates_reads_updates_and_deletes_a_node() {
    let (_dir, mut space) = new_space();
    let node = space
        .create_node(
            NewNode::note("  First   note ", "Some *Markdown*.").with_tags(["#Rust", "knode"]),
            &claude(),
        )
        .unwrap();
    assert_eq!(node.kind, kinds::NOTE);
    assert_eq!(node.title, "First note");
    assert_eq!(node.body, "Some *Markdown*.");
    assert_eq!(node.tags, ["knode", "rust"]);
    assert_eq!(node.created_at, node.updated_at);
    assert_eq!(node.created_by, claude());
    assert_eq!(node.created_by.kind, ActorKind::Agent);
    assert_eq!(node.id.created_at(), node.created_at);

    let updated = space
        .update_node(
            &node.id,
            NodeUpdate {
                kind: Some(kinds::DECISION.into()),
                body: Some("Changed.".into()),
                tags: Some(vec!["rust".into()]),
                ..Default::default()
            },
            &me(),
        )
        .unwrap();
    assert_eq!(updated.kind, "decision");
    assert_eq!(updated.body, "Changed.");
    assert_eq!(updated.tags, ["rust"]);
    assert_eq!(updated.created_by, claude());
    assert_eq!(updated.updated_by, me());
    assert!(updated.updated_at >= node.updated_at);

    space.delete_node(&node.id, &me()).unwrap();
    assert_eq!(space.get_node(&node.id).unwrap(), None);
    assert_eq!(space.node_count().unwrap(), 0);
    assert!(matches!(
        space.update_node(&node.id, set_body("x"), &me()),
        Err(Error::NotFound(_))
    ));
    assert!(matches!(
        space.delete_node(&node.id, &me()),
        Err(Error::NotFound(_))
    ));
}

#[test]
fn rejects_bad_nodes() {
    let (_dir, mut space) = new_space();
    assert!(space.create_node(NewNode::note(" ", ""), &me()).is_err());
    assert!(
        space
            .create_node(NewNode::note("A", "").with_kind("Not A Kind"), &me())
            .is_err()
    );
    assert!(
        space
            .create_node(NewNode::note("A", ""), &Provenance::agent(" "))
            .is_err()
    );
    let empty_kind = space
        .create_node(NewNode::note("A", "").with_kind(""), &me())
        .unwrap();
    assert_eq!(empty_kind.kind, "note");
}

#[test]
fn an_update_that_changes_nothing_writes_nothing() {
    let (_dir, mut space) = new_space();
    let node = space
        .create_node(NewNode::note("Same", "Body"), &me())
        .unwrap();
    let same = space
        .update_node(
            &node.id,
            NodeUpdate {
                title: Some("Same".into()),
                body: Some("Body".into()),
                ..Default::default()
            },
            &claude(),
        )
        .unwrap();
    assert_eq!(same, node);
    assert_eq!(space.history(&node.id).unwrap().len(), 1);
}

#[test]
fn keeps_every_version_in_the_history() {
    let (_dir, mut space) = new_space();
    let node = space
        .create_node(NewNode::note("V1", "one"), &me())
        .unwrap();
    space
        .update_node(&node.id, set_body("two"), &claude())
        .unwrap();
    space.update_node(&node.id, rename("V3"), &me()).unwrap();
    space.delete_node(&node.id, &claude()).unwrap();

    let history = space.history(&node.id).unwrap();
    let ops: Vec<_> = history.iter().map(|h| h.op).collect();
    assert_eq!(
        ops,
        [
            ChangeOp::Create,
            ChangeOp::Update,
            ChangeOp::Update,
            ChangeOp::Delete
        ]
    );
    assert_eq!(history[0].body, "one");
    assert_eq!(history[1].body, "two");
    assert_eq!(history[1].changed_by, claude());
    assert_eq!(history[2].title, "V3");
    assert_eq!(history[3].title, "V3");
    assert!(history.windows(2).all(|w| w[0].seq < w[1].seq));
}

#[test]
fn reuses_provenance_rows() {
    let (_dir, mut space) = new_space();
    for i in 0..3 {
        space
            .create_node(NewNode::note(format!("N{i}"), ""), &claude())
            .unwrap();
    }
    space.create_node(NewNode::note("Mine", ""), &me()).unwrap();
    let count: i64 = db(space.path())
        .query_row("SELECT count(*) FROM provenance", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 2);
}

#[test]
fn lists_recent_nodes_and_finds_titles() {
    let (_dir, mut space) = new_space();
    let a = space
        .create_node(NewNode::note("Alpha", ""), &me())
        .unwrap();
    let b = space.create_node(NewNode::note("Beta", ""), &me()).unwrap();
    let c = space
        .create_node(NewNode::note("alpha", ""), &me())
        .unwrap();
    let ids: Vec<_> = space
        .recent(10)
        .unwrap()
        .into_iter()
        .map(|n| n.id)
        .collect();
    assert_eq!(ids, [c.id, b.id, a.id]);
    assert_eq!(space.recent(1).unwrap().len(), 1);

    let found: Vec<_> = space
        .find_by_title("ALPHA ")
        .unwrap()
        .into_iter()
        .map(|n| n.id)
        .collect();
    assert_eq!(found, [a.id, c.id]);
}

// Links

#[test]
fn body_links_become_links_by_id() {
    let (_dir, mut space) = new_space();
    let roadmap = space
        .create_node(NewNode::note("Roadmap", "v0.1 this week"), &me())
        .unwrap();
    let plan = space
        .create_node(
            NewNode::note("Plan", "See [[roadmap]] and [[Roadmap|the roadmap]]."),
            &claude(),
        )
        .unwrap();

    let links = space.links_from(&plan.id).unwrap();
    assert_eq!(links.len(), 1);
    assert_eq!(links[0].to, roadmap.id);
    assert_eq!(links[0].to_title, "Roadmap");
    assert_eq!(links[0].kind, link_kinds::MENTIONS);
    assert_eq!(links[0].created_by, claude());
    let back = space.backlinks(&roadmap.id).unwrap();
    assert_eq!(back.len(), 1);
    assert_eq!(back[0].from, plan.id);
    assert_eq!(back[0].from_title, "Plan");

    space
        .update_node(&plan.id, set_body("No links now."), &me())
        .unwrap();
    assert!(space.backlinks(&roadmap.id).unwrap().is_empty());
}

#[test]
fn links_by_id_and_by_heading() {
    let (_dir, mut space) = new_space();
    let target = space.create_node(NewNode::note("Spec", ""), &me()).unwrap();
    let issue = space
        .create_node(NewNode::note("Issue #3", ""), &me())
        .unwrap();
    let source = space
        .create_node(
            NewNode::note(
                "Source",
                format!("[[{}]] and [[Spec#Schema]] and [[Issue #3]]", target.id),
            ),
            &me(),
        )
        .unwrap();
    let to: Vec<_> = space
        .links_from(&source.id)
        .unwrap()
        .into_iter()
        .map(|l| l.to)
        .collect();
    assert_eq!(to.len(), 2);
    assert!(to.contains(&target.id));
    assert!(to.contains(&issue.id));
}

#[test]
fn unresolved_links_resolve_when_the_title_appears() {
    let (_dir, mut space) = new_space();
    let source = space
        .create_node(
            NewNode::note("Source", "Later: [[Not yet]] and [[Other#Part]]"),
            &me(),
        )
        .unwrap();
    assert_eq!(
        space.unresolved_links(&source.id).unwrap(),
        ["Not yet", "Other"]
    );

    let later = space
        .create_node(NewNode::note("not  YET", ""), &me())
        .unwrap();
    assert_eq!(space.backlinks(&later.id).unwrap()[0].from, source.id);
    assert_eq!(space.unresolved_links(&source.id).unwrap(), ["Other"]);

    // A rename can resolve them too.
    let other = space
        .create_node(NewNode::note("Draft", ""), &me())
        .unwrap();
    space
        .update_node(&other.id, rename("Other"), &me())
        .unwrap();
    assert_eq!(space.backlinks(&other.id).unwrap()[0].from, source.id);
    assert!(space.unresolved_links(&source.id).unwrap().is_empty());
}

#[test]
fn renaming_rewrites_the_links_that_point_here() {
    let (_dir, mut space) = new_space();
    let target = space.create_node(NewNode::note("Old", ""), &me()).unwrap();
    let source = space
        .create_node(
            NewNode::note("Source", "See [[Old]], [[old|this]] and `[[Old]]`."),
            &me(),
        )
        .unwrap();
    space
        .update_node(&target.id, rename("New name"), &claude())
        .unwrap();

    let source_now = space.get_node(&source.id).unwrap().unwrap();
    assert_eq!(
        source_now.body,
        "See [[New name]], [[New name|this]] and `[[Old]]`."
    );
    assert_eq!(source_now.updated_by, claude());
    assert_eq!(space.backlinks(&target.id).unwrap()[0].from, source.id);
    let history = space.history(&source.id).unwrap();
    assert_eq!(history.last().unwrap().op, ChangeOp::Update);
    assert!(space.search("name", 10).unwrap().len() == 2);
}

#[test]
fn deleting_a_node_leaves_its_mentions_waiting() {
    let (_dir, mut space) = new_space();
    let target = space.create_node(NewNode::note("Gone", ""), &me()).unwrap();
    let source = space
        .create_node(NewNode::note("Source", "[[Gone]]"), &me())
        .unwrap();
    space.delete_node(&target.id, &me()).unwrap();
    assert!(space.links_from(&source.id).unwrap().is_empty());
    assert_eq!(space.unresolved_links(&source.id).unwrap(), ["Gone"]);

    let back = space.create_node(NewNode::note("Gone", ""), &me()).unwrap();
    assert_eq!(space.backlinks(&back.id).unwrap()[0].from, source.id);
}

#[test]
fn typed_links_by_hand() {
    let (_dir, mut space) = new_space();
    let a = space.create_node(NewNode::note("A", ""), &me()).unwrap();
    let b = space.create_node(NewNode::note("B", ""), &me()).unwrap();

    let link = space
        .link(&a.id, &b.id, link_kinds::PART_OF, &claude())
        .unwrap();
    assert_eq!(link.kind, "part-of");
    assert_eq!(link.created_by, claude());
    // Linking again returns the same link.
    assert_eq!(space.link(&a.id, &b.id, "part-of", &me()).unwrap(), link);
    space.link(&a.id, &b.id, "", &me()).unwrap();
    assert_eq!(space.links_from(&a.id).unwrap().len(), 2);
    assert_eq!(space.backlinks(&b.id).unwrap().len(), 2);

    // A body change keeps links made by hand.
    space.update_node(&a.id, set_body("text"), &me()).unwrap();
    assert_eq!(space.links_from(&a.id).unwrap().len(), 2);

    assert!(space.unlink(&a.id, &b.id, "part-of").unwrap());
    assert!(!space.unlink(&a.id, &b.id, "part-of").unwrap());
    assert_eq!(space.links_from(&a.id).unwrap()[0].kind, "related");

    assert!(space.link(&a.id, &b.id, "mentions", &me()).is_err());
    assert!(space.link(&a.id, &b.id, "Bad Kind", &me()).is_err());
    space.delete_node(&b.id, &me()).unwrap();
    assert!(space.links_from(&a.id).unwrap().is_empty());
    assert!(matches!(
        space.link(&a.id, &b.id, "related", &me()),
        Err(Error::NotFound(_))
    ));
}

// Properties

fn conversation_properties(session: &str) -> Properties {
    Properties::from([
        (
            property_keys::CONVERSATION_AGENT.into(),
            "claude-code".into(),
        ),
        (property_keys::CONVERSATION_SESSION.into(), session.into()),
        (
            property_keys::CONVERSATION_PROJECT.into(),
            "/Users/example/project".into(),
        ),
    ])
}

#[test]
fn creates_a_node_with_properties() {
    let (_dir, mut space) = new_space();
    let node = space
        .create_node_with_properties(
            NewNode::note("Chat", "Hello").with_kind(kinds::CONVERSATION),
            &conversation_properties("s-1"),
            &claude(),
        )
        .unwrap();
    assert_eq!(
        space.properties(&node.id).unwrap(),
        conversation_properties("s-1")
    );
    assert_eq!(
        space
            .property(&node.id, property_keys::CONVERSATION_SESSION)
            .unwrap()
            .as_deref(),
        Some("s-1")
    );
    assert_eq!(space.property(&node.id, "missing").unwrap(), None);
    // Properties are metadata: the body has no frontmatter and the history has one version.
    assert_eq!(space.get_node(&node.id).unwrap().unwrap().body, "Hello");
    assert_eq!(space.history(&node.id).unwrap().len(), 1);
}

#[test]
fn sets_replaces_and_removes_properties() {
    let (_dir, mut space) = new_space();
    let node = space.create_node(NewNode::note("A", ""), &me()).unwrap();
    let before = space.get_node(&node.id).unwrap().unwrap();

    assert!(
        space
            .set_property(&node.id, "status", "draft", &me())
            .unwrap()
    );
    assert!(
        !space
            .set_property(&node.id, "status", "draft", &me())
            .unwrap()
    );
    assert!(
        space
            .set_property(&node.id, "status", "done", &claude())
            .unwrap()
    );
    assert!(
        space
            .set_properties(&node.id, &conversation_properties("s-2"), &claude())
            .unwrap()
    );
    let properties = space.properties(&node.id).unwrap();
    assert_eq!(properties.len(), 4);
    assert_eq!(properties["status"], "done");

    assert!(space.remove_property(&node.id, "status").unwrap());
    assert!(!space.remove_property(&node.id, "status").unwrap());
    assert_eq!(
        space.properties(&node.id).unwrap(),
        conversation_properties("s-2")
    );

    // Setting properties leaves the node and its history as they were.
    let after = space.get_node(&node.id).unwrap().unwrap();
    assert_eq!(after.updated_at, before.updated_at);
    assert_eq!(space.history(&node.id).unwrap().len(), 1);

    // Who set each property is kept.
    let actor: String = db(space.path())
        .query_row(
            "SELECT p.actor FROM properties x JOIN provenance p ON p.id = x.updated_by
             WHERE x.key = ?1",
            [property_keys::CONVERSATION_AGENT],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(actor, "claude-code");
}

#[test]
fn rejects_bad_property_keys_and_missing_nodes() {
    let (_dir, mut space) = new_space();
    let node = space.create_node(NewNode::note("A", ""), &me()).unwrap();
    for key in ["", "Agent", "1a", "a b", "a/b"] {
        assert!(matches!(
            space.set_property(&node.id, key, "x", &me()),
            Err(Error::Invalid(_))
        ));
    }
    let bad = Properties::from([("Bad".to_string(), "x".to_string())]);
    assert!(
        space
            .create_node_with_properties(NewNode::note("B", ""), &bad, &me())
            .is_err()
    );
    assert!(space.find_by_title("B").unwrap().is_empty());

    space.delete_node(&node.id, &me()).unwrap();
    assert!(matches!(
        space.set_property(&node.id, "status", "x", &me()),
        Err(Error::NotFound(_))
    ));
}

#[test]
fn finds_nodes_by_property() {
    let (_dir, mut space) = new_space();
    let first = space
        .create_node_with_properties(
            NewNode::note("One", ""),
            &conversation_properties("s-1"),
            &claude(),
        )
        .unwrap();
    let second = space
        .create_node_with_properties(
            NewNode::note("Two", ""),
            &conversation_properties("s-2"),
            &claude(),
        )
        .unwrap();
    let found = space
        .find_by_property(property_keys::CONVERSATION_SESSION, "s-2")
        .unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].id, second.id);
    let both = space
        .find_by_property(property_keys::CONVERSATION_AGENT, "claude-code")
        .unwrap();
    assert_eq!(
        both.iter().map(|n| n.id).collect::<Vec<_>>(),
        [first.id, second.id]
    );
    assert!(
        space
            .find_by_property(property_keys::CONVERSATION_SESSION, "s-3")
            .unwrap()
            .is_empty()
    );

    // The lookup uses the index.
    let plan: String = db(space.path())
        .query_row(
            "EXPLAIN QUERY PLAN SELECT node_id FROM properties WHERE key = 'a' AND value = 'b'",
            [],
            |row| row.get(3),
        )
        .unwrap();
    assert!(plan.contains("properties_key_value"), "{plan}");

    // Deleting a node deletes its properties.
    space.delete_node(&first.id, &me()).unwrap();
    assert!(space.properties(&first.id).unwrap().is_empty());
    assert_eq!(
        space
            .find_by_property(property_keys::CONVERSATION_AGENT, "claude-code")
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn opens_a_version_1_space_and_adds_properties() {
    let (dir, space) = new_space();
    let path = space.path().to_path_buf();
    drop(space);
    {
        let conn = db(&path);
        conn.execute_batch("DROP TABLE properties").unwrap();
        conn.pragma_update(None, "user_version", 1).unwrap();
    }
    let mut space = Space::open(&path).unwrap();
    let version: u32 = db(&path)
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, SCHEMA_VERSION);
    let node = space.create_node(NewNode::note("A", ""), &me()).unwrap();
    assert!(space.set_property(&node.id, "status", "x", &me()).unwrap());
    drop(dir);
}

// Search

#[test]
fn searches_with_ranking_and_snippets() {
    let (_dir, mut space) = new_space();
    let in_body = space
        .create_node(
            NewNode::note(
                "Meeting notes",
                "We talked about **SQLite** and why WAL mode matters for agents.",
            ),
            &me(),
        )
        .unwrap();
    let in_title = space
        .create_node(
            NewNode::note("SQLite", "The database inside every space."),
            &me(),
        )
        .unwrap();
    space
        .create_node(NewNode::note("Unrelated", "Nothing to see."), &me())
        .unwrap();

    let hits = space.search("sqlite", 10).unwrap();
    assert_eq!(hits.len(), 2);
    assert_eq!(hits[0].node.id, in_title.id);
    assert_eq!(hits[1].node.id, in_body.id);
    assert!(hits[0].score > hits[1].score);
    assert!(
        hits[1].snippet.contains("**SQLite**"),
        "{}",
        hits[1].snippet
    );
    // Markdown is not indexed: `**` in the body is not in the snippet twice.
    assert!(!hits[1].snippet.contains("****"));
}

#[test]
fn search_ignores_case_and_accents_and_matches_prefixes() {
    let (_dir, mut space) = new_space();
    let node = space
        .create_node(
            NewNode::note("Café de ideas", "Pensamientos sobre memoria."),
            &me(),
        )
        .unwrap();
    for query in ["cafe", "CAFÉ", "ide", "cafe ideas", "memo"] {
        let hits = space.search(query, 10).unwrap();
        assert_eq!(hits.len(), 1, "{query}");
        assert_eq!(hits[0].node.id, node.id);
    }
    assert!(space.search("cafe xyz", 10).unwrap().is_empty());
    assert!(space.search("", 10).unwrap().is_empty());
    assert!(space.search("\"( * -", 10).unwrap().is_empty());
    // Syntax characters are words, not operators.
    assert!(space.search("AND OR NOT", 10).unwrap().is_empty());
}

#[test]
fn search_follows_updates_and_deletes() {
    let (_dir, mut space) = new_space();
    let node = space
        .create_node(NewNode::note("Draft", "apples"), &me())
        .unwrap();
    space
        .update_node(&node.id, set_body("oranges"), &me())
        .unwrap();
    assert!(space.search("apples", 10).unwrap().is_empty());
    assert_eq!(space.search("oranges", 10).unwrap().len(), 1);
    space.update_node(&node.id, rename("Fruit"), &me()).unwrap();
    assert_eq!(space.search("fruit", 10).unwrap().len(), 1);
    assert!(space.search("draft", 10).unwrap().is_empty());
    space.delete_node(&node.id, &me()).unwrap();
    assert!(space.search("oranges", 10).unwrap().is_empty());
}

#[test]
fn search_respects_the_limit() {
    let (_dir, mut space) = new_space();
    for i in 0..20 {
        space
            .create_node(NewNode::note(format!("Note {i}"), "shared word"), &me())
            .unwrap();
    }
    assert_eq!(space.search("shared", 5).unwrap().len(), 5);
    assert_eq!(space.search("shared", 100).unwrap().len(), 20);
}

/// Searches 10,000 nodes. Run with `cargo test --release -- --ignored` to check the target of
/// under 10 ms per search.
#[test]
#[ignore]
fn searches_ten_thousand_nodes_quickly() {
    let (_dir, mut space) = new_space();
    let words = [
        "memory", "agent", "rust", "sqlite", "search", "graph", "link", "note", "idea", "space",
        "swift", "sync", "loro", "format", "markdown", "service",
    ];
    for i in 0..10_000usize {
        let body = (0..60)
            .map(|j| words[(i * 7 + j * 13) % words.len()])
            .collect::<Vec<_>>()
            .join(" ");
        space
            .create_node(NewNode::note(format!("Node {i}"), body), &me())
            .unwrap();
    }
    for query in ["memory agent", "loro", "mark", "node 9999"] {
        space.search(query, 20).unwrap();
        let start = Instant::now();
        let runs = 20;
        for _ in 0..runs {
            assert!(!space.search(query, 20).unwrap().is_empty());
        }
        let each = start.elapsed() / runs;
        println!("{query:?}: {each:?} per search");
        assert!(each.as_millis() < 10, "{query:?} took {each:?}");
    }
}
