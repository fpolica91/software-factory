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

/// Stable durable identity for the repository that owns a memory.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct FactoryRepositoryId(Arc<str>);

impl FactoryRepositoryId {
    pub fn new(value: impl Into<String>) -> Result<Self, FactoryMemoryError> {
        let value = value.into();
        let value = validate_name("Factory repository identity", &value)?;
        Ok(Self(value.into()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Complete isolation boundary used for every memory store operation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FactoryMemoryScope {
    namespace: Arc<str>,
    repository_id: FactoryRepositoryId,
}

impl FactoryMemoryScope {
    pub(crate) fn new(namespace: Arc<str>, repository_id: FactoryRepositoryId) -> Self {
        Self {
            namespace,
            repository_id,
        }
    }

    pub fn namespace(&self) -> &str {
        &self.namespace
    }

    pub fn repository_id(&self) -> &str {
        self.repository_id.as_str()
    }

    fn owns(&self, memory: &FactoryMemoryRecord) -> bool {
        memory.namespace == self.namespace() && memory.repository_id == self.repository_id()
    }
}

/// Durable memory payload stored in Qdrant.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct FactoryMemoryRecord {
    pub id: String,
    pub content: String,
    pub namespace: String,
    pub repository_id: String,
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
        scope: &'a FactoryMemoryScope,
        source_thread_id: &'a str,
        content: String,
        tags: Vec<String>,
    ) -> FactoryMemoryFuture<'a, FactoryMemoryRecord>;

    fn recall<'a>(
        &'a self,
        scope: &'a FactoryMemoryScope,
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

/// One repository-scoped view shared by tools and automatic recall.
#[derive(Clone)]
pub(crate) struct RepositoryScopedMemory {
    store: Arc<dyn FactoryMemoryStore>,
    scope: FactoryMemoryScope,
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
        Self::qdrant_with_vectorizer(config, Arc::new(LexicalSparseVectorizer))
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

    pub(crate) fn for_repository(
        &self,
        repository_id: FactoryRepositoryId,
    ) -> RepositoryScopedMemory {
        RepositoryScopedMemory {
            store: Arc::clone(&self.store),
            scope: FactoryMemoryScope::new(Arc::clone(&self.namespace), repository_id),
        }
    }
}

impl RepositoryScopedMemory {
    pub(crate) fn scope(&self) -> &FactoryMemoryScope {
        &self.scope
    }

    pub(crate) async fn remember(
        &self,
        source_thread_id: &str,
        content: String,
        tags: Vec<String>,
    ) -> Result<FactoryMemoryRecord, FactoryMemoryError> {
        let memory = self
            .store
            .remember(&self.scope, source_thread_id, content, tags)
            .await?;
        if !self.scope.owns(&memory) {
            return Err(FactoryMemoryError::new(
                "Factory memory store returned a record outside its repository scope",
            ));
        }
        Ok(memory)
    }

    pub(crate) async fn recall(
        &self,
        query: &str,
        limit: usize,
    ) -> Result<Vec<FactoryMemoryHit>, FactoryMemoryError> {
        let mut memories = self.store.recall(&self.scope, query, limit).await?;
        memories.retain(|memory| self.scope.owns(&memory.memory));
        memories.truncate(limit);
        Ok(memories)
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
