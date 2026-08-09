use crate::ids::AttemptId;
use crate::ids::JobId;
use crate::ids::ThreadId;
use thiserror::Error;

pub type Result<T> = std::result::Result<T, CoordinatorError>;

#[derive(Debug, Error)]
pub enum CoordinatorError {
    #[error("coordinator database operation failed: {0}")]
    Database(#[from] sqlx::Error),
    #[error("coordinator JSON serialization failed: {0}")]
    Serialization(#[from] serde_json::Error),
    #[error("invalid durable job definition: {0}")]
    InvalidJobDefinition(String),
    #[error("invalid coordinator input: {0}")]
    InvalidInput(String),
    #[error("job {0} was not found")]
    JobNotFound(JobId),
    #[error("job {job_id} is already terminal ({state})")]
    JobNotCancellable { job_id: JobId, state: String },
    #[error(
        "job {job_id} is {state}, not succeeded; only a succeeded job accepts continuation feedback"
    )]
    JobNotContinuable { job_id: JobId, state: String },
    #[error("job {0} has a pending cancellation request")]
    JobCancellationRequested(JobId),
    #[error("workspace for job {0} was not found")]
    WorkspaceNotFound(JobId),
    #[error("execution environment for job {0} was not found")]
    ExecutionEnvironmentNotFound(JobId),
    #[error("execution environment generation {generation} for job {job_id} is stale")]
    ExecutionEnvironmentGenerationStale { job_id: JobId, generation: u64 },
    #[error("workspace operation failed: {0}")]
    Workspace(String),
    #[error("workspace for job {job_id} must be rebound: {reason}")]
    WorkspaceRebindRequired { job_id: JobId, reason: String },
    #[error("attempt {0} was not found or is not running")]
    AttemptNotRunning(AttemptId),
    #[error("attempt {0} has no renewable lease for this coordinator instance")]
    AttemptLeaseUnavailable(AttemptId),
    #[error("durable correlation does not match its job, operation, and attempt")]
    CorrelationMismatch,
    #[error("checkpoint correlation does not belong to its attempt")]
    CheckpointCorrelationMismatch,
    #[error("attempt {attempt_id} has no correlation with Factory thread {thread_id}")]
    ThreadStateOwnershipMismatch {
        attempt_id: AttemptId,
        thread_id: ThreadId,
    },
    #[error("database contains unsupported {kind} state {value:?}")]
    UnsupportedState { kind: &'static str, value: String },
    #[error("numeric value for {field} is outside the coordinator domain")]
    NumericRange { field: &'static str },
}
