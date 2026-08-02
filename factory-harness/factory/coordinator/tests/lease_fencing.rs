use chrono::Duration as ChronoDuration;
use chrono::Utc;
use factory_coordinator::AttemptFailure;
use factory_coordinator::AttemptFence;
use factory_coordinator::AttemptSettlement;
use factory_coordinator::ClaimRequest;
use factory_coordinator::CoordinatorError;
use factory_coordinator::CoordinatorInstanceId;
use factory_coordinator::CoordinatorStore;
use factory_coordinator::Correlation;
use factory_coordinator::JobDefinition;
use factory_coordinator::JobState;
use factory_coordinator::NewCheckpoint;
use factory_coordinator::OperationDefinition;
use factory_coordinator::RecoveryCause;
use factory_coordinator::RequestId;
use factory_coordinator::ResumeStrategy;
use factory_coordinator::ThreadId;
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
            kind: format!("lease-fencing-test-{}", Uuid::new_v4()),
            input: json!({}),
            operations: vec![OperationDefinition {
                kind: "test-operation".to_string(),
                input: json!({}),
                max_attempts,
            }],
        })
        .await
        .unwrap()
}

fn checkpoint(attempt_id: &factory_coordinator::AttemptId, marker: &str) -> NewCheckpoint {
    NewCheckpoint {
        attempt_id: attempt_id.clone(),
        kind: "test.checkpoint".to_string(),
        payload: json!({ "marker": marker }),
        workspace_root: None,
        workspace_revision: None,
        correlation_id: None,
    }
}

fn assert_fenced(error: CoordinatorError) {
    assert!(matches!(
        error,
        CoordinatorError::AttemptLeaseUnavailable(_)
    ));
}

