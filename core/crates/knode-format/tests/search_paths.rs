//! Imported filenames remain searchable as notes, paths and spaces change.
use knode_format::{
    NewNode, NodeUpdate, Properties, Provenance, SCHEMA_VERSION, Space, property_keys,
};

fn ada() -> Provenance {
    Provenance::person("Ada")
}

fn assert_index(space: &Space) {
    let conn = rusqlite::Connection::open(space.path().join("knode.db")).unwrap();
    // rank=1 checks the index against its external content, not just its internal structure.
    conn.execute(
        "INSERT INTO nodes_fts (nodes_fts, rank) VALUES ('integrity-check', 1)",
        [],
    )
    .unwrap();
}

#[test]
fn searches_paths_with_snippets_and_keeps_them_in_step() {
    let dir = tempfile::tempdir().unwrap();
    let mut space = Space::create(dir.path().join("Paths.knode"), "Paths").unwrap();
    let node = space
        .create_node_with_properties(
            NewNode::note("Release instructions", "Publish the application."),
            &Properties::from([(
                property_keys::MARKDOWN_PATH.into(),
                "Projects/Café/DEPLOY.md".into(),
            )]),
            &ada(),
        )
        .unwrap();
    for query in ["DEPLOY.md", "deploy", "projects cafe", "deploy application"] {
        let hits = space.search(query, 10).unwrap();
        assert_eq!(hits.len(), 1, "{query}");
        assert_eq!(hits[0].node.id, node.id);
    }
    assert!(
        space.search("deploy", 1).unwrap()[0]
            .snippet
            .contains("**DEPLOY**")
    );
    assert_index(&space);

    space
        .update_node(
            &node.id,
            NodeUpdate {
                title: Some("Shipping guide".into()),
                body: Some("Distribute the package.".into()),
                ..Default::default()
            },
            &ada(),
        )
        .unwrap();
    assert_eq!(space.search("deploy distribute", 10).unwrap().len(), 1);
    assert!(space.search("application", 10).unwrap().is_empty());
    assert_index(&space);

    // The public API uses an upsert, which must not delete an FTS entry twice.
    space
        .set_property(
            &node.id,
            property_keys::MARKDOWN_PATH,
            "Ops/RELEASE.md",
            &ada(),
        )
        .unwrap();
    assert!(space.search("deploy", 10).unwrap().is_empty());
    assert_eq!(space.search("release.md", 10).unwrap()[0].node.id, node.id);
    space
        .set_property(&node.id, "private.reference", "secretfilename", &ada())
        .unwrap();
    assert!(space.search("secretfilename", 10).unwrap().is_empty());
    assert_index(&space);

    space
        .remove_property(&node.id, property_keys::MARKDOWN_PATH)
        .unwrap();
    assert!(space.search("release.md", 10).unwrap().is_empty());
    assert_eq!(space.search("shipping", 10).unwrap().len(), 1);
    assert_index(&space);
    space
        .set_property(
            &node.id,
            property_keys::MARKDOWN_PATH,
            "Ops/FINAL.md",
            &ada(),
        )
        .unwrap();
    space.delete_node(&node.id, &ada()).unwrap();
    assert!(space.search("final.md", 10).unwrap().is_empty());
    assert_index(&space);
}

#[test]
fn reindexes_changes_to_a_property_key_or_owner() {
    let dir = tempfile::tempdir().unwrap();
    let mut space = Space::create(dir.path().join("Paths.knode"), "Paths").unwrap();
    let a = space
        .create_node(NewNode::note("First", "One"), &ada())
        .unwrap();
    let b = space
        .create_node(NewNode::note("Second", "Two"), &ada())
        .unwrap();
    space
        .set_property(&a.id, "import.reference", "Unique.md", &ada())
        .unwrap();
    let conn = rusqlite::Connection::open(space.path().join("knode.db")).unwrap();
    conn.execute(
        "UPDATE properties SET key = 'markdown.path' WHERE node_id = ?1",
        [a.id.to_string()],
    )
    .unwrap();
    assert_eq!(space.search("unique", 10).unwrap()[0].node.id, a.id);
    assert_index(&space);
    conn.execute(
        "UPDATE properties SET node_id = ?1 WHERE node_id = ?2",
        [b.id.to_string(), a.id.to_string()],
    )
    .unwrap();
    assert_eq!(space.search("unique", 10).unwrap()[0].node.id, b.id);
    assert_index(&space);
    conn.execute(
        "UPDATE properties SET key = 'import.reference' WHERE node_id = ?1",
        [b.id.to_string()],
    )
    .unwrap();
    assert!(space.search("unique", 10).unwrap().is_empty());
    assert_index(&space);
    conn.execute("INSERT INTO nodes_fts (nodes_fts) VALUES ('rebuild')", [])
        .unwrap();
    assert_index(&space);
    assert_eq!(space.search("second", 10).unwrap().len(), 1);
}

#[test]
fn migrates_existing_paths_without_changing_notes_or_history() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("Old.knode");
    let space = Space::create(&path, "Old").unwrap();
    let space_id = space.manifest().id.clone();
    drop(space);
    std::fs::remove_file(path.join("knode.db")).unwrap();
    {
        let conn = rusqlite::Connection::open(path.join("knode.db")).unwrap();
        for sql in [
            include_str!("../src/sql/0001-initial.sql"),
            include_str!("../src/sql/0002-properties.sql"),
            include_str!("../src/sql/0003-v02.sql"),
            include_str!("../src/sql/0004-v03.sql"),
        ] {
            conn.execute_batch(sql).unwrap();
        }
        conn.execute(
            "INSERT INTO settings (key,value) VALUES ('space.id',?1)",
            [space_id],
        )
        .unwrap();
        conn.execute_batch("INSERT INTO provenance (id,actor_kind,actor) VALUES (1,'person','Ada');
            INSERT INTO nodes (id,title,title_key,body,plain,created_at,updated_at,created_by,updated_by)
            VALUES ('01JABCDEFGHJKMNPQRSTVWXYZ0','Guide','guide','Publish it','Publish it',1,1,1,1);
            INSERT INTO properties (node_id,key,value,updated_at,updated_by)
            VALUES ('01JABCDEFGHJKMNPQRSTVWXYZ0','markdown.path','Projects/DEPLOY.md',1,1);
            PRAGMA user_version = 4;").unwrap();
    }
    let space = Space::open(&path).unwrap();
    assert_eq!(space.search("deploy.md", 10).unwrap().len(), 1);
    assert_eq!(space.node_count().unwrap(), 1);
    assert!(space.changes(10, None).unwrap().is_empty());
    assert_index(&space);
    let conn = rusqlite::Connection::open(path.join("knode.db")).unwrap();
    let version: u32 = conn
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(version, SCHEMA_VERSION);
    drop(space);
    assert_eq!(
        Space::open(&path)
            .unwrap()
            .search("deploy", 10)
            .unwrap()
            .len(),
        1
    );
}
