use super::CoordinatorStore;
use crate::Result;
use crate::ids::JobId;
use sqlx::PgPool;
use sqlx::Postgres;
use sqlx::pool::PoolConnection;
use sqlx::postgres::PgAdvisoryLock;
use sqlx::postgres::PgAdvisoryLockGuard;

const WORKSPACE_EXECUTION_PREFIX: &str = "software-factory:workspace-execution:";
const WORKSPACE_REPOSITORY_PREFIX: &str = "software-factory:workspace-repository:";

/// Cross-process ownership of one Factory-managed workspace resource.
///
/// The guard owns a dedicated PostgreSQL connection. Process death closes the
/// connection and releases the advisory lock; normal completion releases it
/// explicitly after the Codex runtime and workspace cleanup have drained.
pub struct WorkspaceExecutionGuard {
    guard: Option<PgAdvisoryLockGuard<PoolConnection<Postgres>>>,
}

impl WorkspaceExecutionGuard {
    pub async fn release(mut self) -> Result<()> {
        if let Some(guard) = self.guard.take() {
            guard.release_now().await?;
        }
        Ok(())
    }
}

impl CoordinatorStore {
    /// Serializes every runtime and lifecycle mutation for one job workspace.
    pub async fn acquire_workspace_execution(
        &self,
        job_id: &JobId,
    ) -> Result<WorkspaceExecutionGuard> {
        acquire(
            &self.lock_pool,
            format!("{WORKSPACE_EXECUTION_PREFIX}{}", job_id.as_str()),
        )
        .await
    }

    /// Serializes materialization of the bare mirror shared by repositories.
    pub(crate) async fn acquire_workspace_repository(
        &self,
        repository: &str,
    ) -> Result<WorkspaceExecutionGuard> {
        acquire(
            &self.lock_pool,
            format!("{WORKSPACE_REPOSITORY_PREFIX}{repository}"),
        )
        .await
    }
}

async fn acquire(pool: &PgPool, key: String) -> Result<WorkspaceExecutionGuard> {
    let connection = pool.acquire().await?;
    let guard = PgAdvisoryLock::new(key).acquire(connection).await?;
    Ok(WorkspaceExecutionGuard { guard: Some(guard) })
}
