use super::CoordinatorStore;
use super::database_sequence;
use super::new_id;
use crate::domain::AttemptFence;
use crate::domain::ExecutionEnvironmentDesiredState;
use crate::domain::ExecutionEnvironmentRecord;
use crate::error::CoordinatorError;
use crate::error::Result;
use crate::ids::JobId;
use crate::rows::ExecutionEnvironmentRow;
use sqlx::Postgres;
use sqlx::Transaction;

const SELECT_ENVIRONMENT: &str = r#"
    SELECT job_id, environment_id, backend, generation, desired_state, status,
           backend_ref, url, error, created_at, updated_at
    FROM factory_execution_environments
    WHERE job_id = $1
"#;

const SELECT_ENVIRONMENT_FOR_UPDATE: &str = r#"
    SELECT job_id, environment_id, backend, generation, desired_state, status,
           backend_ref, url, error, created_at, updated_at
    FROM factory_execution_environments
    WHERE job_id = $1
    FOR UPDATE
"#;

const MARK_READY: &str = r#"
    UPDATE factory_execution_environments
    SET status = 'ready', backend_ref = $3, url = $4, error = NULL,
        updated_at = clock_timestamp()
    WHERE job_id = $1 AND generation = $2 AND desired_state = 'active'
    RETURNING job_id, environment_id, backend, generation, desired_state, status,
              backend_ref, url, error, created_at, updated_at
"#;

const MARK_FAILED: &str = r#"
    UPDATE factory_execution_environments
    SET status = 'failed', error = $3, updated_at = clock_timestamp()
    WHERE job_id = $1 AND generation = $2 AND desired_state = 'active'
    RETURNING job_id, environment_id, backend, generation, desired_state, status,
              backend_ref, url, error, created_at, updated_at
"#;

const REQUEST_RELEASE: &str = r#"
    UPDATE factory_execution_environments
    SET desired_state = 'released',
        status = CASE WHEN status = 'released' THEN 'released' ELSE 'releasing' END,
        error = NULL,
        updated_at = clock_timestamp()
    WHERE job_id = $1 AND generation = $2
    RETURNING job_id, environment_id, backend, generation, desired_state, status,
              backend_ref, url, error, created_at, updated_at
"#;

const MARK_RELEASED: &str = r#"
    UPDATE factory_execution_environments
    SET status = 'released', url = NULL, error = NULL,
        updated_at = clock_timestamp()
    WHERE job_id = $1 AND generation = $2 AND desired_state = 'released'
    RETURNING job_id, environment_id, backend, generation, desired_state, status,
              backend_ref, url, error, created_at, updated_at
"#;

const LIST_RELEASING: &str = r#"
    SELECT job_id, environment_id, backend, generation, desired_state, status,
           backend_ref, url, error, created_at, updated_at
    FROM factory_execution_environments
    WHERE backend = $1 AND desired_state = 'released' AND status = 'releasing'
    ORDER BY updated_at, job_id
"#;

impl CoordinatorStore {
    /// Creates the one durable environment identity for the fenced job, or
    /// returns the existing identity unchanged after a retry or lease transfer.
    pub async fn ensure_execution_environment(
        &self,
        fence: &AttemptFence,
        backend: &str,
    ) -> Result<ExecutionEnvironmentRecord> {
        validate_nonempty("execution environment backend", backend)?;
        let mut transaction = self.pool.begin().await?;
        let context = super::attempts::lock_attempt_context(&mut transaction, fence).await?;
        let job_id = JobId::new(context.job_id);
        if context.job_status == "cancelling" {
            return Err(CoordinatorError::JobCancellationRequested(job_id));
        }

        sqlx::query(
            r#"
            INSERT INTO factory_execution_environments (
                job_id, environment_id, backend, generation, desired_state, status
            ) VALUES ($1, $2, $3, 1, 'active', 'provisioning')
            ON CONFLICT (job_id) DO NOTHING
            "#,
        )
        .bind(job_id.as_str())
        .bind(new_id())
        .bind(backend)
        .execute(&mut *transaction)
        .await?;

        let environment = load_environment_for_update(&mut transaction, &job_id)
            .await?
            .ok_or_else(|| CoordinatorError::ExecutionEnvironmentNotFound(job_id.clone()))?;
        if environment.backend != backend {
            return Err(CoordinatorError::InvalidInput(format!(
                "job {} already uses execution environment backend {:?}",
                job_id, environment.backend
            )));
        }
        if environment.desired_state != ExecutionEnvironmentDesiredState::Active {
            return Err(CoordinatorError::InvalidInput(format!(
                "execution environment for job {} is awaiting release",
                job_id
            )));
        }
        transaction.commit().await?;
        Ok(environment)
    }

