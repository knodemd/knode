use std::path::Path;

use candle_core::{DType, Device, Tensor};
use candle_nn::VarBuilder;
use candle_transformers::models::bert::{BertModel, Config};
use tokenizers::{Tokenizer, TruncationParams};

use crate::chunk::Chunk;
use crate::model::{Manifest, Pooling};
use crate::{Error, Result};

/// The most texts that go through the model at once.
const MAX_BATCH: usize = 32;
/// The most tokens (padding included) in one batch, which bounds the memory used by attention.
/// Texts are sorted by length first, so a batch carries little padding.
const MAX_BATCH_TOKENS: usize = 4096;

/// A loaded embedding model: text in, normalized vectors out.
///
/// Vectors have unit length, so the dot product of two of them is their cosine similarity.
pub struct Embedder {
    manifest: Manifest,
    model: BertModel,
    tokenizer: Tokenizer,
    device: Device,
}

impl std::fmt::Debug for Embedder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Embedder")
            .field("model_id", &self.manifest.id)
            .field("dimension", &self.manifest.dimension)
            .field("device", &self.device)
            .finish()
    }
}

impl Embedder {
    /// Loads the model in `model_dir`: `knode-model.json`, `config.json`, `tokenizer.json` and
    /// `model.safetensors`. Uses the GPU when built with the `metal` feature and one is there,
    /// else the CPU.
    pub fn load(model_dir: &Path) -> Result<Embedder> {
        Self::load_on(model_dir, default_device())
    }

    /// Loads the model in `model_dir` on the CPU.
    pub fn load_cpu(model_dir: &Path) -> Result<Embedder> {
        Self::load_on(model_dir, Device::Cpu)
    }

    fn load_on(model_dir: &Path, device: Device) -> Result<Embedder> {
        let manifest = Manifest::read(model_dir)?;
        let invalid = |message: String| Error::invalid(model_dir, message);

        let config_path = model_dir.join("config.json");
        let config_text =
            std::fs::read_to_string(&config_path).map_err(|err| Error::io(&config_path, err))?;
        let config: Config = serde_json::from_str(&config_text)
            .map_err(|err| invalid(format!("config.json: {err}")))?;
        if config.hidden_size != manifest.dimension {
            return Err(invalid(format!(
                "config.json has {} dimensions, {} expected",
                config.hidden_size, manifest.dimension
            )));
        }

        let mut tokenizer = Tokenizer::from_file(model_dir.join("tokenizer.json"))
            .map_err(|err| invalid(format!("tokenizer.json: {err}")))?;
        let max_tokens = manifest.max_tokens.min(config.max_position_embeddings);
        tokenizer
            .with_truncation(Some(TruncationParams {
                max_length: max_tokens,
                ..Default::default()
            }))
            .map_err(|err| invalid(format!("tokenizer.json: {err}")))?;
        tokenizer.with_padding(None);

        let weights = model_dir.join("model.safetensors");
        if !weights.is_file() {
            return Err(invalid("model.safetensors is missing".into()));
        }
        // SAFETY: the file is memory-mapped read-only while the weights are loaded; Knode never
        // writes into a model folder once its manifest exists.
        let vb = unsafe { VarBuilder::from_mmaped_safetensors(&[weights], DType::F32, &device)? };
        let model = BertModel::load(vb, &config)
            .map_err(|err| invalid(format!("model.safetensors: {err}")))?;

        Ok(Embedder {
            manifest,
            model,
            tokenizer,
            device,
        })
    }

    /// The model id, recorded next to every embedding it makes.
    pub fn model_id(&self) -> &str {
        &self.manifest.id
    }

    /// The size of each vector.
    pub fn dimension(&self) -> usize {
        self.manifest.dimension
    }

    /// The most tokens read from one text; the rest of a longer text is ignored.
    pub fn max_tokens(&self) -> usize {
        self.manifest.max_tokens
    }

    /// The manifest of the loaded model.
    pub fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    /// Embeds passages to be indexed (the text of nodes or their chunks), one vector per text, in
    /// order. The model's passage prefix is added.
    pub fn embed(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
        self.embed_with_prefix(&self.manifest.passage_prefix, texts)
    }

    /// Embeds a search query. The model's query prefix is added, so the vector is comparable with
    /// those of [`Embedder::embed`].
    pub fn embed_query(&self, query: &str) -> Result<Vec<f32>> {
        let mut vectors = self.embed_with_prefix(&self.manifest.query_prefix, &[query])?;
        Ok(vectors.pop().expect("one vector per text"))
    }

