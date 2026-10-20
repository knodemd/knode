use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::{Error, Result};

/// The file Knode writes in a model folder once every other file is in place and verified. Its
/// presence means the model is complete.
pub const MANIFEST_FILE: &str = "knode-model.json";

/// How token vectors become one vector per text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Pooling {
    /// The mean of the token vectors, ignoring padding.
    Mean,
    /// The vector of the first token (`[CLS]`).
    Cls,
}

/// One file of a model, as published by its source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelFile {
    /// The path inside the source repository, which is also its name in the model folder.
    pub name: String,
    /// Its size in bytes.
    pub size: u64,
    /// Its SHA-256, in lowercase hex.
    pub sha256: String,
}

/// A model Knode can download and run: where it comes from, the files it needs and how to use it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelSpec {
    /// A short, stable id, also the folder name under `models/`, and what a space records next to
    /// each embedding.
    pub id: String,
    /// The Hugging Face repository it comes from.
    pub repo: String,
    /// The exact commit of that repository, so the files never change under Knode.
    pub revision: String,
    /// The files to download: `config.json`, `tokenizer.json` and `model.safetensors`.
    pub files: Vec<ModelFile>,
    /// The size of each vector.
    pub dimension: usize,
    /// The most tokens the model reads from one text; longer texts are cut.
    pub max_tokens: usize,
    /// Text put before a search query.
    pub query_prefix: String,
    /// Text put before a passage being indexed.
    pub passage_prefix: String,
    /// How token vectors are pooled.
    pub pooling: Pooling,
}

impl ModelSpec {
    /// The model Knode uses by default: `intfloat/multilingual-e5-small`.
    ///
    /// MIT licensed, 118 million parameters (471 MB in f32), 384 dimensions, trained for retrieval
    /// in about 100 languages, reads up to 512 tokens. Queries and passages carry the `query: `
    /// and `passage: ` prefixes it was trained with.
    pub fn default_model() -> ModelSpec {
        ModelSpec {
            id: "multilingual-e5-small".into(),
            repo: "intfloat/multilingual-e5-small".into(),
            revision: "614241f622f53c4eeff9890bdc4f31cfecc418b3".into(),
            files: vec![
                ModelFile {
                    name: "config.json".into(),
                    size: 655,
                    sha256: "69137736cab8b8903a07fe8afaafdda25aac55415a12a55d1bffa9f581abf959"
                        .into(),
                },
                ModelFile {
                    name: "tokenizer.json".into(),
                    size: 17_082_730,
                    sha256: "0b44a9d7b51c3c62626640cda0e2c2f70fdacdc25bbbd68038369d14ebdf4c39"
                        .into(),
                },
                ModelFile {
                    name: "model.safetensors".into(),
                    size: 470_641_600,
                    sha256: "1a55775f53449dac10a2bcbc312469fac40b96d53198c407081a831f81c98477"
                        .into(),
                },
            ],
            dimension: 384,
            max_tokens: 512,
            query_prefix: "query: ".into(),
            passage_prefix: "passage: ".into(),
            pooling: Pooling::Mean,
        }
    }

    /// The manifest written into the model folder once it is complete.
    pub fn manifest(&self) -> Manifest {
        Manifest {
            id: self.id.clone(),
            source: format!("{}@{}", self.repo, self.revision),
            dimension: self.dimension,
            max_tokens: self.max_tokens,
            query_prefix: self.query_prefix.clone(),
            passage_prefix: self.passage_prefix.clone(),
            pooling: self.pooling,
        }
    }

    /// The total size of the files to download, in bytes.
    pub fn download_size(&self) -> u64 {
        self.files.iter().map(|file| file.size).sum()
    }
}

/// What Knode needs to know to run a model folder, kept in [`MANIFEST_FILE`].
///
/// The folder also holds `config.json` (a BERT configuration), `tokenizer.json` and
/// `model.safetensors`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    /// The model id, as in [`ModelSpec::id`].
    pub id: String,
    /// Where the files came from, `repo@revision`.
    pub source: String,
    /// The size of each vector.
    pub dimension: usize,
    /// The most tokens read from one text.
    pub max_tokens: usize,
    /// Text put before a search query.
    #[serde(default)]
    pub query_prefix: String,
    /// Text put before a passage being indexed.
    #[serde(default)]
    pub passage_prefix: String,
    /// How token vectors are pooled.
    pub pooling: Pooling,
}

impl Manifest {
    /// Reads the manifest of a model folder.
    pub fn read(model_dir: &Path) -> Result<Manifest> {
        let path = model_dir.join(MANIFEST_FILE);
        let text = std::fs::read_to_string(&path).map_err(|err| {
            if err.kind() == std::io::ErrorKind::NotFound {
                Error::invalid(model_dir, format!("{MANIFEST_FILE} is missing"))
            } else {
                Error::io(&path, err)
            }
        })?;
        serde_json::from_str(&text)
            .map_err(|err| Error::invalid(model_dir, format!("{MANIFEST_FILE}: {err}")))
    }

    /// Writes the manifest into a model folder, replacing it atomically.
    pub fn write(&self, model_dir: &Path) -> Result<()> {
        let path = model_dir.join(MANIFEST_FILE);
        let tmp = model_dir.join(format!("{MANIFEST_FILE}.{}.part", std::process::id()));
        let text = serde_json::to_string_pretty(self).expect("a manifest always serializes");
        std::fs::write(&tmp, text + "\n").map_err(|err| Error::io(&tmp, err))?;
        std::fs::rename(&tmp, &path).map_err(|err| Error::io(&path, err))
    }
}

/// The folder where Knode keeps its models: `$KNODE_HOME/models`.
pub fn models_dir() -> Result<PathBuf> {
    Ok(knode_config::Paths::from_env()?.home.join("models"))
}
