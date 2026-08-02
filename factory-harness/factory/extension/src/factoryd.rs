use reqwest::Client;
use reqwest::StatusCode;
use reqwest::Url;
use serde::Deserialize;
use serde::Serialize;

use crate::FactoryBackendError;
use crate::FactoryBackendFuture;
use crate::FactoryEventReference;
use crate::FactoryState;
use crate::FactoryStateBackend;
use crate::FactoryStateDocument;
use crate::FactoryStateDurability;

/// HTTP backend for factoryd's attempt-fenced durable thread state.
#[derive(Clone, Debug)]
pub struct FactorydStateBackend {
    client: Client,
    base_url: Url,
    fence: FactoryStateFence,
}

/// Lease identity attached to every durable Factory state mutation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FactoryStateFence {
    attempt_id: String,
    owner_instance_id: String,
    lease_epoch: u64,
}

impl FactoryStateFence {
    pub fn new(
        attempt_id: impl Into<String>,
        owner_instance_id: impl Into<String>,
        lease_epoch: u64,
    ) -> Result<Self, FactoryBackendError> {
        let fence = Self {
            attempt_id: attempt_id.into(),
            owner_instance_id: owner_instance_id.into(),
            lease_epoch,
        };
        if fence.attempt_id.trim().is_empty() {
            return Err(FactoryBackendError::new("attempt ID must not be empty"));
        }
        if fence.owner_instance_id.trim().is_empty() {
            return Err(FactoryBackendError::new(
                "coordinator instance ID must not be empty",
            ));
        }
        if fence.lease_epoch == 0 {
            return Err(FactoryBackendError::new("lease epoch must be positive"));
        }
        Ok(fence)
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FactorydThreadStateRecord {
    state: FactoryStateDocument,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FactorydJobEventRecord {
    sequence: u64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct FactorydThreadStateWriteRequest<'a> {
    attempt_id: &'a str,
    owner_instance_id: &'a str,
    lease_epoch: u64,
    state: &'a FactoryStateDocument,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct FactorydAttemptEventWriteRequest<'a> {
    owner_instance_id: &'a str,
    lease_epoch: u64,
    kind: &'a str,
    payload: &'a serde_json::Value,
    deduplication_key: &'a str,
}

impl FactorydStateBackend {
    /// Creates a backend rooted at the factoryd API base and bound to one
    /// live attempt lease generation.
    pub fn new(
        base_url: impl AsRef<str>,
        fence: FactoryStateFence,
    ) -> Result<Self, FactoryBackendError> {
        let mut base_url = Url::parse(base_url.as_ref())
            .map_err(|error| FactoryBackendError::new(format!("invalid factoryd URL: {error}")))?;
        if base_url.cannot_be_a_base() {
            return Err(FactoryBackendError::new(
                "factoryd URL must be a hierarchical URL",
            ));
        }
        if !base_url.path().ends_with('/') {
            let path = format!("{}/", base_url.path());
            base_url.set_path(&path);
        }
        Ok(Self {
            client: Client::new(),
            base_url,
            fence,
        })
    }

    fn state_url(&self, thread_id: &str) -> Result<Url, FactoryBackendError> {
        let mut url = self.base_url.clone();
        {
            let mut segments = url.path_segments_mut().map_err(|_| {
                FactoryBackendError::new("factoryd URL cannot accept path segments")
            })?;
            segments
                .pop_if_empty()
                .extend(["threads", thread_id, "state"]);
        }
        Ok(url)
    }

    fn event_url(&self) -> Result<Url, FactoryBackendError> {
        let mut url = self.base_url.clone();
        {
            let mut segments = url.path_segments_mut().map_err(|_| {
                FactoryBackendError::new("factoryd URL cannot accept path segments")
            })?;
            segments
                .pop_if_empty()
                .extend(["attempts", self.fence.attempt_id.as_str(), "events"]);
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
            record.state.into_state().map(Some)
        })
    }

    fn save<'a>(&'a self, thread_id: &'a str, state: FactoryState) -> FactoryBackendFuture<'a, ()> {
        Box::pin(async move {
            let document = FactoryStateDocument::from_state(&state)?;
            let request = FactorydThreadStateWriteRequest {
                attempt_id: self.fence.attempt_id.as_str(),
                owner_instance_id: self.fence.owner_instance_id.as_str(),
                lease_epoch: self.fence.lease_epoch,
                state: &document,
            };
            let response = self
                .client
                .put(self.state_url(thread_id)?)
                .json(&request)
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
            let stored = record.state.into_state()?;
            if stored != state {
                return Err(FactoryBackendError::new(
                    "factoryd save response did not preserve the exact Factory state",
                ));
            }
            Ok(())
        })
    }

    fn append_event<'a>(
        &'a self,
        kind: &'a str,
        payload: serde_json::Value,
        deduplication_key: &'a str,
    ) -> FactoryBackendFuture<'a, Option<FactoryEventReference>> {
        Box::pin(async move {
            let request = FactorydAttemptEventWriteRequest {
                owner_instance_id: self.fence.owner_instance_id.as_str(),
                lease_epoch: self.fence.lease_epoch,
                kind,
                payload: &payload,
                deduplication_key,
            };
            let response = self
                .client
                .post(self.event_url()?)
                .json(&request)
                .send()
                .await
                .map_err(|error| request_error("append event", error))?;
            if !response.status().is_success() {
                return Err(response_error("append event", response).await);
            }
            let record = response
                .json::<FactorydJobEventRecord>()
                .await
                .map_err(|error| request_error("decode append-event response", error))?;
            Ok(Some(FactoryEventReference {
                sequence: record.sequence,
            }))
        })
    }

    fn durability(&self) -> FactoryStateDurability {
        FactoryStateDurability::Durable
    }
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

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn thread_state_write_has_one_flat_fence_shape() {
        let fence = FactoryStateFence::new("attempt-1", "worker-1", 7).unwrap();
        let state = FactoryState::default();
        let document = FactoryStateDocument::from_state(&state).unwrap();
        let request = FactorydThreadStateWriteRequest {
            attempt_id: fence.attempt_id.as_str(),
            owner_instance_id: fence.owner_instance_id.as_str(),
            lease_epoch: fence.lease_epoch,
            state: &document,
        };

        let value = serde_json::to_value(request).unwrap();
        let object = value.as_object().unwrap();
        assert_eq!(object.len(), 4);
        assert_eq!(object.get("attemptId"), Some(&json!("attempt-1")));
        assert_eq!(object.get("ownerInstanceId"), Some(&json!("worker-1")));
        assert_eq!(object.get("leaseEpoch"), Some(&json!(7)));
        assert!(object.get("state").unwrap().is_object());
    }

    #[test]
    fn attempt_event_write_carries_fence_and_full_payload() {
        let payload = json!({
            "call_id": "call-1",
            "prompt": "inspect README.md",
            "agents": [{ "thread_id": "child-1", "message": "done" }]
        });
        let request = FactorydAttemptEventWriteRequest {
            owner_instance_id: "worker-1",
            lease_epoch: 7,
            kind: "factory.subagent.activity",
            payload: &payload,
            deduplication_key: "factory.subagent.activity:abc123",
        };

        assert_eq!(
            serde_json::to_value(request).unwrap(),
            json!({
                "ownerInstanceId": "worker-1",
                "leaseEpoch": 7,
                "kind": "factory.subagent.activity",
                "payload": payload,
                "deduplicationKey": "factory.subagent.activity:abc123",
            })
        );
    }
}
