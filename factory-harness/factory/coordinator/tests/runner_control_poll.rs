use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::time::Duration;
use std::time::Instant;

use chrono::Utc;
use factory_coordinator::AttemptState;
use factory_coordinator::CancellationHandle;
use factory_coordinator::ClaimRequest;
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
use factory_coordinator::RecoveryCause;
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
    lifecycle: Arc<Mutex<Vec<&'static str>>>,
}

#[derive(Clone)]
struct EnvironmentBlockingExecutor {
    store: CoordinatorStore,
    started: Arc<Notify>,
    cleanup_complete: Arc<AtomicBool>,
    cancellation_release_called: Arc<AtomicBool>,
}

#[derive(Clone)]
struct AcknowledgementFailureExecutor {
    store: CoordinatorStore,
    database_url: String,
    runner_shutdown: CancellationHandle,
    started: Arc<Notify>,
    cleanup_complete: Arc<AtomicBool>,
    release_complete: Arc<AtomicBool>,
}

impl OperationExecutor for AcknowledgementFailureExecutor {
    fn execute(
        &self,
        context: OperationExecutionContext,
        cancellation: CancellationHandle,
    ) -> Pin<Box<dyn Future<Output = OperationExecutionResult> + Send + '_>> {
        let store = self.store.clone();
        let started = Arc::clone(&self.started);
        Box::pin(async move {
            store
                .ensure_execution_environment(&context.lease().fence, "test")
                .await?;
            started.notify_one();
            cancellation.cancelled().await;
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
            cleanup_complete.store(true, Ordering::Release);
            Ok(())
        })
    }

    fn release_cancelled_execution_environment(
        &self,
        context: OperationExecutionContext,
    ) -> Pin<Box<dyn Future<Output = factory_coordinator::Result<()>> + Send + '_>> {
        let store = self.store.clone();
        let database_url = self.database_url.clone();
        let runner_shutdown = self.runner_shutdown.clone();
        let cleanup_complete = Arc::clone(&self.cleanup_complete);
        let release_complete = Arc::clone(&self.release_complete);
        Box::pin(async move {
            assert!(
                cleanup_complete.load(Ordering::Acquire),
                "release must follow runtime/worktree cleanup"
            );
            let environment = store
                .request_cancelling_execution_environment_release(&context.lease().fence)
                .await?
                .expect("the cancelling job has an execution environment");
            store
                .mark_execution_environment_released(&context.job().job_id, environment.generation)
                .await?;
            release_complete.store(true, Ordering::Release);

            // Force the final acknowledgement to fail after cleanup and
            // release have completed, then stop this runner before it can
            // reclaim its own relinquished lease.
            let mut connection = PgConnection::connect(&database_url).await?;
            let affected = sqlx::query(
                "UPDATE factory_jobs SET status = 'running' WHERE job_id = $1 AND status = 'cancelling'",
            )
            .bind(context.job().job_id.as_str())
            .execute(&mut connection)
            .await?;
            assert_eq!(affected.rows_affected(), 1);
            runner_shutdown.cancel();
            Ok(())
        })
    }
}

