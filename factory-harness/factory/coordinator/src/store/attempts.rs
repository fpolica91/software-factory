use super::CoordinatorStore;
use super::database_lease_epoch;
use super::new_id;
use crate::correlation::Correlation;
use crate::domain::AttemptFailure;
use crate::domain::AttemptFence;
use crate::domain::AttemptRecord;
use crate::domain::AttemptSettlement;
use crate::domain::CheckpointId;
use crate::domain::CheckpointRecord;
use crate::domain::CorrelationRecordId;
use crate::domain::DurableCorrelationRecord;
use crate::domain::FactoryThreadStateRecord;
use crate::domain::NewAttemptEvent;
use crate::domain::NewCheckpoint;
use crate::error::CoordinatorError;
use crate::error::Result;
use crate::ids::AttemptId;
use crate::ids::ThreadId;
use crate::rows::AttemptRow;
use crate::rows::CheckpointRow;
use crate::rows::CorrelationRow;
use crate::rows::ThreadStateRow;
use serde_json::Value;

impl CoordinatorStore {
    pub async fn load_attempt(&self, attempt_id: &AttemptId) -> Result<AttemptRecord> {
        let row = sqlx::query_as::<_, AttemptRow>(
            r#"
            SELECT attempt_id, operation_id, attempt_number, status,
                   owner_instance_id, lease_epoch, lease_expires_at, recovery_cause,
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
        fence: &AttemptFence,
        correlation: &Correlation,
    ) -> Result<DurableCorrelationRecord> {
        if correlation.attempt_id != fence.attempt_id {
            return Err(CoordinatorError::InvalidInput(
                "correlation attempt does not match its lease fence".to_string(),
            ));
        }
        let mut transaction = self.pool.begin().await?;
        lock_attempt_context(&mut transaction, fence).await?;
        let correlation_id = CorrelationRecordId::new(new_id());
        let row = sqlx::query_as::<_, CorrelationRow>(
            r#"
            INSERT INTO factory_runtime_correlations (
                correlation_id, job_id, operation_id, attempt_id,
                request_id, thread_id, turn_id, item_id
            )
            SELECT $1, $2, $3, $4, $5, $6, $7, $8
            FROM factory_attempts attempt
            JOIN factory_operations operation
                ON operation.operation_id = attempt.operation_id
            WHERE attempt.attempt_id = $4
              AND attempt.operation_id = $3
              AND operation.job_id = $2
            RETURNING correlation_id, job_id, operation_id, attempt_id,
                      request_id, thread_id, turn_id, item_id, observed_at
            "#,
        )
        .bind(correlation_id.as_str())
        .bind(correlation.job_id.as_str())
        .bind(correlation.operation_id.as_str())
        .bind(correlation.attempt_id.as_str())
        .bind(correlation.request_id.as_str())
        .bind(correlation.thread_id.as_ref().map(|id| id.as_str()))
        .bind(correlation.turn_id.as_ref().map(|id| id.as_str()))
        .bind(correlation.item_id.as_ref().map(|id| id.as_str()))
        .fetch_optional(&mut *transaction)
        .await?
        .ok_or(CoordinatorError::CorrelationMismatch)?;
        transaction.commit().await?;
        Ok(row.into())
    }

    /// Persists a progress checkpoint only while `fence` still owns a live
    /// generation of the attempt lease.
    pub async fn save_checkpoint(
        &self,
        fence: &AttemptFence,
        checkpoint: NewCheckpoint,
    ) -> Result<CheckpointRecord> {
        validate_checkpoint(fence, &checkpoint)?;
        let mut transaction = self.pool.begin().await?;
        lock_attempt_context(&mut transaction, fence).await?;
        let record = insert_checkpoint(&mut transaction, checkpoint).await?;
        transaction.commit().await?;
        Ok(record)
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
        fence: &AttemptFence,
        lease_seconds: u32,
    ) -> Result<AttemptRecord> {
        if lease_seconds == 0 {
            return Err(CoordinatorError::InvalidInput(
                "renewed leases must be at least one second".to_string(),
            ));
        }
        let lease_seconds = i64::from(lease_seconds);
        let lease_epoch = database_lease_epoch(fence.lease_epoch)?;
        let row = sqlx::query_as::<_, AttemptRow>(
            r#"
            UPDATE factory_attempts
            SET lease_expires_at = clock_timestamp() + ($4 * interval '1 second')
            WHERE attempt_id = $1
              AND owner_instance_id = $2
              AND lease_epoch = $3
              AND status = 'running'
              AND lease_expires_at > clock_timestamp()
            RETURNING attempt_id, operation_id, attempt_number, status,
                      owner_instance_id, lease_epoch, lease_expires_at, recovery_cause,
                      resumes_attempt_id, resumes_checkpoint_id, failure,
                      started_at, finished_at
            "#,
        )
        .bind(fence.attempt_id.as_str())
        .bind(fence.owner_instance_id.as_str())
        .bind(lease_epoch)
        .bind(lease_seconds)
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| CoordinatorError::AttemptLeaseUnavailable(fence.attempt_id.clone()))?;
        AttemptRecord::try_from(row)
    }