#[tokio::test]
#[ignore = "requires FACTORY_COORDINATOR_TEST_DATABASE_URL"]
async fn expired_max_attempt_one_transfers_same_attempt_and_fences_stale_owner() {
    let store = store().await;
    let job = one_operation_job(&store, 1).await;
    let operation = &job.operations[0];

    assert!(matches!(
        store
            .claim_recovery_for_operation(
                &operation.operation_id,
                &ClaimRequest {
                    owner_instance_id: CoordinatorInstanceId::new("invalid-worker"),
                    lease_seconds: 0,
                    execution_profile: None,
                },
            )
            .await
            .unwrap_err(),
        CoordinatorError::InvalidInput(_)
    ));

    let first = store
        .claim_recovery_for_operation(
            &operation.operation_id,
            &ClaimRequest {
                owner_instance_id: CoordinatorInstanceId::new("worker-one"),
                lease_seconds: 1,
                execution_profile: None,
            },
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(first.attempt.attempt_number, 1);
    assert_eq!(first.attempt.lease_epoch, 1);
    let before_transfer = store
        .save_checkpoint(
            &first.fence,
            checkpoint(&first.attempt.attempt_id, "before-transfer"),
        )
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(1_200)).await;

    let recovered = store
        .claim_recovery_for_operation(
            &operation.operation_id,
            &ClaimRequest {
                owner_instance_id: CoordinatorInstanceId::new("worker-two"),
                lease_seconds: 60,
                execution_profile: None,
            },
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(recovered.selection.cause, RecoveryCause::LeaseExpired);
    assert_eq!(recovered.attempt.attempt_id, first.attempt.attempt_id);
    assert_eq!(recovered.attempt.attempt_number, 1);
    assert_eq!(recovered.attempt.lease_epoch, 2);
    assert_eq!(
        recovered.attempt.resumes_attempt_id.as_ref(),
        Some(&first.attempt.attempt_id)
    );
    assert_eq!(
        recovered.attempt.resumes_checkpoint_id.as_ref(),
        Some(&before_transfer.checkpoint_id)
    );
    assert!(matches!(
        &recovered.selection.resume,
        ResumeStrategy::FromCheckpoint(checkpoint)
            if checkpoint.checkpoint_id == before_transfer.checkpoint_id
    ));

    assert_fenced(store.renew_attempt(&first.fence, 60).await.unwrap_err());
    assert_fenced(
        store
            .save_checkpoint(
                &first.fence,
                checkpoint(&first.attempt.attempt_id, "stale-progress"),
            )
            .await
            .unwrap_err(),
    );
    assert_fenced(
        store
            .settle_attempt(
                &first.fence,
                AttemptSettlement::Succeeded,
                Some(checkpoint(&first.attempt.attempt_id, "stale-final")),
            )
            .await
            .unwrap_err(),
    );

    let same_owner_stale_epoch = AttemptFence {
        attempt_id: recovered.attempt.attempt_id.clone(),
        owner_instance_id: recovered.attempt.owner_instance_id.clone(),
        lease_epoch: first.attempt.lease_epoch,
    };
    assert_fenced(
        store
            .renew_attempt(&same_owner_stale_epoch, 60)
            .await
            .unwrap_err(),
    );
    assert_fenced(
        store
            .save_checkpoint(
                &same_owner_stale_epoch,
                checkpoint(&recovered.attempt.attempt_id, "stale-epoch-progress"),
            )
            .await
            .unwrap_err(),
    );
    assert_fenced(
        store
            .settle_attempt(
                &same_owner_stale_epoch,
                AttemptSettlement::Succeeded,
                Some(checkpoint(
                    &recovered.attempt.attempt_id,
                    "stale-epoch-final",
                )),
            )
            .await
            .unwrap_err(),
    );

    let thread_id = ThreadId::new(format!("thread-{}", Uuid::new_v4()));
    let correlation = Correlation {
        job_id: job.job.job_id.clone(),
        operation_id: operation.operation_id.clone(),
        attempt_id: recovered.attempt.attempt_id.clone(),
        request_id: RequestId::new("fenced-request"),
        thread_id: Some(thread_id.clone()),
        turn_id: None,
        item_id: None,
    };
    assert_fenced(
        store
            .append_correlation(&same_owner_stale_epoch, &correlation)
            .await
            .unwrap_err(),
    );
    store
        .append_correlation(&recovered.fence, &correlation)
        .await
        .unwrap();

    let thread_state = json!({ "progress": { "stage": "execute" } });
    assert_fenced(
        store
            .put_thread_state(&first.fence, &thread_id, thread_state.clone())
            .await
            .unwrap_err(),
    );
    assert_fenced(
        store
            .put_thread_state(&same_owner_stale_epoch, &thread_id, thread_state.clone())
            .await
            .unwrap_err(),
    );
    assert!(matches!(
        store
            .put_thread_state(
                &recovered.fence,
                &ThreadId::new(format!("unrelated-thread-{}", Uuid::new_v4())),
                thread_state.clone(),
            )
            .await
            .unwrap_err(),
        CoordinatorError::ThreadStateOwnershipMismatch { .. }
    ));
    let stored_thread_state = store
        .put_thread_state(&recovered.fence, &thread_id, thread_state.clone())
        .await
        .unwrap();
    assert_eq!(stored_thread_state.state, thread_state);
    assert_eq!(
        store
            .load_thread_state(&thread_id)
            .await
            .unwrap()
            .unwrap()
            .state,
        stored_thread_state.state
    );

    let progress = store
        .save_checkpoint(
            &recovered.fence,
            checkpoint(&recovered.attempt.attempt_id, "current-progress"),
        )
        .await
        .unwrap();
    assert_eq!(progress.sequence, 2);
    let final_checkpoint = store
        .settle_attempt(
            &recovered.fence,
            AttemptSettlement::Succeeded,
            Some(checkpoint(&recovered.attempt.attempt_id, "current-final")),
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(final_checkpoint.sequence, 3);

    let attempts = store.list_job_attempts(&job.job.job_id).await.unwrap();
    assert_eq!(attempts.len(), 1);
    assert_eq!(attempts[0].lease_epoch, 2);
    assert_eq!(
        store.load_job(&job.job.job_id).await.unwrap().job.state,
        JobState::Succeeded
    );
    store.close().await;
}

#[tokio::test]
#[ignore = "requires FACTORY_COORDINATOR_TEST_DATABASE_URL"]
async fn explicit_executor_failure_consumes_a_business_retry() {
    let store = store().await;
    let job = one_operation_job(&store, 2).await;
    let operation = &job.operations[0];
    let first = store
        .claim_recovery_for_operation(
            &operation.operation_id,
            &ClaimRequest {
                owner_instance_id: CoordinatorInstanceId::new("retry-worker"),
                lease_seconds: 60,
                execution_profile: None,
            },
        )
        .await
        .unwrap()
        .unwrap();
    let failure = AttemptFailure::RetryAt {
        retry_at: Utc::now() - ChronoDuration::seconds(60),
        detail: json!({ "cause": "executorFailure" }),
    };
    let expected_failure = serde_json::to_value(&failure).unwrap();
    store
        .settle_attempt(&first.fence, AttemptSettlement::Failed(failure), None)
        .await
        .unwrap();
    assert_eq!(
        store
            .load_attempt(&first.attempt.attempt_id)
            .await
            .unwrap()
            .failure,
        Some(expected_failure)
    );

    let retry = store
        .claim_recovery_for_operation(
            &operation.operation_id,
            &ClaimRequest {
                owner_instance_id: CoordinatorInstanceId::new("retry-worker"),
                lease_seconds: 60,
                execution_profile: None,
            },
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(retry.selection.cause, RecoveryCause::RetryScheduled);
    assert_ne!(retry.attempt.attempt_id, first.attempt.attempt_id);
    assert_eq!(retry.attempt.attempt_number, 2);
    assert_eq!(retry.attempt.lease_epoch, 1);
    store
        .settle_attempt(&retry.fence, AttemptSettlement::Succeeded, None)
        .await
        .unwrap();

    let attempts = store.list_job_attempts(&job.job.job_id).await.unwrap();
    assert_eq!(attempts.len(), 2);
    assert_eq!(
        store.load_job(&job.job.job_id).await.unwrap().job.state,
        JobState::Succeeded
    );
    store.close().await;
}
