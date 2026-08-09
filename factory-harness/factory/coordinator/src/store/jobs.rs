use super::CoordinatorStore;
use super::new_id;
use crate::domain::AttemptFence;
use crate::domain::AttemptRecord;
use crate::domain::DurableJob;
use crate::domain::JobDefinition;
use crate::domain::JobRecord;
use crate::domain::JobState;
use crate::domain::OperationRecord;
use crate::domain::StageCheckpointRecord;
use crate::domain::WorkspaceBinding;
use crate::domain::WorkspaceRecord;
use crate::error::CoordinatorError;
use crate::error::Result;
use crate::ids::JobId;
use crate::rows::AttemptRow;
use crate::rows::JobRow;
use crate::rows::OperationRow;
use crate::rows::StageCheckpointRow;
use crate::rows::WorkspaceRow;

impl CoordinatorStore {
    pub async fn create_job(&self, definition: JobDefinition) -> Result<DurableJob> {
        validate_job_definition(&definition)?;
        let job_id = JobId::new(new_id());
        let mut transaction = self.pool.begin().await?;
        let job_row = sqlx::query_as::<_, JobRow>(
            r#"
            INSERT INTO factory_jobs (job_id, kind, input, status)
            VALUES ($1, $2, $3, $4)
            RETURNING job_id, kind, input, status, created_at, updated_at
            "#,
        )
        .bind(job_id.as_str())
        .bind(&definition.kind)
        .bind(&definition.input)
        .bind(JobState::Queued.as_database_value())
        .fetch_one(&mut *transaction)
        .await?;

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

    /// Reopens a succeeded factory.task with user feedback: appends the
    /// feedback to the durable task text, appends one iterate, review,
    /// remediate continuation round, and requeues the job. The managed
    /// worktree and parent Codex thread are reused by the appended stages.
    pub async fn continue_job(&self, job_id: &JobId, feedback: &str) -> Result<DurableJob> {
        let feedback = feedback.trim();
        if feedback.is_empty() {
            return Err(CoordinatorError::InvalidInput(
                "continuation feedback must not be empty".to_string(),
            ));
        }
        let _workspace_guard = self.acquire_workspace_execution(job_id).await?;
        let mut transaction = self.pool.begin().await?;
        let job_row = sqlx::query_as::<_, JobRow>(
            r#"
            SELECT job_id, kind, input, status, created_at, updated_at
            FROM factory_jobs
            WHERE job_id = $1
            FOR UPDATE
            "#,
        )
        .bind(job_id.as_str())
        .fetch_optional(&mut *transaction)
        .await?
        .ok_or_else(|| CoordinatorError::JobNotFound(job_id.clone()))?;
        let job = JobRecord::try_from(job_row)?;
        if job.kind != "factory.task" {
            return Err(CoordinatorError::InvalidInput(format!(
                "job kind {:?} does not support continuation",
                job.kind
            )));
        }
        if job.state != JobState::Succeeded {
            return Err(CoordinatorError::JobNotContinuable {
                job_id: job_id.clone(),
                state: job.state.as_database_value().to_string(),
            });
        }
        let workspace_active = sqlx::query_scalar::<_, bool>(
            r#"
            SELECT EXISTS (
                SELECT 1 FROM factory_workspaces
                WHERE job_id = $1 AND status = 'active'
            )
            "#,
        )
        .bind(job_id.as_str())
        .fetch_one(&mut *transaction)
        .await?;
        if !workspace_active {
            return Err(CoordinatorError::WorkspaceNotFound(job_id.clone()));
        }

        super::environments::reactivate_in_transaction(&mut transaction, job_id).await?;

        let next_ordinal = sqlx::query_scalar::<_, i32>(
            "SELECT COALESCE(MAX(ordinal), -1) + 1 FROM factory_operations WHERE job_id = $1",
        )
        .bind(job_id.as_str())
        .fetch_one(&mut *transaction)
        .await?;
        let round = continuation_round(next_ordinal)?;

        let mut input = job.input.clone();
        let task = input
            .get("task")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| {
                CoordinatorError::InvalidInput("job input has no task text".to_string())
            })?;
        input["task"] = serde_json::Value::String(format!(
            "{}\n\n## Follow-up feedback (round {round})\n\n{feedback}",
            task.trim_end()
        ));
        sqlx::query(
            r#"
            UPDATE factory_jobs
            SET input = $2, status = 'queued', updated_at = clock_timestamp()
            WHERE job_id = $1
            "#,
        )
        .bind(job_id.as_str())
        .bind(&input)
        .execute(&mut *transaction)
        .await?;

        const CONTINUATION_KINDS: [&str; 3] = ["codex.iterate", "codex.review", "codex.remediate"];
        for (offset, kind) in CONTINUATION_KINDS.into_iter().enumerate() {
            let operation_input = if offset == 0 {
                serde_json::json!({ "feedback": feedback, "round": round })
            } else {
                serde_json::json!({})
            };
            sqlx::query(
                r#"
                INSERT INTO factory_operations (
                    operation_id, job_id, ordinal, kind, input, status, max_attempts
                ) VALUES ($1, $2, $3, $4, $5, 'ready', 3)
                "#,
            )
            .bind(new_id())
            .bind(job_id.as_str())
            .bind(next_ordinal + i32::try_from(offset).expect("offset is 0..3"))
            .bind(kind)
            .bind(&operation_input)
            .execute(&mut *transaction)
            .await?;
        }
        transaction.commit().await?;

        let _ = self
            .append_job_event(crate::domain::NewJobEvent {
                job_id: job_id.clone(),
                kind: "job.continued".to_string(),
                payload: serde_json::json!({ "feedback": feedback, "round": round }),
            })
            .await;
        self.load_job(job_id).await
    }

