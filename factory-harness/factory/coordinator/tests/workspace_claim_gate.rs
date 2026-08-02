use factory_coordinator::ClaimRequest;
use factory_coordinator::CoordinatorInstanceId;
use factory_coordinator::CoordinatorStore;
use factory_coordinator::ExecutionProfile;
use factory_coordinator::JobDefinition;
use factory_coordinator::JobState;
use factory_coordinator::OperationDefinition;
use factory_coordinator::WorkspaceBinding;
use serde_json::json;

fn database_url() -> String {
    std::env::var("FACTORY_COORDINATOR_TEST_DATABASE_URL")
        .expect("set FACTORY_COORDINATOR_TEST_DATABASE_URL to a disposable PostgreSQL database")
}

#[tokio::test]
#[ignore = "requires FACTORY_COORDINATOR_TEST_DATABASE_URL"]
async fn factory_task_cannot_be_claimed_until_its_workspace_is_active() {
    let store = CoordinatorStore::connect(&database_url()).await.unwrap();
    store.migrate().await.unwrap();
    let job = store
        .create_job(JobDefinition {
            kind: "factory.task".to_string(),
            input: json!({
                "task": "exercise workspace claim gate",
                "executionProfile": { "provider": "test", "model": "test-model" }
            }),
            operations: vec![OperationDefinition {
                kind: "plan".to_string(),
                input: json!({}),
                max_attempts: 1,
            }],
        })
        .await
        .unwrap();
    let claim = ClaimRequest {
        owner_instance_id: CoordinatorInstanceId::new("workspace-gate-worker"),
        lease_seconds: 60,
        execution_profile: Some(ExecutionProfile {
            provider: "test".to_string(),
            model: "test-model".to_string(),
        }),
    };

    assert!(
        store
            .select_recovery_for_operation(&job.operations[0].operation_id)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        store
            .claim_recovery_for_operation(&job.operations[0].operation_id, &claim)
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        store.load_job(&job.job.job_id).await.unwrap().job.state,
        JobState::Queued
    );
    assert!(
        store
            .list_job_attempts(&job.job.job_id)
            .await
            .unwrap()
            .is_empty()
    );

    store
        .put_workspace(&WorkspaceBinding {
            job_id: job.job.job_id.clone(),
            repository_id: "local:test-repository".to_string(),
            repository: "/tmp/source-repository".to_string(),
            base_ref: "HEAD".to_string(),
            base_revision: "0123456789abcdef".to_string(),
            branch_name: "factory/workspace-claim-gate".to_string(),
            root: "/tmp/factory-workspace".to_string(),
            revision: "0123456789abcdef".to_string(),
        })
        .await
        .unwrap();

    let mismatched_claim = ClaimRequest {
        owner_instance_id: CoordinatorInstanceId::new("wrong-profile-worker"),
        lease_seconds: 60,
        execution_profile: Some(ExecutionProfile {
            provider: "other".to_string(),
            model: "other-model".to_string(),
        }),
    };
    assert!(
        store
            .claim_recovery_for_operation(&job.operations[0].operation_id, &mismatched_claim,)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        store
            .list_job_attempts(&job.job.job_id)
            .await
            .unwrap()
            .is_empty()
    );

    let unpinned = store
        .create_job(JobDefinition {
            kind: "factory.task".to_string(),
            input: json!({ "task": "legacy task without an execution profile" }),
            operations: vec![OperationDefinition {
                kind: "plan".to_string(),
                input: json!({}),
                max_attempts: 1,
            }],
        })
        .await
        .unwrap();
    store
        .put_workspace(&WorkspaceBinding {
            job_id: unpinned.job.job_id.clone(),
            repository_id: "local:legacy-repository".to_string(),
            repository: "/tmp/legacy-source-repository".to_string(),
            base_ref: "HEAD".to_string(),
            base_revision: "fedcba9876543210".to_string(),
            branch_name: "factory/unpinned-workspace-claim-gate".to_string(),
            root: "/tmp/factory-unpinned-workspace".to_string(),
            revision: "fedcba9876543210".to_string(),
        })
        .await
        .unwrap();
    assert!(
        store
            .claim_recovery_for_operation(&unpinned.operations[0].operation_id, &claim)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        store
            .list_job_attempts(&unpinned.job.job_id)
            .await
            .unwrap()
            .is_empty()
    );

    let lease = store
        .claim_recovery_for_operation(&job.operations[0].operation_id, &claim)
        .await
        .unwrap()
        .expect("an active workspace must make factory.task claimable");
    assert_eq!(lease.selection.job_id, job.job.job_id);
    assert_eq!(lease.selection.operation_id, job.operations[0].operation_id);
    assert_eq!(
        store.load_job(&job.job.job_id).await.unwrap().job.state,
        JobState::Running
    );
    assert_eq!(
        store
            .list_job_attempts(&job.job.job_id)
            .await
            .unwrap()
            .len(),
        1
    );

    store.close().await;
}
