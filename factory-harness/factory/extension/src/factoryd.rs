use std::collections::HashMap;

use reqwest::Client;
use reqwest::StatusCode;
use reqwest::Url;
use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;

use crate::FactoryBackendError;
use crate::FactoryBackendFuture;
use crate::FactoryProgressStatus;
use crate::FactoryRemediationRecord;
use crate::FactoryReviewReport;
use crate::FactoryState;
use crate::FactoryStateBackend;
use crate::FactoryStateDurability;
use crate::FactorySubagentActivity;
use crate::FactoryWorkUnit;

/// HTTP backend for factoryd's durable thread-state document.
#[derive(Clone, Debug)]
pub struct FactorydStateBackend {
    client: Client,
    base_url: Url,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FactorydThreadStateRecord {
    state: FactorydThreadStateDocument,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct FactorydThreadStateDocument {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    decomposition: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    progress: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    review: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    remediation: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    subagents: Option<Value>,
}

#[derive(Debug, Deserialize, Serialize)]
struct DecompositionDocument {
    revision: u64,
    work_units: Vec<WorkUnitDefinition>,
}

#[derive(Debug, Deserialize, Serialize)]
struct WorkUnitDefinition {
    id: String,
    title: String,
    description: String,
    depends_on: Vec<String>,
}

#[derive(Debug, Deserialize, Serialize)]
struct ProgressDocument {
    work_units: Vec<WorkUnitProgress>,
}

#[derive(Debug, Deserialize, Serialize)]
struct WorkUnitProgress {
    id: String,
    status: FactoryProgressStatus,
    progress_summary: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
struct RemediationDocument {
    records: Vec<FactoryRemediationRecord>,
}

#[derive(Debug, Deserialize, Serialize)]
struct SubagentsDocument {
    activities: Vec<FactorySubagentActivity>,
}

impl FactorydStateBackend {
    /// Creates a backend rooted at the factoryd API base, normally
    /// `http://127.0.0.1:8787/v1`.
    pub fn new(base_url: impl AsRef<str>) -> Result<Self, FactoryBackendError> {
        let mut base_url = Url::parse(base_url.as_ref())
            .map_err(|error| FactoryBackendError::new(format!("invalid FACTORYD_URL: {error}")))?;
        if base_url.cannot_be_a_base() {
            return Err(FactoryBackendError::new(
                "FACTORYD_URL must be a hierarchical URL",
            ));
        }
        if !base_url.path().ends_with('/') {
            let path = format!("{}/", base_url.path());
            base_url.set_path(&path);
        }
        Ok(Self {
            client: Client::new(),
            base_url,
        })
    }

    fn state_url(&self, thread_id: &str) -> Result<Url, FactoryBackendError> {
        let mut url = self.base_url.clone();
        {
            let mut segments = url.path_segments_mut().map_err(|_| {
                FactoryBackendError::new("FACTORYD_URL cannot accept path segments")
            })?;
            segments
                .pop_if_empty()
                .extend(["threads", thread_id, "state"]);
        }
        Ok(url)
    }
}

impl FactoryStateBackend for FactorydStateBackend {
    fn load<'a>(&'a self, thread_id: &'a str) -> FactoryBackendFuture<'a, Option<FactoryState>> {
        Box::pin(async move {
            let response = self
                .client
                .get(self.state_url(thread_id)?)
                .send()
                .await
                .map_err(|error| request_error("load", error))?;
            if response.status() == StatusCode::NOT_FOUND {
                return Ok(None);
            }
            if !response.status().is_success() {
                return Err(response_error("load", response).await);
            }
            let record = response
                .json::<FactorydThreadStateRecord>()
                .await
                .map_err(|error| request_error("decode load response", error))?;
            decode_document(record.state).map(Some)
        })
    }

    fn save<'a>(&'a self, thread_id: &'a str, state: FactoryState) -> FactoryBackendFuture<'a, ()> {
        Box::pin(async move {
            let document = encode_document(&state)?;
            let response = self
                .client
                .put(self.state_url(thread_id)?)
                .json(&document)
                .send()
                .await
                .map_err(|error| request_error("save", error))?;
            if !response.status().is_success() {
                return Err(response_error("save", response).await);
            }
            let record = response
                .json::<FactorydThreadStateRecord>()
                .await
                .map_err(|error| request_error("decode save response", error))?;
            let stored = decode_document(record.state)?;
            if stored != state {
                return Err(FactoryBackendError::new(
                    "factoryd save response did not preserve the exact Factory state",
                ));
            }
            Ok(())
        })
    }

    fn durability(&self) -> FactoryStateDurability {
        FactoryStateDurability::Durable
    }
}

fn encode_document(
    state: &FactoryState,
) -> Result<FactorydThreadStateDocument, FactoryBackendError> {
    let decomposition = DecompositionDocument {
        revision: state.revision,
        work_units: state
            .work_units
            .iter()
            .map(|unit| WorkUnitDefinition {
                id: unit.id.clone(),
                title: unit.title.clone(),
                description: unit.description.clone(),
                depends_on: unit.depends_on.clone(),
            })
            .collect(),
    };
    let progress = ProgressDocument {
        work_units: state
            .work_units
            .iter()
            .map(|unit| WorkUnitProgress {
                id: unit.id.clone(),
                status: unit.status,
                progress_summary: unit.progress_summary.clone(),
            })
            .collect(),
    };
    Ok(FactorydThreadStateDocument {
        decomposition: Some(to_value("decomposition", decomposition)?),
        progress: Some(to_value("progress", progress)?),
        review: state
            .review
            .as_ref()
            .map(|review| to_value("review", review))
            .transpose()?,
        remediation: Some(to_value(
            "remediation",
            RemediationDocument {
                records: state.remediations.clone(),
            },
        )?),
        subagents: Some(to_value(
            "subagents",
            SubagentsDocument {
                activities: state.subagents.clone(),
            },
        )?),
    })
}

fn decode_document(
    document: FactorydThreadStateDocument,
) -> Result<FactoryState, FactoryBackendError> {
    let Some(decomposition) = document.decomposition else {
        if document.progress.is_none()
            && document.review.is_none()
            && document.remediation.is_none()
            && document.subagents.is_none()
        {
            return Ok(FactoryState::default());
        }
        return Err(FactoryBackendError::new(
            "factoryd thread state has contributor data without a decomposition",
        ));
    };
    let decomposition: DecompositionDocument = from_value("decomposition", decomposition)?;
    let progress = document
        .progress
        .ok_or_else(|| FactoryBackendError::new("factoryd thread state is missing progress"))?;
    let progress: ProgressDocument = from_value("progress", progress)?;
    let mut progress_by_id = HashMap::new();
    for entry in progress.work_units {
        if progress_by_id.insert(entry.id.clone(), entry).is_some() {
            return Err(FactoryBackendError::new(
                "factoryd progress contains a duplicate work-unit ID",
            ));
        }
    }
    let mut work_units = Vec::with_capacity(decomposition.work_units.len());
    for definition in decomposition.work_units {
        let progress = progress_by_id.remove(&definition.id).ok_or_else(|| {
            FactoryBackendError::new(format!(
                "factoryd progress is missing work unit {}",
                definition.id
            ))
        })?;
        work_units.push(FactoryWorkUnit {
            id: definition.id,
            title: definition.title,
            description: definition.description,
            depends_on: definition.depends_on,
            status: progress.status,
            progress_summary: progress.progress_summary,
        });
    }
    if !progress_by_id.is_empty() {
        return Err(FactoryBackendError::new(
            "factoryd progress references an unknown work-unit ID",
        ));
    }
    let review = document
        .review
        .map(|value| from_value::<FactoryReviewReport>("review", value))
        .transpose()?;
    let remediations = document
        .remediation
        .map(|value| from_value::<RemediationDocument>("remediation", value))
        .transpose()?
        .map_or_else(Vec::new, |document| document.records);
    let subagents = document
        .subagents
        .map(|value| from_value::<SubagentsDocument>("subagents", value))
        .transpose()?
        .map_or_else(Vec::new, |document| document.activities);
    Ok(FactoryState {
        revision: decomposition.revision,
        work_units,
        review,
        remediations,
        subagents,
    })
}

fn to_value(contributor: &str, value: impl Serialize) -> Result<Value, FactoryBackendError> {
    serde_json::to_value(value).map_err(|error| {
        FactoryBackendError::new(format!("failed to encode Factory {contributor}: {error}"))
    })
}

fn from_value<T: for<'de> Deserialize<'de>>(
    contributor: &str,
    value: Value,
) -> Result<T, FactoryBackendError> {
    serde_json::from_value(value).map_err(|error| {
        FactoryBackendError::new(format!("failed to decode Factory {contributor}: {error}"))
    })
}

fn request_error(operation: &str, error: reqwest::Error) -> FactoryBackendError {
    FactoryBackendError::new(format!("factoryd {operation} failed: {error}"))
}

async fn response_error(operation: &str, response: reqwest::Response) -> FactoryBackendError {
    let status = response.status();
    let body = response
        .text()
        .await
        .unwrap_or_else(|error| format!("failed to read error response: {error}"));
    FactoryBackendError::new(format!(
        "factoryd {operation} returned HTTP {status}: {body}"
    ))
}
