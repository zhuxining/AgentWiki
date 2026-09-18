//! FastEmbed adapter boundary.
//!
//! Model construction lives here; retrieval and synchronization must not
//! import FastEmbed types directly.

use fastembed::{Embedding, EmbeddingModel, TextEmbedding, TextInitOptions};

/// Local embedding adapter backed by FastEmbed's ONNX runtime.
pub struct Embedder(TextEmbedding);

impl Embedder {
    /// Load the first supported Chinese model. Model files are cached locally.
    pub fn bge_small_zh() -> Result<Self, String> {
        TextEmbedding::try_new(TextInitOptions::new(EmbeddingModel::BGESmallZHV15))
            .map(Self)
            .map_err(|e| e.to_string())
    }

    /// Embed a batch of prepared chunk inputs.
    pub fn embed(&mut self, inputs: Vec<String>) -> Result<Vec<Embedding>, String> {
        self.0.embed(inputs, None).map_err(|e| e.to_string())
    }
}
