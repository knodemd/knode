//! Semantic search (schema version 4, spec section 19): embedding models, the chunks of each
//! node and their vectors in sqlite-vec tables, search by meaning and hybrid search.
//!
//! Embedding text is not done here: an embedding engine (such as `knode-embed`) reads the nodes
//! that wait with [`Space::stale_nodes`], cuts them with [`chunk_node_text`], embeds the chunks
//! outside any transaction and stores them with [`Space::store_chunks`].

use std::collections::HashMap;

use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};

use crate::chunking::{CHUNKING, TextChunk, chunk_node_text};
use crate::space::{SUMMARY_COLUMNS, add_change_with_detail, limit_param, provenance_id, summary};
use crate::{
    Error, NodeId, NodeSummary, Provenance, Result, Space, Timestamp, actions, setting_keys,
};

/// The constant of reciprocal rank fusion (19.6).
pub const RRF_K: f64 = 60.0;
/// The most nodes taken from each list, words and meaning, in a hybrid search (19.6).
pub const HYBRID_LIST_SIZE: usize = 50;
/// The nearest chunks a search by meaning reads before ranking nodes (19.5).
pub const NEAREST_CHUNKS: usize = 100;
/// The number of results of a hybrid search when none is given (19.6).
pub const DEFAULT_HYBRID_RESULTS: usize = 20;
/// About how many words a snippet from a chunk keeps.
const SNIPPET_WORDS: usize = 16;

/// What identifies an embedding model (19.2). Two models are the same only when every field is
/// equal; vectors of different models are never compared.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct EmbeddingModelSpec {
    /// The model's name where it is published, such as `intfloat/multilingual-e5-small`.
    pub name: String,
    /// The version of its weights, a release or a commit hash; `''` for none.
    pub revision: String,
    /// The length of its vectors, 1 to 8192.
    pub dimensions: u32,
    /// The chunking rule its chunks follow. This library writes [`CHUNKING`] (`knode-1`).
    pub chunking: String,
    /// Text put before a query when it is embedded, such as `query: `.
    pub query_prefix: String,
    /// Text put before a chunk when it is embedded, such as `passage: `.
    pub document_prefix: String,
}

impl EmbeddingModelSpec {
    /// A model with no revision or prefixes, chunked by `knode-1`.
    pub fn new(name: impl Into<String>, dimensions: u32) -> Self {
        EmbeddingModelSpec {
            name: name.into(),
            revision: String::new(),
            dimensions,
            chunking: CHUNKING.to_string(),
            query_prefix: String::new(),
            document_prefix: String::new(),
        }
    }
}

/// An embedding model whose vectors the space holds: a row of `embedding_models`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmbeddingModel {
    /// Its row id, which names its vector table `embedding_vectors_<id>`.
    pub id: i64,
    /// What identifies it.
    pub spec: EmbeddingModelSpec,
    /// When it was added.
    pub created_at: Timestamp,
}

impl EmbeddingModel {
    /// The name of its sqlite-vec table.
    pub fn vector_table(&self) -> String {
        vector_table(self.id)
    }
}

/// A node that waits for embedding by a model: it has no chunks of it, because it is new or
/// because its title or text changed since.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StaleNode {
    /// The node.
    pub id: NodeId,
    /// Its title, embedded with every chunk.
    pub title: String,
    /// Its body, in Markdown, where the headings of its chunks come from.
    pub body: String,
    /// Its plain text (section 8), which chunks are ranges of.
    pub plain: String,
}

impl StaleNode {
    /// Its chunks by rule `knode-1`.
    pub fn chunks(&self) -> Vec<TextChunk> {
        chunk_node_text(&self.body, &self.plain)
    }
}

/// A chunk and its vector, to store with [`Space::store_chunks`].
#[derive(Debug, Clone, PartialEq)]
pub struct ChunkVector {
    /// The chunk.
    pub chunk: TextChunk,
    /// Its vector, of the model's dimensions.
    pub vector: Vec<f32>,
}

/// A node found by meaning.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VectorHit {
    /// The node.
    pub node: NodeSummary,
    /// The cosine distance of its nearest chunk to the query, 0 to 2; lower is closer.
    pub distance: f64,
    /// Its nearest chunk, with its text.
    pub chunk: TextChunk,
    /// About 16 words from the start of its nearest chunk, with `…` where text was cut.
    pub snippet: String,
}

