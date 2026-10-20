//! Embeddings and search by meaning (schema version 4, spec section 19), through the public
//! API, with vectors made up by the tests: no model is needed.
#![cfg(feature = "vector-search")]

use knode_format::{
    ChunkVector, EmbeddingModelSpec, Error, NewNode, NodeId, NodeUpdate, Provenance, SearchMode,
    Space, StaleNode, actions, embedding_text, setting_keys,
};
use tempfile::TempDir;

fn new_space() -> (TempDir, Space) {
    let dir = tempfile::tempdir().unwrap();
    let space = Space::create(dir.path().join("Test.knode"), "Test").unwrap();
    (dir, space)
}

fn ada() -> Provenance {
    Provenance::person("Ada")
}

fn indexer() -> Provenance {
    Provenance::system()
}

fn spec(dimensions: u32) -> EmbeddingModelSpec {
    EmbeddingModelSpec {
        name: "test/bag-of-words".into(),
        revision: "1".into(),
        dimensions,
        query_prefix: "query: ".into(),
        document_prefix: "passage: ".into(),
        ..EmbeddingModelSpec::new("", dimensions)
    }
}

fn note(space: &mut Space, title: &str, body: &str) -> NodeId {
    space
        .create_node(NewNode::note(title, body), &ada())
        .unwrap()
        .id
}

/// A made-up embedding: words, lowercased and without accents, hashed into buckets, normalized.
/// Texts that share words are close, in any language.
fn embed(text: &str, dimensions: usize) -> Vec<f32> {
    let mut v = vec![0f32; dimensions];
    for word in text.split(|c: char| !c.is_alphanumeric()) {
        if word.is_empty() {
            continue;
        }
        let word: String = word
            .to_lowercase()
            .chars()
            .map(|c| match c {
                'á' => 'a',
                'é' => 'e',
                'í' => 'i',
                'ó' => 'o',
                'ú' => 'u',
                'ñ' => 'n',
                c => c,
            })
            .collect();
        let mut hash: u64 = 0xcbf29ce484222325;
        for byte in word.bytes() {
            hash = (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3);
        }
        v[(hash % dimensions as u64) as usize] += 1.0;
    }
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-6);
    v.iter().map(|x| x / norm).collect()
}

/// Embeds and stores every node that waits, as an indexer would.
fn index_all(space: &mut Space, model: i64, dimensions: usize) -> usize {
    let mut stored = 0;
    loop {
        let stale = space.stale_nodes(model, 3).unwrap();
        if stale.is_empty() {
            return stored;
        }
        for node in stale {
            let chunks = embed_node(&node, dimensions);
            assert!(space.store_chunks(model, &node, &chunks).unwrap());
            stored += 1;
        }
    }
}

fn embed_node(node: &StaleNode, dimensions: usize) -> Vec<ChunkVector> {
    node.chunks()
        .into_iter()
        .map(|chunk| {
            let vector = embed(&embedding_text(&node.title, &chunk), dimensions);
            ChunkVector { chunk, vector }
        })
        .collect()
}

fn count(space: &Space, sql: &str) -> i64 {
    // Read through a second connection without sqlite-vec, like any SQLite reader: vectors are
    // counted in the `_rowids` table sqlite-vec keeps for each vector table.
    let db = rusqlite_path(space);
    let conn = rusqlite::Connection::open(db).unwrap();
    conn.query_row(sql, [], |row| row.get(0)).unwrap()
}

fn rusqlite_path(space: &Space) -> std::path::PathBuf {
    space.path().join(knode_format::DATABASE_FILE)
}

#[test]
fn sqlite_vec_is_loaded_in_new_and_opened_spaces() {
    let (dir, space) = new_space();
    assert!(space.vector_search_available());
    drop(space);
    let space = Space::open(dir.path().join("Test.knode")).unwrap();
    assert!(space.vector_search_available());
    assert!(space.embedding_model().unwrap().is_none());
    assert!(space.embedding_models().unwrap().is_empty());
}

