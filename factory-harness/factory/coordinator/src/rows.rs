use crate::domain::AttemptRecord;
use crate::domain::AttemptState;
use crate::domain::CheckpointId;
use crate::domain::CheckpointRecord;
use crate::domain::CoordinatorInstanceId;
use crate::domain::CorrelationRecordId;
use crate::domain::DurableCorrelationRecord;
use crate::domain::FactoryThreadStateDocument;
use crate::domain::FactoryThreadStateRecord;
use crate::domain::JobRecord;
use crate::domain::JobState;
use crate::domain::OperationRecord;
use crate::domain::OperationState;
use crate::domain::PendingRequestId;
use crate::domain::PendingRequestRecord;
use crate::domain::PendingRequestState;
use crate::domain::RecoveryCause;
use crate::domain::RecoverySelection;
use crate::domain::ResumeStrategy;
use crate::domain::StageCheckpointRecord;
use crate::domain::WorkspaceRecord;
use crate::domain::WorkspaceState;
use crate::error::CoordinatorError;
use crate::error::Result;
use chrono::DateTime;
use chrono::Utc;
use factory_protocol::FactoryCorrelation;
use factory_protocol::FactoryRawServerRequest;
use factory_protocol::FactoryRawServerResponse;
use factory_protocol::FactoryServerRequestMethod;
use factory_protocol::ids::AttemptId;
use factory_protocol::ids::FactoryRequestId;
use factory_protocol::ids::FactoryRpcRequestId;
use factory_protocol::ids::ItemId;
use factory_protocol::ids::JobId;
use factory_protocol::ids::OperationId;
use factory_protocol::ids::TaskRunExternalId;
use factory_protocol::ids::ThreadId;
use factory_protocol::ids::TurnId;
use factory_protocol::ids::WorkflowRunId;
use serde_json::Value;
use sqlx::FromRow;

#[derive(Debug, FromRow)]
pub(crate) struct JobRow {
    pub job_id: String,
    pub kind: String,
    pub input: Value,
    pub status: String,
    pub workflow_run_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl TryFrom<JobRow> for JobRecord {
    type Error = CoordinatorError;

    fn try_from(row: JobRow) -> Result<Self> {
        Ok(Self {
            job_id: JobId::new(row.job_id),
            kind: row.kind,
            input: row.input,
            state: JobState::from_database_value(&row.status)?,
            workflow_run_id: row.workflow_run_id.map(WorkflowRunId::new),
            created_at: row.created_at,
            updated_at: row.updated_at,
        })
    }
}

#[derive(Debug, FromRow)]
pub(crate) struct OperationRow {
    pub operation_id: String,
    pub job_id: String,
    pub ordinal: i32,
    pub kind: String,
    pub input: Value,
    pub status: String,
    pub max_attempts: i32,
    pub next_eligible_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl TryFrom<OperationRow> for OperationRecord {
    type Error = CoordinatorError;

    fn try_from(row: OperationRow) -> Result<Self> {
        Ok(Self {
            operation_id: OperationId::new(row.operation_id),
            job_id: JobId::new(row.job_id),
            ordinal: to_u32(row.ordinal, "operation ordinal")?,
            kind: row.kind,
            input: row.input,
            state: OperationState::from_database_value(&row.status)?,
            max_attempts: to_u32(row.max_attempts, "maximum attempts")?,
            next_eligible_at: row.next_eligible_at,
            created_at: row.created_at,
            updated_at: row.updated_at,
        })
    }
}

#[derive(Debug, FromRow)]
pub(crate) struct AttemptRow {
    pub attempt_id: String,
    pub operation_id: String,
    pub attempt_number: i32,
    pub status: String,
    pub owner_instance_id: String,
    pub lease_expires_at: DateTime<Utc>,
    pub recovery_cause: String,
    pub resumes_attempt_id: Option<String>,
    pub resumes_checkpoint_id: Option<String>,
    pub failure: Option<Value>,
    pub started_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
}

impl TryFrom<AttemptRow> for AttemptRecord {
    type Error = CoordinatorError;