/// How [`Space::hybrid_search`] ranks (19.6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum SearchMode {
    /// By words alone (full-text search).
    Words,
    /// By meaning alone.
    Meaning,
    /// Words and meaning together, with reciprocal rank fusion. The default.
    #[default]
    Hybrid,
}

/// A result of [`Space::hybrid_search`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HybridHit {
    /// The node.
    pub node: NodeSummary,
    /// The snippet of the full-text search when the node was found by words, with matches in
    /// `**bold**`; otherwise about 16 words of its nearest chunk.
    pub snippet: String,
    /// Its reciprocal rank fusion score; higher is better. Comparable across spaces.
    pub score: f64,
    /// Its place in the words list, from 1, when it is in it.
    pub word_rank: Option<u32>,
    /// Its place in the meaning list, from 1, when it is in it.
    pub meaning_rank: Option<u32>,
}

/// What [`Space::hybrid_search`] returns.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HybridResults {
    /// The mode the search ran in: [`SearchMode::Words`] when hybrid was asked for but the
    /// space has no model in use, sqlite-vec is not loaded or no query vector was given, which
    /// whoever searched should be told.
    pub mode: SearchMode,
    /// The results, best first.
    pub hits: Vec<HybridHit>,
}

/// The name of the vector table of model `id`.
fn vector_table(id: i64) -> String {
    format!("embedding_vectors_{id}")
}

/// A vector as sqlite-vec reads it: 32-bit floats, little-endian.
fn vector_blob(vector: &[f32]) -> Vec<u8> {
    vector.iter().flat_map(|x| x.to_le_bytes()).collect()
}

fn table_exists(conn: &Connection, name: &str) -> Result<bool> {
    Ok(conn
        .query_row(
            "SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name = ?1",
            [name],
            |_| Ok(()),
        )
        .optional()?
        .is_some())
}

/// Whether sqlite-vec is loaded into the connection.
pub(crate) fn vec_loaded(conn: &Connection) -> bool {
    conn.query_row("SELECT vec_version()", [], |row| row.get::<_, String>(0))
        .is_ok()
}

fn unavailable() -> Error {
    Error::VectorSearchUnavailable(
        "sqlite-vec is not loaded (knode-format was built without the `vector-search` feature)"
            .into(),
    )
}

const MODEL_COLUMNS: &str = "id, name, revision, dimensions, chunking, query_prefix, \
     document_prefix, created_at";

fn read_model(row: &rusqlite::Row<'_>) -> rusqlite::Result<EmbeddingModel> {
    Ok(EmbeddingModel {
        id: row.get(0)?,
        spec: EmbeddingModelSpec {
            name: row.get(1)?,
            revision: row.get(2)?,
            dimensions: row.get(3)?,
            chunking: row.get(4)?,
            query_prefix: row.get(5)?,
            document_prefix: row.get(6)?,
        },
        created_at: Timestamp(row.get(7)?),
    })
}

fn model_by_id(conn: &Connection, id: i64) -> Result<Option<EmbeddingModel>> {
    Ok(conn
        .query_row(
            &format!("SELECT {MODEL_COLUMNS} FROM embedding_models WHERE id = ?1"),
            [id],
            read_model,
        )
        .optional()?)
}

fn require_model(conn: &Connection, id: i64) -> Result<EmbeddingModel> {
    model_by_id(conn, id)?.ok_or_else(|| Error::Invalid(format!("no embedding model {id}")))
}

/// Sets `embedding.model` and records the change, as every setting is (15.1).
fn set_model_setting(tx: &Transaction<'_>, id: i64, by: &Provenance) -> Result<()> {
    let key = setting_keys::EMBEDDING_MODEL;
    let value = id.to_string();
    let current: Option<String> = tx
        .query_row("SELECT value FROM settings WHERE key = ?1", [key], |row| {
            row.get(0)
        })
        .optional()?;
    if current.as_deref() == Some(value.as_str()) {
        return Ok(());
    }
    tx.execute(
        "INSERT INTO settings (key, value) VALUES (?1, ?2)
         ON CONFLICT (key) DO UPDATE SET value = excluded.value",
        params![key, value],
    )?;
    let prov = provenance_id(tx, by)?;
    let detail = serde_json::json!({ "value": value }).to_string();
    add_change_with_detail(
        tx,
        Timestamp::now(),
        prov,
        actions::SETTINGS_SET,
        key,
        None,
        &detail,
    )?;
    Ok(())
}

