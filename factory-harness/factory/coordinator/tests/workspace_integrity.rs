use factory_coordinator::AttemptSettlement;
use factory_coordinator::ClaimRequest;
use factory_coordinator::CoordinatorInstanceId;
use factory_coordinator::CoordinatorStore;
use factory_coordinator::EnsureWorkspaceRequest;
use factory_coordinator::JobDefinition;
use factory_coordinator::OperationDefinition;
use factory_coordinator::WorkspaceManager;
use serde_json::json;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
use uuid::Uuid;

fn database_url() -> String {
    std::env::var("FACTORY_COORDINATOR_TEST_DATABASE_URL")
        .expect("set FACTORY_COORDINATOR_TEST_DATABASE_URL to a disposable PostgreSQL database")
}

struct TestRoot(PathBuf);

impl TestRoot {
    fn new() -> Self {
        Self(std::env::temp_dir().join(format!(
            "factory-workspace-integrity-{}-{}",
            std::process::id(),
            Uuid::new_v4()
        )))
    }
}

impl Drop for TestRoot {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn git<const N: usize>(args: [&str; N]) -> String {
    let output = Command::new("git").args(args).output().unwrap();
    assert!(
        output.status.success(),
        "git failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn path_text(path: &Path) -> &str {
    path.to_str().unwrap()
}

async fn create_job(store: &CoordinatorStore) -> factory_coordinator::DurableJob {
    store
        .create_job(JobDefinition {
            kind: format!("workspace-integrity-test-{}", Uuid::new_v4()),
            input: json!({}),
            operations: vec![OperationDefinition {
                kind: "execute".to_string(),
                input: json!({}),
                max_attempts: 1,
            }],
        })
        .await
        .unwrap()
}

async fn succeed_job(store: &CoordinatorStore, job: &factory_coordinator::DurableJob) {
    let lease = store
        .claim_recovery_for_operation(
            &job.operations[0].operation_id,
            &ClaimRequest {
                owner_instance_id: CoordinatorInstanceId::new(format!(
                    "workspace-integrity-worker-{}",
                    Uuid::new_v4()
                )),
                lease_seconds: 60,
                execution_profile: None,
            },
        )
        .await
        .unwrap()
        .unwrap();
    store
        .settle_attempt(&lease.fence, AttemptSettlement::Succeeded, None)
        .await
        .unwrap();
}

#[tokio::test]
#[ignore = "requires FACTORY_COORDINATOR_TEST_DATABASE_URL"]
async fn same_repository_jobs_and_rematerialization_keep_immutable_bases() {
    let root = TestRoot::new();
    std::fs::create_dir_all(&root.0).unwrap();
    let source = root.0.join("source");
    let workspaces = root.0.join("workspaces");
    git(["init", "-b", "main", path_text(&source)]);
    git([
        "-C",
        path_text(&source),
        "config",
        "user.name",
        "Factory Workspace Test",
    ]);
    git([
        "-C",
        path_text(&source),
        "config",
        "user.email",
        "factory-workspace@example.invalid",
    ]);
    std::fs::write(source.join("README.md"), b"immutable base\n").unwrap();
    git(["-C", path_text(&source), "add", "README.md"]);
    git(["-C", path_text(&source), "commit", "-m", "immutable base"]);
    let first_revision = git(["-C", path_text(&source), "rev-parse", "HEAD"]);

    let store = CoordinatorStore::connect(&database_url()).await.unwrap();
    store.migrate().await.unwrap();
    let manager = WorkspaceManager::new(&workspaces).unwrap();
    let repository_id = format!("fixture:shared: {}", Uuid::new_v4());
    let request = EnsureWorkspaceRequest {
        repository_id,
        repository: path_text(&source).to_string(),
        base_ref: "main".to_string(),
    };
    let first_job = create_job(&store).await;
    let second_job = create_job(&store).await;
    let recreate_job = create_job(&store).await;

    let first = manager
        .ensure(&store, &first_job.job.job_id, request.clone())
        .await
        .unwrap();
    let recreate = manager
        .ensure(&store, &recreate_job.job.job_id, request.clone())
        .await
        .unwrap();
    assert_eq!(first.base_revision, first_revision);
    assert_eq!(recreate.base_revision, first_revision);
    std::fs::write(Path::new(&first.root).join("JOB-ONE.txt"), b"job one\n").unwrap();

    let (refreshed_first, second) = tokio::join!(
        manager.refresh_revision(&store, &first_job.job.job_id),
        manager.ensure(&store, &second_job.job.job_id, request.clone())
    );
    let refreshed_first = refreshed_first.unwrap();
    let second = second.unwrap();
    std::fs::write(Path::new(&second.root).join("JOB-TWO.txt"), b"job two\n").unwrap();
    assert_eq!(
        git(["-C", &refreshed_first.root, "rev-parse", "HEAD"]),
        first_revision
    );
    assert_eq!(
        git(["-C", &second.root, "rev-parse", "HEAD"]),
        first_revision
    );
    assert_eq!(
        std::fs::read(Path::new(&refreshed_first.root).join("JOB-ONE.txt")).unwrap(),
        b"job one\n"
    );

    for workspace in [&refreshed_first, &second] {
        let snapshot = manager.capture_review_snapshot(workspace).await.unwrap();
        std::fs::write(
            Path::new(&workspace.root).join("README.md"),
            b"review mutation\n",
        )
        .unwrap();
        assert!(
            manager
                .restore_review_snapshot(workspace, snapshot)
                .await
                .unwrap()
        );
        manager
            .acknowledge_review_mutation(workspace)
            .await
            .unwrap();
        assert_eq!(
            std::fs::read(Path::new(&workspace.root).join("README.md")).unwrap(),
            b"immutable base\n"
        );
    }

    succeed_job(&store, &first_job).await;
    succeed_job(&store, &second_job).await;
    let first_result = manager
        .export_result(&store, &first_job.job.job_id)
        .await
        .unwrap();
    let second_result = manager
        .export_result(&store, &second_job.job.job_id)
        .await
        .unwrap();
    assert_eq!(first_result.base_revision, first_revision);
    assert_eq!(second_result.base_revision, first_revision);
    assert!(String::from_utf8_lossy(&first_result.patch).contains("JOB-ONE.txt"));
    assert!(!String::from_utf8_lossy(&first_result.patch).contains("JOB-TWO.txt"));
    assert!(String::from_utf8_lossy(&second_result.patch).contains("JOB-TWO.txt"));
    assert!(!String::from_utf8_lossy(&second_result.patch).contains("JOB-ONE.txt"));

    manager
        .remove(&store, &recreate_job.job.job_id)
        .await
        .unwrap();
    std::fs::write(source.join("README.md"), b"moving branch revision\n").unwrap();
    git(["-C", path_text(&source), "add", "README.md"]);
    git([
        "-C",
        path_text(&source),
        "commit",
        "-m",
        "move upstream branch",
    ]);
    let moved_revision = git(["-C", path_text(&source), "rev-parse", "HEAD"]);
    assert_ne!(moved_revision, first_revision);

    let rematerialized = manager
        .ensure(&store, &recreate_job.job.job_id, request)
        .await
        .unwrap();
    assert_eq!(rematerialized.base_revision, first_revision);
    assert_eq!(rematerialized.revision, first_revision);
    assert_eq!(
        git(["-C", &rematerialized.root, "rev-parse", "HEAD"]),
        first_revision
    );
    assert_eq!(
        std::fs::read(Path::new(&rematerialized.root).join("README.md")).unwrap(),
        b"immutable base\n"
    );

    store.close().await;
}