    pub async fn load_execution_environment(
        &self,
        job_id: &JobId,
    ) -> Result<Option<ExecutionEnvironmentRecord>> {
        sqlx::query_as::<_, ExecutionEnvironmentRow>(SELECT_ENVIRONMENT)
            .bind(job_id.as_str())
            .fetch_optional(&self.pool)
            .await?
            .map(ExecutionEnvironmentRecord::try_from)
            .transpose()
    }

    /// Persists a backend object's stable address before the provisioner makes
    /// an external call. The first locator wins for one active generation;
    /// retries and lease transfers receive that exact persisted value.
    pub async fn reserve_execution_environment_locator(
        &self,
        fence: &AttemptFence,
        generation: u64,
        locator: &str,
    ) -> Result<ExecutionEnvironmentRecord> {
        validate_nonempty("execution environment locator", locator)?;
        let generation = database_sequence(generation, "execution environment generation")?;
        let mut transaction = self.pool.begin().await?;
        let context = super::attempts::lock_attempt_context(&mut transaction, fence).await?;
        let job_id = JobId::new(context.job_id);
        if context.job_status == "cancelling" {
            return Err(CoordinatorError::JobCancellationRequested(job_id));
        }

        let current = load_environment_for_update(&mut transaction, &job_id)
            .await?
            .ok_or_else(|| CoordinatorError::ExecutionEnvironmentNotFound(job_id.clone()))?;
        let requested_generation =
            u64::try_from(generation).map_err(|_| CoordinatorError::NumericRange {
                field: "execution environment generation",
            })?;
        if current.generation != requested_generation {
            return Err(CoordinatorError::ExecutionEnvironmentGenerationStale {
                job_id,
                generation: requested_generation,
            });
        }
        if current.desired_state != ExecutionEnvironmentDesiredState::Active {
            return Err(CoordinatorError::InvalidInput(format!(
                "execution environment for job {} has desired state {:?}",
                job_id, current.desired_state
            )));
        }

        let environment = if current.backend_ref.is_some() {
            current
        } else {
            sqlx::query_as::<_, ExecutionEnvironmentRow>(
                r#"
                UPDATE factory_execution_environments
                SET backend_ref = $3, updated_at = clock_timestamp()
                WHERE job_id = $1
                  AND generation = $2
                  AND desired_state = 'active'
                  AND backend_ref IS NULL
                RETURNING job_id, environment_id, backend, generation, desired_state, status,
                          backend_ref, url, error, created_at, updated_at
                "#,
            )
            .bind(job_id.as_str())
            .bind(generation)
            .bind(locator)
            .fetch_one(&mut *transaction)
            .await?
            .try_into()?
        };
        transaction.commit().await?;
        Ok(environment)
    }

    /// Returns durable teardown work left by terminal settlement, queued
    /// cancellation, or a worker that stopped after requesting release.
    pub async fn list_releasing_execution_environments(
        &self,
        backend: &str,
    ) -> Result<Vec<ExecutionEnvironmentRecord>> {
        validate_nonempty("execution environment backend", backend)?;
        sqlx::query_as::<_, ExecutionEnvironmentRow>(LIST_RELEASING)
            .bind(backend)
            .fetch_all(&self.pool)
            .await?
            .into_iter()
            .map(ExecutionEnvironmentRecord::try_from)
            .collect()
    }

    /// Moves the current generation to `releasing` while the cancelling
    /// attempt still owns its fence. The runner uses this after Codex and
    /// worktree cleanup and before acknowledging terminal cancellation.
    pub async fn request_cancelling_execution_environment_release(
        &self,
        fence: &AttemptFence,
    ) -> Result<Option<ExecutionEnvironmentRecord>> {
        let mut transaction = self.pool.begin().await?;
        let context = super::attempts::lock_attempt_context(&mut transaction, fence).await?;
        let job_id = JobId::new(context.job_id);
        if context.job_status != "cancelling" {
            return Err(CoordinatorError::JobNotCancellable {
                job_id,
                state: context.job_status,
            });
        }
        let Some(current) = load_environment_for_update(&mut transaction, &job_id).await? else {
            transaction.commit().await?;
            return Ok(None);
        };
        let generation = database_sequence(current.generation, "execution environment generation")?;
        let environment = sqlx::query_as::<_, ExecutionEnvironmentRow>(REQUEST_RELEASE)
            .bind(job_id.as_str())
            .bind(generation)
            .fetch_optional(&mut *transaction)
            .await?;
        let environment = require_generation_match(
            &mut transaction,
            &job_id,
            generation,
            environment,
            ExecutionEnvironmentDesiredState::Released,
        )
        .await?;
        transaction.commit().await?;
        Ok(Some(environment))
    }

