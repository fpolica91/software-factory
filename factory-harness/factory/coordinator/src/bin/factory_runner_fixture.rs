use anyhow::Context;
use anyhow::Result;
use chrono::Utc;
use clap::Parser;
use factory_coordinator::AttemptFailure;
use factory_coordinator::CancellationHandle;
use factory_coordinator::CoordinatorInstanceId;
use factory_coordinator::CoordinatorStore;
use factory_coordinator::DurableRunner;
use factory_coordinator::NewAttemptEvent;
use factory_coordinator::OperationCheckpoint;
use factory_coordinator::OperationExecutionContext;
use factory_coordinator::OperationExecutionResult;
use factory_coordinator::OperationExecutor;
use factory_coordinator::OperationOutcome;
use factory_coordinator::RecoveryLease;
use factory_coordinator::RunnerConfig;
use serde_json::json;
use std::future::Future;
use std::io::Write as _;
use std::path::PathBuf;
use std::pin::Pin;
use std::time::Duration;

const SUCCESS_KIND: &str = "acceptance.runner.success";
const RETRY_KIND: &str = "acceptance.runner.retry";
const PLAN_VALIDATION_RETRY_KIND: &str = "acceptance.runner.plan-validation-retry";
const REVIEW_PANIC_RETRY_KIND: &str = "acceptance.runner.review-panic-retry";
const CANCEL_KIND: &str = "acceptance.runner.cancel";
const RECOVER_KIND: &str = "acceptance.runner.recover";
const FENCE_KIND: &str = "acceptance.runner.fence";
const SHUTDOWN_KIND: &str = "acceptance.runner.shutdown";
const INVALID_CHECKPOINT_KIND: &str = "acceptance.runner.invalid-checkpoint";
const SLOW_SUCCESS_KIND: &str = "acceptance.runner.slow-success";
const OVERLAP_KIND: &str = "acceptance.runner.workspace-overlap";

#[derive(Debug, Parser)]
#[command(about = "Acceptance-only durable runner fixture")]
struct Cli {
    #[arg(long)]
    database_url: String,
    #[arg(long)]
    worker_id: String,
    #[arg(long, default_value_t = 4)]
    lease_seconds: u64,
    #[arg(long, default_value_t = 100)]
    poll_milliseconds: u64,
    #[arg(long, default_value_t = 1)]
    slots: usize,
    /// Gate used only by the workspace-overlap acceptance scenario. Epoch one
    /// remains alive and owns the workspace lock until this path exists.
    #[arg(long)]
    drain_gate: Option<PathBuf>,
}

struct FixtureExecutor {
    store: CoordinatorStore,
    drain_gate: Option<PathBuf>,
}

impl OperationExecutor for FixtureExecutor {
    fn execute(
        &self,
        context: OperationExecutionContext,
        cancellation: CancellationHandle,
    ) -> Pin<Box<dyn Future<Output = OperationExecutionResult> + Send + '_>> {
        let lease = context.lease().clone();
        let store = self.store.clone();
        let drain_gate = self.drain_gate.clone();
        Box::pin(async move {
            emit(&lease, "started");
            let outcome = match lease.selection.operation_kind.as_str() {
                SUCCESS_KIND => complete(&lease, "success"),
                RETRY_KIND if lease.attempt.attempt_number == 1 => {
                    emit(&lease, "retryScheduled");
                    OperationOutcome::Fail {
                        checkpoint: Some(checkpoint(&lease, "failed-first")),
                        failure: AttemptFailure::RetryAt {
                            retry_at: Utc::now(),
                            detail: json!({ "reason": "acceptance fail-first" }),
                        },
                    }
                }
                RETRY_KIND => complete(&lease, "retry-success"),
                PLAN_VALIDATION_RETRY_KIND if lease.attempt.attempt_number == 1 => {
                    emit(&lease, "planValidationRejected");
                    OperationOutcome::Fail {
                        checkpoint: Some(checkpoint(&lease, "invalid-plan-business-result")),
                        failure: AttemptFailure::RetryAt {
                            retry_at: Utc::now(),
                            detail: json!({
                                "cause": "stageExecutionRetry",
                                "message": "Plan validation rejected the business result"
                            }),
                        },
                    }
                }
                PLAN_VALIDATION_RETRY_KIND => complete(&lease, "plan-validation-recovered"),
                REVIEW_PANIC_RETRY_KIND if lease.attempt.attempt_number == 1 => {
                    emit(&lease, "reviewTaskPanicking");
                    panic!("acceptance review task panic");
                }
                REVIEW_PANIC_RETRY_KIND => complete(&lease, "review-panic-recovered"),
                CANCEL_KIND => {
                    cancellation.cancelled().await;
                    emit(&lease, "cancellationObserved");
                    OperationOutcome::Complete {
                        checkpoint: None,
                        completion_event: None,
                    }
                }
                RECOVER_KIND if lease.attempt.lease_epoch == 1 => {
                    emit(&lease, "waitingForProcessKill");
                    cancellation.cancelled().await;
                    OperationOutcome::Complete {
                        checkpoint: None,
                        completion_event: None,
                    }
                }
                RECOVER_KIND => complete(&lease, "lease-recovered"),
                FENCE_KIND if lease.attempt.lease_epoch == 1 => {
                    emit(&lease, "waitingForProcessKill");
                    cancellation.cancelled().await;
                    OperationOutcome::Complete {
                        checkpoint: None,
                        completion_event: None,
                    }
                }
                FENCE_KIND => {
                    emit(&lease, "newLeaseHeld");
                    cancellation.cancelled().await;
                    emit(&lease, "cancellationObserved");
                    OperationOutcome::Complete {
                        checkpoint: None,
                        completion_event: None,
                    }
                }
                SHUTDOWN_KIND if lease.attempt.lease_epoch == 1 => {
                    cancellation.cancelled().await;
                    emit(&lease, "cancellationObserved");
                    OperationOutcome::Complete {
                        checkpoint: None,
                        completion_event: None,
                    }
                }
                SHUTDOWN_KIND => complete(&lease, "shutdown-recovered"),
                INVALID_CHECKPOINT_KIND => OperationOutcome::Complete {
                    checkpoint: Some(OperationCheckpoint {
                        kind: String::new(),
                        payload: json!({ "scenario": "isolated-invalid-checkpoint" }),
                        workspace_root: None,
                        workspace_revision: None,
                        correlation_id: None,
                    }),
                    completion_event: None,
                },
                SLOW_SUCCESS_KIND => {
                    tokio::time::sleep(Duration::from_millis(500)).await;
                    complete(&lease, "isolated-success")
                }
                OVERLAP_KIND => {
                    emit(&lease, "waitingForWorkspace");
                    let guard = store
                        .acquire_workspace_execution(&lease.selection.job_id)
                        .await?;
                    emit(&lease, "workspaceEntered");
                    if lease.attempt.lease_epoch == 1 {
                        cancellation.cancelled().await;
                        emit(&lease, "cancellationObserved");
                        let gate = drain_gate.as_ref().ok_or_else(|| {
                            factory_coordinator::CoordinatorError::InvalidInput(
                                "workspace-overlap fixture requires --drain-gate".to_string(),
                            )
                        })?;
                        while !gate.exists() {
                            tokio::time::sleep(Duration::from_millis(25)).await;
                        }
                        emit(&lease, "runtimeDrained");
                        guard.release().await?;
                        OperationOutcome::Complete {
                            checkpoint: None,
                            completion_event: None,
                        }
                    } else {
                        let outcome = complete(&lease, "workspace-fenced");
                        guard.release().await?;
                        outcome
                    }
                }
                kind => OperationOutcome::Fail {
                    checkpoint: None,
                    failure: AttemptFailure::Terminal {
                        detail: json!({ "reason": "unsupported acceptance kind", "kind": kind }),
                    },
                },
            };
            Ok(outcome)
        })
    }
}

