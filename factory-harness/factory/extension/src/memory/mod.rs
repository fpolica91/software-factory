mod extension;
mod qdrant;
mod vectorizer;

use std::error::Error;
use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use serde::Deserialize;
use serde::Serialize;

pub use qdrant::QdrantMemoryConfig;
use qdrant::QdrantMemoryStore;
pub use vectorizer::DenseDistance;
pub use vectorizer::LexicalSparseVectorizer;
pub use vectorizer::MemoryVector;
pub use vectorizer::MemoryVectorKind;
pub use vectorizer::MemoryVectorizer;

pub(crate) use extension::install_memory;

/// Failure returned by the optional Factory memory subsystem.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FactoryMemoryError {
    message: String,
}

impl FactoryMemoryError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for FactoryMemoryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl Error for FactoryMemoryError {}

pub type FactoryMemoryFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, FactoryMemoryError>> + Send + 'a>>;

/// Durable memory payload stored in Qdrant.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct FactoryMemoryRecord {
    pub id: String,
    pub content: String,
    pub namespace: String,
    pub tags: Vec<String>,
    pub source_thread_id: String,
    pub created_at: String,
    pub updated_at: String,
    pub vectorizer: String,
}

/// One ranked memory result.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct FactoryMemoryHit {
    #[serde(flatten)]
    pub memory: FactoryMemoryRecord,
    pub score: f32,
}

/// Storage boundary consumed by native tools and automatic turn recall.
pub trait FactoryMemoryStore: Send + Sync {
    fn remember<'a>(
        &'a self,
        namespace: &'a str,
        source_thread_id: &'a str,
        content: String,
        tags: Vec<String>,
    ) -> FactoryMemoryFuture<'a, FactoryMemoryRecord>;

    fn recall<'a>(
        &'a self,
        namespace: &'a str,
        query: &'a str,
        limit: usize,
    ) -> FactoryMemoryFuture<'a, Vec<FactoryMemoryHit>>;
}

/// Installed memory capability shared by its native tools and input contributor.
#[derive(Clone)]
pub struct FactoryMemory {
    store: Arc<dyn FactoryMemoryStore>,
    namespace: Arc<str>,
}

impl fmt::Debug for FactoryMemory {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FactoryMemory")
            .field("namespace", &self.namespace)
            .finish_non_exhaustive()
    }
}

impl FactoryMemory {
    /// Creates the Qdrant baseline with deterministic lexical sparse vectors.
    pub fn qdrant(config: QdrantMemoryConfig) -> Result<Self, FactoryMemoryError> {
        Self::qdrant_with_vectorizer(config, Arc::new(LexicalSparseVectorizer::default()))
    }

    /// Creates Qdrant memory with a host-provided vectorizer. A later optional
    /// dense provider such as Ollama can implement [`MemoryVectorizer`] here.
    pub fn qdrant_with_vectorizer(
        config: QdrantMemoryConfig,
        vectorizer: Arc<dyn MemoryVectorizer>,
    ) -> Result<Self, FactoryMemoryError> {
        let namespace = validate_name("Factory memory namespace", &config.namespace)?.to_string();
        let store = Arc::new(QdrantMemoryStore::new(config, vectorizer)?);
        Ok(Self {
            store,
            namespace: namespace.into(),
        })
    }

    /// Creates a memory capability over another store implementation.
    pub fn with_store(
        namespace: impl Into<String>,
        store: Arc<dyn FactoryMemoryStore>,
    ) -> Result<Self, FactoryMemoryError> {
        let namespace = namespace.into();
        let namespace = validate_name("Factory memory namespace", &namespace)?;
        Ok(Self {
            store,
            namespace: namespace.into(),
        })
    }

    pub fn namespace(&self) -> &str {
        &self.namespace
    }

    pub(crate) fn store(&self) -> Arc<dyn FactoryMemoryStore> {
        Arc::clone(&self.store)
    }
}

fn validate_name<'a>(label: &str, value: &'a str) -> Result<&'a str, FactoryMemoryError> {
    let value = value.trim();
    if value.is_empty() {
        Err(FactoryMemoryError::new(format!(
            "{label} must not be empty"
        )))
    } else {
        Ok(value)
    }
}
