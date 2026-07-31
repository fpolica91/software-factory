use crate::domain::AttemptFailure;
use crate::domain::AttemptRecord;
use crate::domain::CheckpointId;
use crate::domain::CheckpointRecord;
use crate::domain::ClaimRequest;
use crate::domain::CorrelationRecordId;
use crate::domain::DurableCorrelationRecord;
use crate::domain::DurableJob;
use crate::domain::FactoryThreadStateDocument;
use crate::domain::FactoryThreadStateRecord;
use crate::domain::JobDefinition;
use crate::domain::JobRecord;
use crate::domain::JobState;
use crate::domain::NewCheckpoint;
use crate::domain::NewPendingRequest;
use crate::domain::OperationRecord;
use crate::domain::PendingRequestId;
use crate::domain::PendingRequestRecord;
use crate::domain::PendingRequestResolution;
use crate::domain::PendingRequestState;
use crate::domain::RecoveryCause;
use crate::domain::RecoveryLease;
use crate::domain::RecoverySelection;
use crate::domain::RenewLeaseRequest;
use crate::domain::ResumeStrategy;
use crate::domain::StageCheckpointRecord;
use crate::domain::WorkspaceRecord;
use crate::error::CoordinatorError;
use crate::error::Result;
use crate::rows::AttemptRow;
use crate::rows::CheckpointRow;
use crate::rows::CorrelationRow;
use crate::rows::JobRow;
use crate::rows::OperationRow;
use crate::rows::PendingRequestRow;
use crate::rows::RecoverySelectionRow;
use crate::rows::StageCheckpointRow;
use crate::rows::ThreadStateRow;
use crate::rows::WorkspaceRow;
use crate::schema;
use factory_protocol::FactoryCorrelation;
use factory_protocol::ids::AttemptId;
use factory_protocol::ids::JobId;
use factory_protocol::ids::OperationId;
use factory_protocol::ids::ThreadId;
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
use uuid::Uuid;

macro_rules! pending_request_sql {
    ($suffix:literal) => {
        concat!(
            r#"
            SELECT
                pending.pending_request_id,
                operation.job_id,
                pending.attempt_id,
                attempt.operation_id,
                pending.request_id,
                pending.method,
                pending.params,
                CASE
                    WHEN pending.response IS NOT NULL THEN 'resolved'
                    WHEN attempt.status = 'running'
                         AND attempt.lease_expires_at > clock_timestamp() THEN 'pending'
                    ELSE 'inactive'
                END AS state,
                pending.response,
                pending.created_at,
                pending.resolved_at
            FROM factory_pending_requests pending
            JOIN factory_attempts attempt ON attempt.attempt_id = pending.attempt_id
            JOIN factory_operations operation ON operation.operation_id = attempt.operation_id
            "#,
            $suffix
        )
    };
}

const RECOVERY_SELECTION_SQL: &str = r#"
SELECT
    j.job_id,
    o.operation_id,
    o.kind AS operation_kind,
    o.status AS operation_status,
    COALESCE(previous_attempt.attempt_number, 0) AS attempts_made,
    o.max_attempts,
    previous_attempt.attempt_id AS previous_attempt_id,
    checkpoint.checkpoint_id,
    checkpoint.attempt_id AS checkpoint_attempt_id,
    checkpoint.sequence AS checkpoint_sequence,
    checkpoint.kind AS checkpoint_kind,
    checkpoint.payload AS checkpoint_payload,
    checkpoint.workspace_root AS checkpoint_workspace_root,
    checkpoint.workspace_revision AS checkpoint_workspace_revision,
    checkpoint.correlation_id AS checkpoint_correlation_id,
    checkpoint.created_at AS checkpoint_created_at,
    correlation.correlation_id,
    correlation.job_id AS correlation_job_id,
    correlation.operation_id AS correlation_operation_id,
    correlation.attempt_id AS correlation_attempt_id,
    correlation.workflow_run_id AS correlation_workflow_run_id,
    correlation.task_run_external_id AS correlation_task_run_external_id,
    correlation.request_id AS correlation_request_id,
    correlation.thread_id AS correlation_thread_id,
    correlation.turn_id AS correlation_turn_id,
    correlation.item_id AS correlation_item_id,
    correlation.observed_at AS correlation_observed_at
FROM factory_jobs j
JOIN factory_operations o ON o.job_id = j.job_id
LEFT JOIN LATERAL (
    SELECT a.attempt_id, a.attempt_number, a.status, a.lease_expires_at
    FROM factory_attempts a
    WHERE a.operation_id = o.operation_id
    ORDER BY a.attempt_number DESC
    LIMIT 1
) previous_attempt ON true
LEFT JOIN LATERAL (
    SELECT c.*
    FROM factory_checkpoints c
    JOIN factory_attempts checkpoint_attempt
        ON checkpoint_attempt.attempt_id = c.attempt_id
    JOIN factory_operations checkpoint_operation
        ON checkpoint_operation.operation_id = checkpoint_attempt.operation_id
    WHERE checkpoint_operation.job_id = j.job_id
      AND checkpoint_operation.ordinal <= o.ordinal
    ORDER BY checkpoint_operation.ordinal DESC,
             checkpoint_attempt.attempt_number DESC,
             c.sequence DESC
    LIMIT 1
) checkpoint ON true
LEFT JOIN factory_runtime_correlations correlation
    ON correlation.correlation_id = checkpoint.correlation_id
WHERE ($1::TEXT IS NULL OR j.job_id = $1)
  AND ($2::TEXT IS NULL OR o.operation_id = $2)
  AND j.status IN ('queued', 'running')
  AND COALESCE(previous_attempt.attempt_number, 0) < o.max_attempts
  AND NOT EXISTS (
      SELECT 1
      FROM factory_operations predecessor
      WHERE predecessor.job_id = o.job_id
        AND predecessor.ordinal < o.ordinal
        AND predecessor.status <> 'succeeded'
  )
  AND (
      o.status = 'ready'
      OR (o.status = 'retry_wait' AND o.next_eligible_at <= clock_timestamp())
      OR (
          o.status = 'running'
          AND previous_attempt.status = 'running'
          AND previous_attempt.lease_expires_at <= clock_timestamp()
      )
  )
