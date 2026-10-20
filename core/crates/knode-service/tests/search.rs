//! Search by words, by meaning and both through a real service, with a tiny model written on
//! the fly: nothing is downloaded.

// Without search by meaning, only the tests of the fallback to words run.
#![cfg_attr(not(semantic), allow(dead_code, unused_imports))]

use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use knode_format::{NewNode, NodeUpdate, Provenance, SearchMode};
use knode_service::{Client, Config, IndexOptions, IndexStatus, ServiceHandle, start};
use tempfile::TempDir;

const WAIT: Duration = Duration::from_secs(30);

struct Fixture {
    dir: TempDir,
    service: Option<ServiceHandle>,
}

impl Fixture {
    /// A service that indexes, with the tiny model in place when `model` is set.
    fn indexing(model: bool, config: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(home.join("config.toml"), config).unwrap();
        #[cfg(semantic)]
        if model {
            let models = home.join("models/multilingual-e5-small");
            std::fs::create_dir_all(&models).unwrap();
            knode_embed::test_support::write_tiny_model(&models);
        }
        #[cfg(not(semantic))]
        let _ = model;
        let options = IndexOptions {
            pause_on_battery: false,
            debounce: Duration::from_millis(20),
            download: false,
        };
        let service = start(Config::new(&home).with_index(options)).unwrap();
        Fixture {
            dir,
            service: Some(service),
        }
    }

    /// A service that does not index.
    fn plain() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let service = start(Config::new(dir.path().join("home"))).unwrap();
        Fixture {
            dir,
            service: Some(service),
        }
    }

    fn client(&self) -> Client {
        Client::connect(self.dir.path().join("home/service.sock")).unwrap()
    }

    fn space(&self, client: &Client, notes: &[(&str, &str)]) -> PathBuf {
        let path = self.dir.path().join("Test.knode");
        client.create_space(&path, "Test").unwrap();
        let me = Provenance::person("Ada");
        for (title, body) in notes {
            client
                .create_node(&path, NewNode::note(*title, *body), &me)
                .unwrap();
        }
        std::fs::canonicalize(path).unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        if let Some(service) = self.service.take() {
            service.stop().unwrap();
        }
    }
}

/// Waits until `space` is indexed: nothing waits and nothing is being indexed.
fn indexed(client: &Client, space: &Path) -> IndexStatus {
    let deadline = Instant::now() + WAIT;
    loop {
        let status = client.index_status().unwrap();
        let done = status
            .spaces
            .iter()
            .any(|s| s.space == space && s.waiting == 0 && !s.indexing && s.nodes > 0);
        if done {
            return status;
        }
        assert!(Instant::now() < deadline, "not indexed: {status:#?}");
        thread::sleep(Duration::from_millis(20));
    }
}

const NOTES: &[(&str, &str)] = &[
    ("Cat", "# the cat\n\nthe cat sat"),
    ("Dog", "a dog ran far away"),
    ("Away", "far away from the dog"),
];

#[cfg(semantic)]
#[test]
fn writes_are_indexed_and_searched_by_meaning() {
    use knode_service::ModelState;

    let fixture = Fixture::indexing(true, "");
    let client = fixture.client();
    let space = fixture.space(&client, NOTES);
    let status = indexed(&client, &space);
    assert!(status.available && status.enabled && status.running);
    assert_eq!(status.model.name, "multilingual-e5-small");
    assert_eq!(status.model.state, ModelState::Ready);
    assert_eq!(status.spaces[0].nodes, 3);

    // Hybrid: words and meaning together; every node is near the query in some way.
    let found = client
        .search_with_mode(&space, "cat", SearchMode::Hybrid, 10)
        .unwrap();
    assert_eq!(found.mode, SearchMode::Hybrid);
    assert_eq!(found.note, None);
    assert_eq!(found.hits.len(), 3);
    assert_eq!(found.hits[0].node.title, "Cat");
    assert_eq!(found.hits[0].word_rank, Some(1));
    assert!(found.hits.iter().all(|h| h.meaning_rank.is_some()));

    // Meaning alone finds nodes that share no word with the query.
    let found = client
        .search_with_mode(&space, "away", SearchMode::Meaning, 10)
        .unwrap();
    assert_eq!(found.mode, SearchMode::Meaning);
    assert_eq!(found.hits.len(), 3);
    assert!(found.hits.iter().all(|h| h.word_rank.is_none()));

    // Words alone.
    let found = client
        .search_with_mode(&space, "cat", SearchMode::Words, 10)
        .unwrap();
    assert_eq!(found.mode, SearchMode::Words);
    assert_eq!(found.hits.len(), 1);

    // Without a mode: the bare list older clients expect, ranked as hybrid.
    let hits = client.search(&space, "cat", 10).unwrap();
    assert_eq!(hits.len(), 3);
    assert_eq!(hits[0].node.title, "Cat");

    // A new note and an edit are indexed again after the write.
    let me = Provenance::person("Ada");
    let dog = client
        .create_node(&space, NewNode::note("Sat", "the dog sat"), &me)
        .unwrap();
    client
        .update_node(
            &space,
            &dog.id,
            NodeUpdate {
                body: Some("a dog sat far away".into()),
                ..Default::default()
            },
            &me,
        )
        .unwrap();
    let status = indexed(&client, &space);
    assert_eq!(status.spaces[0].nodes, 4);
    let found = client
        .search_with_mode(&space, "zebra", SearchMode::Meaning, 10)
        .unwrap();
    assert_eq!(found.hits.len(), 4);

    // `service.status` reports the index too.
    let status = client.status().unwrap();
    assert_eq!(status.index.unwrap().spaces[0].waiting, 0);
}

