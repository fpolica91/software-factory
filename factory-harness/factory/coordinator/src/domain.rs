use crate::error::CoordinatorError;
use crate::error::Result;
use chrono::DateTime;
use chrono::Utc;
use factory_protocol::FactoryCorrelation;
use factory_protocol::FactoryRawServerRequest;
use factory_protocol::FactoryRawServerResponse;
use factory_protocol::ids::AttemptId;
use factory_protocol::ids::JobId;
use factory_protocol::ids::OperationId;
use factory_protocol::ids::ThreadId;
use factory_protocol::ids::WorkflowRunId;
use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;
use std::fmt;

macro_rules! coordinator_id {
    ($name:ident) => {
        #[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            pub fn new(value: impl Into<String>) -> Self {
                Self(value.into())
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(&self.0)
            }
        }
    };
}

coordinator_id!(CheckpointId);
coordinator_id!(CorrelationRecordId);
coordinator_id!(CoordinatorInstanceId);
coordinator_id!(PendingRequestId);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum WorkspaceState {
    Active,
    Removed,
}

impl WorkspaceState {
    pub(crate) fn from_database_value(value: &str) -> Result<Self> {
        match value {
            "active" => Ok(Self::Active),
            "removed" => Ok(Self::Removed),
            value => Err(CoordinatorError::UnsupportedState {
                kind: "workspace",
                value: value.to_string(),
            }),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum JobState {
    Queued,
    Running,
    Succeeded,
    Failed,
    Cancelled,
}

impl JobState {
    pub(crate) fn as_database_value(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    pub(crate) fn from_database_value(value: &str) -> Result<Self> {
        match value {
            "queued" => Ok(Self::Queued),
            "running" => Ok(Self::Running),
            "succeeded" => Ok(Self::Succeeded),
            "failed" => Ok(Self::Failed),
            "cancelled" => Ok(Self::Cancelled),
            value => Err(CoordinatorError::UnsupportedState {
                kind: "job",
                value: value.to_string(),
            }),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum OperationState {
    Ready,
    Running,
    RetryWait,
    Succeeded,
    Failed,
    Cancelled,
}

impl OperationState {
    pub(crate) fn from_database_value(value: &str) -> Result<Self> {
        match value {
            "ready" => Ok(Self::Ready),
            "running" => Ok(Self::Running),
            "retry_wait" => Ok(Self::RetryWait),
            "succeeded" => Ok(Self::Succeeded),
            "failed" => Ok(Self::Failed),
            "cancelled" => Ok(Self::Cancelled),
            value => Err(CoordinatorError::UnsupportedState {
                kind: "operation",
                value: value.to_string(),
            }),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AttemptState {
    Running,
    Succeeded,
    Failed,
    Abandoned,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PendingRequestState {
    Pending,
    Resolved,
    Inactive,
}

impl PendingRequestState {
    pub(crate) fn from_database_value(value: &str) -> Result<Self> {
        match value {
            "pending" => Ok(Self::Pending),
            "resolved" => Ok(Self::Resolved),
            "inactive" => Ok(Self::Inactive),
            value => Err(CoordinatorError::UnsupportedState {
                kind: "pending request",
                value: value.to_string(),
            }),
        }
    }
}

impl AttemptState {
    pub(crate) fn from_database_value(value: &str) -> Result<Self> {
        match value {
            "running" => Ok(Self::Running),
            "succeeded" => Ok(Self::Succeeded),
            "failed" => Ok(Self::Failed),
            "abandoned" => Ok(Self::Abandoned),
            value => Err(CoordinatorError::UnsupportedState {
                kind: "attempt",
                value: value.to_string(),
            }),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JobDefinition {
    pub kind: String,
    pub input: Value,
    pub workflow_run_id: Option<WorkflowRunId>,
    pub operations: Vec<OperationDefinition>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OperationDefinition {
    pub kind: String,
    pub input: Value,
    pub max_attempts: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JobRecord {
    pub job_id: JobId,
    pub kind: String,
    pub input: Value,
    pub state: JobState,
    pub workflow_run_id: Option<WorkflowRunId>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OperationRecord {
    pub operation_id: OperationId,
    pub job_id: JobId,
    pub ordinal: u32,
    pub kind: String,
    pub input: Value,
    pub state: OperationState,
    pub max_attempts: u32,
    pub next_eligible_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DurableJob {
    pub job: JobRecord,
    pub operations: Vec<OperationRecord>,
}

/// Latest durable completed-stage output available for a job operation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StageCheckpointRecord {
    pub operation_id: OperationId,
    pub ordinal: u32,
    pub operation_kind: String,
    pub checkpoint: CheckpointRecord,
}

/// Repository-neutral request to materialize the durable workspace for a job.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnsureWorkspaceRequest {
    pub repository: String,
    #[serde(default = "default_workspace_base_ref")]
    pub base_ref: String,
}

fn default_workspace_base_ref() -> String {
    "HEAD".to_string()
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceRecord {
    pub job_id: JobId,
    pub repository: String,
    pub base_ref: String,
    pub branch_name: String,
    pub root: String,
    pub revision: String,
    pub state: WorkspaceState,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AttemptRecord {
    pub attempt_id: AttemptId,
    pub operation_id: OperationId,
    pub attempt_number: u32,
    pub state: AttemptState,
    pub owner_instance_id: CoordinatorInstanceId,
    pub lease_expires_at: DateTime<Utc>,
    pub recovery_cause: RecoveryCause,
    pub resumes_attempt_id: Option<AttemptId>,
    pub resumes_checkpoint_id: Option<CheckpointId>,
    pub failure: Option<Value>,
    pub started_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DurableCorrelationRecord {
    pub correlation_id: CorrelationRecordId,
    pub correlation: FactoryCorrelation,
    pub observed_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewPendingRequest {
    pub attempt_id: AttemptId,
    pub request: FactoryRawServerRequest,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingRequestResolution {
    pub response: FactoryRawServerResponse,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingRequestRecord {
    pub pending_request_id: PendingRequestId,
    pub job_id: JobId,
    pub operation_id: OperationId,
    pub attempt_id: AttemptId,
    pub request: FactoryRawServerRequest,
    pub state: PendingRequestState,
    pub response: Option<FactoryRawServerResponse>,
    pub created_at: DateTime<Utc>,
    pub resolved_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewCheckpoint {
    pub attempt_id: AttemptId,
    pub kind: String,
    pub payload: Value,
    pub workspace_root: Option<String>,
    pub workspace_revision: Option<String>,
    pub correlation_id: Option<CorrelationRecordId>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckpointRecord {
    pub checkpoint_id: CheckpointId,
    pub attempt_id: AttemptId,
    pub sequence: u64,
    pub kind: String,
    pub payload: Value,
    pub workspace_root: Option<String>,
    pub workspace_revision: Option<String>,
    pub correlation_id: Option<CorrelationRecordId>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RecoveryCause {
    NewOperation,
    RetryScheduled,
    LeaseExpired,
}

impl RecoveryCause {
    pub(crate) fn as_database_value(self) -> &'static str {
        match self {
            Self::NewOperation => "new_operation",
            Self::RetryScheduled => "retry_scheduled",
            Self::LeaseExpired => "lease_expired",
        }
    }

    pub(crate) fn from_database_value(value: &str) -> Result<Self> {
        match value {
            "new_operation" => Ok(Self::NewOperation),
            "retry_scheduled" => Ok(Self::RetryScheduled),
            "lease_expired" => Ok(Self::LeaseExpired),
            value => Err(CoordinatorError::UnsupportedState {
                kind: "recovery cause",
                value: value.to_string(),
            }),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind", content = "checkpoint")]
pub enum ResumeStrategy {
    Fresh,
    FromCheckpoint(CheckpointRecord),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecoverySelection {
    pub job_id: JobId,
    pub operation_id: OperationId,
    pub operation_kind: String,
    pub cause: RecoveryCause,
    pub previous_attempt_id: Option<AttemptId>,
    pub next_attempt_number: u32,
    pub max_attempts: u32,
    pub resume: ResumeStrategy,
    pub checkpoint_correlation: Option<DurableCorrelationRecord>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClaimRequest {
    pub owner_instance_id: CoordinatorInstanceId,
    pub lease_seconds: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RenewLeaseRequest {
    pub owner_instance_id: CoordinatorInstanceId,
    pub lease_seconds: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecoveryLease {
    pub selection: RecoverySelection,
    pub attempt: AttemptRecord,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "disposition")]
pub enum AttemptFailure {
    RetryAt {
        #[serde(rename = "retryAt")]
        retry_at: DateTime<Utc>,
        detail: Value,
    },
    Terminal {
        detail: Value,
    },
}

/// Factory-owned state that must survive Codex process and thread rehydration.
///
/// Each field is independently owned by its native extension contributor. The
/// coordinator stores the document without interpreting contributor payloads.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FactoryThreadStateDocument {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decomposition: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remediation: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subagents: Option<Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FactoryThreadStateRecord {
    pub thread_id: ThreadId,
    pub state: FactoryThreadStateDocument,
    pub revision: u64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