    fn try_from(row: AttemptRow) -> Result<Self> {
        Ok(Self {
            attempt_id: AttemptId::new(row.attempt_id),
            operation_id: OperationId::new(row.operation_id),
            attempt_number: to_u32(row.attempt_number, "attempt number")?,
            state: AttemptState::from_database_value(&row.status)?,
            owner_instance_id: CoordinatorInstanceId::new(row.owner_instance_id),
            lease_expires_at: row.lease_expires_at,
            recovery_cause: RecoveryCause::from_database_value(&row.recovery_cause)?,
            resumes_attempt_id: row.resumes_attempt_id.map(AttemptId::new),
            resumes_checkpoint_id: row.resumes_checkpoint_id.map(CheckpointId::new),
            failure: row.failure,
            started_at: row.started_at,
            finished_at: row.finished_at,
        })
    }
}

#[derive(Debug, FromRow)]
pub(crate) struct CorrelationRow {
    pub correlation_id: String,
    pub job_id: String,
    pub operation_id: String,
    pub attempt_id: String,
    pub workflow_run_id: Option<String>,
    pub task_run_external_id: Option<String>,
    pub request_id: String,
    pub thread_id: Option<String>,
    pub turn_id: Option<String>,
    pub item_id: Option<String>,
    pub observed_at: DateTime<Utc>,
}

impl From<CorrelationRow> for DurableCorrelationRecord {
    fn from(row: CorrelationRow) -> Self {
        Self {
            correlation_id: CorrelationRecordId::new(row.correlation_id),
            correlation: FactoryCorrelation {
                job_id: JobId::new(row.job_id),
                operation_id: OperationId::new(row.operation_id),
                attempt_id: AttemptId::new(row.attempt_id),
                workflow_run_id: row.workflow_run_id.map(WorkflowRunId::new),
                task_run_external_id: row.task_run_external_id.map(TaskRunExternalId::new),
                request_id: FactoryRequestId::new(row.request_id),
                thread_id: row.thread_id.map(ThreadId::new),
                turn_id: row.turn_id.map(TurnId::new),
                item_id: row.item_id.map(ItemId::new),
            },
            observed_at: row.observed_at,
        }
    }
}

#[derive(Debug, FromRow)]
pub(crate) struct PendingRequestRow {
    pub pending_request_id: String,
    pub job_id: String,
    pub operation_id: String,
    pub attempt_id: String,
    pub request_id: Value,
    pub method: String,
    pub params: Value,
    pub state: String,
    pub response: Option<Value>,
    pub created_at: DateTime<Utc>,
    pub resolved_at: Option<DateTime<Utc>>,
}

impl TryFrom<PendingRequestRow> for PendingRequestRecord {
    type Error = CoordinatorError;

    fn try_from(row: PendingRequestRow) -> Result<Self> {
        let request_id = serde_json::from_value::<FactoryRpcRequestId>(row.request_id)
            .map_err(|error| CoordinatorError::PendingRequestPayload(error.to_string()))?;
        let method = FactoryServerRequestMethod::new(row.method);
        let request = FactoryRawServerRequest {
            request_id: request_id.clone(),
            method: method.clone(),
            params: row.params,
        };
        let response = row.response.map(|response| FactoryRawServerResponse {
            request_id,
            method,
            response,
        });
        Ok(Self {
            pending_request_id: PendingRequestId::new(row.pending_request_id),
            job_id: JobId::new(row.job_id),
            operation_id: OperationId::new(row.operation_id),
            attempt_id: AttemptId::new(row.attempt_id),
            request,
            state: PendingRequestState::from_database_value(&row.state)?,
            response,
            created_at: row.created_at,
            resolved_at: row.resolved_at,
        })
    }
}

#[derive(Debug, FromRow)]
pub(crate) struct CheckpointRow {
    pub checkpoint_id: String,
    pub attempt_id: String,
    pub sequence: i64,
    pub kind: String,
    pub payload: Value,
    pub workspace_root: Option<String>,
    pub workspace_revision: Option<String>,
    pub correlation_id: Option<String>,
    pub created_at: DateTime<Utc>,
}

impl TryFrom<CheckpointRow> for CheckpointRecord {
    type Error = CoordinatorError;