#[cfg(semantic)]
#[test]
fn without_the_model_search_is_by_words_and_says_why() {
    use knode_service::ModelState;

    let fixture = Fixture::indexing(false, "");
    let client = fixture.client();
    let space = fixture.space(&client, NOTES);
    let found = client
        .search_with_mode(&space, "cat", SearchMode::Hybrid, 10)
        .unwrap();
    assert_eq!(found.mode, SearchMode::Words);
    assert_eq!(found.hits.len(), 1);
    assert!(
        found.note.as_deref().unwrap().contains("downloading"),
        "{found:?}"
    );
    // Meaning falls back to words too, rather than failing.
    let found = client
        .search_with_mode(&space, "cat", SearchMode::Meaning, 10)
        .unwrap();
    assert_eq!(found.mode, SearchMode::Words);
    let status = client.index_status().unwrap();
    assert_eq!(status.model.state, ModelState::Missing);
    assert!(status.model.size > 400_000_000);
}

#[cfg(semantic)]
#[test]
fn embeddings_off_searches_by_words() {
    let fixture = Fixture::indexing(true, "embeddings = \"off\"\n");
    let client = fixture.client();
    let space = fixture.space(&client, NOTES);
    let found = client
        .search_with_mode(&space, "cat", SearchMode::Hybrid, 10)
        .unwrap();
    assert_eq!(found.mode, SearchMode::Words);
    assert!(found.note.as_deref().unwrap().contains("off"), "{found:?}");
    let status = client.index_status().unwrap();
    assert!(!status.enabled);
    assert!(status.spaces.iter().all(|s| !s.indexing));

    // Turned on again, the open spaces are indexed.
    std::fs::write(
        fixture.dir.path().join("home/config.toml"),
        "embeddings = \"on\"\n",
    )
    .unwrap();
    let status = client.index_run(None).unwrap();
    assert!(status.enabled);
    indexed(&client, &space);
    let found = client
        .search_with_mode(&space, "cat", SearchMode::Hybrid, 10)
        .unwrap();
    assert_eq!(found.mode, SearchMode::Hybrid);
}

#[test]
fn a_service_without_indexing_searches_by_words() {
    let fixture = Fixture::plain();
    let client = fixture.client();
    let space = fixture.space(&client, NOTES);
    let found = client
        .search_with_mode(&space, "cat", SearchMode::Hybrid, 10)
        .unwrap();
    assert_eq!(found.mode, SearchMode::Words);
    assert_eq!(found.hits.len(), 1);
    assert!(found.note.is_some());
    let status = client.index_status().unwrap();
    assert!(!status.running);
    assert_eq!(status.available, cfg!(semantic));
}

#[cfg(not(semantic))]
#[test]
fn a_build_without_semantic_searches_by_words() {
    let fixture = Fixture::indexing(true, "");
    let client = fixture.client();
    let space = fixture.space(&client, NOTES);
    let found = client
        .search_with_mode(&space, "cat", SearchMode::Meaning, 10)
        .unwrap();
    assert_eq!(found.mode, SearchMode::Words);
    assert!(
        found.note.as_deref().unwrap().contains("words only"),
        "{found:?}"
    );
    let status = client.index_status().unwrap();
    assert!(!status.available);
}
