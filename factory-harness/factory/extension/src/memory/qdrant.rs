use std::sync::Arc;

use chrono::SecondsFormat;
use chrono::Utc;
use reqwest::Client;
use reqwest::RequestBuilder;
use reqwest::StatusCode;
use reqwest::Url;
use serde::Deserialize;
use serde_json::Map;
use serde_json::Value;
use serde_json::json;
use tokio::sync::OnceCell;
use uuid::Uuid;

use crate::memory::FactoryMemoryError;
use crate::memory::FactoryMemoryFuture;
use crate::memory::FactoryMemoryHit;
use crate::memory::FactoryMemoryRecord;
use crate::memory::FactoryMemoryStore;
use crate::memory::MemoryVector;
use crate::memory::MemoryVectorKind;
use crate::memory::MemoryVectorizer;
use crate::memory::validate_name;

#[derive(Clone, Debug)]
pub struct QdrantMemoryConfig {
    pub url: String,
    pub api_key: Option<String>,
    pub collection: String,
    pub namespace: String,
}

pub(crate) struct QdrantMemoryStore {
    client: Client,
    base_url: Url,
    api_key: Option<Arc<str>>,
    collection: Arc<str>,
    vectorizer: Arc<dyn MemoryVectorizer>,
    collection_ready: OnceCell<()>,
}

impl std::fmt::Debug for QdrantMemoryStore {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("QdrantMemoryStore")
            .field("base_url", &self.base_url)
            .field("collection", &self.collection)
            .field("vectorizer", &self.vectorizer.name())
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Deserialize)]
struct QdrantEnvelope<T> {
    result: T,
}

#[derive(Debug, Deserialize)]
struct QueryResult {
    #[serde(default)]
    points: Vec<ScoredPoint>,
}

#[derive(Debug, Deserialize)]
struct ScoredPoint {
    id: Value,
    score: f32,
    payload: Option<Value>,
}

impl QdrantMemoryStore {
    pub(crate) fn new(
        config: QdrantMemoryConfig,
        vectorizer: Arc<dyn MemoryVectorizer>,
    ) -> Result<Self, FactoryMemoryError> {
        let collection = validate_name("Qdrant memory collection", &config.collection)?;
        let mut base_url = Url::parse(&config.url).map_err(|error| {
            FactoryMemoryError::new(format!("invalid FACTORY_QDRANT_URL: {error}"))
        })?;
        if base_url.cannot_be_a_base() {
            return Err(FactoryMemoryError::new(
                "FACTORY_QDRANT_URL must be a hierarchical URL",
            ));
        }
        if !base_url.path().ends_with('/') {
            base_url.set_path(&format!("{}/", base_url.path()));
        }
        Ok(Self {
            client: Client::new(),
            base_url,
            api_key: config.api_key.map(Into::into),
            collection: collection.to_string().into(),
            vectorizer,
            collection_ready: OnceCell::new(),
        })
    }

    async fn ensure_collection(&self) -> Result<(), FactoryMemoryError> {
        self.collection_ready
            .get_or_try_init(|| async {
                let vector_name = self.vectorizer.vector_name();
                let body = match self.vectorizer.kind() {
                    MemoryVectorKind::Sparse => {
                        let mut sparse_vectors = Map::new();
                        sparse_vectors.insert(vector_name.to_string(), json!({}));
                        json!({"sparse_vectors": sparse_vectors})
                    }
                    MemoryVectorKind::Dense { size, distance } => {
                        let mut vectors = Map::new();
                        vectors.insert(
                            vector_name.to_string(),
                            json!({"size": size, "distance": distance.qdrant_name()}),
                        );
                        json!({"vectors": vectors})
                    }
                };
                let response = self
                    .request(self.client.put(self.collection_url()?))
                    .json(&body)
                    .send()
                    .await
                    .map_err(|error| request_error("create collection", error))?;
                if response.status().is_success() || response.status() == StatusCode::CONFLICT {
                    Ok(())
                } else {
                    Err(response_error("create collection", response).await)
                }
            })
            .await
            .copied()
    }

    fn request(&self, request: RequestBuilder) -> RequestBuilder {
        match &self.api_key {
            Some(api_key) => request.header("api-key", api_key.as_ref()),
            None => request,
        }
    }

    fn collection_url(&self) -> Result<Url, FactoryMemoryError> {
        self.url(&["collections", &self.collection])
    }

    fn points_url(&self) -> Result<Url, FactoryMemoryError> {
        self.url(&["collections", &self.collection, "points"])
    }

    fn query_url(&self) -> Result<Url, FactoryMemoryError> {
        self.url(&["collections", &self.collection, "points", "query"])
    }

    fn url(&self, segments: &[&str]) -> Result<Url, FactoryMemoryError> {
        let mut url = self.base_url.clone();
        {
            let mut path = url.path_segments_mut().map_err(|_| {
                FactoryMemoryError::new("FACTORY_QDRANT_URL cannot accept path segments")
            })?;
            path.pop_if_empty().extend(segments.iter().copied());
        }
        Ok(url)
    }

