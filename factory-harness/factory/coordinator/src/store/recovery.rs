use super::CoordinatorStore;
use super::new_id;
use crate::domain::AttemptRecord;
use crate::domain::ClaimRequest;
use crate::domain::RecoveryCause;
use crate::domain::RecoveryLease;
use crate::domain::RecoverySelection;
use crate::domain::ResumeStrategy;
use crate::error::CoordinatorError;
use crate::error::Result;
use crate::ids::AttemptId;
use crate::ids::JobId;
use crate::ids::OperationId;
use crate::rows::AttemptRow;
use crate::rows::RecoverySelectionRow;

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
  AND j.status IN ('queued', 'running', 'cancelling')
  AND (
      j.kind <> 'factory.task'
      OR EXISTS (
          SELECT 1
          FROM factory_workspaces workspace
          WHERE workspace.job_id = j.job_id
            AND workspace.status = 'active'
      )
  )
  AND NOT EXISTS (
      SELECT 1
      FROM factory_operations predecessor
      WHERE predecessor.job_id = o.job_id
        AND predecessor.ordinal < o.ordinal
        AND predecessor.status <> 'succeeded'
  )
  AND (
      (
          j.status IN ('queued', 'running')
          AND
          COALESCE(previous_attempt.attempt_number, 0) < o.max_attempts
          AND (
              o.status = 'ready'
              OR (o.status = 'retry_wait' AND o.next_eligible_at <= clock_timestamp())
          )
      )
      OR (
          o.status = 'running'
          AND previous_attempt.status = 'running'
          AND previous_attempt.lease_expires_at <= clock_timestamp()
      )
  )
ORDER BY j.created_at, o.ordinal
LIMIT 1
"#;