    /// Publishes the backend handle and Codex execution URL while the same
    /// attempt lease and environment generation still own activation.
    pub async fn mark_execution_environment_ready(
        &self,
        fence: &AttemptFence,
        generation: u64,
        backend_ref: &str,
        url: &str,
    ) -> Result<ExecutionEnvironmentRecord> {
        validate_nonempty("execution environment backend reference", backend_ref)?;
        validate_nonempty("execution environment URL", url)?;
        let generation = database_sequence(generation, "execution environment generation")?;
        let mut transaction = self.pool.begin().await?;
        let context = super::attempts::lock_attempt_context(&mut transaction, fence).await?;
        let job_id = JobId::new(context.job_id);
        if context.job_status == "cancelling" {
            return Err(CoordinatorError::JobCancellationRequested(job_id));
        }
        let environment = sqlx::query_as::<_, ExecutionEnvironmentRow>(MARK_READY)
            .bind(job_id.as_str())
            .bind(generation)
            .bind(backend_ref)
            .bind(url)
            .fetch_optional(&mut *transaction)
            .await?;
        let environment = require_generation_match(
            &mut transaction,
            &job_id,
            generation,
            environment,
            ExecutionEnvironmentDesiredState::Active,
        )
        .await?;
        transaction.commit().await?;
        Ok(environment)
    }

    /// Records a provisioning failure while the same attempt lease and
    /// environment generation still own activation.
    pub async fn mark_execution_environment_failed(
        &self,
        fence: &AttemptFence,
        generation: u64,
        error: &str,
    ) -> Result<ExecutionEnvironmentRecord> {
        validate_nonempty("execution environment error", error)?;
        let generation = database_sequence(generation, "execution environment generation")?;
        let mut transaction = self.pool.begin().await?;
        let context = super::attempts::lock_attempt_context(&mut transaction, fence).await?;
        let job_id = JobId::new(context.job_id);
        if context.job_status == "cancelling" {
            return Err(CoordinatorError::JobCancellationRequested(job_id));
        }
        let environment = sqlx::query_as::<_, ExecutionEnvironmentRow>(MARK_FAILED)
            .bind(job_id.as_str())
            .bind(generation)
            .bind(error)
            .fetch_optional(&mut *transaction)
            .await?;
        let environment = require_generation_match(
            &mut transaction,
            &job_id,
            generation,
            environment,
            ExecutionEnvironmentDesiredState::Active,
        )
        .await?;
        transaction.commit().await?;
        Ok(environment)
    }

    /// Requests teardown only if the caller observed the current generation.
    /// Terminal job transactions use the same transition internally.
    pub async fn request_execution_environment_release(
        &self,
        job_id: &JobId,
        generation: u64,
    ) -> Result<ExecutionEnvironmentRecord> {
        let generation = database_sequence(generation, "execution environment generation")?;
        let mut transaction = self.pool.begin().await?;
        let job_status = sqlx::query_scalar::<_, String>(
            "SELECT status FROM factory_jobs WHERE job_id = $1 FOR UPDATE",
        )
        .bind(job_id.as_str())
        .fetch_optional(&mut *transaction)
        .await?
        .ok_or_else(|| CoordinatorError::JobNotFound(job_id.clone()))?;
        if !matches!(job_status.as_str(), "succeeded" | "failed" | "cancelled") {
            return Err(CoordinatorError::InvalidInput(format!(
                "execution environment for nonterminal job {} cannot be released",
                job_id
            )));
        }
        let environment = sqlx::query_as::<_, ExecutionEnvironmentRow>(REQUEST_RELEASE)
            .bind(job_id.as_str())
            .bind(generation)
            .fetch_optional(&mut *transaction)
            .await?;
        let environment = require_generation_match(
            &mut transaction,
            job_id,
            generation,
            environment,
            ExecutionEnvironmentDesiredState::Released,
        )
        .await?;
        transaction.commit().await?;
        Ok(environment)
    }