    fn try_from(row: CheckpointRow) -> Result<Self> {
        Ok(Self {
            checkpoint_id: CheckpointId::new(row.checkpoint_id),
            attempt_id: AttemptId::new(row.attempt_id),
            sequence: u64::try_from(row.sequence).map_err(|_| CoordinatorError::NumericRange {
                field: "checkpoint sequence",
            })?,
            kind: row.kind,
            payload: row.payload,
            workspace_root: row.workspace_root,
            workspace_revision: row.workspace_revision,
            correlation_id: row.correlation_id.map(CorrelationRecordId::new),
            created_at: row.created_at,
        })
    }
}

#[derive(Debug, FromRow)]
pub(crate) struct StageCheckpointRow {
    pub operation_id: String,
    pub ordinal: i32,
    pub operation_kind: String,
    pub checkpoint_id: String,
    pub attempt_id: String,
    pub sequence: i64,
    pub checkpoint_kind: String,
    pub payload: Value,
    pub workspace_root: Option<String>,
    pub workspace_revision: Option<String>,
    pub correlation_id: Option<String>,
    pub created_at: DateTime<Utc>,
}

impl TryFrom<StageCheckpointRow> for StageCheckpointRecord {
    type Error = CoordinatorError;

    fn try_from(row: StageCheckpointRow) -> Result<Self> {
        Ok(Self {
            operation_id: OperationId::new(row.operation_id),
            ordinal: to_u32(row.ordinal, "operation ordinal")?,
            operation_kind: row.operation_kind,
            checkpoint: CheckpointRecord {
                checkpoint_id: CheckpointId::new(row.checkpoint_id),
                attempt_id: AttemptId::new(row.attempt_id),
                sequence: u64::try_from(row.sequence).map_err(|_| {
                    CoordinatorError::NumericRange {
                        field: "checkpoint sequence",
                    }
                })?,
                kind: row.checkpoint_kind,
                payload: row.payload,
                workspace_root: row.workspace_root,
                workspace_revision: row.workspace_revision,
                correlation_id: row.correlation_id.map(CorrelationRecordId::new),
                created_at: row.created_at,
            },
        })
    }
}

#[derive(Debug, FromRow)]
pub(crate) struct ThreadStateRow {
    pub thread_id: String,
    pub state: Value,
    pub revision: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl TryFrom<ThreadStateRow> for FactoryThreadStateRecord {
    type Error = CoordinatorError;

    fn try_from(row: ThreadStateRow) -> Result<Self> {
        Ok(Self {
            thread_id: ThreadId::new(row.thread_id),
            state: serde_json::from_value::<FactoryThreadStateDocument>(row.state)?,
            revision: u64::try_from(row.revision).map_err(|_| CoordinatorError::NumericRange {
                field: "thread state revision",
            })?,
            created_at: row.created_at,
            updated_at: row.updated_at,
        })
    }
}

#[derive(Debug, FromRow)]
pub(crate) struct WorkspaceRow {
    pub job_id: String,
    pub repository: String,
    pub base_ref: String,
    pub branch_name: String,
    pub root: String,
    pub revision: String,
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl TryFrom<WorkspaceRow> for WorkspaceRecord {
    type Error = CoordinatorError;

    fn try_from(row: WorkspaceRow) -> Result<Self> {
        Ok(Self {
            job_id: JobId::new(row.job_id),
            repository: row.repository,
            base_ref: row.base_ref,
            branch_name: row.branch_name,
            root: row.root,
            revision: row.revision,
            state: WorkspaceState::from_database_value(&row.status)?,
            created_at: row.created_at,
            updated_at: row.updated_at,
        })
    }
}

#[derive(Debug, FromRow)]
pub(crate) struct RecoverySelectionRow {
    pub job_id: String,
    pub operation_id: String,
    pub operation_kind: String,
    pub operation_status: String,
    pub attempts_made: i32,
    pub max_attempts: i32,
    pub previous_attempt_id: Option<String>,
    pub checkpoint_id: Option<String>,
    pub checkpoint_attempt_id: Option<String>,
    pub checkpoint_sequence: Option<i64>,
    pub checkpoint_kind: Option<String>,
    pub checkpoint_payload: Option<Value>,
    pub checkpoint_workspace_root: Option<String>,
    pub checkpoint_workspace_revision: Option<String>,
    pub checkpoint_correlation_id: Option<String>,
    pub checkpoint_created_at: Option<DateTime<Utc>>,
    pub correlation_id: Option<String>,
    pub correlation_job_id: Option<String>,
    pub correlation_operation_id: Option<String>,
    pub correlation_attempt_id: Option<String>,
    pub correlation_workflow_run_id: Option<String>,
    pub correlation_task_run_external_id: Option<String>,
    pub correlation_request_id: Option<String>,
    pub correlation_thread_id: Option<String>,
    pub correlation_turn_id: Option<String>,
    pub correlation_item_id: Option<String>,
    pub correlation_observed_at: Option<DateTime<Utc>>,
}

impl TryFrom<RecoverySelectionRow> for RecoverySelection {
    type Error = CoordinatorError;

