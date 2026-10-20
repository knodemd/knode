//! Local semantic search for Knode.
//!
//! Knode finds notes by meaning with a small embedding model that runs on the device; nothing is
//! sent to a server. This crate is the engine:
//!
//! - [`ModelSpec`] describes a model and the files it needs, with their SHA-256. The default,
//!   [`ModelSpec::default_model`], is `multilingual-e5-small` (MIT, 384 dimensions, about 100
//!   languages).
//! - [`ensure_model`] downloads a model on first use into `$KNODE_HOME/models/<id>` and checks
//!   every file (needs the `download` feature, on by default).
//! - [`Embedder`] loads a model folder and turns text into normalized vectors.
//! - [`chunk()`] cuts a node into passages by the format's chunking rule `knode-1`.
//! - [`Indexer`] embeds the nodes of a space that wait for it, in batches, and stores their
//!   vectors through `knode-format`, which keeps them in sqlite-vec tables and searches them.
//! - [`rrf`] fuses rankings into one, as hybrid search does.

mod chunk;
#[cfg(feature = "download")]
mod download;
mod embedder;
mod error;
mod indexer;
mod model;
mod rrf;
#[cfg(test)]
mod test_support;

pub use chunk::{
    CHUNK_MAX_CHARS, CHUNK_OVERLAP_CHARS, CHUNKING, Chunk, chunk, embedding_text, estimate_tokens,
};
#[cfg(feature = "download")]
pub use download::{Downloader, Progress, ensure_model};
pub use embedder::Embedder;
pub use error::{Error, Result};
pub use indexer::{BatchReport, IndexProgress, IndexReport, Indexer, NodeEmbedding};
pub use model::{MANIFEST_FILE, Manifest, ModelFile, ModelSpec, Pooling, models_dir};
pub use rrf::{RRF_K, rrf, rrf_many};
