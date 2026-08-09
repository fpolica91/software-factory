use sqlx::PgPool;

const MIGRATION_LOCK_KEY: i64 = 0x4661_6374_6f72_7944;

const MIGRATION_1: &str = r#"
CREATE TABLE factory_jobs (
    job_id TEXT PRIMARY KEY,
    kind TEXT NOT NULL CHECK (length(kind) > 0),
    input JSONB NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('queued', 'running', 'cancelling', 'succeeded', 'failed', 'cancelled')),
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

const MIGRATION_6: &str = r#"
ALTER TABLE factory_attempts
    ADD COLUMN lease_epoch BIGINT NOT NULL DEFAULT 1
    CHECK (lease_epoch > 0);
"#;

const MIGRATION_7: &str = r#"
CREATE TABLE factory_job_events (
    sequence BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    job_id TEXT NOT NULL REFERENCES factory_jobs(job_id) ON DELETE CASCADE,
    operation_id TEXT,
    attempt_id TEXT,
    kind TEXT NOT NULL CHECK (length(kind) > 0),
    payload JSONB NOT NULL,
    deduplication_key TEXT CHECK (deduplication_key IS NULL OR length(deduplication_key) > 0),
    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    CHECK (attempt_id IS NULL OR operation_id IS NOT NULL),
    FOREIGN KEY (operation_id, job_id)
        REFERENCES factory_operations(operation_id, job_id) ON DELETE CASCADE,
    FOREIGN KEY (attempt_id, operation_id)
        REFERENCES factory_attempts(attempt_id, operation_id) ON DELETE CASCADE
);

CREATE INDEX factory_job_events_job_sequence_idx
    ON factory_job_events (job_id, sequence);
CREATE UNIQUE INDEX factory_job_events_job_deduplication_idx
    ON factory_job_events (job_id, deduplication_key)
    WHERE deduplication_key IS NOT NULL;
"#;

// Remove the unused coordinator approval workflow from databases created by
// older development builds. Native autonomous sessions answer Codex server
// requests directly and never read this table.
const REMOVE_PENDING_REQUESTS: &str = "DROP TABLE IF EXISTS factory_pending_requests CASCADE;";

// Drop external-workflow columns left by the deleted Hatchet-backed runner.
// Native Factory jobs and Codex correlations use their own durable IDs.
const REMOVE_EXTERNAL_WORKFLOW_IDS: &str = r#"
DROP INDEX IF EXISTS factory_jobs_workflow_run_id_unique_idx;
ALTER TABLE factory_jobs DROP COLUMN IF EXISTS workflow_run_id;
ALTER TABLE factory_runtime_correlations DROP COLUMN IF EXISTS workflow_run_id;
ALTER TABLE factory_runtime_correlations DROP COLUMN IF EXISTS task_run_external_id;
"#;

const ADD_JOB_EVENT_DEDUPLICATION: &str = r#"
ALTER TABLE factory_job_events
    ADD COLUMN IF NOT EXISTS deduplication_key TEXT
    CHECK (deduplication_key IS NULL OR length(deduplication_key) > 0);
CREATE UNIQUE INDEX IF NOT EXISTS factory_job_events_job_deduplication_idx
    ON factory_job_events (job_id, deduplication_key)
    WHERE deduplication_key IS NOT NULL;
"#;

// Repository identity is deliberately separate from its clone transport.
// Every pre-migration workspace is assigned a job-scoped legacy identity so
// the fixed historical `/workspace/project` locator can never alias a newly
// identified host checkout. The original resolved revision becomes its
// immutable result-application base.
const ADD_WORKSPACE_IDENTITY_AND_BASE_REVISION: &str = r#"
ALTER TABLE factory_workspaces
    ADD COLUMN IF NOT EXISTS repository_id TEXT;
ALTER TABLE factory_workspaces
    ADD COLUMN IF NOT EXISTS base_revision TEXT;

UPDATE factory_workspaces
SET repository_id = 'legacy:' || job_id
WHERE repository_id IS NULL;
UPDATE factory_workspaces
SET base_revision = revision
WHERE base_revision IS NULL;

ALTER TABLE factory_workspaces
    ALTER COLUMN repository_id SET NOT NULL;
ALTER TABLE factory_workspaces
    ALTER COLUMN base_revision SET NOT NULL;
ALTER TABLE factory_workspaces
    ADD CONSTRAINT factory_workspaces_repository_id_nonempty
    CHECK (length(repository_id) > 0);
ALTER TABLE factory_workspaces
    ADD CONSTRAINT factory_workspaces_base_revision_nonempty
    CHECK (length(base_revision) > 0);
"#;

// A running job keeps its live attempt fenced while the worker interrupts and
// drains Codex, restores disposable Plan/review state, and then acknowledges
// cancellation. Queued jobs still move directly to `cancelled`.
const ADD_CANCELLATION_REQUEST_STATE: &str = r#"
ALTER TABLE factory_jobs
    DROP CONSTRAINT IF EXISTS factory_jobs_status_check;
ALTER TABLE factory_jobs
    ADD CONSTRAINT factory_jobs_status_check
    CHECK (status IN ('queued', 'running', 'cancelling', 'succeeded', 'failed', 'cancelled'));
"#;

// Early Rust builds pinned Anthropic jobs under the UI label `claude` even
// though the canonical provider profile is `anthropic`. Normalize retained
// durable inputs once so claim and runtime boundaries remain exact matches.
const NORMALIZE_ANTHROPIC_JOB_PROFILE: &str = r#"
UPDATE factory_jobs
SET input = jsonb_set(
    input,
    '{executionProfile,provider}',
    to_jsonb('anthropic'::TEXT),
    false
)
WHERE kind = 'factory.task'
  AND input #>> '{executionProfile,provider}' = 'claude';
"#;

// One stable environment identity belongs to one durable job. Its generation
// changes only when continuation reactivates a terminal job, fencing teardown
// work retained from the preceding generation.
const ADD_EXECUTION_ENVIRONMENTS: &str = r#"
CREATE TABLE factory_execution_environments (
    job_id TEXT PRIMARY KEY REFERENCES factory_jobs(job_id) ON DELETE CASCADE,
    environment_id TEXT NOT NULL UNIQUE CHECK (length(environment_id) > 0),
    backend TEXT NOT NULL CHECK (length(backend) > 0),
    generation BIGINT NOT NULL CHECK (generation > 0),
    desired_state TEXT NOT NULL CHECK (desired_state IN ('active', 'released')),
    status TEXT NOT NULL CHECK (status IN ('provisioning', 'ready', 'releasing', 'released', 'failed')),
    backend_ref TEXT CHECK (backend_ref IS NULL OR length(backend_ref) > 0),
    url TEXT CHECK (url IS NULL OR length(url) > 0),
    error TEXT CHECK (error IS NULL OR length(error) > 0),
    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp()
);
"#;

const MIGRATIONS: &[(i32, &str)] = &[
    (1, MIGRATION_1),
    (2, MIGRATION_2),
    (3, MIGRATION_3),
    (6, MIGRATION_6),
    (7, MIGRATION_7),
    (8, REMOVE_PENDING_REQUESTS),
    (9, REMOVE_EXTERNAL_WORKFLOW_IDS),
    (10, ADD_JOB_EVENT_DEDUPLICATION),
    (11, ADD_WORKSPACE_IDENTITY_AND_BASE_REVISION),
    (12, ADD_CANCELLATION_REQUEST_STATE),
    (13, NORMALIZE_ANTHROPIC_JOB_PROFILE),
    (14, ADD_EXECUTION_ENVIRONMENTS),
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
