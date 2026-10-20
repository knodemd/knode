//! The `loro_docs` and `loro_peers` accessors (section 21), through the public API.

use knode_format::{Error, NewNode, NodeUpdate, Provenance, Space, actions};
use tempfile::TempDir;

fn new_space() -> (TempDir, Space) {
    let dir = tempfile::tempdir().unwrap();
    let space = Space::create(dir.path().join("Test.knode"), "Test").unwrap();
    (dir, space)
}

#[test]
fn a_body_written_with_loro_keeps_its_version() {
    let (_dir, mut space) = new_space();
    let ada = Provenance::person("Ada");
    let claude = Provenance::agent("claude-code");
    let node = space
        .create_node(NewNode::note("Plan", "One."), &ada)
        .unwrap();
    assert_eq!(space.loro_version(&node.id).unwrap(), None);

    space.set_loro_version(&node.id, r#"{"1": 4}"#).unwrap();
    assert_eq!(space.history(&node.id).unwrap().len(), 1);
    assert_eq!(
        space.loro_version(&node.id).unwrap().as_deref(),
        Some(r#"{"1": 4}"#)
    );

    let written = space
        .write_loro_body(&node.id, "One. [[Two]]", r#"{"1": 4, "2": 6}"#, &claude)
        .unwrap();
    assert_eq!(written.body, "One. [[Two]]");
    assert_eq!(written.updated_by, claude);
    assert_eq!(space.unresolved_links(&node.id).unwrap(), vec!["Two"]);
    let history = space.history(&node.id).unwrap();
    assert_eq!(history.len(), 2);
    assert_eq!(
        space.changes(1, None).unwrap()[0].action,
        actions::NODE_UPDATE
    );
    assert_eq!(
        space.loro_version(&node.id).unwrap().as_deref(),
        Some(r#"{"1": 4, "2": 6}"#)
    );

    // The same body again only moves the version.
    space
        .write_loro_body(&node.id, "One. [[Two]]", r#"{"1": 5, "2": 6}"#, &claude)
        .unwrap();
    assert_eq!(space.history(&node.id).unwrap().len(), 2);

    // A body written without Loro marks the document as behind.
    space
        .update_node(
            &node.id,
            NodeUpdate {
                body: Some("Three.".into()),
                ..Default::default()
            },
            &ada,
        )
        .unwrap();
    assert_eq!(space.loro_version(&node.id).unwrap(), None);
}

#[test]
fn versions_are_checked() {
    let (_dir, mut space) = new_space();
    let ada = Provenance::person("Ada");
    let node = space.create_node(NewNode::note("A", "a"), &ada).unwrap();
    assert!(matches!(
        space.set_loro_version(&node.id, "not json"),
        Err(Error::Invalid(_))
    ));
    assert!(matches!(
        space.write_loro_body(&node.id, "b", r#"{"x": 1}"#, &ada),
        Err(Error::Invalid(_))
    ));
    let missing = knode_format::NodeId::parse("01M5B73W76PSMH5FH4R0T2KGN7").unwrap();
    assert!(matches!(
        space.set_loro_version(&missing, "{}"),
        Err(Error::NotFound(_))
    ));
}

#[test]
fn peers_belong_to_one_provenance() {
    let (_dir, mut space) = new_space();
    let ada = Provenance::person("Ada");
    let codex = Provenance::agent("codex").with_session("s1");
    assert_eq!(space.loro_peer(42).unwrap(), None);
    space.add_loro_peer(42, &codex).unwrap();
    space.add_loro_peer(42, &codex).unwrap();
    space.add_loro_peer(u64::MAX, &ada).unwrap();
    assert_eq!(space.loro_peer(42).unwrap(), Some(codex));
    assert_eq!(space.loro_peer(u64::MAX).unwrap(), Some(ada.clone()));
    assert!(matches!(
        space.add_loro_peer(42, &ada),
        Err(Error::Invalid(_))
    ));
}

#[test]
fn the_document_folder_is_inside_sync() {
    let (dir, mut space) = new_space();
    let node = space
        .create_node(NewNode::note("A", "a"), &Provenance::person("Ada"))
        .unwrap();
    assert_eq!(
        space.loro_dir(&node.id),
        dir.path()
            .join("Test.knode/sync/nodes")
            .join(node.id.to_string())
    );
}