ORDER BY j.created_at, o.ordinal
LIMIT 1
FOR UPDATE OF j, o SKIP LOCKED
"#;

/// PostgreSQL-backed durable coordinator state.
///
/// Separate `CoordinatorStore` values can connect from independently-lived
/// `factoryd` processes. Recovery claims serialize only on the selected
/// operation and use `SKIP LOCKED`, allowing multiple coordinators to recover
/// distinct work concurrently.
#[derive(Clone)]
pub struct CoordinatorStore {
    pool: PgPool,
}

impl CoordinatorStore {
    pub async fn connect(database_url: &str) -> Result<Self> {
        let pool = PgPoolOptions::new()
            .max_connections(8)
            .connect(database_url)
            .await?;
        Ok(Self { pool })
    }

    pub async fn migrate(&self) -> Result<()> {
        schema::migrate(&self.pool).await?;
        Ok(())
    }

    pub async fn close(self) {
        self.pool.close().await;
    }

    pub async fn create_job(&self, definition: JobDefinition) -> Result<DurableJob> {
        validate_job_definition(&definition)?;
        let job_id = JobId::new(new_id());
        let workflow_run_id = definition.workflow_run_id.as_ref().map(|id| id.as_str());
        let mut transaction = self.pool.begin().await?;
        let job_row = if let Some(workflow_run_id) = workflow_run_id {
            let inserted = sqlx::query_as::<_, JobRow>(
                r#"
                INSERT INTO factory_jobs (
                    job_id, kind, input, status, workflow_run_id
                ) VALUES ($1, $2, $3, $4, $5)
                ON CONFLICT (workflow_run_id) WHERE workflow_run_id IS NOT NULL
                DO NOTHING
                RETURNING job_id, kind, input, status, workflow_run_id,
                          created_at, updated_at
                "#,
            )
            .bind(job_id.as_str())
            .bind(&definition.kind)
            .bind(&definition.input)
            .bind(JobState::Queued.as_database_value())
            .bind(workflow_run_id)
            .fetch_optional(&mut *transaction)
            .await?;

            match inserted {
                Some(job_row) => job_row,
                None => {
                    let existing_job = sqlx::query_as::<_, JobRow>(
                        r#"
                        SELECT job_id, kind, input, status, workflow_run_id,
                               created_at, updated_at
                        FROM factory_jobs
                        WHERE workflow_run_id = $1
                        "#,
                    )
                    .bind(workflow_run_id)
                    .fetch_one(&mut *transaction)
                    .await?;
                    let operation_rows = sqlx::query_as::<_, OperationRow>(
                        r#"
                        SELECT operation_id, job_id, ordinal, kind, input, status,
                               max_attempts, next_eligible_at, created_at, updated_at
                        FROM factory_operations
                        WHERE job_id = $1
                        ORDER BY ordinal
                        "#,
                    )
                    .bind(&existing_job.job_id)
                    .fetch_all(&mut *transaction)
                    .await?;
                    let existing = DurableJob {
                        job: JobRecord::try_from(existing_job)?,
                        operations: operation_rows
                            .into_iter()
                            .map(OperationRecord::try_from)
                            .collect::<Result<Vec<_>>>()?,
                    };
                    if !job_matches_definition(&existing, &definition) {
                        return Err(CoordinatorError::WorkflowRunConflict(
                            workflow_run_id.to_owned(),
                        ));
                    }
                    transaction.commit().await?;
                    return Ok(existing);
                }
            }
        } else {
            sqlx::query_as::<_, JobRow>(
                r#"
                INSERT INTO factory_jobs (
                    job_id, kind, input, status, workflow_run_id
                ) VALUES ($1, $2, $3, $4, $5)
                RETURNING job_id, kind, input, status, workflow_run_id,
                          created_at, updated_at
                "#,
            )
            .bind(job_id.as_str())
            .bind(&definition.kind)
            .bind(&definition.input)
            .bind(JobState::Queued.as_database_value())
            .bind(workflow_run_id)
            .fetch_one(&mut *transaction)
            .await?
        };

        let mut operations = Vec::with_capacity(definition.operations.len());
        for (ordinal, operation) in definition.operations.iter().enumerate() {
            let ordinal = i32::try_from(ordinal).map_err(|_| CoordinatorError::NumericRange {
                field: "operation ordinal",
            })?;
            let max_attempts = i32::try_from(operation.max_attempts).map_err(|_| {
                CoordinatorError::NumericRange {
                    field: "maximum attempts",
                }
            })?;
            let row = sqlx::query_as::<_, OperationRow>(
                r#"
                INSERT INTO factory_operations (
                    operation_id, job_id, ordinal, kind, input, status,
                    max_attempts
                ) VALUES ($1, $2, $3, $4, $5, 'ready', $6)
                RETURNING operation_id, job_id, ordinal, kind, input, status,
                          max_attempts, next_eligible_at, created_at, updated_at
                "#,
            )
            .bind(new_id())
            .bind(job_id.as_str())
            .bind(ordinal)
            .bind(&operation.kind)
            .bind(&operation.input)
            .bind(max_attempts)
            .fetch_one(&mut *transaction)
            .await?;
            operations.push(OperationRecord::try_from(row)?);
        }
        transaction.commit().await?;
        Ok(DurableJob {
            job: JobRecord::try_from(job_row)?,
            operations,
        })
    }

