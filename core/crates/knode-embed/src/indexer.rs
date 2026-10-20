//! Embedding the nodes of a space that wait for it, and storing their vectors.

use std::ops::ControlFlow;

use knode_format::{ChunkVector, EmbeddingModel, Provenance, Space, StaleNode, embedding_text};

use crate::{Embedder, Result};

/// Embeds the nodes of a space with one [`Embedder`] and stores their chunks and vectors.
///
/// A node waits for embedding when it has no chunks of the model: it is new, or its title or
/// text changed (the space drops its chunks then). Indexing is resumable: every node is stored
/// in its own transaction, so stopping at any point loses at most the batch being embedded, and
/// the next run starts from the nodes that still wait.
///
/// [`Indexer::run`] does everything while holding the space. A caller that shares the space,
/// such as Knode Service, can keep it free while the model runs instead: read a batch with
/// [`Space::stale_nodes`], embed it with [`Indexer::embed_nodes`] without the space, then store
/// it with [`Indexer::store`].
#[derive(Debug, Clone)]
pub struct Indexer<'a> {
    embedder: &'a Embedder,
    batch_size: usize,
    by: Provenance,
}

/// Where a run of [`Indexer::run`] is, after each batch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IndexProgress {
    /// The model's id in the space.
    pub model_id: i64,
    /// Nodes embedded and stored so far in this run.
    pub done: u64,
    /// Nodes that still wait.
    pub remaining: u64,
}

impl IndexProgress {
    /// The share done, from 0 to 1.
    pub fn fraction(&self) -> f64 {
        let total = self.done + self.remaining;
        if total == 0 {
            1.0
        } else {
            self.done as f64 / total as f64
        }
    }
}

/// The chunks of one node with their vectors, ready to store.
#[derive(Debug, Clone, PartialEq)]
pub struct NodeEmbedding {
    /// The node as it was read and embedded.
    pub node: StaleNode,
    /// Its chunks and their vectors.
    pub chunks: Vec<ChunkVector>,
}

/// What one batch did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BatchReport {
    /// Nodes embedded and stored.
    pub stored: u64,
    /// Nodes that changed or were deleted while they were embedded; those still there wait
    /// again.
    pub skipped: u64,
    /// Chunks stored.
    pub chunks: u64,
}

/// What a run of [`Indexer::run`] did.
#[derive(Debug, Clone, PartialEq)]
pub struct IndexReport {
    /// The model, as the space records it.
    pub model: EmbeddingModel,
    /// Nodes embedded and stored.
    pub stored: u64,
    /// Nodes skipped because they changed meanwhile.
    pub skipped: u64,
    /// Chunks stored.
    pub chunks: u64,
    /// Orphan vectors deleted at the end.
    pub orphans_deleted: u64,
    /// Whether every node is embedded with the model now.
    pub complete: bool,
    /// Whether the model became the one in use in this run, replacing an older one.
    pub switched: bool,
}

/// How many batches in a row may store nothing before a run gives up: nodes that change faster
/// than they are embedded are left for the next run.
const MAX_IDLE_BATCHES: u32 = 3;

impl<'a> Indexer<'a> {
    /// How many nodes are embedded at once by default.
    pub const DEFAULT_BATCH_SIZE: usize = 16;

    /// An indexer for `embedder`, recording its changes to the space (the model in use) as
    /// made by Knode itself ([`Provenance::system`]).
    pub fn new(embedder: &'a Embedder) -> Self {
        Indexer {
            embedder,
            batch_size: Self::DEFAULT_BATCH_SIZE,
            by: Provenance::system(),
        }
    }

    /// Embeds `size` nodes at once (at least 1).
    pub fn with_batch_size(mut self, size: usize) -> Self {
        self.batch_size = size.max(1);
        self
    }

    /// Records changes to the space as made by `by`.
    pub fn with_provenance(mut self, by: Provenance) -> Self {
        self.by = by;
        self
    }

    /// The embedder.
    pub fn embedder(&self) -> &Embedder {
        self.embedder
    }

    /// Adds the embedder's model to the space, or finds it, and returns it. The first model of a
    /// space is in use at once; another one is in use once every node is embedded with it.
    pub fn prepare(&self, space: &mut Space) -> Result<EmbeddingModel> {
        Ok(space.add_embedding_model(&self.embedder.model_spec(), &self.by)?)
    }