/// Deletes a model: its chunks, its vector table and its row.
fn drop_model(tx: &Transaction<'_>, id: i64) -> Result<()> {
    tx.execute("DELETE FROM embedding_chunks WHERE model_id = ?1", [id])?;
    tx.execute_batch(&format!("DROP TABLE IF EXISTS {}", vector_table(id)))?;
    tx.execute("DELETE FROM embedding_models WHERE id = ?1", [id])?;
    Ok(())
}

fn check_spec(spec: &EmbeddingModelSpec) -> Result<()> {
    let name = spec.name.chars().count();
    if name == 0 || name > 200 || spec.name.trim().is_empty() {
        return Err(Error::Invalid(
            "the name of an embedding model has 1 to 200 characters".into(),
        ));
    }
    if !(1..=8192).contains(&spec.dimensions) {
        return Err(Error::Invalid(
            "an embedding model has 1 to 8192 dimensions".into(),
        ));
    }
    if spec.chunking != CHUNKING {
        return Err(Error::Invalid(format!(
            "chunking {:?} is not supported; this library chunks by {CHUNKING}",
            spec.chunking
        )));
    }
    Ok(())
}

/// Cuts a chunk's text to about [`SNIPPET_WORDS`] words from its start.
fn chunk_snippet(text: &str) -> String {
    let words: Vec<&str> = text.split_whitespace().collect();
    if words.len() <= SNIPPET_WORDS {
        words.join(" ")
    } else {
        format!("{}…", words[..SNIPPET_WORDS].join(" "))
    }
}

impl Space {
    /// Whether this space can hold vectors and search by meaning: sqlite-vec is loaded into its
    /// connection. Without it a space still opens, keeps its chunks in step and searches by
    /// words.
    pub fn vector_search_available(&self) -> bool {
        vec_loaded(&self.conn)
    }

    /// The embedding models whose vectors the space holds, oldest first.
    pub fn embedding_models(&self) -> Result<Vec<EmbeddingModel>> {
        let mut stmt = self.conn.prepare_cached(&format!(
            "SELECT {MODEL_COLUMNS} FROM embedding_models ORDER BY id"
        ))?;
        let rows = stmt.query_map([], read_model)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// The model in use: the one the setting `embedding.model` names, which searches by meaning
    /// use. `None` when the space has no semantic search.
    pub fn embedding_model(&self) -> Result<Option<EmbeddingModel>> {
        let Some(value) = self.setting(setting_keys::EMBEDDING_MODEL)? else {
            return Ok(None);
        };
        match value.trim().parse::<i64>() {
            Ok(id) => model_by_id(&self.conn, id),
            Err(_) => Ok(None),
        }
    }

    /// Adds an embedding model, or finds the one with the same spec, and returns it (19.2).
    ///
    /// A new model gets its row and its vector table in one transaction. When the space has no
    /// model in use, it becomes the one in use, recorded as a change of `embedding.model` by
    /// `by`. Otherwise the model in use stays until [`Space::use_embedding_model`] switches,
    /// once every node is embedded with the new one.
    ///
    /// Fails with [`Error::VectorSearchUnavailable`] when sqlite-vec is not loaded.
    pub fn add_embedding_model(
        &mut self,
        spec: &EmbeddingModelSpec,
        by: &Provenance,
    ) -> Result<EmbeddingModel> {
        check_spec(spec)?;
        if !self.vector_search_available() {
            return Err(unavailable());
        }
        let in_use = self.embedding_model()?;
        let tx = self.conn.transaction()?;
        let existing: Option<i64> = tx
            .query_row(
                "SELECT id FROM embedding_models
                 WHERE name = ?1 AND revision = ?2 AND dimensions = ?3 AND chunking = ?4
                   AND query_prefix = ?5 AND document_prefix = ?6",
                params![
                    spec.name,
                    spec.revision,
                    spec.dimensions,
                    spec.chunking,
                    spec.query_prefix,
                    spec.document_prefix
                ],
                |row| row.get(0),
            )
            .optional()?;
        let id = match existing {
            Some(id) => id,
            None => {
                tx.execute(
                    "INSERT INTO embedding_models (name, revision, dimensions, chunking,
                         query_prefix, document_prefix, created_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                    params![
                        spec.name,
                        spec.revision,
                        spec.dimensions,
                        spec.chunking,
                        spec.query_prefix,
                        spec.document_prefix,
                        Timestamp::now().0
                    ],
                )?;
                tx.last_insert_rowid()
            }
        };
        let table = vector_table(id);
        if !table_exists(&tx, &table)? {
            // A model without its table (made where sqlite-vec was missing) is embedded again.
            tx.execute("DELETE FROM embedding_chunks WHERE model_id = ?1", [id])?;
            tx.execute_batch(&format!(
                "CREATE VIRTUAL TABLE {table} USING vec0 (
                     embedding float[{}] distance_metric=cosine
                 )",
                spec.dimensions
            ))?;
        }
        if in_use.is_none() {
            set_model_setting(&tx, id, by)?;
        }
        let model = require_model(&tx, id)?;
        tx.commit()?;
        Ok(model)
    }

