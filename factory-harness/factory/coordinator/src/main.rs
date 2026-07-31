use anyhow::Context;
use anyhow::Result;
use anyhow::bail;
use clap::Parser;
use clap::Subcommand;
use factory_coordinator::AttemptState;
use factory_coordinator::CheckpointId;
use factory_coordinator::ClaimRequest;
use factory_coordinator::CoordinatorInstanceId;
use factory_coordinator::CoordinatorStore;
use factory_coordinator::CorrelationRecordId;
use factory_coordinator::JobDefinition;
use factory_coordinator::JobState;
use factory_coordinator::NewCheckpoint;
use factory_coordinator::OperationDefinition;
use factory_coordinator::RecoveryCause;
use factory_coordinator::ResumeStrategy;
use factory_protocol::FactoryCorrelation;
use factory_protocol::ids::AttemptId;
use factory_protocol::ids::FactoryRequestId;
use factory_protocol::ids::ItemId;
use factory_protocol::ids::JobId;
use factory_protocol::ids::OperationId;
use factory_protocol::ids::TaskRunExternalId;
use factory_protocol::ids::ThreadId;
use factory_protocol::ids::TurnId;
use factory_protocol::ids::WorkflowRunId;
use serde::Serialize;
use serde_json::json;
use std::io::Write;
use std::net::SocketAddr;
use tokio::net::TcpListener;

const ACCEPTANCE_KIND: &str = "factoryd.durable-recovery.acceptance";
const ACCEPTANCE_MARKER: &str = "factoryd-postgres-recovery-v1";

