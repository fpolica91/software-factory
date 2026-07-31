use sqlx::PgPool;

const MIGRATION_LOCK_KEY: i64 = 0x4661_6374_6f72_7944;

const MIGRATION_1: &str = r#"
CREATE TABLE factory_jobs (
    job_id TEXT PRIMARY KEY,
    kind TEXT NOT NULL CHECK (length(kind) > 0),
    input JSONB NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('queued', 'running', 'succeeded', 'failed', 'cancelled')),
    workflow_run_id TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp()
);

CREATE TABLE factory_operations (
    operation_id TEXT PRIMARY KEY,
    job_id TEXT NOT NULL REFERENCES factory_jobs(job_id) ON DELETE CASCADE,
    ordinal INTEGER NOT NULL CHECK (ordinal >= 0),
    kind TEXT NOT NULL CHECK (length(kind) > 0),
    input JSONB NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('ready', 'running', 'retry_wait', 'succeeded', 'failed', 'cancelled')),
    max_attempts INTEGER NOT NULL CHECK (max_attempts > 0),
    next_eligible_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    UNIQUE (job_id, ordinal),
    UNIQUE (operation_id, job_id)
);

CREATE TABLE factory_attempts (
    attempt_id TEXT PRIMARY KEY,
    operation_id TEXT NOT NULL REFERENCES factory_operations(operation_id) ON DELETE CASCADE,
    attempt_number INTEGER NOT NULL CHECK (attempt_number > 0),
    status TEXT NOT NULL CHECK (status IN ('running', 'succeeded', 'failed', 'abandoned')),
    owner_instance_id TEXT NOT NULL,
    lease_expires_at TIMESTAMPTZ NOT NULL,
    recovery_cause TEXT NOT NULL CHECK (recovery_cause IN ('new_operation', 'retry_scheduled', 'lease_expired')),
    resumes_attempt_id TEXT REFERENCES factory_attempts(attempt_id),
    resumes_checkpoint_id TEXT,
    failure JSONB,
    started_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    finished_at TIMESTAMPTZ,
    UNIQUE (operation_id, attempt_number),
    UNIQUE (attempt_id, operation_id)
);

CREATE TABLE factory_runtime_correlations (
    correlation_id TEXT PRIMARY KEY,
    job_id TEXT NOT NULL,
    operation_id TEXT NOT NULL,
    attempt_id TEXT NOT NULL,
    workflow_run_id TEXT,
    task_run_external_id TEXT,
    request_id TEXT NOT NULL,
    thread_id TEXT,
    turn_id TEXT,
    item_id TEXT,
    observed_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    FOREIGN KEY (operation_id, job_id)
        REFERENCES factory_operations(operation_id, job_id) ON DELETE CASCADE,
    FOREIGN KEY (attempt_id, operation_id)
        REFERENCES factory_attempts(attempt_id, operation_id) ON DELETE CASCADE
);

CREATE TABLE factory_checkpoints (
    checkpoint_id TEXT PRIMARY KEY,
    attempt_id TEXT NOT NULL REFERENCES factory_attempts(attempt_id) ON DELETE CASCADE,
    sequence BIGINT NOT NULL CHECK (sequence > 0),
    kind TEXT NOT NULL CHECK (length(kind) > 0),
    payload JSONB NOT NULL,
    workspace_root TEXT,
    workspace_revision TEXT,
    correlation_id TEXT REFERENCES factory_runtime_correlations(correlation_id),
    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    UNIQUE (attempt_id, sequence)
);

ALTER TABLE factory_attempts
    ADD CONSTRAINT factory_attempts_resume_checkpoint_fk
    FOREIGN KEY (resumes_checkpoint_id)
    REFERENCES factory_checkpoints(checkpoint_id);

CREATE INDEX factory_operations_recovery_idx
    ON factory_operations (status, next_eligible_at, job_id, ordinal);
CREATE INDEX factory_attempts_operation_latest_idx
    ON factory_attempts (operation_id, attempt_number DESC);
CREATE INDEX factory_attempts_expired_lease_idx
    ON factory_attempts (lease_expires_at)
    WHERE status = 'running';
CREATE INDEX factory_checkpoints_attempt_latest_idx
    ON factory_checkpoints (attempt_id, sequence DESC);
CREATE INDEX factory_correlations_attempt_observed_idx
    ON factory_runtime_correlations (attempt_id, observed_at DESC);
"#;

const MIGRATION_2: &str = r#"
CREATE TABLE factory_thread_states (
    thread_id TEXT PRIMARY KEY,
    state JSONB NOT NULL,
    revision BIGINT NOT NULL DEFAULT 1 CHECK (revision > 0),
    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp()
);
"#;

const MIGRATION_3: &str = r#"
CREATE TABLE factory_workspaces (
    job_id TEXT PRIMARY KEY REFERENCES factory_jobs(job_id) ON DELETE CASCADE,
    repository TEXT NOT NULL CHECK (length(repository) > 0),
    base_ref TEXT NOT NULL CHECK (length(base_ref) > 0),
    branch_name TEXT NOT NULL CHECK (length(branch_name) > 0),
    root TEXT NOT NULL CHECK (length(root) > 0),
    revision TEXT NOT NULL CHECK (length(revision) > 0),
    status TEXT NOT NULL CHECK (status IN ('active', 'removed')),
    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp()
);
"#;

const MIGRATION_4: &str = r#"
CREATE TABLE factory_pending_requests (
    pending_request_id TEXT PRIMARY KEY,
    attempt_id TEXT NOT NULL REFERENCES factory_attempts(attempt_id) ON DELETE CASCADE,
    request_id JSONB NOT NULL CHECK (jsonb_typeof(request_id) IN ('string', 'number')),
    method TEXT NOT NULL CHECK (length(method) > 0),
    params JSONB NOT NULL,
    response JSONB,
    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    resolved_at TIMESTAMPTZ,
    CHECK ((response IS NULL) = (resolved_at IS NULL)),
    UNIQUE (attempt_id, request_id)
);

CREATE INDEX factory_pending_requests_actionable_idx
    ON factory_pending_requests (created_at, attempt_id)
    WHERE response IS NULL;
"#;

const MIGRATION_5: &str = r#"
CREATE UNIQUE INDEX factory_jobs_workflow_run_id_unique_idx
    ON factory_jobs (workflow_run_id)
    WHERE workflow_run_id IS NOT NULL;
"#;

const MIGRATIONS: &[(i32, &str)] = &[
    (1, MIGRATION_1),
    (2, MIGRATION_2),
    (3, MIGRATION_3),
    (4, MIGRATION_4),
    (5, MIGRATION_5),
];

pub(crate) async fn migrate(pool: &PgPool) -> Result<(), sqlx::Error> {
    let mut transaction = pool.begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(MIGRATION_LOCK_KEY)
        .execute(&mut *transaction)
        .await?;
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS factory_coordinator_schema_migrations (
            version INTEGER PRIMARY KEY,
            applied_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp()
        )
        "#,
    )
    .execute(&mut *transaction)
    .await?;

    for (version, migration) in MIGRATIONS {
        let is_applied = sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS (SELECT 1 FROM factory_coordinator_schema_migrations WHERE version = $1)",
        )
        .bind(version)
        .fetch_one(&mut *transaction)
        .await?;

        if !is_applied {
            sqlx::raw_sql(*migration).execute(&mut *transaction).await?;
            sqlx::query("INSERT INTO factory_coordinator_schema_migrations (version) VALUES ($1)")
                .bind(version)
                .execute(&mut *transaction)
                .await?;
        }
    }

    transaction.commit().await
}