    /// Expires a live lease without settling its logical attempt. Graceful
    /// worker shutdown uses this only after the executor has drained and
    /// restored disposable state, allowing another process to reclaim the
    /// exact attempt and checkpoint immediately rather than waiting for the
    /// normal (potentially 900-second) lease duration.
    pub async fn relinquish_attempt(&self, fence: &AttemptFence) -> Result<()> {
        let lease_epoch = database_lease_epoch(fence.lease_epoch)?;
        let affected = sqlx::query(
            r#"
            UPDATE factory_attempts
            SET lease_expires_at = clock_timestamp()
            WHERE attempt_id = $1
              AND owner_instance_id = $2
              AND lease_epoch = $3
              AND status = 'running'
              AND lease_expires_at > clock_timestamp()
            "#,
        )
        .bind(fence.attempt_id.as_str())
        .bind(fence.owner_instance_id.as_str())
        .bind(lease_epoch)
        .execute(&self.pool)
        .await?;
        if affected.rows_affected() == 0 {
            return Err(CoordinatorError::AttemptLeaseUnavailable(
                fence.attempt_id.clone(),
            ));
        }
        Ok(())
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
        fence: &AttemptFence,
        thread_id: &ThreadId,
        state: Value,
    ) -> Result<FactoryThreadStateRecord> {
        let mut transaction = self.pool.begin().await?;
        lock_attempt_context(&mut transaction, fence).await?;
        let belongs_to_attempt = sqlx::query_scalar::<_, bool>(
            r#"
            SELECT EXISTS (
                SELECT 1
                FROM factory_runtime_correlations
                WHERE attempt_id = $1 AND thread_id = $2
            )
            "#,
        )
        .bind(fence.attempt_id.as_str())
        .bind(thread_id.as_str())
        .fetch_one(&mut *transaction)
        .await?;
        if !belongs_to_attempt {
            return Err(CoordinatorError::ThreadStateOwnershipMismatch {
                attempt_id: fence.attempt_id.clone(),
                thread_id: thread_id.clone(),
            });
        }
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
        .fetch_one(&mut *transaction)
        .await?;
        let record = FactoryThreadStateRecord::try_from(row)?;
        transaction.commit().await?;
        Ok(record)
    }
    /// Atomically persists an optional final checkpoint and settles a live
    /// attempt lease. A stale owner can do neither half of the operation.
    pub async fn settle_attempt(
        &self,
        fence: &AttemptFence,
        settlement: AttemptSettlement,
        checkpoint: Option<NewCheckpoint>,
    ) -> Result<Option<CheckpointRecord>> {
        self.settle_attempt_with_event(fence, settlement, checkpoint, None)
            .await
    }