    fn validate_vector(&self, vector: &MemoryVector) -> Result<(), FactoryMemoryError> {
        match (self.vectorizer.kind(), vector) {
            (MemoryVectorKind::Sparse, MemoryVector::Sparse { indices, values })
                if indices.len() == values.len() =>
            {
                Ok(())
            }
            (MemoryVectorKind::Dense { size, .. }, MemoryVector::Dense(values))
                if values.len() == size =>
            {
                Ok(())
            }
            _ => Err(FactoryMemoryError::new(format!(
                "vectorizer {} returned a vector incompatible with its declared Qdrant shape",
                self.vectorizer.name()
            ))),
        }
    }

    fn vector_value(&self, vector: MemoryVector) -> Result<Value, FactoryMemoryError> {
        self.validate_vector(&vector)?;
        match vector {
            MemoryVector::Sparse { indices, values } => {
                Ok(json!({"indices": indices, "values": values}))
            }
            MemoryVector::Dense(values) => Ok(json!(values)),
        }
    }
}

impl FactoryMemoryStore for QdrantMemoryStore {
    fn remember<'a>(
        &'a self,
        namespace: &'a str,
        source_thread_id: &'a str,
        content: String,
        tags: Vec<String>,
    ) -> FactoryMemoryFuture<'a, FactoryMemoryRecord> {
        Box::pin(async move {
            self.ensure_collection().await?;
            let vector = self.vectorizer.vectorize(&content).await?;
            if vector.is_empty() {
                return Err(FactoryMemoryError::new(
                    "Factory memory content produced no lexical terms",
                ));
            }
            let vector = self.vector_value(vector)?;
            let timestamp = Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true);
            let record = FactoryMemoryRecord {
                id: Uuid::new_v4().to_string(),
                content,
                namespace: namespace.to_string(),
                tags,
                source_thread_id: source_thread_id.to_string(),
                created_at: timestamp.clone(),
                updated_at: timestamp,
                vectorizer: self.vectorizer.name().to_string(),
            };
            let mut vectors = Map::new();
            vectors.insert(self.vectorizer.vector_name().to_string(), vector);
            let mut url = self.points_url()?;
            url.query_pairs_mut().append_pair("wait", "true");
            let response = self
                .request(self.client.put(url))
                .json(&json!({
                    "points": [{
                        "id": record.id,
                        "vector": vectors,
                        "payload": record,
                    }]
                }))
                .send()
                .await
                .map_err(|error| request_error("remember", error))?;
            if response.status().is_success() {
                Ok(record)
            } else {
                Err(response_error("remember", response).await)
            }
        })
    }

    fn recall<'a>(
        &'a self,
        namespace: &'a str,
        query: &'a str,
        limit: usize,
    ) -> FactoryMemoryFuture<'a, Vec<FactoryMemoryHit>> {
        Box::pin(async move {
            self.ensure_collection().await?;
            let vector = self.vectorizer.vectorize(query).await?;
            if vector.is_empty() {
                return Ok(Vec::new());
            }
            let query = self.vector_value(vector)?;
            let response = self
                .request(self.client.post(self.query_url()?))
                .json(&json!({
                    "query": query,
                    "using": self.vectorizer.vector_name(),
                    "filter": {
                        "must": [{"key": "namespace", "match": {"value": namespace}}]
                    },
                    "limit": limit,
                    "with_payload": true,
                    "with_vector": false,
                }))
                .send()
                .await
                .map_err(|error| request_error("recall", error))?;
            if !response.status().is_success() {
                return Err(response_error("recall", response).await);
            }
            let points = response
                .json::<QdrantEnvelope<QueryResult>>()
                .await
                .map_err(|error| request_error("decode recall response", error))?
                .result
                .points;
            points
                .into_iter()
                .map(|point| {
                    let payload = point.payload.ok_or_else(|| {
                        FactoryMemoryError::new("Qdrant memory point is missing its payload")
                    })?;
                    let memory: FactoryMemoryRecord =
                        serde_json::from_value(payload).map_err(|error| {
                            FactoryMemoryError::new(format!(
                                "failed to decode Qdrant memory payload: {error}"
                            ))
                        })?;
                    if point.id != Value::String(memory.id.clone()) {
                        return Err(FactoryMemoryError::new(
                            "Qdrant memory point ID does not match its payload",
                        ));
                    }
                    Ok(FactoryMemoryHit {
                        memory,
                        score: point.score,
                    })
                })
                .collect()
        })
    }
}

fn request_error(operation: &str, error: reqwest::Error) -> FactoryMemoryError {
    FactoryMemoryError::new(format!("Qdrant {operation} failed: {error}"))
}

async fn response_error(operation: &str, response: reqwest::Response) -> FactoryMemoryError {
    let status = response.status();
    let body = response
        .text()
        .await
        .unwrap_or_else(|error| format!("failed to read error response: {error}"));
    FactoryMemoryError::new(format!("Qdrant {operation} returned HTTP {status}: {body}"))
}
