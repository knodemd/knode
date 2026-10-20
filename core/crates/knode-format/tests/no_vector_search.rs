//! Without the `vector-search` feature (no sqlite-vec), spaces open, keep their chunks in step
//! and search by words (spec section 19.1).
#![cfg(not(feature = "vector-search"))]

use knode_format::{EmbeddingModelSpec, Error, NewNode, Provenance, SearchMode, Space};

#[test]
fn a_space_without_sqlite_vec_searches_by_words() {
    let dir = tempfile::tempdir().unwrap();
    let mut space = Space::create(dir.path().join("Test.knode"), "Test").unwrap();
    let me = Provenance::person("Ada");
    let id = space
        .create_node(NewNode::note("Café", "Notas de cocina."), &me)
        .unwrap()
        .id;
    assert!(!space.vector_search_available());
    assert!(matches!(
        space.add_embedding_model(&EmbeddingModelSpec::new("test/model", 8), &me),
        Err(Error::VectorSearchUnavailable(_))
    ));
    assert!(space.embedding_model().unwrap().is_none());
    // Every node waits for a model that does not exist, which is harmless.
    assert_eq!(space.stale_node_count(1).unwrap(), 1);

    let results = space
        .hybrid_search("cafe", Some(&[0.0; 8]), SearchMode::Hybrid, 20)
        .unwrap();
    assert_eq!(results.mode, SearchMode::Words);
    assert_eq!(results.hits[0].node.id, id);
    assert!(matches!(
        space.hybrid_search("cafe", Some(&[0.0; 8]), SearchMode::Meaning, 20),
        Err(Error::VectorSearchUnavailable(_))
    ));
}