    /// Completes teardown only for the generation the provisioner released.
    pub async fn mark_execution_environment_released(
        &self,
        job_id: &JobId,
        generation: u64,
    ) -> Result<ExecutionEnvironmentRecord> {
        let generation = database_sequence(generation, "execution environment generation")?;
        let mut transaction = self.pool.begin().await?;
        let environment = sqlx::query_as::<_, ExecutionEnvironmentRow>(MARK_RELEASED)
            .bind(job_id.as_str())
            .bind(generation)
            .fetch_optional(&mut *transaction)
            .await?;
        let environment = require_generation_match(
            &mut transaction,
            job_id,
            generation,
            environment,
            ExecutionEnvironmentDesiredState::Released,
        )
        .await?;
        transaction.commit().await?;
        Ok(environment)
    }
}

pub(super) async fn request_release_in_transaction(
    transaction: &mut Transaction<'_, Postgres>,
    job_id: &JobId,
) -> Result<()> {
    sqlx::query(
        r#"
        UPDATE factory_execution_environments
        SET desired_state = 'released',
            status = CASE WHEN status = 'released' THEN 'released' ELSE 'releasing' END,
            error = NULL,
            updated_at = clock_timestamp()
        WHERE job_id = $1
        "#,
    )
    .bind(job_id.as_str())
    .execute(&mut **transaction)
    .await?;
    Ok(())
}

pub(super) async fn reactivate_in_transaction(
    transaction: &mut Transaction<'_, Postgres>,
    job_id: &JobId,
) -> Result<()> {
    let updated = sqlx::query(
        r#"
        UPDATE factory_execution_environments
        SET generation = generation + 1,
            desired_state = 'active',
            status = 'provisioning',
            backend_ref = NULL,
            url = NULL,
            error = NULL,
            updated_at = clock_timestamp()
        WHERE job_id = $1
          AND desired_state = 'released'
          AND status = 'released'
        "#,
    )
    .bind(job_id.as_str())
    .execute(&mut **transaction)
    .await?;
    if updated.rows_affected() == 0 {
        let exists = sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS (SELECT 1 FROM factory_execution_environments WHERE job_id = $1)",
        )
        .bind(job_id.as_str())
        .fetch_one(&mut **transaction)
        .await?;
        if exists {
            return Err(CoordinatorError::InvalidInput(format!(
                "execution environment for job {} must finish release before continuation",
                job_id
            )));
        }
    }
    Ok(())
}

async fn load_environment_for_update(
    transaction: &mut Transaction<'_, Postgres>,
    job_id: &JobId,
) -> Result<Option<ExecutionEnvironmentRecord>> {
    sqlx::query_as::<_, ExecutionEnvironmentRow>(SELECT_ENVIRONMENT_FOR_UPDATE)
        .bind(job_id.as_str())
        .fetch_optional(&mut **transaction)
        .await?
        .map(ExecutionEnvironmentRecord::try_from)
        .transpose()
}

async fn require_generation_match(
    transaction: &mut Transaction<'_, Postgres>,
    job_id: &JobId,
    generation: i64,
    updated: Option<ExecutionEnvironmentRow>,
    desired_state: ExecutionEnvironmentDesiredState,
) -> Result<ExecutionEnvironmentRecord> {
    if let Some(updated) = updated {
        return ExecutionEnvironmentRecord::try_from(updated);
    }
    let current = load_environment_for_update(transaction, job_id)
        .await?
        .ok_or_else(|| CoordinatorError::ExecutionEnvironmentNotFound(job_id.clone()))?;
    let generation = u64::try_from(generation).map_err(|_| CoordinatorError::NumericRange {
        field: "execution environment generation",
    })?;
    if current.generation != generation {
        return Err(CoordinatorError::ExecutionEnvironmentGenerationStale {
            job_id: job_id.clone(),
            generation,
        });
    }
    if current.desired_state != desired_state {
        return Err(CoordinatorError::InvalidInput(format!(
            "execution environment for job {} has desired state {:?}",
            job_id, current.desired_state
        )));
    }
    Err(CoordinatorError::InvalidInput(format!(
        "execution environment for job {} rejected lifecycle transition",
        job_id
    )))
}

fn validate_nonempty(field: &str, value: &str) -> Result<()> {
    if value.trim().is_empty() {
        return Err(CoordinatorError::InvalidInput(format!(
            "{field} must not be empty"
        )));
    }
    Ok(())
}
