use chrono::Utc;
use factory_coordinator::AttemptFailure;
use factory_coordinator::AttemptSettlement;
use factory_coordinator::ClaimRequest;
use factory_coordinator::CoordinatorError;
use factory_coordinator::CoordinatorInstanceId;
use factory_coordinator::CoordinatorStore;
use factory_coordinator::ExecutionEnvironmentDesiredState;
use factory_coordinator::ExecutionEnvironmentStatus;
use factory_coordinator::ExecutionProfile;
use factory_coordinator::JobDefinition;
use factory_coordinator::JobState;
use factory_coordinator::OperationDefinition;
use factory_coordinator::RecoveryCause;
use factory_coordinator::WorkspaceBinding;
use serde_json::json;
use std::time::Duration;
use uuid::Uuid;

fn database_url() -> String {
    std::env::var("FACTORY_COORDINATOR_TEST_DATABASE_URL")
        .expect("set FACTORY_COORDINATOR_TEST_DATABASE_URL to a disposable PostgreSQL database")
}

async fn store() -> CoordinatorStore {
    let store = CoordinatorStore::connect(&database_url()).await.unwrap();
    store.migrate().await.unwrap();
    store
}

async fn one_operation_job(
    store: &CoordinatorStore,
    max_attempts: u32,
) -> factory_coordinator::DurableJob {
    store
        .create_job(JobDefinition {
            kind: format!("execution-environment-test-{}", Uuid::new_v4()),
            input: json!({}),
            operations: vec![OperationDefinition {
                kind: "execute".to_string(),
                input: json!({}),
                max_attempts,
            }],
        })
        .await
        .unwrap()
}

async fn claim(
    store: &CoordinatorStore,
    operation: &factory_coordinator::OperationRecord,
    owner: &str,
    lease_seconds: u32,
) -> factory_coordinator::RecoveryLease {
    store
        .claim_recovery_for_operation(
            &operation.operation_id,
            &ClaimRequest {
                owner_instance_id: CoordinatorInstanceId::new(owner),
                lease_seconds,
                execution_profile: Some(ExecutionProfile {
                    provider: "test-provider".to_string(),
                    model: "test-model".to_string(),
                }),
            },
        )
        .await
        .unwrap()
        .unwrap_or_else(|| {
            panic!(
                "no recovery lease for operation {} ({})",
                operation.kind, operation.operation_id
            )
        })
}

async fn assert_one_environment_row(job_id: &factory_coordinator::JobId) {
    let pool = sqlx::PgPool::connect(&database_url()).await.unwrap();
    let count = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM factory_execution_environments WHERE job_id = $1",
    )
    .bind(job_id.as_str())
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(count, 1);
    pool.close().await;
}

