mod attempts;
mod environments;
mod events;
mod jobs;
mod recovery;
mod workspace_locks;

pub use workspace_locks::WorkspaceExecutionGuard;

use crate::error::CoordinatorError;
use crate::error::Result;
use crate::schema;
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
use uuid::Uuid;

const DEFAULT_QUERY_CONNECTIONS: u32 = 8;
const DEFAULT_LOCK_CONNECTIONS: u32 = 8;
const WORKER_LOCK_HEADROOM: u32 = 2;

/// PostgreSQL-backed durable coordinator state.
///
/// Separate `CoordinatorStore` values can connect from independently-lived
/// `factoryd` processes. Recovery claims serialize only on the selected
/// operation and use `SKIP LOCKED`, allowing multiple coordinators to recover
/// distinct work concurrently.
#[derive(Clone)]
pub struct CoordinatorStore {
    pool: PgPool,
    lock_pool: PgPool,
}

impl CoordinatorStore {
    /// Upper bound accepted by the native worker. At this limit one worker's
    /// two pools use at most 42 PostgreSQL connections: eight for durable
    /// state and 34 for long-lived workspace/repository advisory locks.
    pub const MAX_WORKER_SLOTS: usize = 32;

    pub async fn connect(database_url: &str) -> Result<Self> {
        Self::connect_with_limits(
            database_url,
            DEFAULT_QUERY_CONNECTIONS,
            DEFAULT_LOCK_CONNECTIONS,
        )
        .await
    }

    /// Builds worker pools from the accepted concurrency. Advisory locks use
    /// their own pool, so filling every operation slot can never consume the
    /// connections needed by lease heartbeats, events, and checkpoints.
    pub async fn connect_for_worker(database_url: &str, slots: usize) -> Result<Self> {
        let (query_connections, lock_connections) = worker_pool_limits(slots)?;
        Self::connect_with_limits(database_url, query_connections, lock_connections).await
    }

    async fn connect_with_limits(
        database_url: &str,
        query_connections: u32,
        lock_connections: u32,
    ) -> Result<Self> {
        let pool = PgPoolOptions::new()
            .max_connections(query_connections)
            .connect(database_url)
            .await?;
        let lock_pool = match PgPoolOptions::new()
            .max_connections(lock_connections)
            .connect(database_url)
            .await
        {
            Ok(lock_pool) => lock_pool,
            Err(error) => {
                pool.close().await;
                return Err(error.into());
            }
        };
        Ok(Self { pool, lock_pool })
    }

    pub async fn migrate(&self) -> Result<()> {
        schema::migrate(&self.pool).await?;
        Ok(())
    }

    pub async fn close(self) {
        self.lock_pool.close().await;
        self.pool.close().await;
    }
}

fn worker_pool_limits(slots: usize) -> Result<(u32, u32)> {
    if !(1..=CoordinatorStore::MAX_WORKER_SLOTS).contains(&slots) {
        return Err(CoordinatorError::InvalidInput(format!(
            "worker slots must be between 1 and {}",
            CoordinatorStore::MAX_WORKER_SLOTS
        )));
    }
    let slots = u32::try_from(slots).expect("worker slot cap fits u32");
    Ok((DEFAULT_QUERY_CONNECTIONS, slots + WORKER_LOCK_HEADROOM))
}

pub(super) fn database_lease_epoch(lease_epoch: u64) -> Result<i64> {
    i64::try_from(lease_epoch).map_err(|_| CoordinatorError::NumericRange {
        field: "lease epoch",
    })
}

pub(super) fn database_sequence(sequence: u64, field: &'static str) -> Result<i64> {
    i64::try_from(sequence).map_err(|_| CoordinatorError::NumericRange { field })
}

pub(super) fn new_id() -> String {
    Uuid::new_v4().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worker_pool_limits_reserve_query_and_lock_headroom_at_the_slot_cap() {
        assert_eq!(worker_pool_limits(1).unwrap(), (8, 3));
        assert_eq!(worker_pool_limits(32).unwrap(), (8, 34));
        assert!(worker_pool_limits(0).is_err());
        assert!(worker_pool_limits(33).is_err());
    }
}