    pub async fn load_job(&self, job_id: &JobId) -> Result<DurableJob> {
        let job = sqlx::query_as::<_, JobRow>(
            r#"
            SELECT job_id, kind, input, status, created_at, updated_at
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
            WHERE status IN ('queued', 'running', 'cancelling')
            ORDER BY created_at
            "#,
        )
        .fetch_all(&self.pool)
        .await?;
        let mut jobs = Vec::with_capacity(job_ids.len());
        for job_id in job_ids {
            let job = self.load_job(&JobId::new(job_id)).await?;
            if matches!(
                job.job.state,
                JobState::Queued | JobState::Running | JobState::Cancelling
            ) {
                jobs.push(job);
            }
        }
        Ok(jobs)
    }
    /// Requests durable cancellation for a job.
    ///
    /// A queued job with no live attempt can be cancelled immediately. A
    /// running attempt retains its fence while its worker interrupts and
    /// drains Codex and restores disposable Plan/review state. Only that live
    /// owner (or a replacement owner after lease recovery) can acknowledge the
    /// request and make cancellation terminal.
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
            "cancelled" | "cancelling" => {}
            "queued" | "running" => {
                let has_running_attempt = sqlx::query_scalar::<_, bool>(
                    r#"
                    SELECT EXISTS (
                        SELECT 1
                        FROM factory_attempts AS attempt
                        JOIN factory_operations AS operation
                          ON operation.operation_id = attempt.operation_id
                        WHERE operation.job_id = $1
                          AND attempt.status = 'running'
                    )
                    "#,
                )
                .bind(job_id.as_str())
                .fetch_one(&mut *transaction)
                .await?;
                if has_running_attempt {
                    sqlx::query(
                        r#"
                        UPDATE factory_jobs
                        SET status = 'cancelling', updated_at = clock_timestamp()
                        WHERE job_id = $1
                        "#,
                    )
                    .bind(job_id.as_str())
                    .execute(&mut *transaction)
                    .await?;
                } else {
                    cancel_unfinished_operations(&mut transaction, job_id).await?;
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
                    super::environments::request_release_in_transaction(&mut transaction, job_id)
                        .await?;
                }
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

    /// Acknowledges a running cancellation after the fenced worker has fully
    /// drained its runtime and restored disposable state. If cleanup fails the
    /// worker leaves the job in `cancelling`, relinquishes its lease, and a
    /// replacement owner retries cleanup before acknowledgement.
    pub async fn acknowledge_job_cancellation(&self, fence: &AttemptFence) -> Result<DurableJob> {
        let mut transaction = self.pool.begin().await?;
        let context = super::attempts::lock_attempt_context(&mut transaction, fence).await?;
        let job_id = JobId::new(context.job_id.clone());
        let status =
            sqlx::query_scalar::<_, String>("SELECT status FROM factory_jobs WHERE job_id = $1")
                .bind(job_id.as_str())
                .fetch_one(&mut *transaction)
                .await?;
        if status != "cancelling" {
            return Err(CoordinatorError::JobNotCancellable {
                job_id,
                state: status,
            });
        }
        let environment_status = sqlx::query_scalar::<_, String>(
            "SELECT status FROM factory_execution_environments WHERE job_id = $1 FOR UPDATE",
        )
        .bind(job_id.as_str())
        .fetch_optional(&mut *transaction)
        .await?;
        if environment_status
            .as_deref()
            .is_some_and(|status| status != "released")
        {
            return Err(CoordinatorError::InvalidInput(format!(
                "execution environment for cancelling job {} must be released before acknowledgement",
                job_id
            )));
        }

        sqlx::query(
            r#"
            UPDATE factory_attempts
            SET status = 'abandoned',
                failure = $2,
                finished_at = clock_timestamp()
            WHERE attempt_id = $1
            "#,
        )
        .bind(fence.attempt_id.as_str())
        .bind(serde_json::json!({
            "cause": "jobCancelled",
            "message": "job cancellation acknowledged after runtime cleanup"
        }))
        .execute(&mut *transaction)
        .await?;
        cancel_unfinished_operations(&mut transaction, &job_id).await?;
        sqlx::query(
            r#"
            UPDATE factory_jobs
            SET status = 'cancelled', updated_at = clock_timestamp()
            WHERE job_id = $1 AND status = 'cancelling'
            "#,
        )
        .bind(job_id.as_str())
        .execute(&mut *transaction)
        .await?;
        transaction.commit().await?;
        self.load_job(&job_id).await
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
              AND operation.status = 'succeeded'
              AND checkpoint.kind = 'factory.stage'
              AND checkpoint.payload ->> 'operation' = operation.kind
              AND checkpoint.payload ->> 'phase' = 'completed'
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
                   attempt.lease_epoch,
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
            SELECT job_id, repository_id, repository, base_ref, base_revision,
                   branch_name, root, revision, status, created_at, updated_at
            FROM factory_workspaces
            WHERE job_id = $1
            "#,
        )
        .bind(job_id.as_str())
        .fetch_optional(&self.pool)
        .await?;
        row.map(WorkspaceRecord::try_from).transpose()
    }