    pub async fn load_job(&self, job_id: &JobId) -> Result<DurableJob> {
        let job = sqlx::query_as::<_, JobRow>(
            r#"
            SELECT job_id, kind, input, status, workflow_run_id,
                   created_at, updated_at
            FROM factory_jobs
            WHERE job_id = $1
            "#,
        )
        .bind(job_id.as_str())
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| CoordinatorError::JobNotFound(job_id.clone()))?;
        let operation_rows = sqlx::query_as::<_, OperationRow>(
            r#"
            SELECT operation_id, job_id, ordinal, kind, input, status,
                   max_attempts, next_eligible_at, created_at, updated_at
            FROM factory_operations
            WHERE job_id = $1
            ORDER BY ordinal
            "#,
        )
        .bind(job_id.as_str())
        .fetch_all(&self.pool)
        .await?;
        let operations = operation_rows
            .into_iter()
            .map(OperationRecord::try_from)
            .collect::<Result<Vec<_>>>()?;
        Ok(DurableJob {
            job: JobRecord::try_from(job)?,
            operations,
        })
    }

    pub async fn list_active_jobs(&self) -> Result<Vec<DurableJob>> {
        let job_ids = sqlx::query_scalar::<_, String>(
            r#"
            SELECT job_id
            FROM factory_jobs
            WHERE status IN ('queued', 'running')
            ORDER BY created_at
            "#,
        )
        .fetch_all(&self.pool)
        .await?;
        let mut jobs = Vec::with_capacity(job_ids.len());
        for job_id in job_ids {
            let job = self.load_job(&JobId::new(job_id)).await?;
            if matches!(job.job.state, JobState::Queued | JobState::Running) {
                jobs.push(job);
            }
        }
        Ok(jobs)
    }

    /// Durably cancels unfinished work for a job.
    ///
    /// Running attempts become inactive immediately so pending requests and
    /// expired leases cannot revive the job. The workflow observes the job
    /// state and interrupts any active Codex turn before exiting.
    pub async fn cancel_job(&self, job_id: &JobId) -> Result<DurableJob> {
        let mut transaction = self.pool.begin().await?;
        let status = sqlx::query_scalar::<_, String>(
            r#"
            SELECT status
            FROM factory_jobs
            WHERE job_id = $1
            FOR UPDATE
            "#,
        )
        .bind(job_id.as_str())
        .fetch_optional(&mut *transaction)
        .await?
        .ok_or_else(|| CoordinatorError::JobNotFound(job_id.clone()))?;

        match status.as_str() {
            "cancelled" => {}
            "queued" | "running" => {
                sqlx::query(
                    r#"
                    UPDATE factory_operations
                    SET status = 'cancelled', updated_at = clock_timestamp()
                    WHERE job_id = $1
                      AND status IN ('ready', 'running', 'retry_wait')
                    "#,
                )
                .bind(job_id.as_str())
                .execute(&mut *transaction)
                .await?;
                sqlx::query(
                    r#"
                    UPDATE factory_attempts AS attempt
                    SET status = 'abandoned',
                        failure = $2,
                        finished_at = clock_timestamp()
                    FROM factory_operations AS operation
                    WHERE attempt.operation_id = operation.operation_id
                      AND operation.job_id = $1
                      AND attempt.status = 'running'
                    "#,
                )
                .bind(job_id.as_str())
                .bind(serde_json::json!({
                    "cause": "jobCancelled",
                    "message": "job cancelled by user"
                }))
                .execute(&mut *transaction)
                .await?;
                sqlx::query(
                    r#"
                    UPDATE factory_jobs
                    SET status = 'cancelled', updated_at = clock_timestamp()
                    WHERE job_id = $1
                    "#,
                )
                .bind(job_id.as_str())
                .execute(&mut *transaction)
                .await?;
            }
            _ => {
                return Err(CoordinatorError::JobNotCancellable {
                    job_id: job_id.clone(),
                    state: status,
                });
            }
        }

        transaction.commit().await?;
        self.load_job(job_id).await
    }

    pub async fn list_stage_checkpoints(
        &self,
        job_id: &JobId,
    ) -> Result<Vec<StageCheckpointRecord>> {
        self.load_job(job_id).await?;
        let rows = sqlx::query_as::<_, StageCheckpointRow>(
            r#"
            SELECT DISTINCT ON (operation.operation_id)
                   operation.operation_id,
                   operation.ordinal,
                   operation.kind AS operation_kind,
                   checkpoint.checkpoint_id,
                   checkpoint.attempt_id,
                   checkpoint.sequence,
                   checkpoint.kind AS checkpoint_kind,
                   checkpoint.payload,
                   checkpoint.workspace_root,
                   checkpoint.workspace_revision,
                   checkpoint.correlation_id,
                   checkpoint.created_at
            FROM factory_operations AS operation
            JOIN factory_attempts AS attempt
              ON attempt.operation_id = operation.operation_id
            JOIN factory_checkpoints AS checkpoint
              ON checkpoint.attempt_id = attempt.attempt_id
            WHERE operation.job_id = $1
              AND checkpoint.kind = operation.kind || '.completed'
            ORDER BY operation.operation_id,
                     attempt.attempt_number DESC,
                     checkpoint.sequence DESC
            "#,
        )
        .bind(job_id.as_str())
        .fetch_all(&self.pool)
        .await?;
        let mut checkpoints = rows
            .into_iter()
            .map(StageCheckpointRecord::try_from)
            .collect::<Result<Vec<_>>>()?;
        checkpoints.sort_by_key(|record| record.ordinal);
        Ok(checkpoints)
    }

    pub async fn list_job_attempts(&self, job_id: &JobId) -> Result<Vec<AttemptRecord>> {
        self.load_job(job_id).await?;
        let rows = sqlx::query_as::<_, AttemptRow>(
            r#"
            SELECT attempt.attempt_id,
                   attempt.operation_id,
                   attempt.attempt_number,
                   attempt.status,
                   attempt.owner_instance_id,
                   attempt.lease_expires_at,
                   attempt.recovery_cause,
                   attempt.resumes_attempt_id,
                   attempt.resumes_checkpoint_id,
                   attempt.failure,
                   attempt.started_at,
                   attempt.finished_at
            FROM factory_attempts AS attempt
            JOIN factory_operations AS operation
              ON operation.operation_id = attempt.operation_id
            WHERE operation.job_id = $1
            ORDER BY operation.ordinal, attempt.attempt_number
            "#,
        )
        .bind(job_id.as_str())
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(AttemptRecord::try_from)
            .collect::<Result<Vec<_>>>()
    }

    pub async fn load_workspace(&self, job_id: &JobId) -> Result<Option<WorkspaceRecord>> {
        let row = sqlx::query_as::<_, WorkspaceRow>(
            r#"
            SELECT job_id, repository, base_ref, branch_name, root, revision,
                   status, created_at, updated_at
            FROM factory_workspaces
            WHERE job_id = $1
            "#,
        )
        .bind(job_id.as_str())
        .fetch_optional(&self.pool)
        .await?;
        row.map(WorkspaceRecord::try_from).transpose()
    }

    pub async fn put_workspace(
        &self,
        job_id: &JobId,
        repository: &str,
        base_ref: &str,
        branch_name: &str,
        root: &str,
        revision: &str,
    ) -> Result<WorkspaceRecord> {
        let row = sqlx::query_as::<_, WorkspaceRow>(
            r#"
            INSERT INTO factory_workspaces (
                job_id, repository, base_ref, branch_name, root, revision, status
            ) VALUES ($1, $2, $3, $4, $5, $6, 'active')
            ON CONFLICT (job_id) DO UPDATE SET
                repository = EXCLUDED.repository,
                base_ref = EXCLUDED.base_ref,
                branch_name = EXCLUDED.branch_name,
                root = EXCLUDED.root,
                revision = EXCLUDED.revision,
                status = 'active',
                updated_at = clock_timestamp()
            RETURNING job_id, repository, base_ref, branch_name, root, revision,
                      status, created_at, updated_at
            "#,
        )
        .bind(job_id.as_str())
        .bind(repository)
        .bind(base_ref)
        .bind(branch_name)
        .bind(root)
        .bind(revision)
        .fetch_one(&self.pool)
        .await?;
        WorkspaceRecord::try_from(row)
    }

    pub async fn mark_workspace_removed(&self, job_id: &JobId) -> Result<WorkspaceRecord> {
        let row = sqlx::query_as::<_, WorkspaceRow>(
            r#"
            UPDATE factory_workspaces
            SET status = 'removed', updated_at = clock_timestamp()
            WHERE job_id = $1
            RETURNING job_id, repository, base_ref, branch_name, root, revision,
                      status, created_at, updated_at
            "#,
        )
        .bind(job_id.as_str())
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| CoordinatorError::WorkspaceNotFound(job_id.clone()))?;
        WorkspaceRecord::try_from(row)
    }

    pub async fn load_attempt(&self, attempt_id: &AttemptId) -> Result<AttemptRecord> {
        let row = sqlx::query_as::<_, AttemptRow>(
            r#"
            SELECT attempt_id, operation_id, attempt_number, status,
                   owner_instance_id, lease_expires_at, recovery_cause,
                   resumes_attempt_id, resumes_checkpoint_id, failure,
                   started_at, finished_at
            FROM factory_attempts
            WHERE attempt_id = $1
            "#,
        )
        .bind(attempt_id.as_str())
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| CoordinatorError::AttemptNotRunning(attempt_id.clone()))?;
        AttemptRecord::try_from(row)
    }

    pub async fn append_correlation(
        &self,
        correlation: &FactoryCorrelation,
    ) -> Result<DurableCorrelationRecord> {
        let correlation_id = CorrelationRecordId::new(new_id());
        let row = sqlx::query_as::<_, CorrelationRow>(
            r#"
            INSERT INTO factory_runtime_correlations (
                correlation_id, job_id, operation_id, attempt_id,
                workflow_run_id, task_run_external_id, request_id,
                thread_id, turn_id, item_id
            )
            SELECT $1, $2, $3, $4, $5, $6, $7, $8, $9, $10
            FROM factory_attempts attempt
            JOIN factory_operations operation
                ON operation.operation_id = attempt.operation_id
            WHERE attempt.attempt_id = $4
              AND attempt.operation_id = $3
              AND operation.job_id = $2
            RETURNING correlation_id, job_id, operation_id, attempt_id,
                      workflow_run_id, task_run_external_id, request_id,
                      thread_id, turn_id, item_id, observed_at
            "#,
        )
        .bind(correlation_id.as_str())
        .bind(correlation.job_id.as_str())
        .bind(correlation.operation_id.as_str())
        .bind(correlation.attempt_id.as_str())
        .bind(correlation.workflow_run_id.as_ref().map(|id| id.as_str()))
        .bind(
            correlation
                .task_run_external_id
                .as_ref()
                .map(|id| id.as_str()),
        )
        .bind(correlation.request_id.as_str())
        .bind(correlation.thread_id.as_ref().map(|id| id.as_str()))
        .bind(correlation.turn_id.as_ref().map(|id| id.as_str()))
        .bind(correlation.item_id.as_ref().map(|id| id.as_str()))
        .fetch_optional(&self.pool)
        .await?
        .ok_or(CoordinatorError::CorrelationMismatch)?;
        Ok(row.into())
    }

    pub async fn register_pending_request(
        &self,
        pending: NewPendingRequest,
    ) -> Result<(PendingRequestRecord, bool)> {
        let mut transaction = self.pool.begin().await?;
        let attempt_is_active = sqlx::query_scalar::<_, bool>(
            r#"
            SELECT status = 'running' AND lease_expires_at > clock_timestamp()
            FROM factory_attempts
            WHERE attempt_id = $1
            FOR UPDATE
            "#,
        )
        .bind(pending.attempt_id.as_str())
        .fetch_optional(&mut *transaction)
        .await?
        .unwrap_or(false);
        if !attempt_is_active {
            return Err(CoordinatorError::AttemptNotRunning(pending.attempt_id));
        }

        let request_id = serde_json::to_value(&pending.request.request_id)
            .map_err(|error| CoordinatorError::PendingRequestPayload(error.to_string()))?;
        if let Some(row) = sqlx::query_as::<_, PendingRequestRow>(pending_request_sql!(
            r#"
            WHERE pending.attempt_id = $1 AND pending.request_id = $2
            FOR UPDATE OF pending
            "#
        ))
        .bind(pending.attempt_id.as_str())
        .bind(&request_id)
        .fetch_optional(&mut *transaction)
        .await?
        {
            let existing = PendingRequestRecord::try_from(row)?;
            if existing.request != pending.request {
                return Err(CoordinatorError::PendingRequestConflict(
                    existing.pending_request_id,
                ));
            }
            transaction.commit().await?;
            return Ok((existing, false));
        }

        let pending_request_id = PendingRequestId::new(new_id());
        sqlx::query(
            r#"
            INSERT INTO factory_pending_requests (
                pending_request_id, attempt_id, request_id, method, params
            ) VALUES ($1, $2, $3, $4, $5)
            "#,
        )
        .bind(pending_request_id.as_str())
        .bind(pending.attempt_id.as_str())
        .bind(&request_id)
        .bind(pending.request.method.as_str())
        .bind(&pending.request.params)
        .execute(&mut *transaction)
        .await?;

        let row = sqlx::query_as::<_, PendingRequestRow>(pending_request_sql!(
            "WHERE pending.pending_request_id = $1"
        ))
        .bind(pending_request_id.as_str())
        .fetch_one(&mut *transaction)
        .await?;
        let record = PendingRequestRecord::try_from(row)?;
        transaction.commit().await?;
        Ok((record, true))
    }

    pub async fn list_pending_requests(
        &self,
        job_id: Option<&JobId>,
    ) -> Result<Vec<PendingRequestRecord>> {
        sqlx::query_as::<_, PendingRequestRow>(pending_request_sql!(
            r#"
            WHERE pending.response IS NULL
              AND attempt.status = 'running'
              AND attempt.lease_expires_at > clock_timestamp()
              AND ($1::TEXT IS NULL OR operation.job_id = $1)
            ORDER BY pending.created_at, pending.pending_request_id
            "#
        ))
        .bind(job_id.map(JobId::as_str))
        .fetch_all(&self.pool)
        .await?
        .into_iter()
        .map(PendingRequestRecord::try_from)
        .collect()
    }

    pub async fn load_pending_request(
        &self,
        pending_request_id: &PendingRequestId,
    ) -> Result<PendingRequestRecord> {
        let row = sqlx::query_as::<_, PendingRequestRow>(pending_request_sql!(
            "WHERE pending.pending_request_id = $1"
        ))
        .bind(pending_request_id.as_str())
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| CoordinatorError::PendingRequestNotFound(pending_request_id.clone()))?;
        PendingRequestRecord::try_from(row)
    }

    pub async fn resolve_pending_request(
        &self,
        pending_request_id: &PendingRequestId,
        resolution: PendingRequestResolution,
    ) -> Result<PendingRequestRecord> {
        let mut transaction = self.pool.begin().await?;
        let row = sqlx::query_as::<_, PendingRequestRow>(pending_request_sql!(
            r#"
            WHERE pending.pending_request_id = $1
            FOR UPDATE OF pending, attempt
            "#
        ))
        .bind(pending_request_id.as_str())
        .fetch_optional(&mut *transaction)
        .await?
        .ok_or_else(|| CoordinatorError::PendingRequestNotFound(pending_request_id.clone()))?;
        let existing = PendingRequestRecord::try_from(row)?;
        existing.request.validate_response(&resolution.response)?;
        resolution
            .response
            .clone()
            .decode()
            .map_err(|error| CoordinatorError::PendingRequestPayload(error.to_string()))?;

        if let Some(response) = &existing.response {
            if response == &resolution.response {
                transaction.commit().await?;
                return Ok(existing);
            }
            return Err(CoordinatorError::PendingRequestConflict(
                pending_request_id.clone(),
            ));
        }
        if existing.state != PendingRequestState::Pending {
            return Err(CoordinatorError::PendingRequestInactive(
                pending_request_id.clone(),
            ));
        }

        sqlx::query(
            r#"
            UPDATE factory_pending_requests
            SET response = $2, resolved_at = clock_timestamp()
            WHERE pending_request_id = $1
            "#,
        )
        .bind(pending_request_id.as_str())
        .bind(&resolution.response.response)
        .execute(&mut *transaction)
        .await?;
        let row = sqlx::query_as::<_, PendingRequestRow>(pending_request_sql!(
            "WHERE pending.pending_request_id = $1"
        ))
        .bind(pending_request_id.as_str())
        .fetch_one(&mut *transaction)
        .await?;
        let resolved = PendingRequestRecord::try_from(row)?;
        transaction.commit().await?;
        Ok(resolved)
    }

    pub async fn save_checkpoint(&self, checkpoint: NewCheckpoint) -> Result<CheckpointRecord> {
        if checkpoint.kind.trim().is_empty() {
            return Err(CoordinatorError::InvalidInput(
                "checkpoint kind must not be empty".to_string(),
            ));
        }
        let mut transaction = self.pool.begin().await?;
        let locked_attempt = sqlx::query_scalar::<_, String>(
            r#"
            SELECT attempt_id FROM factory_attempts
            WHERE attempt_id = $1 AND status = 'running'
            FOR UPDATE
            "#,
        )
        .bind(checkpoint.attempt_id.as_str())
        .fetch_optional(&mut *transaction)
        .await?;
        if locked_attempt.is_none() {
            return Err(CoordinatorError::AttemptNotRunning(checkpoint.attempt_id));
        }
        if let Some(correlation_id) = &checkpoint.correlation_id {
            let matches_attempt = sqlx::query_scalar::<_, bool>(
                r#"
                SELECT EXISTS (
                    SELECT 1 FROM factory_runtime_correlations
                    WHERE correlation_id = $1 AND attempt_id = $2
                )
                "#,
            )
            .bind(correlation_id.as_str())
            .bind(checkpoint.attempt_id.as_str())
            .fetch_one(&mut *transaction)
            .await?;
            if !matches_attempt {
                return Err(CoordinatorError::CheckpointCorrelationMismatch);
            }
        }

        let sequence = sqlx::query_scalar::<_, i64>(
            r#"
            SELECT COALESCE(MAX(sequence), 0) + 1
            FROM factory_checkpoints
            WHERE attempt_id = $1
            "#,
        )
        .bind(checkpoint.attempt_id.as_str())
        .fetch_one(&mut *transaction)
        .await?;
        let checkpoint_id = CheckpointId::new(new_id());
        let row = sqlx::query_as::<_, CheckpointRow>(
            r#"
            INSERT INTO factory_checkpoints (
                checkpoint_id, attempt_id, sequence, kind, payload,
                workspace_root, workspace_revision, correlation_id
            ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
            RETURNING checkpoint_id, attempt_id, sequence, kind, payload,
                      workspace_root, workspace_revision, correlation_id,
                      created_at
            "#,
        )
        .bind(checkpoint_id.as_str())
        .bind(checkpoint.attempt_id.as_str())
        .bind(sequence)
        .bind(&checkpoint.kind)
        .bind(&checkpoint.payload)
        .bind(&checkpoint.workspace_root)
        .bind(&checkpoint.workspace_revision)
        .bind(
            checkpoint
                .correlation_id
                .as_ref()
                .map(CorrelationRecordId::as_str),
        )
        .fetch_one(&mut *transaction)
        .await?;
        transaction.commit().await?;
        CheckpointRecord::try_from(row)
    }

    pub async fn load_checkpoint(
        &self,
        checkpoint_id: &CheckpointId,
    ) -> Result<Option<CheckpointRecord>> {
        sqlx::query_as::<_, CheckpointRow>(
            r#"
            SELECT checkpoint_id, attempt_id, sequence, kind, payload,
                   workspace_root, workspace_revision, correlation_id,
                   created_at
            FROM factory_checkpoints
            WHERE checkpoint_id = $1
            "#,
        )
        .bind(checkpoint_id.as_str())
        .fetch_optional(&self.pool)
        .await?
        .map(CheckpointRecord::try_from)
        .transpose()
    }

    pub async fn renew_attempt(
        &self,
        attempt_id: &AttemptId,
        request: &RenewLeaseRequest,
    ) -> Result<AttemptRecord> {
        if request.lease_seconds == 0 {
            return Err(CoordinatorError::InvalidInput(
                "renewed leases must be at least one second".to_string(),
            ));
        }
        let lease_seconds = i64::from(request.lease_seconds);
        let row = sqlx::query_as::<_, AttemptRow>(
            r#"
            UPDATE factory_attempts
            SET lease_expires_at = clock_timestamp() + ($3 * interval '1 second')
            WHERE attempt_id = $1
              AND owner_instance_id = $2
              AND status = 'running'
            RETURNING attempt_id, operation_id, attempt_number, status,
                      owner_instance_id, lease_expires_at, recovery_cause,
                      resumes_attempt_id, resumes_checkpoint_id, failure,
                      started_at, finished_at
            "#,
        )
        .bind(attempt_id.as_str())
        .bind(request.owner_instance_id.as_str())
        .bind(lease_seconds)
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| CoordinatorError::AttemptLeaseUnavailable(attempt_id.clone()))?;
        AttemptRecord::try_from(row)
    }

    pub async fn load_thread_state(
        &self,
        thread_id: &ThreadId,
    ) -> Result<Option<FactoryThreadStateRecord>> {
        sqlx::query_as::<_, ThreadStateRow>(
            r#"
            SELECT thread_id, state, revision, created_at, updated_at
            FROM factory_thread_states
            WHERE thread_id = $1
            "#,
        )
        .bind(thread_id.as_str())
        .fetch_optional(&self.pool)
        .await?
        .map(FactoryThreadStateRecord::try_from)
        .transpose()
    }

    pub async fn put_thread_state(
        &self,
        thread_id: &ThreadId,
        state: FactoryThreadStateDocument,
    ) -> Result<FactoryThreadStateRecord> {
        let state = serde_json::to_value(state)?;
        let row = sqlx::query_as::<_, ThreadStateRow>(
            r#"
            INSERT INTO factory_thread_states (thread_id, state)
            VALUES ($1, $2)
            ON CONFLICT (thread_id) DO UPDATE
            SET state = EXCLUDED.state,
                revision = factory_thread_states.revision + 1,
                updated_at = clock_timestamp()
            RETURNING thread_id, state, revision, created_at, updated_at
            "#,
        )
        .bind(thread_id.as_str())
        .bind(state)
        .fetch_one(&self.pool)
        .await?;
        FactoryThreadStateRecord::try_from(row)
    }

    pub async fn select_next_recovery(&self) -> Result<Option<RecoverySelection>> {
        self.select_recovery(RecoveryScope::Any).await
    }

    pub async fn select_recovery_for_job(
        &self,
        job_id: &JobId,
    ) -> Result<Option<RecoverySelection>> {
        self.select_recovery(RecoveryScope::Job(job_id)).await
    }

    pub async fn select_recovery_for_operation(
        &self,
        operation_id: &OperationId,
    ) -> Result<Option<RecoverySelection>> {
        self.select_recovery(RecoveryScope::Operation(operation_id))
            .await
    }

    pub async fn claim_next_recovery(
        &self,
        request: &ClaimRequest,
    ) -> Result<Option<RecoveryLease>> {
        self.claim_recovery(RecoveryScope::Any, request).await
    }

    pub async fn claim_recovery_for_job(
        &self,
        job_id: &JobId,
        request: &ClaimRequest,
    ) -> Result<Option<RecoveryLease>> {
        self.claim_recovery(RecoveryScope::Job(job_id), request)
            .await
    }

    pub async fn claim_recovery_for_operation(
        &self,
        operation_id: &OperationId,
        request: &ClaimRequest,
    ) -> Result<Option<RecoveryLease>> {
        self.claim_recovery(RecoveryScope::Operation(operation_id), request)
            .await
    }

    pub async fn complete_attempt(&self, attempt_id: &AttemptId) -> Result<()> {
        let mut transaction = self.pool.begin().await?;
        let context = lock_attempt_context(&mut transaction, attempt_id).await?;
        sqlx::query(
            r#"
            UPDATE factory_attempts
            SET status = 'succeeded', finished_at = clock_timestamp()
            WHERE attempt_id = $1
            "#,
        )
        .bind(attempt_id.as_str())
        .execute(&mut *transaction)
        .await?;
        sqlx::query(
            r#"
            UPDATE factory_operations
            SET status = 'succeeded', updated_at = clock_timestamp()
            WHERE operation_id = $1
            "#,
        )
        .bind(&context.operation_id)
        .execute(&mut *transaction)
        .await?;
        let has_remaining = sqlx::query_scalar::<_, bool>(
            r#"
            SELECT EXISTS (
                SELECT 1 FROM factory_operations
                WHERE job_id = $1 AND status <> 'succeeded'
            )
            "#,
        )
        .bind(&context.job_id)
        .fetch_one(&mut *transaction)
        .await?;
        if !has_remaining {
            sqlx::query(
                r#"
                UPDATE factory_jobs
                SET status = 'succeeded', updated_at = clock_timestamp()
                WHERE job_id = $1
                "#,
            )
            .bind(&context.job_id)
            .execute(&mut *transaction)
            .await?;
        }
        transaction.commit().await?;
        Ok(())
    }

    pub async fn fail_attempt(
        &self,
        attempt_id: &AttemptId,
        failure: AttemptFailure,
    ) -> Result<()> {
        let mut transaction = self.pool.begin().await?;
        let context = lock_attempt_context(&mut transaction, attempt_id).await?;
        let (detail, retry_at) = match failure {
            AttemptFailure::RetryAt { retry_at, detail }
                if context.attempt_number < context.max_attempts =>
            {
                (detail, Some(retry_at))
            }
            AttemptFailure::RetryAt { detail, .. } | AttemptFailure::Terminal { detail } => {
                (detail, None)
            }
        };
        sqlx::query(
            r#"
            UPDATE factory_attempts
            SET status = 'failed', failure = $2, finished_at = clock_timestamp()
            WHERE attempt_id = $1
            "#,
        )
        .bind(attempt_id.as_str())
        .bind(&detail)
        .execute(&mut *transaction)
        .await?;
        if let Some(retry_at) = retry_at {
            sqlx::query(
                r#"
                UPDATE factory_operations
                SET status = 'retry_wait', next_eligible_at = $2,
                    updated_at = clock_timestamp()
                WHERE operation_id = $1
                "#,
            )
            .bind(&context.operation_id)
            .bind(retry_at)
            .execute(&mut *transaction)
            .await?;
        } else {
            sqlx::query(
                r#"
                UPDATE factory_operations
                SET status = 'failed', updated_at = clock_timestamp()
                WHERE operation_id = $1
                "#,
            )
            .bind(&context.operation_id)
            .execute(&mut *transaction)
            .await?;
            sqlx::query(
                r#"
                UPDATE factory_jobs
                SET status = 'failed', updated_at = clock_timestamp()
                WHERE job_id = $1
                "#,
            )
            .bind(&context.job_id)
            .execute(&mut *transaction)
            .await?;
        }
        transaction.commit().await?;
        Ok(())
    }

    async fn select_recovery(&self, scope: RecoveryScope<'_>) -> Result<Option<RecoverySelection>> {
        let (job_id, operation_id) = scope.database_ids();
        sqlx::query_as::<_, RecoverySelectionRow>(RECOVERY_SELECTION_SQL)
            .bind(job_id)
            .bind(operation_id)
            .fetch_optional(&self.pool)
            .await?
            .map(RecoverySelection::try_from)
            .transpose()
    }

    async fn claim_recovery(
        &self,
        scope: RecoveryScope<'_>,
        request: &ClaimRequest,
    ) -> Result<Option<RecoveryLease>> {
        let (job_id, operation_id) = scope.database_ids();
        let mut transaction = self.pool.begin().await?;
        let Some(selection_row) = sqlx::query_as::<_, RecoverySelectionRow>(RECOVERY_SELECTION_SQL)
            .bind(job_id)
            .bind(operation_id)
            .fetch_optional(&mut *transaction)
            .await?
        else {
            transaction.rollback().await?;
            return Ok(None);
        };
        let selection = RecoverySelection::try_from(selection_row)?;
        let claimed_job = sqlx::query(
            r#"
            UPDATE factory_jobs
            SET status = 'running', updated_at = clock_timestamp()
            WHERE job_id = $1 AND status IN ('queued', 'running')
            "#,
        )
        .bind(selection.job_id.as_str())
        .execute(&mut *transaction)
        .await?;
        if claimed_job.rows_affected() == 0 {
            transaction.rollback().await?;
            return Ok(None);
        }
        if selection.cause == RecoveryCause::LeaseExpired
            && let Some(previous_attempt_id) = &selection.previous_attempt_id
        {
            sqlx::query(
                r#"
                UPDATE factory_attempts
                SET status = 'abandoned',
                    failure = jsonb_build_object('cause', 'leaseExpired'),
                    finished_at = clock_timestamp()
                WHERE attempt_id = $1 AND status = 'running'
                "#,
            )
            .bind(previous_attempt_id.as_str())
            .execute(&mut *transaction)
            .await?;
        }

        let (resumes_attempt_id, resumes_checkpoint_id) = match &selection.resume {
            ResumeStrategy::Fresh => (None, None),
            ResumeStrategy::FromCheckpoint(checkpoint) => (
                Some(checkpoint.attempt_id.as_str()),
                Some(checkpoint.checkpoint_id.as_str()),
            ),
        };
        let attempt_id = AttemptId::new(new_id());
        let attempt_number = i32::try_from(selection.next_attempt_number).map_err(|_| {
            CoordinatorError::NumericRange {
                field: "attempt number",
            }
        })?;
        let lease_seconds = i64::from(request.lease_seconds);
        let attempt_row = sqlx::query_as::<_, AttemptRow>(
            r#"
            INSERT INTO factory_attempts (
                attempt_id, operation_id, attempt_number, status,
                owner_instance_id, lease_expires_at, recovery_cause,
                resumes_attempt_id, resumes_checkpoint_id
            ) VALUES (
                $1, $2, $3, 'running', $4,
                clock_timestamp() + ($5 * interval '1 second'),
                $6, $7, $8
            )
            RETURNING attempt_id, operation_id, attempt_number, status,
                      owner_instance_id, lease_expires_at, recovery_cause,
                      resumes_attempt_id, resumes_checkpoint_id, failure,
                      started_at, finished_at
            "#,
        )
        .bind(attempt_id.as_str())
        .bind(selection.operation_id.as_str())
        .bind(attempt_number)
        .bind(request.owner_instance_id.as_str())
        .bind(lease_seconds)
        .bind(selection.cause.as_database_value())
        .bind(resumes_attempt_id)
        .bind(resumes_checkpoint_id)
        .fetch_one(&mut *transaction)
        .await?;
        sqlx::query(
            r#"
            UPDATE factory_operations
            SET status = 'running', updated_at = clock_timestamp()
            WHERE operation_id = $1
            "#,
        )
        .bind(selection.operation_id.as_str())
        .execute(&mut *transaction)
        .await?;
        transaction.commit().await?;
        Ok(Some(RecoveryLease {
            selection,
            attempt: AttemptRecord::try_from(attempt_row)?,
        }))
    }
}

