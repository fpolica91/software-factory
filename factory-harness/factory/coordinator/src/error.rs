use crate::domain::PendingRequestId;
use factory_protocol::FactoryServerResponsePairingError;
use factory_protocol::ids::AttemptId;
use factory_protocol::ids::JobId;
use thiserror::Error;

pub type Result<T> = std::result::Result<T, CoordinatorError>;

#[derive(Debug, Error)]
pub enum CoordinatorError {
    #[error("coordinator database operation failed: {0}")]
    Database(#[from] sqlx::Error),
    #[error("invalid durable job definition: {0}")]
    InvalidJobDefinition(String),
    #[error("workflow run {0} already created a different durable job definition")]
    WorkflowRunConflict(String),
    #[error("invalid coordinator input: {0}")]
    InvalidInput(String),
    #[error("job {0} was not found")]
    JobNotFound(JobId),
    #[error("job {job_id} is already terminal ({state})")]
    JobNotCancellable { job_id: JobId, state: String },
    #[error("workspace for job {0} was not found")]
    WorkspaceNotFound(JobId),
    #[error("workspace operation failed: {0}")]
    Workspace(String),
    #[error("attempt {0} was not found or is not running")]
    AttemptNotRunning(AttemptId),
    #[error("attempt {0} has no renewable lease for this coordinator instance")]
    AttemptLeaseUnavailable(AttemptId),
    #[error("durable correlation does not match its job, operation, and attempt")]
    CorrelationMismatch,
    #[error("checkpoint correlation does not belong to its attempt")]
    CheckpointCorrelationMismatch,
    #[error("pending request {0} was not found")]
    PendingRequestNotFound(PendingRequestId),
    #[error("pending request {0} is no longer attached to an active attempt lease")]
    PendingRequestInactive(PendingRequestId),
    #[error("pending request {0} conflicts with an existing request or resolution")]
    PendingRequestConflict(PendingRequestId),
    #[error("pending request response identity does not match: {0}")]
    PendingRequestPairing(#[from] FactoryServerResponsePairingError),
    #[error("pending request payload is invalid: {0}")]
    PendingRequestPayload(String),
    #[error("persisted Factory thread state could not be decoded: {0}")]
    ThreadStateDecode(#[from] serde_json::Error),
    #[error("database contains unsupported {kind} state {value:?}")]
    UnsupportedState { kind: &'static str, value: String },
    #[error("numeric value for {field} is outside the coordinator domain")]
    NumericRange { field: &'static str },
}
