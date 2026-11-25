// SPDX-License-Identifier: MIT OR Apache-2.0
//! The provider's embedding model, loaded only from files an operator put
//! in a directory, and only when every file has the pinned SHA-256. Nothing
//! is ever downloaded: a missing or different file refuses semantic search.
//!
//! Model: BAAI/bge-small-en-v1.5 as exported to ONNX by Xenova, revision
//! `ea104dacec62c0de699686887e3f920caeb4f3e3` of
//! `https://huggingface.co/Xenova/bge-small-en-v1.5` (the same artifact the
//! daemon's downloader fetches for `EmbeddingProfile::bge_small_en_v15`).

use std::path::Path;

use fastembed::{
    InitOptionsUserDefined, Pooling, TextEmbedding, TokenizerFiles, UserDefinedEmbeddingModel,
};

#[cfg(test)]
use super::ProviderErrorCode;
use super::{ProviderError, ProviderResult, hex_sha256};
use crate::error::{Error, Result};
use crate::semantic::{Embedder, EmbeddingProfile};

/// Upstream revision the hashes below were taken from.
pub const PINNED_MODEL_REVISION: &str = "ea104dacec62c0de699686887e3f920caeb4f3e3";

/// `(file name in the model directory, upstream path, SHA-256)`.
pub const PINNED_MODEL_FILES: [(&str, &str, &str); 5] = [
    (
        "model.onnx",
        "onnx/model.onnx",
        "828e1496d7fabb79cfa4dcd84fa38625c0d3d21da474a00f08db0f559940cf35",
    ),
    (
        "tokenizer.json",
        "tokenizer.json",
        "d241a60d5e8f04cc1b2b3e9ef7a4921b27bf526d9f6050ab90f9267a1f9e5c66",
    ),
    (
        "config.json",
        "config.json",
        "fa73f90bf92c8cace1fbcb709626306f2bdbc9ea3e5b5f94b440df9b6aa56350",
    ),
    (
        "special_tokens_map.json",
        "special_tokens_map.json",
        "b6d346be366a7d1d48332dbc9fdf3bf8960b5d879522b7799ddba59e76237ee3",
    ),
    (
        "tokenizer_config.json",
        "tokenizer_config.json",
        "9261e7d79b44c8195c1cada2b453e55b00aeb81e907a6664974b4d7776172ab3",
    ),
];

/// Passages embedded per ONNX run (the daemon's figure: RSS grows with it).
const BATCH: usize = 8;

struct PinnedEmbedder {
    profile: EmbeddingProfile,
    model: TextEmbedding,
}

impl Embedder for PinnedEmbedder {
    fn profile(&self) -> &EmbeddingProfile {
        &self.profile
    }

    fn embed_passages(&mut self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        let refs: Vec<&str> = texts.iter().map(String::as_str).collect();
        self.model
            .embed(refs, Some(BATCH))
            .map_err(|error| Error::Semantic(format!("embedding failed: {error}")))
    }

    fn embed_query(&mut self, text: &str) -> Result<Vec<f32>> {
        let prompted = format!("{}{}", self.profile.query_prefix, text);
        self.model
            .embed(vec![prompted.as_str()], Some(1))
            .map_err(|error| Error::Semantic(format!("query embedding failed: {error}")))?
            .pop()
            .ok_or_else(|| Error::Semantic("query embedding returned no vector".to_owned()))
    }
}

/// Reads and checks every pinned file in `dir`. Refuses (never fetches) when
/// a file is missing, unreadable, or not byte-identical to the pinned one.
fn read_pinned(dir: &Path) -> ProviderResult<Vec<Vec<u8>>> {
    PINNED_MODEL_FILES
        .iter()
        .map(|(name, _, expected)| {
            let path = dir.join(name);
            let bytes = std::fs::read(&path).map_err(|error| {
                ProviderError::bad(format!(
                    "model file {} is missing or unreadable ({error}); semantic search needs the pinned files and never downloads them",
                    path.display()
                ))
            })?;
            if hex_sha256(&bytes) != *expected {
                return Err(ProviderError::bad(format!(
                    "model file {} is not the pinned file (SHA-256 differs)",
                    path.display()
                )));
            }
            Ok(bytes)
        })
        .collect()
}

/// Loads the pinned model from `dir` with `threads` ONNX intra-op threads
/// (`None`: every available CPU).
pub fn load_pinned_embedder(
    dir: &Path,
    threads: Option<usize>,
) -> ProviderResult<Box<dyn Embedder>> {
    let mut files = read_pinned(dir)?.into_iter();
    let mut next = || files.next().unwrap_or_default();
    let onnx = next();
    let tokenizer_files = TokenizerFiles {
        tokenizer_file: next(),
        config_file: next(),
        special_tokens_map_file: next(),
        tokenizer_config_file: next(),
    };
    let model = UserDefinedEmbeddingModel::new(onnx, tokenizer_files).with_pooling(Pooling::Cls);
    let mut profile = EmbeddingProfile::bge_small_en_v15();
    let mut options = InitOptionsUserDefined::new().with_max_length(profile.max_tokens);
    if let Some(threads) = threads {
        options = options.with_intra_threads(threads.max(1));
    }
    let model = TextEmbedding::try_new_from_user_defined(model, options).map_err(|error| {
        ProviderError::internal(format!("embedding model init failed: {error}"))
    })?;
    profile.artifact = format!(
        "Xenova/bge-small-en-v1.5@{PINNED_MODEL_REVISION} onnx/model.onnx (pinned, local files)"
    );
    Ok(Box::new(PinnedEmbedder { profile, model }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_or_different_model_file_is_refused_without_fetching() {
        let dir = tempfile::tempdir().unwrap();
        let error = load_pinned_embedder(dir.path(), Some(1)).err().unwrap();
        assert_eq!(error.code, ProviderErrorCode::BadRequest);
        assert!(error.detail.contains("never downloads"), "{}", error.detail);
        std::fs::write(dir.path().join("model.onnx"), b"not the model").unwrap();
        let error = load_pinned_embedder(dir.path(), Some(1)).err().unwrap();
        assert!(
            error.detail.contains("not the pinned file"),
            "{}",
            error.detail
        );
    }
}
