//! A BERT encoder whose table of word embeddings stays in the model file.
//!
//! The word table of a multilingual model is most of its weights: 96 of the 118 million numbers
//! of multilingual-e5-small, 384 MB. [`candle_transformers`]' `BertModel` copies it into memory
//! (or onto the GPU). This one reads the rows of the tokens it sees straight from the
//! memory-mapped file instead, so the table costs only the pages of the tokens in use, and the
//! system can drop those pages under pressure since they are never written. The rest of the
//! model is `candle_transformers`' own encoder, so the vectors are the same.

use std::path::Path;

use candle_core::safetensors::MmapedSafetensors;
use candle_core::{DType, Device, Module, Tensor};
use candle_nn::{LayerNorm, VarBuilder};
use candle_transformers::models::bert::{BertEncoder, Config};

pub(crate) struct Bert {
    file: MmapedSafetensors,
    /// The name of the word table in the file.
    words: String,
    vocab_size: usize,
    hidden_size: usize,
    /// Position embeddings plus the embedding of token type 0, for the first positions.
    positions: Tensor,
    layer_norm: LayerNorm,
    encoder: BertEncoder,
    device: Device,
}

impl Bert {
    /// Loads the model in `weights` (a safetensors file) for `config`. The tensors are named
    /// `embeddings.*` and `encoder.*`, or the same under the model type (`bert.embeddings.*`).
    pub(crate) fn load(
        weights: &Path,
        config: &Config,
        device: &Device,
    ) -> candle_core::Result<Bert> {
        // SAFETY: the file is mapped read-only; Knode never writes into a model folder once its
        // manifest exists.
        let file = unsafe { MmapedSafetensors::new(weights)? };
        let vb = unsafe { VarBuilder::from_mmaped_safetensors(&[weights], DType::F32, device)? };
        let prefix = match &config.model_type {
            Some(model_type) if file.get("embeddings.word_embeddings.weight").is_err() => {
                format!("{model_type}.")
            }
            _ => String::new(),
        };
        let vb = match prefix.strip_suffix('.') {
            Some(model_type) => vb.pp(model_type),
            None => vb,
        };

        let words = format!("{prefix}embeddings.word_embeddings.weight");
        let view = file.get(&words)?;
        if DType::try_from(view.dtype())? != DType::F32
            || view.shape() != [config.vocab_size, config.hidden_size]
        {
            candle_core::bail!(
                "{words} must be f32 of shape [{}, {}]",
                config.vocab_size,
                config.hidden_size
            );
        }

        let embeddings = vb.pp("embeddings");
        let positions = embeddings
            .get(
                (config.max_position_embeddings, config.hidden_size),
                "position_embeddings.weight",
            )?
            .broadcast_add(
                &embeddings
                    .get(
                        (config.type_vocab_size, config.hidden_size),
                        "token_type_embeddings.weight",
                    )?
                    .narrow(0, 0, 1)?,
            )?;
        let layer_norm = candle_nn::layer_norm(
            config.hidden_size,
            config.layer_norm_eps,
            embeddings.pp("LayerNorm"),
        )?;
        let encoder = BertEncoder::load(vb.pp("encoder"), config)?;
        Ok(Bert {
            file,
            words,
            vocab_size: config.vocab_size,
            hidden_size: config.hidden_size,
            positions,
            layer_norm,
            encoder,
            device: device.clone(),
        })
    }

    /// The last hidden state, `(batch, len, hidden)`, for token `ids` and an attention `mask`
    /// (1 for a token, 0 for padding), both `batch * len` long, row after row.
    pub(crate) fn forward(
        &self,
        ids: &[u32],
        mask: &[u32],
        batch: usize,
        len: usize,
    ) -> candle_core::Result<Tensor> {
        let table = self.file.get(&self.words)?;
        let table = table.data();
        let row_bytes = self.hidden_size * 4;
        let mut rows = Vec::with_capacity(ids.len() * self.hidden_size);
        for &id in ids {
            let id = id as usize;
            if id >= self.vocab_size {
                candle_core::bail!("token {id} is outside the vocabulary");
            }
            let row = &table[id * row_bytes..(id + 1) * row_bytes];
            rows.extend(
                row.chunks_exact(4)
                    .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]])),
            );
        }
        let words = Tensor::from_vec(rows, (batch, len, self.hidden_size), &self.device)?;
        // Token types are all 0, whose embedding is already in `positions`.
        let embeddings = words.broadcast_add(&self.positions.narrow(0, 0, len)?)?;
        let embeddings = self.layer_norm.forward(&embeddings)?;

        // As in `BertModel`: 0 where a token is attended to, the lowest f32 where it is not.
        let bias: Vec<f32> = mask
            .iter()
            .map(|&m| if m == 0 { f32::MIN } else { 0.0 })
            .collect();
        let bias = Tensor::from_vec(bias, (batch, 1, 1, len), &self.device)?;
        self.encoder.forward(&embeddings, &bias)
    }
}