#[test]
fn adding_a_model_creates_its_table_and_puts_it_in_use() {
    let (_dir, mut space) = new_space();
    let model = space.add_embedding_model(&spec(16), &indexer()).unwrap();
    assert_eq!(model.spec, spec(16));
    assert_eq!(
        model.vector_table(),
        format!("embedding_vectors_{}", model.id)
    );
    assert_eq!(space.embedding_model().unwrap(), Some(model.clone()));
    assert_eq!(
        space.setting(setting_keys::EMBEDDING_MODEL).unwrap(),
        Some(model.id.to_string())
    );
    let change = space.changes(1, None).unwrap().remove(0);
    assert_eq!(change.action, actions::SETTINGS_SET);
    assert_eq!(change.subject, setting_keys::EMBEDDING_MODEL);
    assert_eq!(change.changed_by, indexer());
    assert_eq!(
        count(
            &space,
            &format!(
                "SELECT count(*) FROM sqlite_schema WHERE name = '{}'",
                model.vector_table()
            )
        ),
        1
    );

    // The same spec is the same model; nothing new is recorded.
    let changes = space.changes(100, None).unwrap().len();
    let again = space.add_embedding_model(&spec(16), &indexer()).unwrap();
    assert_eq!(again, model);
    assert_eq!(space.changes(100, None).unwrap().len(), changes);

    // Another spec is another model, not in use until switched to.
    let other = space
        .add_embedding_model(
            &EmbeddingModelSpec {
                revision: "2".into(),
                ..spec(16)
            },
            &indexer(),
        )
        .unwrap();
    assert_ne!(other.id, model.id);
    assert_eq!(space.embedding_model().unwrap().unwrap().id, model.id);
    assert_eq!(space.embedding_models().unwrap().len(), 2);
}

#[test]
fn bad_model_specs_are_refused() {
    let (_dir, mut space) = new_space();
    for bad in [
        EmbeddingModelSpec {
            chunking: "other-1".into(),
            ..spec(16)
        },
        spec(0),
        spec(8193),
        EmbeddingModelSpec::new("", 16),
    ] {
        assert!(matches!(
            space.add_embedding_model(&bad, &indexer()),
            Err(Error::Invalid(_))
        ));
    }
    assert!(space.embedding_models().unwrap().is_empty());
}