    /// The number of tokens the model sees for `text`, without special tokens or prefixes.
    pub fn count_tokens(&self, text: &str) -> usize {
        self.tokenizer
            .encode_fast(text, false)
            .map(|encoding| encoding.len())
            .unwrap_or_else(|_| crate::estimate_tokens(text))
    }

    /// Cuts Markdown into chunks by the format's rule `knode-1`, the same for every model.
    pub fn chunk(&self, markdown: &str) -> Vec<Chunk> {
        crate::chunk(markdown)
    }

    /// What identifies this model in a space (spec section 19.2): its published name and
    /// revision (from the manifest's `source`, `repo@revision`), its dimensions, the chunking
    /// rule `knode-1` and its prefixes.
    pub fn model_spec(&self) -> knode_format::EmbeddingModelSpec {
        let source = self.manifest.source.trim();
        let (name, revision) = match source.split_once('@') {
            Some((name, revision)) if !name.is_empty() => (name, revision),
            _ if !source.is_empty() => (source, ""),
            _ => (self.manifest.id.as_str(), ""),
        };
        knode_format::EmbeddingModelSpec {
            name: name.to_string(),
            revision: revision.to_string(),
            dimensions: self.manifest.dimension as u32,
            chunking: crate::CHUNKING.to_string(),
            query_prefix: self.manifest.query_prefix.clone(),
            document_prefix: self.manifest.passage_prefix.clone(),
        }
    }

    fn embed_with_prefix(&self, prefix: &str, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        let inputs: Vec<String> = texts.iter().map(|text| format!("{prefix}{text}")).collect();
        let encodings = self
            .tokenizer
            .encode_batch_fast(inputs, true)
            .map_err(|err| Error::Tokenizer(err.to_string()))?;

        let mut order: Vec<usize> = (0..encodings.len()).collect();
        order.sort_by_key(|&i| encodings[i].len());
        let mut batches: Vec<&[usize]> = Vec::new();
        let mut from = 0;
        while from < order.len() {
            // Sorted by length, so the last text of a batch is its longest.
            let mut to = from + 1;
            while to < order.len()
                && to - from < MAX_BATCH
                && (to + 1 - from) * encodings[order[to]].len() <= MAX_BATCH_TOKENS
            {
                to += 1;
            }
            batches.push(&order[from..to]);
            from = to;
        }

        let mut out = vec![Vec::new(); encodings.len()];
        for batch in batches {
            let longest = encodings[batch[batch.len() - 1]].len().max(1);
            let mut ids = Vec::with_capacity(batch.len() * longest);
            let mut mask = Vec::with_capacity(batch.len() * longest);
            for &i in batch {
                let encoding = &encodings[i];
                ids.extend(encoding.get_ids());
                mask.extend(encoding.get_attention_mask());
                let pad = longest - encoding.len();
                ids.extend(std::iter::repeat_n(0u32, pad));
                mask.extend(std::iter::repeat_n(0u32, pad));
            }
            let shape = (batch.len(), longest);
            let ids = Tensor::from_vec(ids, shape, &self.device)?;
            let mask = Tensor::from_vec(mask, shape, &self.device)?;
            let types = ids.zeros_like()?;
            let hidden = self.model.forward(&ids, &types, Some(&mask))?;
            let pooled = match self.manifest.pooling {
                Pooling::Mean => {
                    let mask = mask.to_dtype(DType::F32)?.unsqueeze(2)?;
                    let summed = hidden.broadcast_mul(&mask)?.sum(1)?;
                    let counts = mask.sum(1)?.clamp(1e-9, f64::MAX)?;
                    summed.broadcast_div(&counts)?
                }
                Pooling::Cls => hidden.narrow(1, 0, 1)?.squeeze(1)?,
            };
            let norms = pooled
                .sqr()?
                .sum_keepdim(1)?
                .sqrt()?
                .clamp(1e-12, f64::MAX)?;
            let normalized = pooled.broadcast_div(&norms)?;
            for (&i, vector) in batch.iter().zip(normalized.to_vec2::<f32>()?) {
                out[i] = vector;
            }
        }
        Ok(out)
    }
}