#[tokio::test]
#[ignore = "requires FACTORY_COORDINATOR_TEST_DATABASE_URL"]
async fn identity_and_generation_survive_lease_transfer_and_business_retry() {
    let store = store().await;
    let job = one_operation_job(&store, 2).await;
    let operation = &job.operations[0];

    let first = claim(&store, operation, "environment-worker-one", 1).await;
    let initial = store
        .ensure_execution_environment(&first.fence, "docker")
        .await
        .unwrap();
    assert_eq!(initial.generation, 1);
    assert_eq!(initial.status, ExecutionEnvironmentStatus::Provisioning);
    assert_eq!(
        store
            .ensure_execution_environment(&first.fence, "docker")
            .await
            .unwrap(),
        initial
    );
    let located = store
        .reserve_execution_environment_locator(&first.fence, 1, "factory/pod-generation-one")
        .await
        .unwrap();
    assert_eq!(
        located.backend_ref.as_deref(),
        Some("factory/pod-generation-one")
    );
    assert_eq!(
        store
            .reserve_execution_environment_locator(&first.fence, 1, "other/replacement")
            .await
            .unwrap()
            .backend_ref,
        located.backend_ref,
        "the first durable locator is write-once"
    );
    assert!(matches!(
        store
            .reserve_execution_environment_locator(&first.fence, 2, "factory/stale-generation")
            .await
            .unwrap_err(),
        CoordinatorError::ExecutionEnvironmentGenerationStale { generation: 2, .. }
    ));
    assert_one_environment_row(&job.job.job_id).await;

    tokio::time::sleep(Duration::from_millis(1_200)).await;
    let recovered = claim(&store, operation, "environment-worker-two", 60).await;
    assert_eq!(recovered.selection.cause, RecoveryCause::LeaseExpired);
    assert_eq!(recovered.attempt.attempt_id, first.attempt.attempt_id);
    assert_eq!(recovered.attempt.lease_epoch, 2);
    let after_transfer = store
        .ensure_execution_environment(&recovered.fence, "docker")
        .await
        .unwrap();
    assert_eq!(after_transfer.environment_id, initial.environment_id);
    assert_eq!(after_transfer.generation, initial.generation);
    assert!(matches!(
        store
            .reserve_execution_environment_locator(&first.fence, 1, "factory/stale-owner")
            .await
            .unwrap_err(),
        CoordinatorError::AttemptLeaseUnavailable(_)
    ));
    assert!(matches!(
        store
            .mark_execution_environment_failed(&first.fence, 1, "stale owner")
            .await
            .unwrap_err(),
        CoordinatorError::AttemptLeaseUnavailable(_)
    ));
    assert_eq!(
        store
            .mark_execution_environment_failed(&recovered.fence, 1, "provision failed")
            .await
            .unwrap()
            .status,
        ExecutionEnvironmentStatus::Failed
    );
    let ready = store
        .mark_execution_environment_ready(
            &recovered.fence,
            1,
            "container-generation-one",
            "ws://environment:4500",
        )
        .await
        .unwrap();
    assert_eq!(ready.status, ExecutionEnvironmentStatus::Ready);
    assert!(matches!(
        store
            .request_execution_environment_release(&job.job.job_id, 1)
            .await
            .unwrap_err(),
        CoordinatorError::InvalidInput(_)
    ));
    assert_eq!(
        store
            .load_execution_environment(&job.job.job_id)
            .await
            .unwrap()
            .unwrap()
            .status,
        ExecutionEnvironmentStatus::Ready
    );

    store
        .settle_attempt(
            &recovered.fence,
            AttemptSettlement::Failed(AttemptFailure::RetryAt {
                retry_at: Utc::now(),
                detail: json!({ "cause": "fixture retry" }),
            }),
            None,
        )
        .await
        .unwrap();
    let retry = claim(&store, operation, "environment-worker-three", 60).await;
    assert_eq!(retry.selection.cause, RecoveryCause::RetryScheduled);
    assert_eq!(retry.attempt.attempt_number, 2);
    let after_retry = store
        .ensure_execution_environment(&retry.fence, "docker")
        .await
        .unwrap();
    assert_eq!(after_retry.environment_id, initial.environment_id);
    assert_eq!(after_retry.generation, initial.generation);
    assert_one_environment_row(&job.job.job_id).await;

    store
        .settle_attempt(&retry.fence, AttemptSettlement::Succeeded, None)
        .await
        .unwrap();
    let terminal = store
        .load_execution_environment(&job.job.job_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        terminal.desired_state,
        ExecutionEnvironmentDesiredState::Released
    );
    assert_eq!(terminal.status, ExecutionEnvironmentStatus::Releasing);
    store.close().await;
}

