use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

use factory_coordinator::AttemptFailure;
use factory_coordinator::AttemptSettlement;
use factory_coordinator::ClaimRequest;
use factory_coordinator::CoordinatorInstanceId;
use factory_coordinator::CoordinatorStore;
use factory_coordinator::ExecutionProfile;
use factory_coordinator::FactoryTaskInput;
use factory_coordinator::JobDefinition;
use factory_coordinator::OperationDefinition;
use factory_coordinator::WorkspaceBinding;
use factory_coordinator::WorkspaceManager;
use serde_json::json;
use sqlx::Connection;
use sqlx::PgConnection;

const SECRET: &str = "disposable-profile-guard-secret-must-not-print";

fn database_url() -> String {
    std::env::var("FACTORY_COORDINATOR_TEST_DATABASE_URL")
        .expect("set FACTORY_COORDINATOR_TEST_DATABASE_URL to a disposable PostgreSQL database")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires FACTORY_COORDINATOR_TEST_DATABASE_URL"]
async fn live_factoryd_refuses_force_overrides_and_matching_profile_succeeds() {
    let fixture = TestRoot::new();
    std::fs::create_dir_all(&fixture.0).unwrap();
    let config = fixture.0.join("factory.env");
    write_config(&config, "openai", "gpt-5.6-sol");

    let store = CoordinatorStore::connect(&database_url()).await.unwrap();
    store.migrate().await.unwrap();
    assert!(
        store.list_active_jobs().await.unwrap().is_empty(),
        "the acceptance database must be disposable and empty"
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let server_store = store.clone();
    let workspaces = WorkspaceManager::new(fixture.0.join("workspaces")).unwrap();
    let server = tokio::spawn(async move {
        factory_coordinator::serve_http_with_workspaces(server_store, workspaces, listener)
            .await
            .unwrap();
    });

    let old_job = create_job(&store, "openai", "gpt-5.6-sol").await;
    let refusal = invoke_configure(&config, &url, false, "deepseek", "deepseek-v4-flash").await;
    assert!(!refusal.status.success());
    let refusal_text = combined_output(&refusal);
    assert!(refusal_text.contains(old_job.job.job_id.as_str()));
    assert!(refusal_text.contains("factory status"));
    assert!(refusal_text.contains("factory stop"));
    assert!(!refusal_text.contains(SECRET));

    let forced = invoke_configure(&config, &url, true, "deepseek", "deepseek-v4-flash").await;
    assert!(forced.status.success(), "{}", combined_output(&forced));
    let forced_text = combined_output(&forced);
    assert!(forced_text.contains("Warning: forcing provider/model switch"));
    assert!(!forced_text.contains(SECRET));
    assert_eq!(
        store.load_job(&old_job.job.job_id).await.unwrap().job.state,
        factory_coordinator::JobState::Queued,
        "--force must not cancel or migrate the blocked job"
    );

    store.cancel_job(&old_job.job.job_id).await.unwrap();
    let matching_job = create_job(&store, "deepseek", "deepseek-v4-flash").await;
    write_config(&config, "openai", "gpt-5.6-sol");
    let matching = invoke_configure(&config, &url, false, "deepseek", "deepseek-v4-flash").await;
    assert!(matching.status.success(), "{}", combined_output(&matching));
    assert!(!combined_output(&matching).contains(SECRET));

    store.cancel_job(&matching_job.job.job_id).await.unwrap();
    server.abort();
    store.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires FACTORY_COORDINATOR_TEST_DATABASE_URL"]
async fn retained_claude_job_is_migrated_then_matches_guard_and_claim() {
    let fixture = TestRoot::new();
    std::fs::create_dir_all(&fixture.0).unwrap();
    let config = fixture.0.join("factory.env");
    write_config(&config, "openai", "gpt-5.6-sol");

    let database_url = database_url();
    let store = CoordinatorStore::connect(&database_url).await.unwrap();
    store.migrate().await.unwrap();
    assert!(store.list_active_jobs().await.unwrap().is_empty());
    let job = create_job(&store, "claude", "claude-sonnet-5").await;
    store
        .put_workspace(&WorkspaceBinding {
            job_id: job.job.job_id.clone(),
            repository_id: "local:retained-anthropic-profile".to_string(),
            repository: "/tmp/retained-anthropic-profile".to_string(),
            base_ref: "HEAD".to_string(),
            base_revision: "0123456789abcdef".to_string(),
            branch_name: "factory/retained-anthropic-profile".to_string(),
            root: "/tmp/retained-anthropic-profile-worktree".to_string(),
            revision: "0123456789abcdef".to_string(),
        })
        .await
        .unwrap();

    // Re-run only the one upgrade to model a retained database created before
    // migration 13 existed.
    let mut connection = PgConnection::connect(&database_url).await.unwrap();
    sqlx::query("DELETE FROM factory_coordinator_schema_migrations WHERE version = 13")
        .execute(&mut connection)
        .await
        .unwrap();
    connection.close().await.unwrap();
    store.migrate().await.unwrap();

    let migrated = store.load_job(&job.job.job_id).await.unwrap();
    let input: FactoryTaskInput = serde_json::from_value(migrated.job.input).unwrap();
    assert_eq!(
        input.execution_profile.unwrap(),
        ExecutionProfile {
            provider: "anthropic".to_string(),
            model: "claude-sonnet-5".to_string(),
        }
    );

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let server_store = store.clone();
    let workspaces = WorkspaceManager::new(fixture.0.join("workspaces")).unwrap();
    let server = tokio::spawn(async move {
        factory_coordinator::serve_http_with_workspaces(server_store, workspaces, listener)
            .await
            .unwrap();
    });

    let matching = invoke_configure(&config, &url, false, "anthropic", "claude-sonnet-5").await;
    assert!(matching.status.success(), "{}", combined_output(&matching));
    assert!(!combined_output(&matching).contains("Warning:"));
    assert!(!combined_output(&matching).contains(SECRET));

    let alias_claim = ClaimRequest {
        owner_instance_id: CoordinatorInstanceId::new("legacy-alias-worker"),
        lease_seconds: 60,
        execution_profile: Some(ExecutionProfile {
            provider: "claude".to_string(),
            model: "claude-sonnet-5".to_string(),
        }),
    };
    assert!(
        store
            .claim_recovery_for_operation(&job.operations[0].operation_id, &alias_claim)
            .await
            .unwrap()
            .is_none(),
        "claim matching is canonical and does not carry a claude compatibility path"
    );
    let lease = store
        .claim_recovery_for_operation(
            &job.operations[0].operation_id,
            &ClaimRequest {
                owner_instance_id: CoordinatorInstanceId::new("canonical-anthropic-worker"),
                lease_seconds: 60,
                execution_profile: Some(ExecutionProfile {
                    provider: "anthropic".to_string(),
                    model: "claude-sonnet-5".to_string(),
                }),
            },
        )
        .await
        .unwrap()
        .expect("the migrated canonical profile must be claimable");
    store
        .settle_attempt(
            &lease.fence,
            AttemptSettlement::Failed(AttemptFailure::Terminal {
                detail: json!({ "cause": "acceptanceComplete" }),
            }),
            None,
        )
        .await
        .unwrap();

    server.abort();
    store.close().await;
}

async fn create_job(
    store: &CoordinatorStore,
    provider: &str,
    model: &str,
) -> factory_coordinator::DurableJob {
    store
        .create_job(JobDefinition {
            kind: "factory.task".to_string(),
            input: serde_json::to_value(FactoryTaskInput {
                task: "profile guard acceptance".to_string(),
                execution_profile: Some(ExecutionProfile {
                    provider: provider.to_string(),
                    model: model.to_string(),
                }),
                repository_id: None,
                developer_instructions: None,
            })
            .unwrap(),
            operations: vec![OperationDefinition {
                kind: "codex.plan".to_string(),
                input: json!({}),
                max_attempts: 1,
            }],
        })
        .await
        .unwrap()
}

async fn invoke_configure(
    config: &Path,
    factoryd_url: &str,
    force: bool,
    provider: &str,
    model: &str,
) -> std::process::Output {
    let config = config.to_path_buf();
    let factoryd_url = factoryd_url.to_string();
    let provider = provider.to_string();
    let model = model.to_string();
    tokio::task::spawn_blocking(move || {
        let mut command = Command::new(env!("CARGO_BIN_EXE_factory"));
        command
            .args(["--factoryd-url", &factoryd_url, "--config-file"])
            .arg(config)
            .args([
                "configure",
                "--provider",
                &provider,
                "--model",
                &model,
                "--api-key",
                SECRET,
            ]);
        if force {
            command.arg("--force");
        }
        for name in [
            "OPENAI_API_KEY",
            "ANTHROPIC_API_KEY",
            "DEEPSEEK_API_KEY",
            "ZAI_API_KEY",
        ] {
            command.env_remove(name);
        }
        command.output().unwrap()
    })
    .await
    .unwrap()
}

fn write_config(path: &Path, provider: &str, model: &str) {
    std::fs::write(
        path,
        format!(
            "FACTORY_PROVIDER_ADAPTER=\"{provider}\"\nFACTORY_MODEL=\"{model}\"\nOPENAI_API_KEY=\"old-key\"\nFACTORY_OPENAI_BASE_URL=\"https://api.openai.com/v1\"\n"
        ),
    )
    .unwrap();
}

fn combined_output(output: &std::process::Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

struct TestRoot(PathBuf);

impl TestRoot {
    fn new() -> Self {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        Self(std::env::temp_dir().join(format!(
            "factory-profile-guard-postgres-{}-{stamp}",
            std::process::id()
        )))
    }
}

impl Drop for TestRoot {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