fn default_device() -> Device {
    #[cfg(feature = "metal")]
    if let Ok(device) = Device::new_metal(0) {
        return device;
    }
    Device::Cpu
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ModelSpec;
    use crate::test_support::write_tiny_model;

    fn dot(a: &[f32], b: &[f32]) -> f32 {
        a.iter().zip(b).map(|(x, y)| x * y).sum()
    }

    fn tiny() -> (tempfile::TempDir, Embedder) {
        let dir = tempfile::tempdir().unwrap();
        write_tiny_model(dir.path());
        let embedder = Embedder::load(dir.path()).unwrap();
        (dir, embedder)
    }

    #[test]
    fn embeds_normalized_vectors_in_order() {
        let (_dir, embedder) = tiny();
        assert_eq!(embedder.model_id(), "tiny-test");
        assert_eq!(embedder.dimension(), 8);

        let texts = ["the cat sat", "a dog ran far away", "the cat sat"];
        let vectors = embedder.embed(&texts).unwrap();
        assert_eq!(vectors.len(), 3);
        for vector in &vectors {
            assert_eq!(vector.len(), 8);
            let norm = dot(vector, vector).sqrt();
            assert!((norm - 1.0).abs() < 1e-5, "norm {norm}");
        }
        assert_eq!(vectors[0], vectors[2]);
        assert!(dot(&vectors[0], &vectors[1]) < 0.9999);
    }

    #[test]
    fn padding_does_not_change_a_vector() {
        let (_dir, embedder) = tiny();
        let alone = embedder.embed(&["cat"]).unwrap().remove(0);
        let batched = embedder
            .embed(&["cat", "the dog ran far away from the cat"])
            .unwrap()
            .remove(0);
        for (a, b) in alone.iter().zip(&batched) {
            assert!((a - b).abs() < 1e-5, "{alone:?} vs {batched:?}");
        }
    }

    #[test]
    fn queries_use_their_prefix() {
        let (_dir, embedder) = tiny();
        let query = embedder.embed_query("cat").unwrap();
        let passage = embedder.embed(&["cat"]).unwrap().remove(0);
        assert_eq!(query.len(), 8);
        assert_ne!(query, passage);
    }

    #[test]
    fn empty_input_and_long_input() {
        let (_dir, embedder) = tiny();
        assert!(embedder.embed(&[]).unwrap().is_empty());
        let long = "cat ".repeat(500);
        let vectors = embedder.embed(&["", &long]).unwrap();
        assert_eq!(vectors.len(), 2);
        assert!(vectors.iter().all(|v| v.iter().all(|x| x.is_finite())));
    }

    #[test]
    fn many_texts_cross_batches() {
        let (_dir, embedder) = tiny();
        let texts: Vec<String> = (0..80).map(|i| "dog ".repeat(i % 7 + 1)).collect();
        let refs: Vec<&str> = texts.iter().map(String::as_str).collect();
        let vectors = embedder.embed(&refs).unwrap();
        for (i, text) in refs.iter().enumerate() {
            let single = embedder.embed(&[text]).unwrap().remove(0);
            for (a, b) in single.iter().zip(&vectors[i]) {
                assert!((a - b).abs() < 1e-5);
            }
        }
    }

    #[test]
    fn a_missing_manifest_is_a_clear_error() {
        let dir = tempfile::tempdir().unwrap();
        let err = Embedder::load(dir.path()).unwrap_err();
        assert!(
            err.to_string().contains("knode-model.json is missing"),
            "{err}"
        );
    }

    #[test]
    fn a_dimension_mismatch_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        write_tiny_model(dir.path());
        let mut manifest = Manifest::read(dir.path()).unwrap();
        manifest.dimension = 16;
        manifest.write(dir.path()).unwrap();
        let err = Embedder::load(dir.path()).unwrap_err();
        assert!(err.to_string().contains("dimensions"), "{err}");
    }

    #[test]
    fn chunks_by_knode_1() {
        let (_dir, embedder) = tiny();
        let markdown = format!("# Cats\n\n{}\n", "cat sat ".repeat(200));
        let chunks = embedder.chunk(&markdown);
        assert!(chunks.len() > 1);
        assert_eq!(chunks, crate::chunk(&markdown));
        for chunk in &chunks {
            assert_eq!(chunk.heading, "Cats");
        }
    }

    #[test]
    fn model_spec_comes_from_the_manifest() {
        let (_dir, embedder) = tiny();
        let spec = embedder.model_spec();
        assert_eq!(spec.name, "test");
        assert_eq!(spec.revision, "");
        assert_eq!(spec.dimensions, 8);
        assert_eq!(spec.chunking, "knode-1");
        assert_eq!(spec.query_prefix, "query: ");
        assert_eq!(spec.document_prefix, "passage: ");
        let source = ModelSpec::default_model().manifest().source;
        assert_eq!(
            source.split_once('@').unwrap().0,
            "intfloat/multilingual-e5-small"
        );
    }
}