#[tokio::test]
#[ignore = "requires FACTORY_COORDINATOR_TEST_DATABASE_URL"]
async fn terminal_release_and_continuation_use_a_generation_fence() {
    let store = store().await;
    let job = store
        .create_job(JobDefinition {
            kind: "factory.task".to_string(),
            input: json!({
                "task": "exercise execution environment continuation",
                "executionProfile": {
                    "provider": "test-provider",
                    "model": "test-model"
                }
            }),
            operations: ["plan", "execute", "review", "remediate"]
                .into_iter()
                .map(|kind| OperationDefinition {
                    kind: format!("codex.{kind}"),
                    input: json!({}),
                    max_attempts: 1,
                })
                .collect(),
        })
        .await
        .unwrap();
    store
        .put_workspace(&WorkspaceBinding {
            job_id: job.job.job_id.clone(),
            repository_id: format!("test:environment:{}", Uuid::new_v4()),
            repository: "/tmp/execution-environment-fixture".to_string(),
            base_ref: "HEAD".to_string(),
            base_revision: "fixture-base".to_string(),
            branch_name: format!("factory/{}", job.job.job_id),
            root: format!("/tmp/workspaces/{}", job.job.job_id),
            revision: "fixture-base".to_string(),
        })
        .await
        .unwrap();

    let first = claim(&store, &job.operations[0], "continuation-worker-0", 60).await;
    let environment = store
        .ensure_execution_environment(&first.fence, "docker")
        .await
        .unwrap();
    store
        .mark_execution_environment_ready(
            &first.fence,
            environment.generation,
            "container-before-continuation",
            "ws://environment:4500",
        )
        .await
        .unwrap();
    store
        .settle_attempt(&first.fence, AttemptSettlement::Succeeded, None)
        .await
        .unwrap();
    for (index, operation) in job.operations.iter().enumerate().skip(1) {
        let lease = claim(
            &store,
            operation,
            &format!("continuation-worker-{index}"),
            60,
        )
        .await;
        store
            .settle_attempt(&lease.fence, AttemptSettlement::Succeeded, None)
            .await
            .unwrap();
    }

    assert_eq!(
        store.load_job(&job.job.job_id).await.unwrap().job.state,
        JobState::Succeeded
    );
    let releasing = store
        .load_execution_environment(&job.job.job_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(releasing.environment_id, environment.environment_id);
    assert_eq!(releasing.generation, 1);
    assert_eq!(releasing.status, ExecutionEnvironmentStatus::Releasing);
    assert!(matches!(
        store
            .request_execution_environment_release(&job.job.job_id, 2)
            .await
            .unwrap_err(),
        CoordinatorError::ExecutionEnvironmentGenerationStale { generation: 2, .. }
    ));
    assert_eq!(
        store
            .request_execution_environment_release(&job.job.job_id, 1)
            .await
            .unwrap()
            .status,
        ExecutionEnvironmentStatus::Releasing
    );
    assert!(matches!(
        store
            .mark_execution_environment_released(&job.job.job_id, 2)
            .await
            .unwrap_err(),
        CoordinatorError::ExecutionEnvironmentGenerationStale { generation: 2, .. }
    ));
    let operation_count = store
        .load_job(&job.job.job_id)
        .await
        .unwrap()
        .operations
        .len();
    assert!(matches!(
        store
            .continue_job(&job.job.job_id, "must wait for release")
            .await
            .unwrap_err(),
        CoordinatorError::InvalidInput(message)
            if message.contains("must finish release before continuation")
    ));
    let still_releasing = store
        .load_execution_environment(&job.job.job_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(still_releasing.generation, 1);
    assert_eq!(
        still_releasing.status,
        ExecutionEnvironmentStatus::Releasing
    );
    let unchanged_job = store.load_job(&job.job.job_id).await.unwrap();
    assert_eq!(unchanged_job.job.state, JobState::Succeeded);
    assert_eq!(unchanged_job.operations.len(), operation_count);
    let released = store
        .mark_execution_environment_released(&job.job.job_id, 1)
        .await
        .unwrap();
    assert_eq!(released.status, ExecutionEnvironmentStatus::Released);
    assert!(released.url.is_none());

    store
        .continue_job(&job.job.job_id, "run one more round")
        .await
        .unwrap();
    let continued = store
        .load_execution_environment(&job.job.job_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(continued.environment_id, environment.environment_id);
    assert_eq!(continued.generation, 2);
    assert_eq!(
        continued.desired_state,
        ExecutionEnvironmentDesiredState::Active
    );
    assert_eq!(continued.status, ExecutionEnvironmentStatus::Provisioning);
    assert!(continued.backend_ref.is_none());
    assert!(continued.url.is_none());
    assert!(matches!(
        store
            .mark_execution_environment_released(&job.job.job_id, 1)
            .await
            .unwrap_err(),
        CoordinatorError::ExecutionEnvironmentGenerationStale { generation: 1, .. }
    ));
    assert_one_environment_row(&job.job.job_id).await;
    store.close().await;
}

#[tokio::test]
#[ignore = "requires FACTORY_COORDINATOR_TEST_DATABASE_URL"]
async fn failure_and_both_cancellation_paths_request_release_at_terminal_state() {
    let store = store().await;

    let failed_job = one_operation_job(&store, 1).await;
    let failed_lease = claim(&store, &failed_job.operations[0], "failure-worker", 60).await;
    store
        .ensure_execution_environment(&failed_lease.fence, "docker")
        .await
        .unwrap();
    store
        .settle_attempt(
            &failed_lease.fence,
            AttemptSettlement::Failed(AttemptFailure::Terminal {
                detail: json!({ "cause": "fixture terminal failure" }),
            }),
            None,
        )
        .await
        .unwrap();
    assert_eq!(
        store
            .load_job(&failed_job.job.job_id)
            .await
            .unwrap()
            .job
            .state,
        JobState::Failed
    );
    assert_eq!(
        store
            .load_execution_environment(&failed_job.job.job_id)
            .await
            .unwrap()
            .unwrap()
            .desired_state,
        ExecutionEnvironmentDesiredState::Released
    );

    let queued_cancel = one_operation_job(&store, 2).await;
    let queued_lease = claim(
        &store,
        &queued_cancel.operations[0],
        "queued-cancel-worker",
        60,
    )
    .await;
    store
        .ensure_execution_environment(&queued_lease.fence, "docker")
        .await
        .unwrap();
    store
        .settle_attempt(
            &queued_lease.fence,
            AttemptSettlement::Failed(AttemptFailure::RetryAt {
                retry_at: Utc::now() + chrono::Duration::minutes(1),
                detail: json!({ "cause": "fixture waiting retry" }),
            }),
            None,
        )
        .await
        .unwrap();
    assert_eq!(
        store
            .cancel_job(&queued_cancel.job.job_id)
            .await
            .unwrap()
            .job
            .state,
        JobState::Cancelled
    );
    assert_eq!(
        store
            .load_execution_environment(&queued_cancel.job.job_id)
            .await
            .unwrap()
            .unwrap()
            .status,
        ExecutionEnvironmentStatus::Releasing
    );

    let running_cancel = one_operation_job(&store, 1).await;
    let running_lease = claim(
        &store,
        &running_cancel.operations[0],
        "running-cancel-worker",
        60,
    )
    .await;
    store
        .ensure_execution_environment(&running_lease.fence, "docker")
        .await
        .unwrap();
    assert_eq!(
        store
            .cancel_job(&running_cancel.job.job_id)
            .await
            .unwrap()
            .job
            .state,
        JobState::Cancelling
    );
    assert_eq!(
        store
            .load_execution_environment(&running_cancel.job.job_id)
            .await
            .unwrap()
            .unwrap()
            .desired_state,
        ExecutionEnvironmentDesiredState::Active
    );
    assert_eq!(
        store
            .cancel_job(&running_cancel.job.job_id)
            .await
            .unwrap()
            .job
            .state,
        JobState::Cancelling
    );
    assert!(matches!(
        store
            .acknowledge_job_cancellation(&running_lease.fence)
            .await
            .unwrap_err(),
        CoordinatorError::InvalidInput(message)
            if message.contains("must be released before acknowledgement")
    ));
    let releasing = store
        .request_cancelling_execution_environment_release(&running_lease.fence)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(releasing.status, ExecutionEnvironmentStatus::Releasing);
    store
        .mark_execution_environment_released(&running_cancel.job.job_id, releasing.generation)
        .await
        .unwrap();
    assert_eq!(
        store
            .acknowledge_job_cancellation(&running_lease.fence)
            .await
            .unwrap()
            .job
            .state,
        JobState::Cancelled
    );
    assert_eq!(
        store
            .load_execution_environment(&running_cancel.job.job_id)
            .await
            .unwrap()
            .unwrap()
            .desired_state,
        ExecutionEnvironmentDesiredState::Released
    );
    store.close().await;
}