const RECOVERY_CANDIDATE_SQL: &str = r#"
SELECT o.operation_id
FROM factory_jobs j
JOIN factory_operations o ON o.job_id = j.job_id
LEFT JOIN LATERAL (
    SELECT a.attempt_number, a.status, a.lease_expires_at
    FROM factory_attempts a
    WHERE a.operation_id = o.operation_id
    ORDER BY a.attempt_number DESC
    LIMIT 1
) previous_attempt ON true
WHERE ($1::TEXT IS NULL OR j.job_id = $1)
  AND ($2::TEXT IS NULL OR o.operation_id = $2)
  AND j.status IN ('queued', 'running', 'cancelling')
  AND (
      j.kind <> 'factory.task'
      OR EXISTS (
          SELECT 1
          FROM factory_workspaces workspace
          WHERE workspace.job_id = j.job_id
            AND workspace.status = 'active'
      )
  )
  AND (
      j.kind <> 'factory.task'
      OR (
          $3::TEXT IS NOT NULL
          AND $4::TEXT IS NOT NULL
          AND j.input #>> '{executionProfile,provider}' = $3
          AND j.input #>> '{executionProfile,model}' = $4
      )
  )
  AND NOT EXISTS (
      SELECT 1
      FROM factory_operations predecessor
      WHERE predecessor.job_id = o.job_id
        AND predecessor.ordinal < o.ordinal
        AND predecessor.status <> 'succeeded'
  )
  AND (
      (
          j.status IN ('queued', 'running')
          AND
          COALESCE(previous_attempt.attempt_number, 0) < o.max_attempts
          AND (
              o.status = 'ready'
              OR (o.status = 'retry_wait' AND o.next_eligible_at <= clock_timestamp())
          )
      )
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
impl CoordinatorStore {
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
        if request.lease_seconds == 0 {
            return Err(CoordinatorError::InvalidInput(
                "claimed leases must be at least one second".to_string(),
            ));
        }
        let (job_id, operation_id) = scope.database_ids();
        let provider = request
            .execution_profile
            .as_ref()
            .map(|profile| profile.provider.as_str());
        let model = request
            .execution_profile
            .as_ref()
            .map(|profile| profile.model.as_str());
        let mut transaction = self.pool.begin().await?;
        let Some(candidate_operation_id) = sqlx::query_scalar::<_, String>(RECOVERY_CANDIDATE_SQL)
            .bind(job_id)
            .bind(operation_id)
            .bind(provider)
            .bind(model)
            .fetch_optional(&mut *transaction)
            .await?
        else {
            transaction.rollback().await?;
            return Ok(None);
        };
        let Some(selection_row) = sqlx::query_as::<_, RecoverySelectionRow>(RECOVERY_SELECTION_SQL)
            .bind(job_id)
            .bind(candidate_operation_id.as_str())
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
            SET status = CASE
                    WHEN status = 'cancelling' THEN status
                    ELSE 'running'
                END,
                updated_at = clock_timestamp()
            WHERE job_id = $1 AND status IN ('queued', 'running', 'cancelling')
            "#,
        )
        .bind(selection.job_id.as_str())
        .execute(&mut *transaction)
        .await?;
        if claimed_job.rows_affected() == 0 {
            transaction.rollback().await?;
            return Ok(None);
        }
        let (resumes_attempt_id, resumes_checkpoint_id) = match &selection.resume {
            ResumeStrategy::Fresh => (None, None),
            ResumeStrategy::FromCheckpoint(checkpoint) => (
                Some(checkpoint.attempt_id.as_str()),
                Some(checkpoint.checkpoint_id.as_str()),
            ),
        };
        let lease_seconds = i64::from(request.lease_seconds);
        let attempt_row = if selection.cause == RecoveryCause::LeaseExpired {
            let Some(previous_attempt_id) = &selection.previous_attempt_id else {
                return Err(CoordinatorError::InvalidInput(
                    "expired lease recovery has no attempt".to_string(),
                ));
            };
            let Some(row) = sqlx::query_as::<_, AttemptRow>(
                r#"
                UPDATE factory_attempts
                SET owner_instance_id = $2,
                    lease_epoch = lease_epoch + 1,
                    lease_expires_at = clock_timestamp() + ($3 * interval '1 second'),
                    recovery_cause = 'lease_expired',
                    resumes_attempt_id = $4,
                    resumes_checkpoint_id = $5
                WHERE attempt_id = $1
                  AND status = 'running'
                  AND lease_expires_at <= clock_timestamp()
                RETURNING attempt_id, operation_id, attempt_number, status,
                          owner_instance_id, lease_epoch, lease_expires_at, recovery_cause,
                          resumes_attempt_id, resumes_checkpoint_id, failure,
                          started_at, finished_at
                "#,
            )
            .bind(previous_attempt_id.as_str())
            .bind(request.owner_instance_id.as_str())
            .bind(lease_seconds)
            .bind(resumes_attempt_id)
            .bind(resumes_checkpoint_id)
            .fetch_optional(&mut *transaction)
            .await?
            else {
                transaction.rollback().await?;
                return Ok(None);
            };
            row
        } else {
            let attempt_id = AttemptId::new(new_id());
            let attempt_number = i32::try_from(selection.next_attempt_number).map_err(|_| {
                CoordinatorError::NumericRange {
                    field: "attempt number",
                }
            })?;
            sqlx::query_as::<_, AttemptRow>(
                r#"
                INSERT INTO factory_attempts (
                    attempt_id, operation_id, attempt_number, status,
                    owner_instance_id, lease_epoch, lease_expires_at, recovery_cause,
                    resumes_attempt_id, resumes_checkpoint_id
                ) VALUES (
                    $1, $2, $3, 'running', $4, 1,
                    clock_timestamp() + ($5 * interval '1 second'),
                    $6, $7, $8
                )
                RETURNING attempt_id, operation_id, attempt_number, status,
                          owner_instance_id, lease_epoch, lease_expires_at, recovery_cause,
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
            .await?
        };
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
        let attempt = AttemptRecord::try_from(attempt_row)?;
        let fence = attempt.fence();
        Ok(Some(RecoveryLease {
            selection,
            attempt,
            fence,
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
