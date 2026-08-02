use super::CoordinatorStore;
use super::attempts::AttemptContextRow;
use super::attempts::lock_attempt_context;
use super::database_sequence;
use crate::domain::AttemptFence;
use crate::domain::JobEventPage;
use crate::domain::JobEventRecord;
use crate::domain::NewAttemptEvent;
use crate::domain::NewJobEvent;
use crate::error::CoordinatorError;
use crate::error::Result;
use crate::ids::JobId;
use crate::rows::JobEventRow;

impl CoordinatorStore {
    /// Appends a job lifecycle event that is not produced by an execution
    /// attempt. Attempt-owned events must use [`Self::append_attempt_event`].
    pub async fn append_job_event(&self, event: NewJobEvent) -> Result<JobEventRecord> {
        validate_event_kind(&event.kind)?;
        let row = sqlx::query_as::<_, JobEventRow>(
            r#"
            INSERT INTO factory_job_events (job_id, kind, payload)
            SELECT job_id, $2, $3
            FROM factory_jobs
            WHERE job_id = $1
            RETURNING sequence, job_id, operation_id, attempt_id,
                      kind, payload, created_at
            "#,
        )
        .bind(event.job_id.as_str())
        .bind(&event.kind)
        .bind(&event.payload)
        .fetch_optional(&self.pool)
        .await?
        .ok_or(CoordinatorError::JobNotFound(event.job_id))?;
        JobEventRecord::try_from(row)
    }

    /// Appends an execution event only while `fence` owns the live attempt.
    pub async fn append_attempt_event(
        &self,
        fence: &AttemptFence,
        event: NewAttemptEvent,
    ) -> Result<JobEventRecord> {
        let mut transaction = self.pool.begin().await?;
        let context = lock_attempt_context(&mut transaction, fence).await?;
        let record = insert_attempt_event(&mut transaction, &context, fence, event).await?;
        transaction.commit().await?;
        Ok(record)
    }

    /// Lists events after an exclusive monotonic cursor in stable order.
    pub async fn list_job_events(
        &self,
        job_id: &JobId,
        after: u64,
        limit: u32,
    ) -> Result<JobEventPage> {
        if limit == 0 || limit > 1_000 {
            return Err(CoordinatorError::InvalidInput(
                "job event page limit must be between 1 and 1000".to_string(),
            ));
        }
        let exists = sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS (SELECT 1 FROM factory_jobs WHERE job_id = $1)",
        )
        .bind(job_id.as_str())
        .fetch_one(&self.pool)
        .await?;
        if !exists {
            return Err(CoordinatorError::JobNotFound(job_id.clone()));
        }

        let after = database_sequence(after, "job event cursor")?;
        let rows = sqlx::query_as::<_, JobEventRow>(
            r#"
            SELECT sequence, job_id, operation_id, attempt_id,
                   kind, payload, created_at
            FROM factory_job_events
            WHERE job_id = $1 AND sequence > $2
            ORDER BY sequence
            LIMIT $3
            "#,
        )
        .bind(job_id.as_str())
        .bind(after)
        .bind(i64::from(limit))
        .fetch_all(&self.pool)
        .await?;
        let events = rows
            .into_iter()
            .map(JobEventRecord::try_from)
            .collect::<Result<Vec<_>>>()?;
        let next_cursor = events.last().map_or(after as u64, |event| event.sequence);
        Ok(JobEventPage {
            events,
            next_cursor,
        })
    }
}

pub(super) async fn insert_attempt_event(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    context: &AttemptContextRow,
    fence: &AttemptFence,
    event: NewAttemptEvent,
) -> Result<JobEventRecord> {
    validate_event_kind(&event.kind)?;
    validate_deduplication_key(event.deduplication_key.as_deref())?;
    let inserted = sqlx::query_as::<_, JobEventRow>(
        r#"
            INSERT INTO factory_job_events (
                job_id, operation_id, attempt_id, kind, payload,
                deduplication_key
            ) VALUES ($1, $2, $3, $4, $5, $6)
            ON CONFLICT (job_id, deduplication_key)
                WHERE deduplication_key IS NOT NULL
                DO NOTHING
            RETURNING sequence, job_id, operation_id, attempt_id,
                      kind, payload, created_at
            "#,
    )
    .bind(&context.job_id)
    .bind(&context.operation_id)
    .bind(fence.attempt_id.as_str())
    .bind(&event.kind)
    .bind(&event.payload)
    .bind(event.deduplication_key.as_deref())
    .fetch_optional(&mut **transaction)
    .await?;
    let row = match inserted {
        Some(row) => row,
        None => {
            let key = event
                .deduplication_key
                .as_deref()
                .expect("only a non-null deduplication key can conflict");
            let existing = sqlx::query_as::<_, JobEventRow>(
                r#"
                    SELECT sequence, job_id, operation_id, attempt_id,
                           kind, payload, created_at
                    FROM factory_job_events
                    WHERE job_id = $1 AND deduplication_key = $2
                    "#,
            )
            .bind(&context.job_id)
            .bind(key)
            .fetch_one(&mut **transaction)
            .await?;
            if existing.kind != event.kind || existing.payload != event.payload {
                return Err(CoordinatorError::InvalidInput(format!(
                    "job event deduplication key {key:?} was reused for different content"
                )));
            }
            existing
        }
    };
    JobEventRecord::try_from(row)
}

fn validate_event_kind(kind: &str) -> Result<()> {
    if kind.trim().is_empty() {
        return Err(CoordinatorError::InvalidInput(
            "job event kind must not be empty".to_string(),
        ));
    }
    Ok(())
}

fn validate_deduplication_key(key: Option<&str>) -> Result<()> {
    if key.is_some_and(|key| key.trim().is_empty()) {
        return Err(CoordinatorError::InvalidInput(
            "job event deduplication key must not be empty".to_string(),
        ));
    }
    Ok(())
}