    fn try_from(row: RecoverySelectionRow) -> Result<Self> {
        let cause = match row.operation_status.as_str() {
            "ready" => RecoveryCause::NewOperation,
            "retry_wait" => RecoveryCause::RetryScheduled,
            "running" => RecoveryCause::LeaseExpired,
            value => {
                return Err(CoordinatorError::UnsupportedState {
                    kind: "recoverable operation",
                    value: value.to_string(),
                });
            }
        };
        let checkpoint: Option<Result<CheckpointRecord>> = row.checkpoint_id.map(|checkpoint_id| {
            Ok(CheckpointRecord {
                checkpoint_id: CheckpointId::new(checkpoint_id),
                attempt_id: AttemptId::new(required(
                    row.checkpoint_attempt_id,
                    "checkpoint attempt",
                )?),
                sequence: u64::try_from(required(row.checkpoint_sequence, "checkpoint sequence")?)
                    .map_err(|_| CoordinatorError::NumericRange {
                        field: "checkpoint sequence",
                    })?,
                kind: required(row.checkpoint_kind, "checkpoint kind")?,
                payload: required(row.checkpoint_payload, "checkpoint payload")?,
                workspace_root: row.checkpoint_workspace_root,
                workspace_revision: row.checkpoint_workspace_revision,
                correlation_id: row.checkpoint_correlation_id.map(CorrelationRecordId::new),
                created_at: required(row.checkpoint_created_at, "checkpoint timestamp")?,
            })
        });
        let resume = match checkpoint.transpose()? {
            Some(checkpoint) => ResumeStrategy::FromCheckpoint(checkpoint),
            None => ResumeStrategy::Fresh,
        };
        let checkpoint_correlation: Option<Result<DurableCorrelationRecord>> =
            row.correlation_id.map(|correlation_id| {
                Ok(DurableCorrelationRecord {
                    correlation_id: CorrelationRecordId::new(correlation_id),
                    correlation: FactoryCorrelation {
                        job_id: JobId::new(required(row.correlation_job_id, "correlation job")?),
                        operation_id: OperationId::new(required(
                            row.correlation_operation_id,
                            "correlation operation",
                        )?),
                        attempt_id: AttemptId::new(required(
                            row.correlation_attempt_id,
                            "correlation attempt",
                        )?),
                        workflow_run_id: row.correlation_workflow_run_id.map(WorkflowRunId::new),
                        task_run_external_id: row
                            .correlation_task_run_external_id
                            .map(TaskRunExternalId::new),
                        request_id: FactoryRequestId::new(required(
                            row.correlation_request_id,
                            "correlation request",
                        )?),
                        thread_id: row.correlation_thread_id.map(ThreadId::new),
                        turn_id: row.correlation_turn_id.map(TurnId::new),
                        item_id: row.correlation_item_id.map(ItemId::new),
                    },
                    observed_at: required(row.correlation_observed_at, "correlation timestamp")?,
                })
            });
        let checkpoint_correlation = checkpoint_correlation.transpose()?;

        Ok(Self {
            job_id: JobId::new(row.job_id),
            operation_id: OperationId::new(row.operation_id),
            operation_kind: row.operation_kind,
            cause,
            previous_attempt_id: row.previous_attempt_id.map(AttemptId::new),
            next_attempt_number: to_u32(row.attempts_made + 1, "next attempt number")?,
            max_attempts: to_u32(row.max_attempts, "maximum attempts")?,
            resume,
            checkpoint_correlation,
        })
    }
}

fn required<T>(value: Option<T>, field: &'static str) -> Result<T> {
    value.ok_or_else(|| CoordinatorError::UnsupportedState {
        kind: "recovery row",
        value: format!("missing {field}"),
    })
}

fn to_u32(value: i32, field: &'static str) -> Result<u32> {
    u32::try_from(value).map_err(|_| CoordinatorError::NumericRange { field })
}