    pub async fn put_workspace(&self, binding: &WorkspaceBinding) -> Result<WorkspaceRecord> {
        let row = sqlx::query_as::<_, WorkspaceRow>(
            r#"
            INSERT INTO factory_workspaces (
                job_id, repository_id, repository, base_ref, base_revision,
                branch_name, root, revision, status
            ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, 'active')
            ON CONFLICT (job_id) DO UPDATE SET
                repository = EXCLUDED.repository,
                base_ref = EXCLUDED.base_ref,
                branch_name = EXCLUDED.branch_name,
                root = EXCLUDED.root,
                revision = EXCLUDED.revision,
                status = 'active',
                updated_at = clock_timestamp()
            RETURNING job_id, repository_id, repository, base_ref, base_revision,
                      branch_name, root, revision, status, created_at, updated_at
            "#,
        )
        .bind(binding.job_id.as_str())
        .bind(&binding.repository_id)
        .bind(&binding.repository)
        .bind(&binding.base_ref)
        .bind(&binding.base_revision)
        .bind(&binding.branch_name)
        .bind(&binding.root)
        .bind(&binding.revision)
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
            RETURNING job_id, repository_id, repository, base_ref, base_revision,
                      branch_name, root, revision, status, created_at, updated_at
            "#,
        )
        .bind(job_id.as_str())
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| CoordinatorError::WorkspaceNotFound(job_id.clone()))?;
        WorkspaceRecord::try_from(row)
    }
}

async fn cancel_unfinished_operations(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    job_id: &JobId,
) -> Result<()> {
    sqlx::query(
        r#"
        UPDATE factory_operations
        SET status = 'cancelled', updated_at = clock_timestamp()
        WHERE job_id = $1
          AND status IN ('ready', 'running', 'retry_wait')
        "#,
    )
    .bind(job_id.as_str())
    .execute(&mut **transaction)
    .await?;
    Ok(())
}

/// Continuation round for the next appended ordinal. The four base stages
/// occupy ordinals 0..=3; each complete round appends exactly three stages.
fn continuation_round(next_ordinal: i32) -> Result<u32> {
    let next = u32::try_from(next_ordinal).map_err(|_| CoordinatorError::NumericRange {
        field: "operation ordinal",
    })?;
    if next < 4 || (next - 4) % 3 != 0 {
        return Err(CoordinatorError::InvalidInput(format!(
            "job operations are not a complete factory.task lifecycle (next ordinal {next})"
        )));
    }
    Ok((next - 4) / 3 + 1)
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

#[cfg(test)]
mod tests {
    use super::continuation_round;

    #[test]
    fn continuation_rounds_start_after_the_four_base_stages() {
        assert_eq!(continuation_round(4).unwrap(), 1);
        assert_eq!(continuation_round(7).unwrap(), 2);
        assert_eq!(continuation_round(10).unwrap(), 3);
        assert!(continuation_round(3).is_err());
        assert!(continuation_round(5).is_err());
        assert!(continuation_round(-1).is_err());
    }
}
