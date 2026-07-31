use std::collections::BTreeMap;
use std::collections::HashMap;

use serde::Deserialize;
use serde::Serialize;

use crate::memory::FactoryMemoryFuture;

/// Qdrant vector shape required by a memory vectorizer.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MemoryVectorKind {
    Sparse,
    Dense {
        size: usize,
        distance: DenseDistance,
    },
}

/// Qdrant dense distance for future optional embedding implementations.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DenseDistance {
    Cosine,
    Dot,
    Euclid,
    Manhattan,
}

impl DenseDistance {
    pub(crate) fn qdrant_name(self) -> &'static str {
        match self {
            Self::Cosine => "Cosine",
            Self::Dot => "Dot",
            Self::Euclid => "Euclid",
            Self::Manhattan => "Manhattan",
        }
    }
}

/// Vector returned by a local lexical or optional external embedding provider.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryVector {
    Sparse { indices: Vec<u32>, values: Vec<f32> },
    Dense(Vec<f32>),
}

impl MemoryVector {
    pub fn is_empty(&self) -> bool {
        match self {
            Self::Sparse { indices, .. } => indices.is_empty(),
            Self::Dense(values) => values.is_empty(),
        }
    }
}

/// Text-to-vector boundary. It is async so an optional Ollama-backed dense
/// implementation can be added without changing tools, recall, or Qdrant.
pub trait MemoryVectorizer: Send + Sync {
    fn name(&self) -> &'static str;

    fn vector_name(&self) -> &'static str;

    fn kind(&self) -> MemoryVectorKind;

    fn vectorize<'a>(&'a self, text: &'a str) -> FactoryMemoryFuture<'a, MemoryVector>;
}

/// Deterministic dependency-free lexical baseline using term-frequency weights
/// on stable FNV-1a token dimensions.
#[derive(Clone, Copy, Debug, Default)]
pub struct LexicalSparseVectorizer;

impl MemoryVectorizer for LexicalSparseVectorizer {
    fn name(&self) -> &'static str {
        "factory-lexical-fnv1a-v1"
    }

    fn vector_name(&self) -> &'static str {
        "factory_lexical"
    }

    fn kind(&self) -> MemoryVectorKind {
        MemoryVectorKind::Sparse
    }

    fn vectorize<'a>(&'a self, text: &'a str) -> FactoryMemoryFuture<'a, MemoryVector> {
        Box::pin(async move {
            let mut token_counts = HashMap::<String, u32>::new();
            for token in lexical_tokens(text) {
                *token_counts.entry(token).or_default() += 1;
            }
            let mut dimensions = BTreeMap::<u32, f32>::new();
            for (token, count) in token_counts {
                let weight = 1.0 + (count as f32).ln();
                *dimensions.entry(fnv1a(&token)).or_default() += weight;
            }
            let (indices, values) = dimensions.into_iter().unzip();
            Ok(MemoryVector::Sparse { indices, values })
        })
    }
}

fn lexical_tokens(text: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut token = String::new();
    for character in text.chars() {
        if character.is_alphanumeric() {
            token.extend(character.to_lowercase());
        } else if !token.is_empty() {
            tokens.push(std::mem::take(&mut token));
        }
    }
    if !token.is_empty() {
        tokens.push(token);
    }
    tokens
}

fn fnv1a(token: &str) -> u32 {
    token
        .as_bytes()
        .iter()
        .fold(2_166_136_261_u32, |hash, byte| {
            (hash ^ u32::from(*byte)).wrapping_mul(16_777_619)
        })
}