    /// Makes model `id` the one in use and deletes every other model with its chunks and
    /// vectors, in one transaction (19.2): models are never mixed. Call it once every node is
    /// embedded with the new model ([`Space::stale_node_count`] is 0). The change of
    /// `embedding.model` is recorded as made by `by`.
    pub fn use_embedding_model(&mut self, id: i64, by: &Provenance) -> Result<()> {
        if !self.vector_search_available() {
            return Err(unavailable());
        }
        let tx = self.conn.transaction()?;
        require_model(&tx, id)?;
        set_model_setting(&tx, id, by)?;
        let others: Vec<i64> = tx
            .prepare("SELECT id FROM embedding_models WHERE id != ?1")?
            .query_map([id], |row| row.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        for other in others {
            drop_model(&tx, other)?;
        }
        tx.commit()?;
        Ok(())
    }

    /// The nodes that wait for embedding by model `id`, at most `limit`, most recently changed
    /// first (newer nodes first within the same millisecond): those without chunks of it (19.4).
    pub fn stale_nodes(&self, id: i64, limit: usize) -> Result<Vec<StaleNode>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT n.id, n.title, n.body, n.plain FROM nodes n
             WHERE NOT EXISTS (
                 SELECT 1 FROM embedding_chunks c WHERE c.model_id = ?1 AND c.node_id = n.id
             )
             ORDER BY n.updated_at DESC, n.id DESC
             LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![id, limit_param(limit)], |row| {
            Ok(StaleNode {
                id: crate::space::node_id(row, 0)?,
                title: row.get(1)?,
                body: row.get(2)?,
                plain: row.get(3)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// How many nodes wait for embedding by model `id`.
    pub fn stale_node_count(&self, id: i64) -> Result<u64> {
        Ok(self.conn.query_row(
            "SELECT count(*) FROM nodes n WHERE NOT EXISTS (
                 SELECT 1 FROM embedding_chunks c WHERE c.model_id = ?1 AND c.node_id = n.id
             )",
            [id],
            |row| row.get::<_, i64>(0),
        )? as u64)
    }

    /// Stores the chunks of a node embedded by model `id`, with their vectors (19.4).
    ///
    /// `node` is the node as it was read and embedded. In one transaction, checks that its
    /// title and plain text are still those, deletes its chunks of the model and their vectors,
    /// and inserts the new ones. Returns `false`, storing nothing, when the node changed or was
    /// deleted meanwhile: it waits again, with its new text.
    pub fn store_chunks(
        &mut self,
        id: i64,
        node: &StaleNode,
        chunks: &[ChunkVector],
    ) -> Result<bool> {
        if !self.vector_search_available() {
            return Err(unavailable());
        }
        let model = require_model(&self.conn, id)?;
        let length = node.plain.chars().count() as u32;
        if chunks.is_empty() {
            return Err(Error::Invalid("a node has at least one chunk".into()));
        }
        for (i, ChunkVector { chunk, vector }) in chunks.iter().enumerate() {
            if chunk.ord as usize != i {
                return Err(Error::Invalid(
                    "chunks are numbered from 0, in order".into(),
                ));
            }
            if chunk.start > chunk.end || chunk.end > length {
                return Err(Error::Invalid(format!(
                    "chunk {i} is not a range of the node's text"
                )));
            }
            if vector.len() != model.spec.dimensions as usize {
                return Err(Error::Invalid(format!(
                    "chunk {i} has a vector of {} dimensions; the model has {}",
                    vector.len(),
                    model.spec.dimensions
                )));
            }
            if !vector.iter().all(|x| x.is_finite()) {
                return Err(Error::Invalid(format!(
                    "chunk {i} has a vector that is not finite"
                )));
            }
        }
        let table = model.vector_table();
        let now = Timestamp::now();
        let node_key = node.id.to_string();
        let tx = self.conn.transaction()?;
        let current: Option<(String, String)> = tx
            .query_row(
                "SELECT title, plain FROM nodes WHERE id = ?1",
                [&node_key],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        if current.as_ref() != Some(&(node.title.clone(), node.plain.clone())) {
            return Ok(false);
        }
        let old: Vec<i64> = tx
            .prepare_cached("SELECT id FROM embedding_chunks WHERE node_id = ?1 AND model_id = ?2")?
            .query_map(params![node_key, id], |row| row.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        {
            let mut delete = tx.prepare_cached(&format!("DELETE FROM {table} WHERE rowid = ?1"))?;
            for chunk_id in &old {
                delete.execute([chunk_id])?;
            }
        }
        tx.execute(
            "DELETE FROM embedding_chunks WHERE node_id = ?1 AND model_id = ?2",
            params![node_key, id],
        )?;
        {
            let mut insert_chunk = tx.prepare_cached(
                "INSERT INTO embedding_chunks (node_id, model_id, ord, text_start, text_end,
                     heading, embedded_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            )?;
            let mut insert_vector = tx.prepare_cached(&format!(
                "INSERT INTO {table} (rowid, embedding) VALUES (?1, ?2)"
            ))?;
            for ChunkVector { chunk, vector } in chunks {
                insert_chunk.execute(params![
                    node_key,
                    id,
                    chunk.ord,
                    chunk.start,
                    chunk.end,
                    chunk.heading,
                    now.0
                ])?;
                let chunk_id = tx.last_insert_rowid();
                insert_vector.execute(params![chunk_id, vector_blob(vector)])?;
            }
        }
        tx.commit()?;
        Ok(true)
    }

    /// Deletes the vectors of model `id` whose chunks are gone (orphans, 19.4) and returns how
    /// many. Orphans are harmless, so this is done from time to time, such as after embedding.
    pub fn delete_orphan_vectors(&mut self, id: i64) -> Result<u64> {
        if !self.vector_search_available() {
            return Err(unavailable());
        }
        let model = require_model(&self.conn, id)?;
        let deleted = self.conn.execute(
            &format!(
                "DELETE FROM {} WHERE rowid NOT IN (
                     SELECT id FROM embedding_chunks WHERE model_id = ?1
                 )",
                model.vector_table()
            ),
            [id],
        )?;
        Ok(deleted as u64)
    }

    /// Finds nodes by meaning with model `id` (19.5): the chunks nearest to `query` (a vector of
    /// the query, embedded with the model's `query_prefix`), each node ranked by its nearest
    /// chunk. Returns at most `limit` nodes, nearest first.
    pub fn vector_search(&self, id: i64, query: &[f32], limit: usize) -> Result<Vec<VectorHit>> {
        if !self.vector_search_available() {
            return Err(unavailable());
        }
        let model = require_model(&self.conn, id)?;
        if query.len() != model.spec.dimensions as usize {
            return Err(Error::Invalid(format!(
                "the query vector has {} dimensions; the model has {}",
                query.len(),
                model.spec.dimensions
            )));
        }
        if limit == 0 {
            return Ok(Vec::new());
        }
        let table = model.vector_table();
        if !table_exists(&self.conn, &table)? {
            return Ok(Vec::new());
        }
        // 100 chunks for the 50 nodes of the recommendation; more when more nodes are asked for.
        let nearest = NEAREST_CHUNKS.max(limit.saturating_mul(2)).min(4096);
        let sql = nearest_sql(&table);
        let mut stmt = self.conn.prepare_cached(&sql)?;
        let rows = stmt.query_map(
            params![vector_blob(query), nearest as i64, id, limit_param(limit)],
            |row| {
                let text: String = row.get(14)?;
                Ok(VectorHit {
                    node: summary(row)?,
                    distance: row.get(9)?,
                    snippet: chunk_snippet(&text),
                    chunk: TextChunk {
                        ord: row.get(10)?,
                        start: row.get(11)?,
                        end: row.get(12)?,
                        heading: row.get(13)?,
                        text,
                    },
                })
            },
        )?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Searches by words, by meaning or both, ranked with reciprocal rank fusion (19.6).
    ///
    /// `query_vector` is the query embedded by the model in use, with its `query_prefix`. Up to
    /// 50 nodes by words and 50 by meaning are fused: each scores `1 / (60 + rank)` in every
    /// list it is in, and ties go to the better rank, then the lower node id. Returns at most
    /// `limit` results ([`DEFAULT_HYBRID_RESULTS`] is the usual).
    ///
    /// [`SearchMode::Hybrid`] runs as [`SearchMode::Words`] when the space has no model in use,
    /// sqlite-vec is not loaded or `query_vector` is `None`; [`HybridResults::mode`] says so.
    /// [`SearchMode::Meaning`] fails then with [`Error::VectorSearchUnavailable`].
    pub fn hybrid_search(
        &self,
        query: &str,
        query_vector: Option<&[f32]>,
        mode: SearchMode,
        limit: usize,
    ) -> Result<HybridResults> {
        let meaning = match mode {
            SearchMode::Words => None,
            SearchMode::Meaning | SearchMode::Hybrid => {
                let model = if self.vector_search_available() {
                    self.embedding_model()?
                } else {
                    None
                };
                match (model, query_vector) {
                    (Some(model), Some(vector)) => Some((model, vector)),
                    _ if mode == SearchMode::Meaning => {
                        return Err(Error::VectorSearchUnavailable(
                            if self.vector_search_available() {
                                "the space has no embedding model in use, or no query vector \
                                 was given"
                                    .into()
                            } else {
                                unavailable().to_string()
                            },
                        ));
                    }
                    _ => None,
                }
            }
        };
        let mode = match (mode, &meaning) {
            (SearchMode::Hybrid, None) => SearchMode::Words,
            (mode, _) => mode,
        };

        let words = if mode == SearchMode::Meaning {
            Vec::new()
        } else {
            self.search(query, HYBRID_LIST_SIZE)?
        };
        let nearest = match &meaning {
            Some((model, vector)) => self.vector_search(model.id, vector, HYBRID_LIST_SIZE)?,
            None => Vec::new(),
        };

        // node -> (hit, word rank, meaning rank)
        let mut fused: HashMap<NodeId, HybridHit> = HashMap::new();
        for (index, hit) in words.into_iter().enumerate() {
            fused.entry(hit.node.id).or_insert(HybridHit {
                node: hit.node,
                snippet: hit.snippet,
                score: 0.0,
                word_rank: Some(index as u32 + 1),
                meaning_rank: None,
            });
        }
        for (index, hit) in nearest.into_iter().enumerate() {
            let rank = Some(index as u32 + 1);
            fused
                .entry(hit.node.id)
                .and_modify(|entry| entry.meaning_rank = rank)
                .or_insert(HybridHit {
                    node: hit.node,
                    snippet: hit.snippet,
                    score: 0.0,
                    word_rank: None,
                    meaning_rank: rank,
                });
        }
        let mut hits: Vec<HybridHit> = fused
            .into_values()
            .map(|mut hit| {
                hit.score = rrf_score(hit.word_rank, hit.meaning_rank);
                hit
            })
            .collect();
        hits.sort_by(|a, b| {
            b.score
                .total_cmp(&a.score)
                .then(best_rank(a).cmp(&best_rank(b)))
                .then(a.node.id.cmp(&b.node.id))
        });
        hits.truncate(limit);
        Ok(HybridResults { mode, hits })
    }
}

/// The reciprocal rank fusion score of ranks in the words and meaning lists (19.6).
pub fn rrf_score(word_rank: Option<u32>, meaning_rank: Option<u32>) -> f64 {
    [word_rank, meaning_rank]
        .into_iter()
        .flatten()
        .map(|rank| 1.0 / (RRF_K + f64::from(rank)))
        .sum()
}

fn best_rank(hit: &HybridHit) -> u32 {
    hit.word_rank
        .unwrap_or(u32::MAX)
        .min(hit.meaning_rank.unwrap_or(u32::MAX))
}

/// The query of [`Space::vector_search`]: the `?2` chunks nearest to `?1` in `table`, the
/// nearest chunk of each node, at most `?4` nodes of model `?3`.
///
/// `hits` is materialized so that sqlite-vec runs its search once. Left to the planner, the
/// join can loop over `embedding_chunks` and run the whole search again for every chunk, which
/// takes minutes in a space of a few thousand notes.
fn nearest_sql(table: &str) -> String {
    // A bare column next to min() comes from the row with the minimum (SQLite), so each
    // node keeps its nearest chunk.
    format!(
        "WITH hits AS MATERIALIZED (
                 SELECT rowid, distance FROM {table} WHERE embedding MATCH ?1 AND k = ?2
             ),
             best AS (
                 SELECT c.node_id, min(hits.distance) AS distance, c.ord, c.text_start,
                        c.text_end, c.heading
                 FROM hits JOIN embedding_chunks c ON c.id = hits.rowid AND c.model_id = ?3
                 GROUP BY c.node_id
             )
             SELECT {SUMMARY_COLUMNS}, best.distance, best.ord, best.text_start, best.text_end,
                    best.heading,
                    substr(n.plain, best.text_start + 1, best.text_end - best.text_start)
             FROM best
             JOIN nodes n ON n.id = best.node_id
             JOIN provenance u ON u.id = n.updated_by
             ORDER BY best.distance, n.id
             LIMIT ?4"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rrf_matches_the_example_of_the_specification() {
        // Words (A, B, C), meaning (C, A, D).
        let a = rrf_score(Some(1), Some(2));
        let c = rrf_score(Some(3), Some(1));
        let b = rrf_score(Some(2), None);
        let d = rrf_score(None, Some(3));
        assert!((a - 0.03252).abs() < 1e-5, "{a}");
        assert!((c - 0.03227).abs() < 1e-5, "{c}");
        assert!((b - 0.01613).abs() < 1e-5, "{b}");
        assert!((d - 0.01587).abs() < 1e-5, "{d}");
        assert!(a > c && c > b && b > d);
    }

    #[test]
    fn snippets_keep_about_sixteen_words() {
        assert_eq!(chunk_snippet("one two"), "one two");
        let long = (0..30).map(|i| i.to_string()).collect::<Vec<_>>().join(" ");
        let snippet = chunk_snippet(&long);
        assert!(snippet.ends_with('…'));
        assert_eq!(snippet.trim_end_matches('…').split(' ').count(), 16);
    }

    #[test]
    fn search_by_meaning_runs_the_vector_search_once() {
        let dir = tempfile::tempdir().unwrap();
        let mut space = Space::create(dir.path().join("Plan.knode"), "Plan").unwrap();
        let spec = EmbeddingModelSpec::new("plan-test", 4);
        let model = space
            .add_embedding_model(&spec, &Provenance::person("Test"))
            .unwrap();
        let mut stmt = space
            .conn
            .prepare(&format!(
                "EXPLAIN QUERY PLAN {}",
                nearest_sql(&model.vector_table())
            ))
            .unwrap();
        let plan: Vec<String> = stmt
            .query_map(params![vector_blob(&[0.0; 4]), 100, model.id, 10], |row| {
                row.get(3)
            })
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert!(
            plan.iter().any(|step| step.starts_with("MATERIALIZE hits")),
            "{plan:#?}"
        );
    }

    #[test]
    fn vector_blobs_are_little_endian_floats() {
        assert_eq!(vector_blob(&[1.0]), 1.0f32.to_le_bytes().to_vec());
    }
}
