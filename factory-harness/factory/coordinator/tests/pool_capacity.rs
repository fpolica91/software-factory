use factory_coordinator::ClaimRequest;
use factory_coordinator::CoordinatorInstanceId;
use factory_coordinator::CoordinatorStore;
use factory_coordinator::Correlation;
use factory_coordinator::JobDefinition;
use factory_coordinator::JobId;
use factory_coordinator::NewAttemptEvent;
use factory_coordinator::OperationDefinition;
use factory_coordinator::RequestId;
use factory_coordinator::ThreadId;
use serde_json::json;
use std::time::Duration;
use uuid::Uuid;

fn database_url() -> String {
    std::env::var("FACTORY_COORDINATOR_TEST_DATABASE_URL")
        .expect("set FACTORY_COORDINATOR_TEST_DATABASE_URL to a disposable PostgreSQL database")
}

#[tokio::test]
#[ignore = "requires FACTORY_COORDINATOR_TEST_DATABASE_URL"]
async fn maximum_worker_slots_leave_durable_control_capacity() {
    let store =
        CoordinatorStore::connect_for_worker(&database_url(), CoordinatorStore::MAX_WORKER_SLOTS)
            .await
            .unwrap();
    store.migrate().await.unwrap();
    let job = store
        .create_job(JobDefinition {
            kind: format!("pool-capacity-test-{}", Uuid::new_v4()),
            input: json!({}),
            operations: vec![OperationDefinition {
                kind: "execute".to_string(),
                input: json!({}),
                max_attempts: 1,
            }],
        })
        .await
        .unwrap();
    let lease = store
        .claim_recovery_for_operation(
            &job.operations[0].operation_id,
            &ClaimRequest {
                owner_instance_id: CoordinatorInstanceId::new("pool-capacity-worker"),
                lease_seconds: 60,
                execution_profile: None,
            },
        )
        .await
        .unwrap()
        .unwrap();

    // Fill every advisory-lock connection: one per accepted worker slot plus
    // the two reserved for nested workspace/repository lifecycle activity.
    let mut guards = Vec::new();
    for index in 0..(CoordinatorStore::MAX_WORKER_SLOTS + 2) {
        guards.push(
            tokio::time::timeout(
                Duration::from_secs(5),
                store.acquire_workspace_execution(&JobId::new(format!(
                    "pool-capacity-lock-{index}-{}",
                    Uuid::new_v4()
                ))),
            )
            .await
            .expect("configured lock capacity must accept every reserved connection")
            .unwrap(),
        );
    }

    let thread_id = ThreadId::new(format!("pool-capacity-thread-{}", Uuid::new_v4()));
    let correlation = Correlation {
        job_id: job.job.job_id.clone(),
        operation_id: job.operations[0].operation_id.clone(),
        attempt_id: lease.attempt.attempt_id.clone(),
        request_id: RequestId::new(format!("pool-capacity-request-{}", Uuid::new_v4())),
        thread_id: Some(thread_id),
        turn_id: None,
        item_id: None,
    };
    let progressed = tokio::time::timeout(Duration::from_secs(10), async {
        tokio::try_join!(
            store.renew_attempt(&lease.fence, 60),
            store.append_correlation(&lease.fence, &correlation),
            store.append_attempt_event(
                &lease.fence,
                NewAttemptEvent {
                    kind: "worker.heartbeat".to_string(),
                    payload: json!({ "slots": CoordinatorStore::MAX_WORKER_SLOTS }),
                    deduplication_key: None,
                },
            ),
        )
    })
    .await
    .expect("durable query/control traffic must not wait for advisory-lock connections")
    .unwrap();
    assert_eq!(progressed.0.attempt_id, lease.attempt.attempt_id);
    assert_eq!(
        progressed.1.correlation.attempt_id,
        lease.attempt.attempt_id
    );
    assert_eq!(progressed.2.kind, "worker.heartbeat");

    for guard in guards {
        guard.release().await.unwrap();
    }
    store.close().await;
}
