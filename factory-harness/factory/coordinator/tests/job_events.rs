use factory_coordinator::AttemptFence;
use factory_coordinator::ClaimRequest;
use factory_coordinator::CoordinatorError;
use factory_coordinator::CoordinatorInstanceId;
use factory_coordinator::CoordinatorStore;
use factory_coordinator::JobDefinition;
use factory_coordinator::NewAttemptEvent;
use factory_coordinator::NewJobEvent;
use factory_coordinator::OperationDefinition;
use serde_json::json;
use uuid::Uuid;

fn database_url() -> String {
    std::env::var("FACTORY_COORDINATOR_TEST_DATABASE_URL")
        .expect("set FACTORY_COORDINATOR_TEST_DATABASE_URL to a disposable PostgreSQL database")
}

#[tokio::test]
#[ignore = "requires FACTORY_COORDINATOR_TEST_DATABASE_URL"]
async fn events_persist_in_cursor_order_and_reject_stale_attempt_owners() {
    let store = CoordinatorStore::connect(&database_url()).await.unwrap();
    store.migrate().await.unwrap();
    let job = store
        .create_job(JobDefinition {
            kind: format!("job-event-test-{}", Uuid::new_v4()),
            input: json!({}),
            operations: vec![OperationDefinition {
                kind: "execute".to_string(),
                input: json!({}),
                max_attempts: 1,
            }],
        })
        .await
        .unwrap();
    let created = store
        .append_job_event(NewJobEvent {
            job_id: job.job.job_id.clone(),
            kind: "job.created".to_string(),
            payload: json!({ "state": "queued" }),
        })
        .await
        .unwrap();
    let lease = store
        .claim_recovery_for_operation(
            &job.operations[0].operation_id,
            &ClaimRequest {
                owner_instance_id: CoordinatorInstanceId::new("event-test-worker"),
                lease_seconds: 60,
                execution_profile: None,
            },
        )
        .await
        .unwrap()
        .unwrap();
    let started = store
        .append_attempt_event(
            &lease.fence,
            NewAttemptEvent {
                kind: "turn.started".to_string(),
                payload: json!({ "turnId": "turn-1" }),
                deduplication_key: None,
            },
        )
        .await
        .unwrap();
    let completed = store
        .append_attempt_event(
            &lease.fence,
            NewAttemptEvent {
                kind: "turn.completed".to_string(),
                payload: json!({ "turnId": "turn-1" }),
                deduplication_key: None,
            },
        )
        .await
        .unwrap();

    assert!(created.sequence < started.sequence && started.sequence < completed.sequence);
    assert_eq!(started.job_id, job.job.job_id);
    assert_eq!(
        started.operation_id.as_ref(),
        Some(&job.operations[0].operation_id)
    );
    assert_eq!(started.attempt_id.as_ref(), Some(&lease.attempt.attempt_id));

    let first_page = store.list_job_events(&job.job.job_id, 0, 2).await.unwrap();
    assert_eq!(first_page.events, vec![created, started]);
    assert_eq!(first_page.next_cursor, first_page.events[1].sequence);
    let second_page = store
        .list_job_events(&job.job.job_id, first_page.next_cursor, 2)
        .await
        .unwrap();
    assert_eq!(second_page.events, vec![completed]);
    let empty_page = store
        .list_job_events(&job.job.job_id, second_page.next_cursor, 2)
        .await
        .unwrap();
    assert!(empty_page.events.is_empty());
    assert_eq!(empty_page.next_cursor, second_page.next_cursor);

    let detailed_activity = NewAttemptEvent {
        kind: "factory.subagent.activity".to_string(),
        payload: json!({ "call_id": "call-1", "status": "completed" }),
        deduplication_key: Some("factory.subagent.activity:stable-call-1".to_string()),
    };
    let archived = store
        .append_attempt_event(&lease.fence, detailed_activity.clone())
        .await
        .unwrap();
    let replayed = store
        .append_attempt_event(&lease.fence, detailed_activity.clone())
        .await
        .unwrap();
    assert_eq!(replayed, archived);
    assert!(matches!(
        store
            .append_attempt_event(
                &lease.fence,
                NewAttemptEvent {
                    payload: json!({ "call_id": "call-1", "status": "different" }),
                    ..detailed_activity
                },
            )
            .await
            .unwrap_err(),
        CoordinatorError::InvalidInput(_)
    ));

    let stale_fence = AttemptFence {
        attempt_id: lease.fence.attempt_id.clone(),
        owner_instance_id: lease.fence.owner_instance_id.clone(),
        lease_epoch: lease.fence.lease_epoch + 1,
    };
    assert!(matches!(
        store
            .append_attempt_event(
                &stale_fence,
                NewAttemptEvent {
                    kind: "turn.invalid".to_string(),
                    payload: json!({}),
                    deduplication_key: None,
                },
            )
            .await
            .unwrap_err(),
        CoordinatorError::AttemptLeaseUnavailable(_)
    ));
    let final_page = store.list_job_events(&job.job.job_id, 0, 10).await.unwrap();
    assert_eq!(final_page.events.len(), 4);
    store.close().await;
}