impl OperationExecutor for EnvironmentBlockingExecutor {
    fn execute(
        &self,
        context: OperationExecutionContext,
        cancellation: CancellationHandle,
    ) -> Pin<Box<dyn Future<Output = OperationExecutionResult> + Send + '_>> {
        let store = self.store.clone();
        let started = Arc::clone(&self.started);
        Box::pin(async move {
            store
                .ensure_execution_environment(&context.lease().fence, "test")
                .await?;
            started.notify_one();
            cancellation.cancelled().await;
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
            cleanup_complete.store(true, Ordering::Release);
            Ok(())
        })
    }

    fn release_cancelled_execution_environment(
        &self,
        _context: OperationExecutionContext,
    ) -> Pin<Box<dyn Future<Output = factory_coordinator::Result<()>> + Send + '_>> {
        let called = Arc::clone(&self.cancellation_release_called);
        Box::pin(async move {
            called.store(true, Ordering::Release);
            Ok(())
        })
    }
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
            started.notify_one();
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
        let lifecycle = Arc::clone(&self.lifecycle);
        Box::pin(async move {
            tokio::time::sleep(Duration::from_millis(150)).await;
            cleanup_complete.store(true, Ordering::Release);
            lifecycle.lock().unwrap().push("cleanup");
            Ok(())
        })
    }

    fn release_cancelled_execution_environment(
        &self,
        _context: OperationExecutionContext,
    ) -> Pin<Box<dyn Future<Output = factory_coordinator::Result<()>> + Send + '_>> {
        let cleanup_complete = Arc::clone(&self.cleanup_complete);
        let lifecycle = Arc::clone(&self.lifecycle);
        Box::pin(async move {
            assert!(
                cleanup_complete.load(Ordering::Acquire),
                "release must follow runtime/worktree cleanup"
            );
            lifecycle.lock().unwrap().push("release");
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
        executor.lifecycle.lock().unwrap().as_slice(),
        &["cleanup", "release"]
    );
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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires FACTORY_COORDINATOR_TEST_DATABASE_URL"]
async fn graceful_worker_shutdown_relinquishes_but_retains_active_environment() {
    let store = CoordinatorStore::connect(&database_url()).await.unwrap();
    store.migrate().await.unwrap();
    let job = store
        .create_job(JobDefinition {
            kind: "acceptance.runner.shutdown-environment".to_string(),
            input: json!({}),
            operations: vec![OperationDefinition {
                kind: "acceptance.runner.shutdown-environment".to_string(),
                input: json!({}),
                max_attempts: 1,
            }],
        })
        .await
        .unwrap();
    let executor = EnvironmentBlockingExecutor {
        store: store.clone(),
        started: Arc::new(Notify::new()),
        cleanup_complete: Arc::new(AtomicBool::new(false)),
        cancellation_release_called: Arc::new(AtomicBool::new(false)),
    };
    let shutdown = CancellationHandle::default();
    let runner = DurableRunner::new(
        store.clone(),
        executor.clone(),
        RunnerConfig {
            worker_id: CoordinatorInstanceId::new("shutdown-environment-worker"),
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
        .expect("executor must ensure its environment");

    shutdown.cancel();
    tokio::time::timeout(Duration::from_secs(3), runner_task)
        .await
        .expect("runner must drain")
        .unwrap()
        .unwrap();

    assert!(executor.cleanup_complete.load(Ordering::Acquire));
    assert!(!executor.cancellation_release_called.load(Ordering::Acquire));
    let environment = store
        .load_execution_environment(&job.job.job_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        environment.desired_state,
        factory_coordinator::ExecutionEnvironmentDesiredState::Active
    );
    assert_eq!(
        environment.status,
        factory_coordinator::ExecutionEnvironmentStatus::Provisioning
    );
    assert_eq!(
        store.load_job(&job.job.job_id).await.unwrap().job.state,
        JobState::Running
    );
    store.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires FACTORY_COORDINATOR_TEST_DATABASE_URL"]
async fn acknowledgement_failure_after_release_relinquishes_the_lease_immediately() {
    let database_url = database_url();
    let store = CoordinatorStore::connect(&database_url).await.unwrap();
    store.migrate().await.unwrap();
    let job = store
        .create_job(JobDefinition {
            kind: "acceptance.runner.cancellation-ack-failure".to_string(),
            input: json!({}),
            operations: vec![OperationDefinition {
                kind: "acceptance.runner.cancellation-ack-failure".to_string(),
                input: json!({}),
                max_attempts: 1,
            }],
        })
        .await
        .unwrap();
    let runner_shutdown = CancellationHandle::default();
    let executor = AcknowledgementFailureExecutor {
        store: store.clone(),
        database_url,
        runner_shutdown: runner_shutdown.clone(),
        started: Arc::new(Notify::new()),
        cleanup_complete: Arc::new(AtomicBool::new(false)),
        release_complete: Arc::new(AtomicBool::new(false)),
    };
    let runner = DurableRunner::new(
        store.clone(),
        executor.clone(),
        RunnerConfig {
            worker_id: CoordinatorInstanceId::new("ack-failure-worker"),
            lease_duration: Duration::from_secs(900),
            poll_interval: Duration::from_millis(25),
            shutdown_grace: Duration::from_secs(2),
            slots: 1,
            execution_profile: None,
        },
    )
    .unwrap();
    let runner_task = tokio::spawn(async move { runner.run(runner_shutdown).await });
    tokio::time::timeout(Duration::from_secs(3), executor.started.notified())
        .await
        .expect("executor must start");
    let attempt_id = store.list_job_attempts(&job.job.job_id).await.unwrap()[0]
        .attempt_id
        .clone();

    assert_eq!(
        store.cancel_job(&job.job.job_id).await.unwrap().job.state,
        JobState::Cancelling
    );
    tokio::time::timeout(Duration::from_secs(3), runner_task)
        .await
        .expect("runner must stop after the forced acknowledgement failure")
        .unwrap()
        .unwrap();

    assert!(executor.cleanup_complete.load(Ordering::Acquire));
    assert!(executor.release_complete.load(Ordering::Acquire));
    let environment = store
        .load_execution_environment(&job.job.job_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        environment.status,
        factory_coordinator::ExecutionEnvironmentStatus::Released
    );
    let failed_ack_attempt = store.load_attempt(&attempt_id).await.unwrap();
    assert_eq!(failed_ack_attempt.state, AttemptState::Running);
    assert!(
        failed_ack_attempt.lease_expires_at <= Utc::now(),
        "acknowledgement failure must not retain the 900-second lease"
    );

    let reclaimed = store
        .claim_next_recovery(&ClaimRequest {
            owner_instance_id: CoordinatorInstanceId::new("ack-recovery-worker"),
            lease_seconds: 900,
            execution_profile: None,
        })
        .await
        .unwrap()
        .expect("the relinquished attempt must be immediately reclaimable");
    assert_eq!(reclaimed.attempt.attempt_id, attempt_id);
    assert_eq!(reclaimed.selection.cause, RecoveryCause::LeaseExpired);
    assert_eq!(
        reclaimed.attempt.lease_epoch,
        failed_ack_attempt.lease_epoch + 1
    );
    store.close().await;
}