#[test]
fn chunks_are_stored_and_nodes_stop_waiting() {
    let (_dir, mut space) = new_space();
    let a = note(&mut space, "Cats", "# Habits\n\nThe cat sleeps all day.");
    let b = note(&mut space, "Empty", "");
    let model = space.add_embedding_model(&spec(16), &indexer()).unwrap();
    assert_eq!(space.stale_node_count(model.id).unwrap(), 2);
    let stale = space.stale_nodes(model.id, 10).unwrap();
    assert_eq!(stale.len(), 2);
    // Most recently changed first.
    assert_eq!(stale[0].id, b);
    assert_eq!(stale[1].id, a);
    assert_eq!(stale[1].plain, "Habits\nThe cat sleeps all day.");

    assert_eq!(index_all(&mut space, model.id, 16), 2);
    assert_eq!(space.stale_node_count(model.id).unwrap(), 0);
    assert!(space.stale_nodes(model.id, 10).unwrap().is_empty());
    let rows: Vec<(i64, i64, i64, String)> = {
        let conn = rusqlite::Connection::open(rusqlite_path(&space)).unwrap();
        conn.prepare(
            "SELECT ord, text_start, text_end, heading FROM embedding_chunks
             WHERE node_id = ?1 ORDER BY ord",
        )
        .unwrap()
        .query_map([a.to_string()], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
    };
    assert_eq!(rows, [(0, 0, 30, "Habits".to_string())]);
    // The empty node has one empty chunk, so its title is embedded.
    assert_eq!(
        count(
            &space,
            &format!("SELECT text_end FROM embedding_chunks WHERE node_id = '{b}'")
        ),
        0
    );
    assert_eq!(
        count(
            &space,
            &format!("SELECT count(*) FROM {}_rowids", model.vector_table())
        ),
        2
    );
}

#[test]
fn edits_make_a_node_wait_again_and_leave_orphans_to_clean() {
    let (_dir, mut space) = new_space();
    let a = note(&mut space, "Plan", "Ship the format.");
    let b = note(&mut space, "Other", "Unrelated.");
    let model = space.add_embedding_model(&spec(16), &indexer()).unwrap();
    index_all(&mut space, model.id, 16);

    // Changing the kind or tags keeps the chunks.
    space
        .update_node(
            &a,
            NodeUpdate {
                tags: Some(vec!["work".into()]),
                ..Default::default()
            },
            &ada(),
        )
        .unwrap();
    assert_eq!(space.stale_node_count(model.id).unwrap(), 0);

    // A new body drops them (trigger embedding_chunks_stale); the vector is an orphan.
    space
        .update_node(
            &a,
            NodeUpdate {
                body: Some("Ship the format and the app.".into()),
                ..Default::default()
            },
            &ada(),
        )
        .unwrap();
    let stale = space.stale_nodes(model.id, 10).unwrap();
    assert_eq!(stale.len(), 1);
    assert_eq!(stale[0].id, a);
    assert_eq!(index_all(&mut space, model.id, 16), 1);
    let table = model.vector_table();
    assert_eq!(
        count(&space, &format!("SELECT count(*) FROM {table}_rowids")),
        3
    );
    assert_eq!(space.delete_orphan_vectors(model.id).unwrap(), 1);
    assert_eq!(
        count(&space, &format!("SELECT count(*) FROM {table}_rowids")),
        2
    );

    // Deleting a node deletes its chunks.
    space.delete_node(&b, &ada()).unwrap();
    assert_eq!(count(&space, "SELECT count(*) FROM embedding_chunks"), 1);
    assert_eq!(space.delete_orphan_vectors(model.id).unwrap(), 1);
}

#[test]
fn a_node_changed_while_it_was_embedded_is_not_stored() {
    let (_dir, mut space) = new_space();
    let a = note(&mut space, "Plan", "First text.");
    let model = space.add_embedding_model(&spec(16), &indexer()).unwrap();
    let read = space.stale_nodes(model.id, 1).unwrap().remove(0);
    let chunks = embed_node(&read, 16);
    space
        .update_node(
            &a,
            NodeUpdate {
                body: Some("Second text.".into()),
                ..Default::default()
            },
            &ada(),
        )
        .unwrap();
    assert!(!space.store_chunks(model.id, &read, &chunks).unwrap());
    assert_eq!(space.stale_node_count(model.id).unwrap(), 1);
    // A deleted node is not stored either.
    let read = space.stale_nodes(model.id, 1).unwrap().remove(0);
    let chunks = embed_node(&read, 16);
    space.delete_node(&a, &ada()).unwrap();
    assert!(!space.store_chunks(model.id, &read, &chunks).unwrap());
    assert_eq!(count(&space, "SELECT count(*) FROM embedding_chunks"), 0);
}

#[test]
fn chunks_that_do_not_fit_the_model_or_the_text_are_refused() {
    let (_dir, mut space) = new_space();
    note(&mut space, "Plan", "Text.");
    let model = space.add_embedding_model(&spec(16), &indexer()).unwrap();
    let node = space.stale_nodes(model.id, 1).unwrap().remove(0);
    let good = embed_node(&node, 16);

    let mut short = good.clone();
    short[0].vector.pop();
    let mut outside = good.clone();
    outside[0].chunk.end = 99;
    let mut misnumbered = good.clone();
    misnumbered[0].chunk.ord = 1;
    let mut nan = good.clone();
    nan[0].vector[0] = f32::NAN;
    for bad in [short, outside, misnumbered, nan, Vec::new()] {
        assert!(matches!(
            space.store_chunks(model.id, &node, &bad),
            Err(Error::Invalid(_))
        ));
    }
    assert!(matches!(
        space.store_chunks(model.id + 1, &node, &good),
        Err(Error::Invalid(_))
    ));
    assert!(space.store_chunks(model.id, &node, &good).unwrap());
}

#[test]
fn switching_models_drops_the_old_one_whole() {
    let (_dir, mut space) = new_space();
    for i in 0..5 {
        note(&mut space, &format!("Note {i}"), "Some text to embed.");
    }
    let old = space.add_embedding_model(&spec(16), &indexer()).unwrap();
    index_all(&mut space, old.id, 16);

    let new = space.add_embedding_model(&spec(8), &indexer()).unwrap();
    // Until the new model has every node, searches keep using the old one.
    assert_eq!(space.embedding_model().unwrap().unwrap().id, old.id);
    assert_eq!(space.stale_node_count(new.id).unwrap(), 5);
    assert_eq!(space.stale_node_count(old.id).unwrap(), 0);
    index_all(&mut space, new.id, 8);
    space.use_embedding_model(new.id, &indexer()).unwrap();

    assert_eq!(space.embedding_model().unwrap(), Some(new.clone()));
    assert_eq!(space.embedding_models().unwrap(), [new.clone()]);
    assert_eq!(
        count(
            &space,
            &format!(
                "SELECT count(*) FROM embedding_chunks WHERE model_id = {}",
                old.id
            )
        ),
        0
    );
    assert_eq!(
        count(
            &space,
            "SELECT count(*) FROM sqlite_schema WHERE name LIKE 'embedding_vectors_%'
             AND type = 'table' AND name NOT LIKE 'embedding_vectors_%\\_%' ESCAPE '\\'"
        ),
        1
    );
    assert_eq!(
        count(
            &space,
            &format!(
                "SELECT count(*) FROM sqlite_schema WHERE name = '{}'",
                old.vector_table()
            )
        ),
        0
    );
    // A search with the new model works; the old one is gone.
    let hits = space
        .vector_search(new.id, &embed("some text", 8), 10)
        .unwrap();
    assert_eq!(hits.len(), 5);
    assert!(matches!(
        space.vector_search(old.id, &embed("some text", 16), 10),
        Err(Error::Invalid(_))
    ));
    assert!(matches!(
        space.use_embedding_model(old.id, &indexer()),
        Err(Error::Invalid(_))
    ));
}

/// A small corpus in English and Spanish.
fn corpus(space: &mut Space) -> Vec<NodeId> {
    [
        (
            "Rice",
            "# Cooking\n\nRinse the rice and simmer it with twice the water.",
        ),
        (
            "Arroz",
            "# Cocina\n\nLava el arroz y cuécelo con el doble de agua.",
        ),
        (
            "Revenue",
            "The quarterly report shows revenue grew while costs stayed flat.",
        ),
        (
            "Vectors",
            "We store embeddings in sqlite-vec next to the full-text index.",
        ),
        ("Café", "Notas sobre el café de especialidad y su tueste."),
    ]
    .into_iter()
    .map(|(title, body)| note(space, title, body))
    .collect()
}

#[test]
fn search_by_meaning_ranks_nodes_by_their_nearest_chunk() {
    let (_dir, mut space) = new_space();
    let ids = corpus(&mut space);
    let model = space.add_embedding_model(&spec(64), &indexer()).unwrap();
    index_all(&mut space, model.id, 64);

    let hits = space
        .vector_search(model.id, &embed("rice water simmer", 64), 10)
        .unwrap();
    assert_eq!(hits.len(), 5);
    assert_eq!(hits[0].node.id, ids[0]);
    assert!(hits.windows(2).all(|w| w[0].distance <= w[1].distance));
    assert!(hits[0].distance < 1.0);
    assert_eq!(hits[0].chunk.heading, "Cooking");
    assert_eq!(
        hits[0].chunk.text,
        "Cooking\nRinse the rice and simmer it with twice the water."
    );
    assert_eq!(
        hits[0].snippet,
        "Cooking Rinse the rice and simmer it with twice the water."
    );

    // Spanish text and accents: "cafe" finds "Café", "cocina arroz" finds "Arroz".
    let hits = space
        .vector_search(model.id, &embed("cocina arroz agua", 64), 1)
        .unwrap();
    assert_eq!(hits[0].node.id, ids[1]);
    let hits = space
        .vector_search(model.id, &embed("cafe tueste", 64), 1)
        .unwrap();
    assert_eq!(hits[0].node.id, ids[4]);

    // A query of the wrong length is refused; a limit of 0 finds nothing.
    assert!(matches!(
        space.vector_search(model.id, &[1.0; 3], 10),
        Err(Error::Invalid(_))
    ));
    assert!(
        space
            .vector_search(model.id, &embed("rice", 64), 0)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn a_long_node_is_found_by_the_part_that_matches() {
    let (_dir, mut space) = new_space();
    let filler = "lorem ipsum dolor sit amet ".repeat(60);
    let body =
        format!("# Intro\n\n{filler}\n\n# Details\n\n{filler}\n\n# Rice\n\nSimmer the rice.");
    let long = note(&mut space, "Long", &body);
    note(&mut space, "Short", "Nothing about food.");
    let model = space.add_embedding_model(&spec(64), &indexer()).unwrap();
    index_all(&mut space, model.id, 64);
    assert!(
        count(
            &space,
            &format!("SELECT count(*) FROM embedding_chunks WHERE node_id = '{long}'")
        ) > 2
    );

    let hits = space
        .vector_search(model.id, &embed("simmer rice", 64), 10)
        .unwrap();
    assert_eq!(hits.len(), 2, "one hit per node");
    assert_eq!(hits[0].node.id, long);
    assert_eq!(hits[0].chunk.heading, "Rice");
    assert!(hits[0].chunk.ord > 0);
}

#[test]
fn hybrid_search_fuses_words_and_meaning() {
    let (_dir, mut space) = new_space();
    let ids = corpus(&mut space);
    let model = space.add_embedding_model(&spec(64), &indexer()).unwrap();
    index_all(&mut space, model.id, 64);

    // Found by words and by meaning.
    let query = "rice";
    let vector = embed("rice water simmer", 64);
    let results = space
        .hybrid_search(query, Some(&vector), SearchMode::Hybrid, 20)
        .unwrap();
    assert_eq!(results.mode, SearchMode::Hybrid);
    assert_eq!(results.hits[0].node.id, ids[0]);
    assert_eq!(results.hits[0].word_rank, Some(1));
    assert_eq!(results.hits[0].meaning_rank, Some(1));
    assert!((results.hits[0].score - 2.0 / 61.0).abs() < 1e-12);
    assert!(results.hits[0].snippet.contains("**"), "words snippet");
    // Every other node is found by meaning alone, after it.
    assert_eq!(results.hits.len(), 5);
    for hit in &results.hits[1..] {
        assert_eq!(hit.word_rank, None);
        assert!(hit.score < results.hits[0].score);
        assert!(!hit.snippet.contains("**"));
    }
    assert!(results.hits.windows(2).all(|w| w[0].score >= w[1].score));
    // The limit cuts the list.
    let results = space
        .hybrid_search(query, Some(&vector), SearchMode::Hybrid, 2)
        .unwrap();
    assert_eq!(results.hits.len(), 2);

    // Words alone and meaning alone.
    let words = space
        .hybrid_search(query, Some(&vector), SearchMode::Words, 20)
        .unwrap();
    assert_eq!(words.mode, SearchMode::Words);
    assert_eq!(words.hits.len(), 1);
    assert!((words.hits[0].score - 1.0 / 61.0).abs() < 1e-12);
    let meaning = space
        .hybrid_search(
            "",
            Some(&embed("cocina arroz", 64)),
            SearchMode::Meaning,
            20,
        )
        .unwrap();
    assert_eq!(meaning.mode, SearchMode::Meaning);
    assert_eq!(meaning.hits[0].node.id, ids[1]);
    assert!(meaning.hits.iter().all(|hit| hit.word_rank.is_none()));
}

#[test]
fn hybrid_search_follows_the_ranks_of_the_specification() {
    // Words (A, B, C) and meaning (C, A, D) give A, C, B, D (spec 19.6).
    let (_dir, mut space) = new_space();
    let a = note(&mut space, "A", "zebra zebra zebra zebra");
    let b = note(&mut space, "B", "zebra zebra one two three four");
    let c = note(
        &mut space,
        "C",
        "zebra one two three four five six seven eight nine ten eleven",
    );
    let d = note(&mut space, "D", "nothing");
    let model = space.add_embedding_model(&spec(3), &indexer()).unwrap();
    // Vectors made by hand: the query is [1, 0, 0].
    let vectors = [
        (a, [0.9f32, 0.43, 0.0]),
        (b, [0.0, 0.0, 1.0]),
        (c, [1.0, 0.0, 0.0]),
        (d, [0.7, 0.71, 0.0]),
    ];
    for node in space.stale_nodes(model.id, 10).unwrap() {
        let vector = vectors.iter().find(|(id, _)| *id == node.id).unwrap().1;
        let chunks: Vec<ChunkVector> = node
            .chunks()
            .into_iter()
            .map(|chunk| ChunkVector {
                chunk,
                vector: vector.to_vec(),
            })
            .collect();
        assert!(space.store_chunks(model.id, &node, &chunks).unwrap());
    }
    let words: Vec<NodeId> = space
        .search("zebra", 10)
        .unwrap()
        .into_iter()
        .map(|hit| hit.node.id)
        .collect();
    assert_eq!(words, [a, b, c], "BM25 order");
    let meaning: Vec<NodeId> = space
        .vector_search(model.id, &[1.0, 0.0, 0.0], 10)
        .unwrap()
        .into_iter()
        .map(|hit| hit.node.id)
        .collect();
    assert_eq!(meaning, [c, a, d, b]);

    let results = space
        .hybrid_search("zebra", Some(&[1.0, 0.0, 0.0]), SearchMode::Hybrid, 3)
        .unwrap();
    let order: Vec<NodeId> = results.hits.iter().map(|hit| hit.node.id).collect();
    assert_eq!(order, [a, c, b]);
    let score = |hit: &knode_format::HybridHit| (hit.score * 1e5).round() / 1e5;
    assert_eq!(score(&results.hits[0]), 0.03252);
    assert_eq!(score(&results.hits[1]), 0.03227);
    // B is in both lists here (4th by meaning), unlike the example, so it scores a little more.
    assert_eq!(results.hits[2].meaning_rank, Some(4));
}

#[test]
fn without_a_model_hybrid_search_is_by_words() {
    let (_dir, mut space) = new_space();
    let ids = corpus(&mut space);
    let results = space
        .hybrid_search("arroz", Some(&embed("arroz", 64)), SearchMode::Hybrid, 20)
        .unwrap();
    assert_eq!(results.mode, SearchMode::Words);
    assert_eq!(results.hits[0].node.id, ids[1]);
    assert!(matches!(
        space.hybrid_search("arroz", None, SearchMode::Meaning, 20),
        Err(Error::VectorSearchUnavailable(_))
    ));
    // With a model in use but no query vector, hybrid is by words too.
    space.add_embedding_model(&spec(64), &indexer()).unwrap();
    let results = space
        .hybrid_search("arroz", None, SearchMode::Hybrid, 20)
        .unwrap();
    assert_eq!(results.mode, SearchMode::Words);
    // A node not embedded yet is found by its words only.
    let results = space
        .hybrid_search("arroz", Some(&embed("arroz", 64)), SearchMode::Hybrid, 20)
        .unwrap();
    assert_eq!(results.mode, SearchMode::Hybrid);
    assert_eq!(results.hits.len(), 1);
    assert_eq!(results.hits[0].meaning_rank, None);
}

#[test]
fn embeddings_survive_reopening() {
    let (dir, mut space) = new_space();
    let ids = corpus(&mut space);
    let model = space.add_embedding_model(&spec(64), &indexer()).unwrap();
    index_all(&mut space, model.id, 64);
    drop(space);
    let space = Space::open(dir.path().join("Test.knode")).unwrap();
    assert_eq!(space.embedding_model().unwrap(), Some(model.clone()));
    let hits = space
        .vector_search(model.id, &embed("revenue costs", 64), 1)
        .unwrap();
    assert_eq!(hits[0].node.id, ids[2]);
}
