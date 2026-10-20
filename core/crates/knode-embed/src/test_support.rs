//! A tiny BERT model with random weights and a word-level tokenizer, written on the fly so tests
//! never download anything.

use std::collections::HashMap;
use std::path::Path;

use candle_core::{DType, Device};
use candle_nn::{VarBuilder, VarMap};
use candle_transformers::models::bert::{BertModel, Config};
use tokenizers::Tokenizer;
use tokenizers::models::wordlevel::WordLevel;
use tokenizers::pre_tokenizers::whitespace::Whitespace;
use tokenizers::processors::template::TemplateProcessing;

use crate::model::{Manifest, Pooling};

pub(crate) fn write_tiny_model(dir: &Path) {
    let words = [
        "[PAD]", "[UNK]", "[CLS]", "[SEP]", "the", "a", "cat", "dog", "sat", "ran", "far", "away",
        "from", "query", "passage", ":",
    ];
    let config = Config {
        vocab_size: words.len(),
        hidden_size: 8,
        num_hidden_layers: 1,
        num_attention_heads: 2,
        intermediate_size: 16,
        max_position_embeddings: 32,
        ..Config::default()
    };
    std::fs::write(
        dir.join("config.json"),
        format!(
            r#"{{"vocab_size":{},"hidden_size":8,"num_hidden_layers":1,"num_attention_heads":2,
"intermediate_size":16,"hidden_act":"gelu","hidden_dropout_prob":0.0,
"max_position_embeddings":32,"type_vocab_size":2,"initializer_range":0.02,
"layer_norm_eps":1e-12,"pad_token_id":0,"classifier_dropout":null,"model_type":"bert"}}"#,
            words.len()
        ),
    )
    .unwrap();

    let varmap = VarMap::new();
    let vb = VarBuilder::from_varmap(&varmap, DType::F32, &Device::Cpu);
    BertModel::load(vb, &config).unwrap();
    varmap.save(dir.join("model.safetensors")).unwrap();

    let vocab: HashMap<String, u32> = words
        .iter()
        .enumerate()
        .map(|(i, w)| (w.to_string(), i as u32))
        .collect();
    let model = WordLevel::builder()
        .vocab(vocab.into_iter().collect())
        .unk_token("[UNK]".into())
        .build()
        .unwrap();
    let mut tokenizer = Tokenizer::new(model);
    tokenizer.with_pre_tokenizer(Some(Whitespace {}));
    tokenizer.with_post_processor(Some(
        TemplateProcessing::builder()
            .try_single("[CLS] $A [SEP]")
            .unwrap()
            .special_tokens(vec![("[CLS]", 2), ("[SEP]", 3)])
            .build()
            .unwrap(),
    ));
    tokenizer.save(dir.join("tokenizer.json"), false).unwrap();

    Manifest {
        id: "tiny-test".into(),
        source: "test".into(),
        dimension: 8,
        max_tokens: 32,
        query_prefix: "query: ".into(),
        passage_prefix: "passage: ".into(),
        pooling: Pooling::Mean,
    }
    .write(dir)
    .unwrap();
}