    /// Embeds nodes: cuts each into chunks by `knode-1` and embeds every chunk as its title,
    /// heading and text, with the model's passage prefix. Needs no space.
    pub fn embed_nodes(&self, nodes: Vec<StaleNode>) -> Result<Vec<NodeEmbedding>> {
        let chunks: Vec<_> = nodes.iter().map(StaleNode::chunks).collect();
        let texts: Vec<String> = nodes
            .iter()
            .zip(&chunks)
            .flat_map(|(node, chunks)| chunks.iter().map(|c| embedding_text(&node.title, c)))
            .collect();
        let refs: Vec<&str> = texts.iter().map(String::as_str).collect();
        let mut vectors = self.embedder.embed(&refs)?.into_iter();
        Ok(nodes
            .into_iter()
            .zip(chunks)
            .map(|(node, chunks)| NodeEmbedding {
                node,
                chunks: chunks
                    .into_iter()
                    .map(|chunk| ChunkVector {
                        chunk,
                        vector: vectors.next().expect("one vector per chunk"),
                    })
                    .collect(),
            })
            .collect())
    }

    /// Stores embedded nodes, each in its own transaction. A node that changed since it was
    /// read is skipped and waits again.
    pub fn store(
        &self,
        space: &mut Space,
        model: &EmbeddingModel,
        embedded: &[NodeEmbedding],
    ) -> Result<BatchReport> {
        let mut report = BatchReport::default();
        for NodeEmbedding { node, chunks } in embedded {
            if space.store_chunks(model.id, node, chunks)? {
                report.stored += 1;
                report.chunks += chunks.len() as u64;
            } else {
                report.skipped += 1;
            }
        }
        Ok(report)
    }

    /// Embeds and stores one batch of the nodes that wait, most recently changed first.
    pub fn index_batch(&self, space: &mut Space, model: &EmbeddingModel) -> Result<BatchReport> {
        let nodes = space.stale_nodes(model.id, self.batch_size)?;
        if nodes.is_empty() {
            return Ok(BatchReport::default());
        }
        let embedded = self.embed_nodes(nodes)?;
        self.store(space, model, &embedded)
    }

    /// Finishes when no node waits: makes the model the one in use if it is not yet, which
    /// deletes the older model's data, and deletes orphan vectors. Returns whether it switched
    /// models and how many orphans it deleted, or `None` when nodes still wait.
    pub fn finish(&self, space: &mut Space, model: &EmbeddingModel) -> Result<Option<(bool, u64)>> {
        if space.stale_node_count(model.id)? > 0 {
            return Ok(None);
        }
        let in_use = space.embedding_model()?.map(|m| m.id);
        let switched = in_use != Some(model.id);
        if switched {
            space.use_embedding_model(model.id, &self.by)?;
        }
        let orphans = space.delete_orphan_vectors(model.id)?;
        Ok(Some((switched, orphans)))
    }

    /// Embeds every node of the space that waits, in batches, then [`finish`](Self::finish)es.
    ///
    /// `progress` is called after each batch; returning [`ControlFlow::Break`] stops the run,
    /// which a later run resumes.
    pub fn run(
        &self,
        space: &mut Space,
        mut progress: impl FnMut(&IndexProgress) -> ControlFlow<()>,
    ) -> Result<IndexReport> {
        let model = self.prepare(space)?;
        let mut report = IndexReport {
            model: model.clone(),
            stored: 0,
            skipped: 0,
            chunks: 0,
            orphans_deleted: 0,
            complete: false,
            switched: false,
        };
        let mut idle = 0;
        loop {
            let batch = self.index_batch(space, &model)?;
            report.stored += batch.stored;
            report.skipped += batch.skipped;
            report.chunks += batch.chunks;
            let remaining = space.stale_node_count(model.id)?;
            if batch.stored + batch.skipped == 0 || remaining == 0 {
                break;
            }
            let state = IndexProgress {
                model_id: model.id,
                done: report.stored,
                remaining,
            };
            if progress(&state).is_break() {
                return Ok(report);
            }
            idle = if batch.stored == 0 { idle + 1 } else { 0 };
            if idle >= MAX_IDLE_BATCHES {
                return Ok(report);
            }
        }
        let _ = progress(&IndexProgress {
            model_id: model.id,
            done: report.stored,
            remaining: 0,
        });
        if let Some((switched, orphans)) = self.finish(space, &model)? {
            report.complete = true;
            report.switched = switched;
            report.orphans_deleted = orphans;
        }
        Ok(report)
    }
}