fn checkpoint(lease: &RecoveryLease, scenario: &str) -> OperationCheckpoint {
    OperationCheckpoint {
        kind: "factory.stage".to_string(),
        payload: json!({
            "operation": lease.selection.operation_kind,
            "phase": "completed",
            "scenario": scenario,
            "attemptNumber": lease.attempt.attempt_number,
            "leaseEpoch": lease.attempt.lease_epoch,
        }),
        workspace_root: None,
        workspace_revision: None,
        correlation_id: None,
    }
}

fn complete(lease: &RecoveryLease, scenario: &str) -> OperationOutcome {
    emit(lease, "completed");
    OperationOutcome::Complete {
        checkpoint: Some(checkpoint(lease, scenario)),
        completion_event: Some(NewAttemptEvent {
            kind: "stage.completed".to_string(),
            payload: json!({
                "stage": lease.selection.operation_kind,
                "scenario": scenario,
            }),
            deduplication_key: None,
        }),
    }
}

fn emit(lease: &RecoveryLease, event: &str) {
    println!(
        "{}",
        json!({
            "event": event,
            "jobId": lease.selection.job_id,
            "attemptId": lease.attempt.attempt_id,
            "attemptNumber": lease.attempt.attempt_number,
            "leaseEpoch": lease.attempt.lease_epoch,
            "recoveryCause": lease.selection.cause,
        })
    );
    let _ = std::io::stdout().flush();
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let store = CoordinatorStore::connect(&cli.database_url)
        .await
        .context("connect runner fixture to PostgreSQL")?;
    store
        .migrate()
        .await
        .context("apply coordinator migrations")?;

    let runner = DurableRunner::new(
        store.clone(),
        FixtureExecutor {
            store,
            drain_gate: cli.drain_gate,
        },
        RunnerConfig {
            worker_id: CoordinatorInstanceId::new(cli.worker_id.clone()),
            lease_duration: Duration::from_secs(cli.lease_seconds),
            poll_interval: Duration::from_millis(cli.poll_milliseconds),
            shutdown_grace: Duration::from_secs(2),
            slots: cli.slots,
            execution_profile: None,
        },
    )?;
    println!("{}", json!({ "event": "ready", "workerId": cli.worker_id }));
    std::io::stdout().flush().context("flush ready event")?;

    let shutdown = CancellationHandle::default();
    let run = runner.run(shutdown.clone());
    tokio::pin!(run);
    tokio::select! {
        result = &mut run => result.context("run durable fixture")?,
        signal = tokio::signal::ctrl_c() => {
            signal.context("wait for runner shutdown signal")?;
            shutdown.cancel();
            run.await.context("stop durable fixture")?;
        }
    }
    Ok(())
}
