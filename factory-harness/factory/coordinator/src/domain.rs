use crate::correlation::Correlation;
use crate::error::CoordinatorError;
use crate::error::Result;
use crate::ids::AttemptId;
use crate::ids::ExecutionEnvironmentId;
use crate::ids::JobId;
use crate::ids::OperationId;
use crate::ids::ThreadId;
use chrono::DateTime;
use chrono::Utc;
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
    Cancelling,
    Succeeded,
    Failed,
    Cancelled,
}

impl JobState {
    pub(crate) fn as_database_value(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Cancelling => "cancelling",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    pub(crate) fn from_database_value(value: &str) -> Result<Self> {
        match value {
            "queued" => Ok(Self::Queued),
            "running" => Ok(Self::Running),
            "cancelling" => Ok(Self::Cancelling),
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ExecutionEnvironmentDesiredState {
    Active,
    Released,
}

impl ExecutionEnvironmentDesiredState {
    pub(crate) fn from_database_value(value: &str) -> Result<Self> {
        match value {
            "active" => Ok(Self::Active),
            "released" => Ok(Self::Released),
            value => Err(CoordinatorError::UnsupportedState {
                kind: "execution environment desired state",
                value: value.to_string(),
            }),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ExecutionEnvironmentStatus {
    Provisioning,
    Ready,
    Releasing,
    Released,
    Failed,
}

impl ExecutionEnvironmentStatus {
    pub(crate) fn from_database_value(value: &str) -> Result<Self> {
        match value {
            "provisioning" => Ok(Self::Provisioning),
            "ready" => Ok(Self::Ready),
            "releasing" => Ok(Self::Releasing),
            "released" => Ok(Self::Released),
            "failed" => Ok(Self::Failed),
            value => Err(CoordinatorError::UnsupportedState {
                kind: "execution environment lifecycle",
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
    pub operations: Vec<OperationDefinition>,
}

/// Exact model execution capability pinned when a Factory task is created.
/// Credentials and provider endpoints remain worker configuration, not job data.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecutionProfile {
    pub provider: String,
    pub model: String,
}

/// Durable input shared directly by the Factory CLI and runtime.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FactoryTaskInput {
    pub task: String,
    #[serde(default)]
    pub execution_profile: Option<ExecutionProfile>,
    #[serde(default)]
    pub repository_id: Option<String>,
    #[serde(default)]
    pub developer_instructions: Option<String>,
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
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// A coordinator lifecycle event that is not owned by an execution attempt.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewJobEvent {
    pub job_id: JobId,
    pub kind: String,
    pub payload: Value,
}

/// An execution event written while an attempt still owns its lease.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewAttemptEvent {
    pub kind: String,
    pub payload: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deduplication_key: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JobEventRecord {
    pub sequence: u64,
    pub job_id: JobId,
    pub operation_id: Option<OperationId>,
    pub attempt_id: Option<AttemptId>,
    pub kind: String,
    pub payload: Value,
    pub created_at: DateTime<Utc>,
}

/// One incremental page from a job's append-only event stream.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JobEventPage {
    pub events: Vec<JobEventRecord>,
    pub next_cursor: u64,
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
    pub repository_id: String,
    pub repository: String,
    #[serde(default = "default_workspace_base_ref")]
    pub base_ref: String,
}

fn default_workspace_base_ref() -> String {
    "HEAD".to_string()
}

/// Complete active binding written when a managed worktree is materialized or
/// its current revision advances.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceBinding {
    pub job_id: JobId,
    pub repository_id: String,
    pub repository: String,
    pub base_ref: String,
    pub base_revision: String,
    pub branch_name: String,
    pub root: String,
    pub revision: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceRecord {
    pub job_id: JobId,
    pub repository_id: String,
    pub repository: String,
    pub base_ref: String,
    pub base_revision: String,
    pub branch_name: String,
    pub root: String,
    pub revision: String,
    pub state: WorkspaceState,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Immutable-base change set exported from a succeeded managed worktree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceResult {
    pub job_id: JobId,
    pub repository_id: String,
    pub base_revision: String,
    pub patch_sha256: String,
    pub patch: Vec<u8>,
}

/// One durable execution-environment identity for a Factory job.
///
/// Retries and lease transfers retain the identity and generation. A
/// continuation reactivates the same identity with a new generation, fencing
/// teardown work that began for the preceding terminal job generation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecutionEnvironmentRecord {
    pub job_id: JobId,
    pub environment_id: ExecutionEnvironmentId,
    pub backend: String,
    pub generation: u64,
    pub desired_state: ExecutionEnvironmentDesiredState,
    pub status: ExecutionEnvironmentStatus,
    pub backend_ref: Option<String>,
    pub url: Option<String>,
    pub error: Option<String>,
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
    pub lease_epoch: u64,
    pub lease_expires_at: DateTime<Utc>,
    pub recovery_cause: RecoveryCause,
    pub resumes_attempt_id: Option<AttemptId>,
    pub resumes_checkpoint_id: Option<CheckpointId>,
    pub failure: Option<Value>,
    pub started_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
}

impl AttemptRecord {
    pub fn fence(&self) -> AttemptFence {
        AttemptFence {
            attempt_id: self.attempt_id.clone(),
            owner_instance_id: self.owner_instance_id.clone(),
            lease_epoch: self.lease_epoch,
        }
    }
}

/// Identifies one exclusive lease generation for a durable attempt.
///
/// An expired attempt keeps its business identity and attempt number when a
/// new worker takes ownership. Incrementing `lease_epoch` makes every handle
/// held by the previous worker permanently stale.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AttemptFence {
    pub attempt_id: AttemptId,
    pub owner_instance_id: CoordinatorInstanceId,
    pub lease_epoch: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DurableCorrelationRecord {
    pub correlation_id: CorrelationRecordId,
    pub correlation: Correlation,
    pub observed_at: DateTime<Utc>,
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
    #[serde(default)]
    pub execution_profile: Option<ExecutionProfile>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RenewLeaseRequest {
    pub owner_instance_id: CoordinatorInstanceId,
    pub lease_epoch: u64,
    pub lease_seconds: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecoveryLease {
    pub selection: RecoverySelection,
    pub attempt: AttemptRecord,
    pub fence: AttemptFence,
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "disposition", content = "failure")]
pub enum AttemptSettlement {
    Succeeded,
    Failed(AttemptFailure),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FactoryThreadStateRecord {
    pub thread_id: ThreadId,
    /// Opaque extension-owned state. The coordinator fences and stores it but
    /// does not duplicate or interpret its document schema.
    pub state: Value,
    pub revision: u64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
