use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::time::Duration;
use std::time::Instant;

use factory_coordinator::AttemptState;
use factory_coordinator::CancellationHandle;
use factory_coordinator::CoordinatorInstanceId;
use factory_coordinator::CoordinatorStore;
use factory_coordinator::DurableRunner;
use factory_coordinator::JobDefinition;
use factory_coordinator::JobState;
use factory_coordinator::OperationDefinition;
use factory_coordinator::OperationExecutionContext;
use factory_coordinator::OperationExecutionResult;
use factory_coordinator::OperationExecutor;
use factory_coordinator::OperationOutcome;
use factory_coordinator::RunnerConfig;
use serde_json::json;
use sqlx::Connection;
use sqlx::PgConnection;
use tokio::sync::Notify;

fn database_url() -> String {
    std::env::var("FACTORY_COORDINATOR_TEST_DATABASE_URL")
        .expect("set FACTORY_COORDINATOR_TEST_DATABASE_URL to a disposable PostgreSQL database")
}

#[derive(Clone, Default)]
struct BlockingExecutor {
    started: Arc<Notify>,
    cancellation_observed: Arc<AtomicBool>,
    cleanup_complete: Arc<AtomicBool>,
}

impl OperationExecutor for BlockingExecutor {
    fn execute(
        &self,
        _context: OperationExecutionContext,
        cancellation: CancellationHandle,
    ) -> Pin<Box<dyn Future<Output = OperationExecutionResult> + Send + '_>> {
        let started = Arc::clone(&self.started);
        let cancellation_observed = Arc::clone(&self.cancellation_observed);
        Box::pin(async move {
            started.notify_waiters();
            cancellation.cancelled().await;
            cancellation_observed.store(true, Ordering::Release);
            Ok(OperationOutcome::Complete {
                checkpoint: None,
                completion_event: None,
            })
        })
    }

    fn cleanup_cancelled(
        &self,
        _context: OperationExecutionContext,
    ) -> Pin<Box<dyn Future<Output = factory_coordinator::Result<()>> + Send + '_>> {
        let cleanup_complete = Arc::clone(&self.cleanup_complete);
        Box::pin(async move {
            tokio::time::sleep(Duration::from_millis(150)).await;
            cleanup_complete.store(true, Ordering::Release);
            Ok(())
        })
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires FACTORY_COORDINATOR_TEST_DATABASE_URL"]
async fn slow_control_read_does_not_cancel_a_heartbeat_owned_runtime() {
    let database_url = database_url();
    let store = CoordinatorStore::connect(&database_url).await.unwrap();
    store.migrate().await.unwrap();
    let job = store
        .create_job(JobDefinition {
            kind: "acceptance.runner.control-poll".to_string(),
            input: json!({}),
            operations: vec![OperationDefinition {
                kind: "acceptance.runner.control-poll".to_string(),
                input: json!({}),
                max_attempts: 1,
            }],
        })
        .await
        .unwrap();

    let executor = BlockingExecutor::default();
    let shutdown = CancellationHandle::default();
    let runner = DurableRunner::new(
        store.clone(),
        executor.clone(),
        RunnerConfig {
            worker_id: CoordinatorInstanceId::new("control-poll-worker"),
            lease_duration: Duration::from_secs(6),
            poll_interval: Duration::from_millis(25),
            shutdown_grace: Duration::from_secs(2),
            slots: 1,
            execution_profile: None,
        },
    )
    .unwrap();
    let runner_shutdown = shutdown.clone();
    let runner_task = tokio::spawn(async move { runner.run(runner_shutdown).await });
    tokio::time::timeout(Duration::from_secs(3), executor.started.notified())
        .await
        .expect("executor must start");

    let attempts = store.list_job_attempts(&job.job.job_id).await.unwrap();
    assert_eq!(attempts.len(), 1);
    let attempt_id = attempts[0].attempt_id.clone();
    let first_expiry = attempts[0].lease_expires_at;

    // The control poll starts with a factory_jobs read. This table lock makes
    // that read exceed its one-second budget without blocking the independent
    // attempt heartbeat.
    let mut blocker = PgConnection::connect(&database_url).await.unwrap();
    let mut transaction = blocker.begin().await.unwrap();
    sqlx::query("LOCK TABLE factory_jobs IN ACCESS EXCLUSIVE MODE")
        .execute(&mut *transaction)
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(2_500)).await;

    let renewed = store.load_attempt(&attempt_id).await.unwrap();
    assert_eq!(renewed.state, AttemptState::Running);
    assert!(
        renewed.lease_expires_at > first_expiry,
        "the independent heartbeat must renew while the control read is delayed"
    );
    assert!(
        !executor.cancellation_observed.load(Ordering::Acquire),
        "a control timeout must not cancel the owned runtime"
    );
    transaction.commit().await.unwrap();

    let cancellation_started = Instant::now();
    assert_eq!(
        store.cancel_job(&job.job.job_id).await.unwrap().job.state,
        JobState::Cancelling
    );
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if store.load_job(&job.job.job_id).await.unwrap().job.state == JobState::Cancelled {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("explicit cancellation must drain and acknowledge promptly");

    assert!(cancellation_started.elapsed() < Duration::from_secs(3));
    assert!(executor.cancellation_observed.load(Ordering::Acquire));
    assert!(executor.cleanup_complete.load(Ordering::Acquire));
    assert_eq!(
        store.load_attempt(&attempt_id).await.unwrap().state,
        AttemptState::Abandoned,
        "acknowledgement must retire the lease instead of parking it until expiry"
    );

    shutdown.cancel();
    tokio::time::timeout(Duration::from_secs(3), runner_task)
        .await
        .expect("runner must stop")
        .unwrap()
        .unwrap();
    store.close().await;
}