#[derive(Debug, Parser)]
#[command(name = "factoryd", about = "Software Factory durable coordinator")]
struct Cli {
    #[arg(long)]
    database_url: String,
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Apply coordinator schema migrations.
    Migrate,
    /// Serve the coordinator JSON API.
    Serve {
        #[arg(long, default_value = "0.0.0.0:8787")]
        bind: SocketAddr,
    },
    /// Persist a leased attempt, full runtime correlation, and checkpoint.
    AcceptanceWrite,
    /// Recover and finish the acceptance job created by a prior process.
    AcceptanceRecover {
        #[arg(long)]
        job_id: String,
    },
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct MigrationReceipt {
    schema: &'static str,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ServerReceipt {
    listening: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct AcceptanceWriteReceipt {
    phase: &'static str,
    job_id: JobId,
    operation_id: OperationId,
    attempt_id: AttemptId,
    correlation_id: CorrelationRecordId,
    checkpoint_id: CheckpointId,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct AcceptanceRecoveryReceipt {
    phase: &'static str,
    job_id: JobId,
    abandoned_attempt_id: AttemptId,
    resumed_attempt_id: AttemptId,
    checkpoint_id: CheckpointId,
    attempt_number: u32,
    final_job_state: JobState,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Migrate => {
            let store = connected_store(&cli.database_url).await?;
            store.close().await;
            print_json(&MigrationReceipt { schema: "current" })?;
        }
        Command::Serve { bind } => serve(&cli.database_url, bind).await?,
        Command::AcceptanceWrite => acceptance_write(&cli.database_url).await?,
        Command::AcceptanceRecover { job_id } => {
            acceptance_recover(&cli.database_url, JobId::new(job_id)).await?
        }
    }
    Ok(())
}

async fn serve(database_url: &str, bind: SocketAddr) -> Result<()> {
    let store = connected_store(database_url).await?;
    let listener = TcpListener::bind(bind)
        .await
        .with_context(|| format!("bind factoryd HTTP server to {bind}"))?;
    let listening = listener
        .local_addr()
        .context("read factoryd HTTP listener address")?;
    print_json(&ServerReceipt {
        listening: listening.to_string(),
    })?;
    std::io::stdout()
        .flush()
        .context("flush factoryd server receipt")?;
    factory_coordinator::serve_http(store, listener)
        .await
        .context("serve factoryd HTTP API")
}

async fn connected_store(database_url: &str) -> Result<CoordinatorStore> {
    let store = CoordinatorStore::connect(database_url)
        .await
        .context("connect factoryd to PostgreSQL")?;
    store
        .migrate()
        .await
        .context("apply factoryd coordinator schema")?;
    Ok(store)
}

async fn acceptance_write(database_url: &str) -> Result<()> {
    let store = connected_store(database_url).await?;
    let workflow_run_id =
        WorkflowRunId::new(format!("acceptance-workflow-{}", uuid::Uuid::new_v4()));
    let durable_job = store
        .create_job(JobDefinition {
            kind: ACCEPTANCE_KIND.to_string(),
            input: json!({ "marker": ACCEPTANCE_MARKER }),
            workflow_run_id: Some(workflow_run_id.clone()),
            operations: vec![OperationDefinition {
                kind: "codex-turn".to_string(),
                input: json!({ "task": "resume after durable checkpoint" }),
                max_attempts: 3,
            }],
        })
        .await
        .context("persist acceptance job")?;
    let operation = durable_job
        .operations
        .first()
        .context("acceptance job has no operation")?;
    let lease = store
        .claim_recovery_for_job(
            &durable_job.job.job_id,
            &ClaimRequest {
                owner_instance_id: CoordinatorInstanceId::new("acceptance-writer"),
                lease_seconds: 0,
            },
        )
        .await
        .context("claim initial acceptance attempt")?
        .context("initial acceptance operation was not eligible")?;
    if lease.selection.cause != RecoveryCause::NewOperation {
        bail!("initial acceptance claim was not a new operation");
    }

    let correlation = store
        .append_correlation(&FactoryCorrelation {
            job_id: durable_job.job.job_id.clone(),
            operation_id: operation.operation_id.clone(),
            attempt_id: lease.attempt.attempt_id.clone(),
            workflow_run_id: Some(workflow_run_id),
            task_run_external_id: Some(TaskRunExternalId::new("acceptance-task-run")),
            request_id: FactoryRequestId::new(format!(
                "acceptance-request-{}",
                durable_job.job.job_id
            )),
            thread_id: Some(ThreadId::new("acceptance-thread")),
            turn_id: Some(TurnId::new("acceptance-turn")),
            item_id: Some(ItemId::new("acceptance-item")),
        })
        .await
        .context("persist complete app-server correlation")?;
    let checkpoint = store
        .save_checkpoint(NewCheckpoint {
            attempt_id: lease.attempt.attempt_id.clone(),
            kind: "turn-progress".to_string(),
            payload: json!({
                "marker": ACCEPTANCE_MARKER,
                "resumeAt": "after-tool-result"
            }),
            workspace_root: None,
            workspace_revision: Some("acceptance-revision-1".to_string()),
            correlation_id: Some(correlation.correlation_id.clone()),
        })
        .await
        .context("persist acceptance checkpoint")?;
    let receipt = AcceptanceWriteReceipt {
        phase: "written",
        job_id: durable_job.job.job_id,
        operation_id: operation.operation_id.clone(),
        attempt_id: lease.attempt.attempt_id,
        correlation_id: correlation.correlation_id,
        checkpoint_id: checkpoint.checkpoint_id,
    };
    store.close().await;
    print_json(&receipt)
}

async fn acceptance_recover(database_url: &str, job_id: JobId) -> Result<()> {
    let store = connected_store(database_url).await?;
    let durable_job = store
        .load_job(&job_id)
        .await
        .context("reload acceptance job from PostgreSQL")?;
    if durable_job.job.kind != ACCEPTANCE_KIND {
        bail!("job {job_id} is not a factoryd recovery acceptance job");
    }
    let selection = store
        .select_recovery_for_job(&job_id)
        .await
        .context("select recovery after process restart")?
        .context("acceptance job is not eligible for recovery")?;
    if selection.cause != RecoveryCause::LeaseExpired {
        bail!("acceptance job did not expose an expired lease");
    }
    let abandoned_attempt_id = selection
        .previous_attempt_id
        .clone()
        .context("recovery selection lost its previous attempt")?;
    let checkpoint = match &selection.resume {
        ResumeStrategy::FromCheckpoint(checkpoint) => checkpoint.clone(),
        ResumeStrategy::Fresh => bail!("recovery selection did not load its checkpoint"),
    };
    let loaded_checkpoint = store
        .load_checkpoint(&checkpoint.checkpoint_id)
        .await
        .context("load selected checkpoint by durable id")?
        .context("selected checkpoint disappeared")?;
    if loaded_checkpoint != checkpoint {
        bail!("checkpoint changed between recovery selection and durable load");
    }
    if checkpoint.payload.get("marker") != Some(&json!(ACCEPTANCE_MARKER)) {
        bail!("recovered checkpoint payload has the wrong marker");
    }
    let checkpoint_correlation = selection
        .checkpoint_correlation
        .as_ref()
        .context("checkpoint lost its bound runtime correlation")?;
    if checkpoint_correlation.correlation.thread_id.is_none()
        || checkpoint_correlation.correlation.turn_id.is_none()
        || checkpoint_correlation.correlation.item_id.is_none()
    {
        bail!("recovered correlation is missing app-server identifiers");
    }

    let recovered = store
        .claim_recovery_for_job(
            &job_id,
            &ClaimRequest {
                owner_instance_id: CoordinatorInstanceId::new("acceptance-recoverer"),
                lease_seconds: 300,
            },
        )
        .await
        .context("claim recovery after process restart")?
        .context("acceptance recovery lost eligibility before claim")?;
    if recovered.attempt.resumes_checkpoint_id.as_ref() != Some(&checkpoint.checkpoint_id)
        || recovered.attempt.resumes_attempt_id.as_ref() != Some(&checkpoint.attempt_id)
    {
        bail!("recovered attempt did not bind its checkpoint and source attempt");
    }
    let abandoned_attempt = store
        .load_attempt(&abandoned_attempt_id)
        .await
        .context("reload abandoned attempt")?;
    if abandoned_attempt.state != AttemptState::Abandoned {
        bail!("expired attempt was not durably marked abandoned");
    }
    store
        .complete_attempt(&recovered.attempt.attempt_id)
        .await
        .context("complete recovered attempt")?;
    let final_job = store
        .load_job(&job_id)
        .await
        .context("reload completed acceptance job")?;
    if final_job.job.state != JobState::Succeeded {
        bail!("recovered job did not reach succeeded state");
    }

    let receipt = AcceptanceRecoveryReceipt {
        phase: "recovered",
        job_id,
        abandoned_attempt_id,
        resumed_attempt_id: recovered.attempt.attempt_id,
        checkpoint_id: checkpoint.checkpoint_id,
        attempt_number: recovered.attempt.attempt_number,
        final_job_state: final_job.job.state,
    };
    store.close().await;
    print_json(&receipt)
}

fn print_json(value: &impl Serialize) -> Result<()> {
    println!("{}", serde_json::to_string(value)?);
    Ok(())
}