#[cfg(test)]
mod tests {
    use knode_format::{NewNode, NodeId, NodeUpdate, SearchMode};

    use super::*;
    use crate::Manifest;
    use crate::test_support::write_tiny_model;

    fn tiny(source: &str) -> (tempfile::TempDir, Embedder) {
        let dir = tempfile::tempdir().unwrap();
        write_tiny_model(dir.path());
        let mut manifest = Manifest::read(dir.path()).unwrap();
        manifest.source = source.into();
        manifest.write(dir.path()).unwrap();
        let embedder = Embedder::load_cpu(dir.path()).unwrap();
        (dir, embedder)
    }

    fn space_with(notes: &[(&str, &str)]) -> (tempfile::TempDir, Space, Vec<NodeId>) {
        let dir = tempfile::tempdir().unwrap();
        let mut space = Space::create(dir.path().join("Test.knode"), "Test").unwrap();
        let me = Provenance::person("Ada");
        let ids = notes
            .iter()
            .map(|(title, body)| {
                space
                    .create_node(NewNode::note(*title, *body), &me)
                    .unwrap()
                    .id
            })
            .collect();
        (dir, space, ids)
    }

    fn corpus() -> Vec<(&'static str, String)> {
        let mut notes = vec![
            ("Cat", "# the cat\n\nthe cat sat".to_string()),
            ("Dog", "a dog ran far away".to_string()),
            (
                "Arroz",
                "Lava el arroz y cuécelo con el doble de agua.".to_string(),
            ),
            ("Empty", String::new()),
        ];
        for i in 0..9 {
            notes.push(("Note", format!("the dog sat {i}")));
        }
        notes
    }

    #[test]
    fn indexes_every_node_in_batches_and_reports_progress() {
        let (_m, embedder) = tiny("test/tiny@1");
        let notes = corpus();
        let refs: Vec<(&str, &str)> = notes.iter().map(|(t, b)| (*t, b.as_str())).collect();
        let (_d, mut space, ids) = space_with(&refs);
        let mut seen = Vec::new();
        let report = Indexer::new(&embedder)
            .with_batch_size(4)
            .run(&mut space, |p| {
                seen.push(*p);
                ControlFlow::Continue(())
            })
            .unwrap();
        assert!(report.complete);
        assert!(!report.switched, "the first model is in use at once");
        assert_eq!(report.stored, ids.len() as u64);
        assert_eq!(report.skipped, 0);
        assert!(report.chunks >= ids.len() as u64);
        let dones: Vec<u64> = seen.iter().map(|p| p.done).collect();
        assert_eq!(dones, [4, 8, 12, 13]);
        assert_eq!(seen.last().unwrap().remaining, 0);
        assert_eq!(seen.last().unwrap().fraction(), 1.0);
        let model = space.embedding_model().unwrap().unwrap();
        assert_eq!(model, report.model);
        assert_eq!(model.spec.name, "test/tiny");
        assert_eq!(model.spec.revision, "1");
        assert_eq!(model.spec.document_prefix, "passage: ");
        assert_eq!(space.stale_node_count(model.id).unwrap(), 0);

        // The nearest node to one of its own chunks is itself.
        let cat = space.stale_nodes(model.id, 1).unwrap();
        assert!(cat.is_empty());
        let node = StaleNode {
            id: ids[0],
            title: "Cat".into(),
            body: notes[0].1.clone(),
            plain: "the cat\nthe cat sat".into(),
        };
        let text = embedding_text(&node.title, &node.chunks()[0]);
        let vector = embedder.embed(&[&text]).unwrap().remove(0);
        let hits = space.vector_search(model.id, &vector, 5).unwrap();
        assert_eq!(hits[0].node.id, ids[0]);
        assert!(hits[0].distance < 1e-4, "{}", hits[0].distance);
        assert_eq!(hits[0].chunk.heading, "the cat");

        // Hybrid search with a query embedded by the model.
        let query = embedder.embed_query("cat").unwrap();
        let results = space
            .hybrid_search("cat", Some(&query), SearchMode::Hybrid, 20)
            .unwrap();
        assert_eq!(results.mode, SearchMode::Hybrid);
        assert_eq!(results.hits[0].node.id, ids[0]);
        assert_eq!(results.hits.len(), ids.len());
    }