#[derive(Clone, Copy)]
enum RecoveryScope<'a> {
    Any,
    Job(&'a JobId),
    Operation(&'a OperationId),
}

impl<'a> RecoveryScope<'a> {
    fn database_ids(self) -> (Option<&'a str>, Option<&'a str>) {
        match self {
            Self::Any => (None, None),
            Self::Job(job_id) => (Some(job_id.as_str()), None),
            Self::Operation(operation_id) => (None, Some(operation_id.as_str())),
        }
    }
}

#[derive(Debug, sqlx::FromRow)]
struct AttemptContextRow {
    operation_id: String,
    job_id: String,
    attempt_number: i32,
    max_attempts: i32,
}

async fn lock_attempt_context(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    attempt_id: &AttemptId,
) -> Result<AttemptContextRow> {
    sqlx::query_as::<_, AttemptContextRow>(
        r#"
        SELECT attempt.operation_id, operation.job_id,
               attempt.attempt_number, operation.max_attempts
        FROM factory_jobs job
        JOIN factory_operations operation
            ON operation.job_id = job.job_id
        JOIN factory_attempts attempt
            ON attempt.operation_id = operation.operation_id
        WHERE attempt.attempt_id = $1 AND attempt.status = 'running'
        FOR UPDATE OF job, operation, attempt
        "#,
    )
    .bind(attempt_id.as_str())
    .fetch_optional(&mut **transaction)
    .await?
    .ok_or_else(|| CoordinatorError::AttemptNotRunning(attempt_id.clone()))
}

fn job_matches_definition(job: &DurableJob, definition: &JobDefinition) -> bool {
    job.job.kind == definition.kind
        && job.job.input == definition.input
        && job.job.workflow_run_id.as_ref() == definition.workflow_run_id.as_ref()
        && job.operations.len() == definition.operations.len()
        && job
            .operations
            .iter()
            .zip(&definition.operations)
            .enumerate()
            .all(|(ordinal, (existing, requested))| {
                u32::try_from(ordinal).is_ok_and(|ordinal| existing.ordinal == ordinal)
                    && existing.kind == requested.kind
                    && existing.input == requested.input
                    && existing.max_attempts == requested.max_attempts
            })
}

fn validate_job_definition(definition: &JobDefinition) -> Result<()> {
    if definition.kind.trim().is_empty() {
        return Err(CoordinatorError::InvalidJobDefinition(
            "job kind must not be empty".to_string(),
        ));
    }
    if definition.operations.is_empty() {
        return Err(CoordinatorError::InvalidJobDefinition(
            "at least one operation is required".to_string(),
        ));
    }
    for operation in &definition.operations {
        if operation.kind.trim().is_empty() {
            return Err(CoordinatorError::InvalidJobDefinition(
                "operation kind must not be empty".to_string(),
            ));
        }
        if operation.max_attempts == 0 {
            return Err(CoordinatorError::InvalidJobDefinition(format!(
                "operation {:?} must allow at least one attempt",
                operation.kind
            )));
        }
        i32::try_from(operation.max_attempts).map_err(|_| {
            CoordinatorError::InvalidJobDefinition(format!(
                "operation {:?} exceeds PostgreSQL's attempt limit",
                operation.kind
            ))
        })?;
    }
    Ok(())
}

fn new_id() -> String {
    Uuid::new_v4().to_string()
}