    /// Atomically settles one attempt with its optional final checkpoint and
    /// completion event. The event cannot become visible before the operation
    /// status transition, and neither write survives if another fails.
    pub(crate) async fn settle_attempt_with_event(
        &self,
        fence: &AttemptFence,
        settlement: AttemptSettlement,
        checkpoint: Option<NewCheckpoint>,
        completion_event: Option<NewAttemptEvent>,
    ) -> Result<Option<CheckpointRecord>> {
        if let Some(checkpoint) = &checkpoint {
            validate_checkpoint(fence, checkpoint)?;
        }
        if completion_event.is_some() && !matches!(&settlement, AttemptSettlement::Succeeded) {
            return Err(CoordinatorError::InvalidInput(
                "only a successful attempt can record a completion event".to_string(),
            ));
        }
        let mut transaction = self.pool.begin().await?;
        let context = lock_attempt_context(&mut transaction, fence).await?;
        if context.job_status == "cancelling" {
            return Err(CoordinatorError::JobCancellationRequested(
                crate::ids::JobId::new(context.job_id),
            ));
        }
        let checkpoint = match checkpoint {
            Some(checkpoint) => Some(insert_checkpoint(&mut transaction, checkpoint).await?),
            None => None,
        };

        match settlement {
            AttemptSettlement::Succeeded => {
                sqlx::query(
                    r#"
                    UPDATE factory_attempts
                    SET status = 'succeeded', finished_at = clock_timestamp()
                    WHERE attempt_id = $1
                    "#,
                )
                .bind(fence.attempt_id.as_str())
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
                    super::environments::request_release_in_transaction(
                        &mut transaction,
                        &crate::ids::JobId::new(context.job_id.clone()),
                    )
                    .await?;
                }
            }
            AttemptSettlement::Failed(failure) => {
                Self::settle_failure(&mut transaction, fence, &context, failure).await?;
            }
        }
        if let Some(event) = completion_event {
            super::events::insert_attempt_event(&mut transaction, &context, fence, event).await?;
        }
        transaction.commit().await?;
        Ok(checkpoint)
    }

    async fn settle_failure(
        transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        fence: &AttemptFence,
        context: &AttemptContextRow,
        failure: AttemptFailure,
    ) -> Result<()> {
        let failure_payload = serde_json::to_value(&failure)?;
        let retry_at = match failure {
            AttemptFailure::RetryAt { retry_at, .. }
                if context.attempt_number < context.max_attempts =>
            {
                Some(retry_at)
            }
            AttemptFailure::RetryAt { .. } | AttemptFailure::Terminal { .. } => None,
        };
        sqlx::query(
            r#"
            UPDATE factory_attempts
            SET status = 'failed', failure = $2, finished_at = clock_timestamp()
            WHERE attempt_id = $1
            "#,
        )
        .bind(fence.attempt_id.as_str())
        .bind(&failure_payload)
        .execute(&mut **transaction)
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
            .execute(&mut **transaction)
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
            .execute(&mut **transaction)
            .await?;
            sqlx::query(
                r#"
                UPDATE factory_jobs
                SET status = 'failed', updated_at = clock_timestamp()
                WHERE job_id = $1
                "#,
            )
            .bind(&context.job_id)
            .execute(&mut **transaction)
            .await?;
            super::environments::request_release_in_transaction(
                transaction,
                &crate::ids::JobId::new(context.job_id.clone()),
            )
            .await?;
        }
        Ok(())
    }
}

#[derive(Debug, sqlx::FromRow)]
pub(super) struct AttemptContextRow {
    pub(super) operation_id: String,
    pub(super) job_id: String,
    pub(super) job_status: String,
    attempt_number: i32,
    max_attempts: i32,
}

pub(super) async fn lock_attempt_context(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    fence: &AttemptFence,
) -> Result<AttemptContextRow> {
    let lease_epoch = database_lease_epoch(fence.lease_epoch)?;
    sqlx::query_as::<_, AttemptContextRow>(
        r#"
        SELECT attempt.operation_id, operation.job_id, job.status AS job_status,
               attempt.attempt_number, operation.max_attempts
        FROM factory_jobs job
        JOIN factory_operations operation
            ON operation.job_id = job.job_id
        JOIN factory_attempts attempt
            ON attempt.operation_id = operation.operation_id
        WHERE attempt.attempt_id = $1
          AND attempt.owner_instance_id = $2
          AND attempt.lease_epoch = $3
          AND attempt.status = 'running'
          AND attempt.lease_expires_at > clock_timestamp()
        FOR UPDATE OF job, operation, attempt
        "#,
    )
    .bind(fence.attempt_id.as_str())
    .bind(fence.owner_instance_id.as_str())
    .bind(lease_epoch)
    .fetch_optional(&mut **transaction)
    .await?
    .ok_or_else(|| CoordinatorError::AttemptLeaseUnavailable(fence.attempt_id.clone()))
}

fn validate_checkpoint(fence: &AttemptFence, checkpoint: &NewCheckpoint) -> Result<()> {
    if checkpoint.attempt_id != fence.attempt_id {
        return Err(CoordinatorError::InvalidInput(
            "checkpoint attempt does not match its lease fence".to_string(),
        ));
    }
    if checkpoint.kind.trim().is_empty() {
        return Err(CoordinatorError::InvalidInput(
            "checkpoint kind must not be empty".to_string(),
        ));
    }
    Ok(())
}
async fn insert_checkpoint(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    checkpoint: NewCheckpoint,
) -> Result<CheckpointRecord> {
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
        .fetch_one(&mut **transaction)
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
    .fetch_one(&mut **transaction)
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
    .fetch_one(&mut **transaction)
    .await?;
    CheckpointRecord::try_from(row)
}