    #[test]
    fn a_stopped_run_resumes_and_edits_are_indexed_again() {
        let (_m, embedder) = tiny("test/tiny@1");
        let notes = corpus();
        let refs: Vec<(&str, &str)> = notes.iter().map(|(t, b)| (*t, b.as_str())).collect();
        let (_d, mut space, ids) = space_with(&refs);
        let indexer = Indexer::new(&embedder).with_batch_size(5);
        let report = indexer.run(&mut space, |_| ControlFlow::Break(())).unwrap();
        assert!(!report.complete);
        assert_eq!(report.stored, 5);
        let model = report.model;
        assert_eq!(space.stale_node_count(model.id).unwrap(), 8);

        let report = indexer
            .run(&mut space, |_| ControlFlow::Continue(()))
            .unwrap();
        assert!(report.complete);
        assert_eq!(report.stored, 8);
        assert_eq!(report.orphans_deleted, 0);

        // An edit drops the node's chunks; the next run embeds it again and removes the orphan.
        space
            .update_node(
                &ids[1],
                NodeUpdate {
                    body: Some("a dog sat".into()),
                    ..Default::default()
                },
                &Provenance::person("Ada"),
            )
            .unwrap();
        assert_eq!(space.stale_node_count(model.id).unwrap(), 1);
        let report = indexer
            .run(&mut space, |_| ControlFlow::Continue(()))
            .unwrap();
        assert!(report.complete);
        assert_eq!(report.stored, 1);
        assert_eq!(report.orphans_deleted, 1);
    }

    #[test]
    fn a_node_changed_during_embedding_is_skipped_and_waits() {
        let (_m, embedder) = tiny("test/tiny@1");
        let (_d, mut space, ids) = space_with(&[("Cat", "the cat sat")]);
        let indexer = Indexer::new(&embedder);
        let model = indexer.prepare(&mut space).unwrap();
        let read = space.stale_nodes(model.id, 10).unwrap();
        let embedded = indexer.embed_nodes(read).unwrap();
        space
            .update_node(
                &ids[0],
                NodeUpdate {
                    title: Some("Dog".into()),
                    ..Default::default()
                },
                &Provenance::person("Ada"),
            )
            .unwrap();
        let batch = indexer.store(&mut space, &model, &embedded).unwrap();
        assert_eq!(
            batch,
            BatchReport {
                stored: 0,
                skipped: 1,
                chunks: 0
            }
        );
        assert_eq!(space.stale_node_count(model.id).unwrap(), 1);
        assert_eq!(indexer.finish(&mut space, &model).unwrap(), None);
    }

    #[test]
    fn a_new_model_replaces_the_old_once_every_node_has_it() {
        let notes = corpus();
        let refs: Vec<(&str, &str)> = notes.iter().map(|(t, b)| (*t, b.as_str())).collect();
        let (_d, mut space, ids) = space_with(&refs);
        let (_m1, old) = tiny("test/tiny@1");
        let first = Indexer::new(&old)
            .run(&mut space, |_| ControlFlow::Continue(()))
            .unwrap();
        let (_m2, new) = tiny("test/tiny@2");
        let indexer = Indexer::new(&new).with_batch_size(4);

        // Stopped halfway, the old model stays in use and searches keep working.
        let partial = indexer.run(&mut space, |_| ControlFlow::Break(())).unwrap();
        assert!(!partial.complete);
        assert_ne!(partial.model.id, first.model.id);
        assert_eq!(space.embedding_model().unwrap().unwrap().id, first.model.id);
        let query = old.embed_query("cat").unwrap();
        assert!(
            !space
                .vector_search(first.model.id, &query, 5)
                .unwrap()
                .is_empty()
        );

        let done = indexer
            .run(&mut space, |_| ControlFlow::Continue(()))
            .unwrap();
        assert!(done.complete);
        assert!(done.switched);
        assert_eq!(done.stored, ids.len() as u64 - 4);
        assert_eq!(space.embedding_model().unwrap().unwrap().id, done.model.id);
        assert_eq!(
            space.embedding_models().unwrap(),
            std::slice::from_ref(&done.model)
        );
        let query = new.embed_query("cat").unwrap();
        assert_eq!(
            space
                .vector_search(done.model.id, &query, 50)
                .unwrap()
                .len(),
            ids.len()
        );
    }
}
