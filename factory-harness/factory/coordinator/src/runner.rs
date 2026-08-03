use crate::AttemptFailure;
use crate::AttemptFence;
use crate::AttemptId;
use crate::AttemptSettlement;
use crate::CheckpointRecord;
use crate::ClaimRequest;
use crate::CoordinatorError;
use crate::CoordinatorInstanceId;
use crate::CoordinatorStore;
use crate::JobRecord;
use crate::JobState;
use crate::NewAttemptEvent;
use crate::NewCheckpoint;
use crate::OperationRecord;
use crate::RecoveryLease;
use crate::Result;
use crate::WorkspaceRecord;
use chrono::DateTime;
use chrono::Utc;
use serde_json::Value;
use serde_json::json;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::time::Duration;
use tokio::sync::Notify;
use tokio::task::JoinError;
use tokio::task::JoinHandle;
use tokio::task::JoinSet;

const LEASE_SLACK: Duration = Duration::from_secs(1);
const JOB_CONTROL_POLL_INTERVAL: Duration = Duration::from_secs(1);

/// Configuration for one durable-operation worker.
#[derive(Debug, Clone)]
pub struct RunnerConfig {
    pub worker_id: CoordinatorInstanceId,
    pub lease_duration: Duration,
    pub poll_interval: Duration,
    pub shutdown_grace: Duration,
    pub slots: usize,
    /// Exact Factory task profile this worker can execute. Fixture runners
    /// leave this unset and continue to claim only non-Factory job kinds.
    pub execution_profile: Option<crate::ExecutionProfile>,
}

/// Cooperative cancellation signal passed to an operation executor.
#[derive(Clone, Debug, Default)]
pub struct CancellationHandle {
    inner: Arc<CancellationState>,
}

#[derive(Debug, Default)]
struct CancellationState {
    cancelled: AtomicBool,
    notified: Notify,
}

impl CancellationHandle {
    pub fn cancel(&self) {
        self.inner.cancelled.store(true, Ordering::Release);
        self.inner.notified.notify_waiters();
    }

    pub fn is_cancelled(&self) -> bool {
        self.inner.cancelled.load(Ordering::Acquire)
    }

    pub async fn cancelled(&self) {
        loop {
            let notified = self.inner.notified.notified();
            if self.is_cancelled() {
                return;
            }
            notified.await;
        }
    }
}

/// A durable checkpoint emitted by an operation.
#[derive(Debug, Clone)]
pub struct OperationCheckpoint {
    pub kind: String,
    pub payload: Value,
    pub workspace_root: Option<String>,
    pub workspace_revision: Option<String>,
    pub correlation_id: Option<crate::CorrelationRecordId>,
}

/// A fenced progress writer. It cannot write after ownership is transferred.
#[derive(Clone)]
pub struct CheckpointWriter {
    store: CoordinatorStore,
    fence: AttemptFence,
}

impl CheckpointWriter {
    pub async fn write(&self, checkpoint: OperationCheckpoint) -> Result<CheckpointRecord> {
        self.store
            .save_checkpoint(&self.fence, checkpoint.into_new(&self.fence))
            .await
    }
}

/// All durable state needed to execute one claimed operation.
#[derive(Clone)]
pub struct OperationExecutionContext {
    lease: RecoveryLease,
    job: JobRecord,
    operation: OperationRecord,
    workspace: Option<WorkspaceRecord>,
    checkpoints: CheckpointWriter,
}

impl OperationExecutionContext {
    pub fn lease(&self) -> &RecoveryLease {
        &self.lease
    }

    pub fn job(&self) -> &JobRecord {
        &self.job
    }

    pub fn operation(&self) -> &OperationRecord {
        &self.operation
    }

    pub fn workspace(&self) -> Option<&WorkspaceRecord> {
        self.workspace.as_ref()
    }

    pub fn checkpoints(&self) -> &CheckpointWriter {
        &self.checkpoints
    }
}

/// The final durable disposition of an operation execution.
#[derive(Debug, Clone)]
pub enum OperationOutcome {
    Complete {
        checkpoint: Option<OperationCheckpoint>,
        completion_event: Option<NewAttemptEvent>,
    },
    Fail {
        checkpoint: Option<OperationCheckpoint>,
        failure: AttemptFailure,
    },
}

pub type OperationExecutionResult = Result<OperationOutcome>;

/// Executes one claimed operation. Cancellation is cooperative and the runner
/// always waits for the executor to finish its runtime and workspace cleanup.
pub trait OperationExecutor: Send + Sync + 'static {
    fn execute(
        &self,
        context: OperationExecutionContext,
        cancellation: CancellationHandle,
    ) -> Pin<Box<dyn Future<Output = OperationExecutionResult> + Send + '_>>;

    /// Restores executor-owned disposable state after a cancellation request
    /// or graceful worker drain. Implementations must be idempotent because a
    /// live execution normally cleans itself before the runner invokes this
    /// final recovery pass. The default is sufficient for executors without
    /// external runtime or workspace state.
    fn cleanup_cancelled(
        &self,
        _context: OperationExecutionContext,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + Send + '_>> {
        Box::pin(async { Ok(()) })
    }

    /// Publishes executor-owned material derived from a successfully settled
    /// operation. The runner invokes this only after the database transaction
    /// has committed; hook failures are warning-only and never roll back or
    /// retry the already-settled operation.
    fn after_successful_settlement(
        &self,
        _context: OperationExecutionContext,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + Send + '_>> {
        Box::pin(async { Ok(()) })
    }
}

/// Claims durable work, maintains its leases, and commits executor outcomes.
pub struct DurableRunner<E> {
    store: CoordinatorStore,
    executor: Arc<E>,
    config: RunnerConfig,
}

impl<E> DurableRunner<E>
where
    E: OperationExecutor,
{
    pub fn new(store: CoordinatorStore, executor: E, config: RunnerConfig) -> Result<Self> {
        validate_config(&config)?;
        Ok(Self {
            store,
            executor: Arc::new(executor),
            config,
        })
    }

    /// Runs until `shutdown` is cancelled. Attempt-local failures are fenced
    /// and left retryable; they never terminate the worker process.
    pub async fn run(&self, shutdown: CancellationHandle) -> Result<()> {
        let mut attempts = JoinSet::new();

        'claiming: loop {
            if shutdown.is_cancelled() {
                break;
            }

            while attempts.len() < self.config.slots {
                if shutdown.is_cancelled() {
                    break 'claiming;
                }
                let claim = ClaimRequest {
                    owner_instance_id: self.config.worker_id.clone(),
                    lease_seconds: lease_seconds(self.config.lease_duration),
                    execution_profile: self.config.execution_profile.clone(),
                };
                let lease = tokio::select! {
                    _ = shutdown.cancelled() => break 'claiming,
                    claimed = self.store.claim_next_recovery(&claim) => match claimed {
                        Ok(Some(lease)) => lease,
                        Ok(None) => break,
                        Err(error) => {
                            eprintln!("factory runner claim failed: {error}");
                            break;
                        }
                    },
                };
                let attempt_id = lease.attempt.attempt_id.clone();
                let store = self.store.clone();
                let executor = Arc::clone(&self.executor);
                let config = self.config.clone();
                let attempt_shutdown = shutdown.clone();
                attempts.spawn(async move {
                    let result =
                        run_attempt(store, executor, config, lease, attempt_shutdown).await;
                    (attempt_id, result)
                });
            }

            tokio::select! {
                _ = shutdown.cancelled() => break,
                joined = attempts.join_next(), if !attempts.is_empty() => {
                    if let Some(joined) = joined {
                        report_attempt(joined);
                    }
                }
                _ = tokio::time::sleep(self.config.poll_interval) => {}
            }
        }

        drain_attempts(&mut attempts, self.config.shutdown_grace).await;
        Ok(())
    }
}

async fn run_attempt<E>(
    store: CoordinatorStore,
    executor: Arc<E>,
    config: RunnerConfig,
    mut lease: RecoveryLease,
    shutdown: CancellationHandle,
) -> Result<()>
where
    E: OperationExecutor,
{
    let fence = lease.fence.clone();
    let job_id = lease.selection.job_id.clone();
    let renewed = store
        .renew_attempt(&fence, lease_seconds(config.lease_duration))
        .await?;
    lease.attempt = renewed;
    let durable_job = store.load_job(&lease.selection.job_id).await?;
    if terminal_job(durable_job.job.state) {
        return Ok(());
    }
    let operation = durable_job
        .operations
        .iter()
        .find(|operation| operation.operation_id == lease.selection.operation_id)
        .cloned()
        .ok_or_else(|| {
            CoordinatorError::InvalidInput(format!(
                "claimed operation {} is missing from job {}",
                lease.selection.operation_id, lease.selection.job_id
            ))
        })?;
    let workspace = store.load_workspace(&lease.selection.job_id).await?;
    let renewed = store
        .renew_attempt(&fence, lease_seconds(config.lease_duration))
        .await?;
    let mut lease_expires_at = renewed.lease_expires_at;
    lease.attempt = renewed;
    let checkpoints = CheckpointWriter {
        store: store.clone(),
        fence: fence.clone(),
    };
    let cancelling_at_start = durable_job.job.state == JobState::Cancelling;
    let context = OperationExecutionContext {
        lease,
        job: durable_job.job,
        operation,
        workspace,
        checkpoints,
    };
    if cancelling_at_start {
        return cleanup_and_acknowledge_cancellation(&store, executor.as_ref(), &context).await;
    }
    let cleanup_context = context.clone();
    let cancellation = CancellationHandle::default();
    let executor_cancellation = cancellation.clone();
    let execution_executor = Arc::clone(&executor);
    let mut execution = IsolatedExecution::spawn(async move {
        execution_executor
            .execute(context, executor_cancellation)
            .await
    });
    let mut heartbeat = tokio::time::interval(config.lease_duration / 3);
    heartbeat.tick().await;
    let mut job_control = tokio::time::interval(JOB_CONTROL_POLL_INTERVAL);
    job_control.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    job_control.tick().await;

    let execution_result = loop {
        tokio::select! {
            joined = &mut execution.handle => break joined,
            _ = shutdown.cancelled() => {
                execution.cancel_and_drain(&cancellation).await;
                return cleanup_and_relinquish(
                    &store,
                    executor.as_ref(),
                    &cleanup_context,
                )
                .await;
            }
            _ = heartbeat.tick() => {
                let renewed = match lease_bounded(
                    lease_expires_at,
                    store.renew_attempt(&fence, lease_seconds(config.lease_duration)),
                )
                .await
                {
                    Ok(Some(renewed)) => renewed,
                    Ok(None) => {
                        execution.cancel_and_drain(&cancellation).await;
                        return Err(CoordinatorError::AttemptLeaseUnavailable(
                            fence.attempt_id.clone(),
                        ));
                    }
                    Err(error @ CoordinatorError::AttemptLeaseUnavailable(_)) => {
                        execution.cancel_and_drain(&cancellation).await;
                        return Err(error);
                    }
                    Err(error) => {
                        execution.cancel_and_drain(&cancellation).await;
                        return Err(error);
                    }
                };
                lease_expires_at = renewed.lease_expires_at;
            }
            _ = job_control.tick() => {
                let job = match control_bounded(lease_expires_at, store.load_job(&job_id)).await {
                    Ok(Some(job)) => job,
                    // Job-state reads are advisory control traffic. A slow or
                    // transiently failed read does not prove that this runner
                    // lost its fence; the independent lease heartbeat is the
                    // ownership authority.
                    Ok(None) => continue,
                    Err(error) => {
                        eprintln!(
                            "factory runner job control poll failed for {job_id}: {error}; retrying while the lease heartbeat remains live"
                        );
                        continue;
                    }
                };
                if job.job.state == JobState::Cancelling {
                    execution.cancel_and_drain(&cancellation).await;
                    return cleanup_and_acknowledge_cancellation(
                        &store,
                        executor.as_ref(),
                        &cleanup_context,
                    )
                    .await;
                }
                if terminal_job(job.job.state) {
                    execution.cancel_and_drain(&cancellation).await;
                    return Ok(());
                }
            }
        }
    };

    let outcome = match execution_result {
        Ok(Ok(outcome)) => outcome,
        Ok(Err(error)) if lease_or_database_error(&error) => {
            cancellation.cancel();
            return Err(error);
        }
        Ok(Err(error)) => OperationOutcome::Fail {
            checkpoint: None,
            failure: AttemptFailure::RetryAt {
                retry_at: Utc::now(),
                detail: json!({
                    "cause": "executorFailed",
                    "message": error.to_string(),
                }),
            },
        },
        Err(error) => OperationOutcome::Fail {
            checkpoint: None,
            failure: AttemptFailure::RetryAt {
                retry_at: Utc::now(),
                detail: json!({
                    "cause": if error.is_panic() { "executorPanicked" } else { "executorTaskLost" },
                    "message": error.to_string(),
                }),
            },
        },
    };
    match settle_outcome(&store, &fence, outcome).await {
        Ok(settled_succeeded) => {
            if settled_succeeded
                && let Err(error) = executor.after_successful_settlement(cleanup_context).await
            {
                eprintln!("factory runner post-settlement publication warning: {error}");
            }
            Ok(())
        }
        Err(CoordinatorError::JobCancellationRequested(_)) => {
            cleanup_and_acknowledge_cancellation(&store, executor.as_ref(), &cleanup_context).await
        }
        Err(error) => Err(error),
    }
}

async fn cleanup_and_acknowledge_cancellation<E: OperationExecutor>(
    store: &CoordinatorStore,
    executor: &E,
    context: &OperationExecutionContext,
) -> Result<()> {
    if let Err(error) = executor.cleanup_cancelled(context.clone()).await {
        let _ = store.relinquish_attempt(&context.lease().fence).await;
        return Err(error);
    }
    store
        .acknowledge_job_cancellation(&context.lease().fence)
        .await?;
    Ok(())
}

async fn cleanup_and_relinquish<E: OperationExecutor>(
    store: &CoordinatorStore,
    executor: &E,
    context: &OperationExecutionContext,
) -> Result<()> {
    let cleanup = executor.cleanup_cancelled(context.clone()).await;
    let relinquish = store.relinquish_attempt(&context.lease().fence).await;
    match (cleanup, relinquish) {
        (Err(error), _) | (Ok(()), Err(error)) => Err(error),
        (Ok(()), Ok(())) => Ok(()),
    }
}

async fn settle_outcome(
    store: &CoordinatorStore,
    fence: &AttemptFence,
    outcome: OperationOutcome,
) -> Result<bool> {
    let (settlement, checkpoint, completion_event) = match outcome {
        OperationOutcome::Complete {
            checkpoint,
            completion_event,
        } => (AttemptSettlement::Succeeded, checkpoint, completion_event),
        OperationOutcome::Fail {
            checkpoint,
            failure,
        } => (AttemptSettlement::Failed(failure), checkpoint, None),
    };
    let fallback_failure = match &settlement {
        AttemptSettlement::Failed(failure) => failure.clone(),
        AttemptSettlement::Succeeded => AttemptFailure::RetryAt {
            retry_at: Utc::now(),
            detail: json!({ "cause": "invalidFinalCheckpoint" }),
        },
    };
    let settlement_succeeded = matches!(&settlement, AttemptSettlement::Succeeded);
    let checkpoint = checkpoint.map(|checkpoint| checkpoint.into_new(fence));
    let has_checkpoint = checkpoint.is_some();
    if checkpoint
        .as_ref()
        .is_some_and(|checkpoint| checkpoint.kind.trim().is_empty())
    {
        store
            .settle_attempt(fence, AttemptSettlement::Failed(fallback_failure), None)
            .await?;
        return Ok(false);
    }

    match store
        .settle_attempt_with_event(fence, settlement, checkpoint, completion_event)
        .await
    {
        Ok(_) => Ok(settlement_succeeded),
        Err(error) if has_checkpoint && invalid_checkpoint(&error) => {
            store
                .settle_attempt(fence, AttemptSettlement::Failed(fallback_failure), None)
                .await?;
            Ok(false)
        }
        Err(error) => Err(error),
    }
}

fn invalid_checkpoint(error: &CoordinatorError) -> bool {
    matches!(error, CoordinatorError::CheckpointCorrelationMismatch)
}

fn lease_or_database_error(error: &CoordinatorError) -> bool {
    matches!(
        error,
        CoordinatorError::Database(_)
            | CoordinatorError::AttemptLeaseUnavailable(_)
            | CoordinatorError::AttemptNotRunning(_)
    )
}

impl OperationCheckpoint {
    fn into_new(self, fence: &AttemptFence) -> NewCheckpoint {
        NewCheckpoint {
            attempt_id: fence.attempt_id.clone(),
            kind: self.kind,
            payload: self.payload,
            workspace_root: self.workspace_root,
            workspace_revision: self.workspace_revision,
            correlation_id: self.correlation_id,
        }
    }
}

struct IsolatedExecution<T> {
    handle: JoinHandle<T>,
}

impl<T> IsolatedExecution<T>
where
    T: Send + 'static,
{
    fn spawn(future: impl Future<Output = T> + Send + 'static) -> Self {
        Self {
            handle: tokio::spawn(future),
        }
    }

    async fn cancel_and_drain(&mut self, cancellation: &CancellationHandle) {
        cancellation.cancel();
        let _ = (&mut self.handle).await;
    }
}

type AttemptTaskResult = (AttemptId, Result<()>);

fn report_attempt(joined: std::result::Result<AttemptTaskResult, JoinError>) {
    match joined {
        Ok((attempt_id, Err(error))) => {
            eprintln!("factory runner attempt {attempt_id} failed: {error}");
        }
        Err(error) => {
            eprintln!("factory runner attempt task failed: {error}");
        }
        Ok((_, Ok(()))) => {}
    }
}

async fn drain_attempts(attempts: &mut JoinSet<AttemptTaskResult>, grace: Duration) {
    let drained = tokio::time::timeout(grace, async {
        while let Some(joined) = attempts.join_next().await {
            report_attempt(joined);
        }
    })
    .await;
    if drained.is_err() {
        eprintln!(
            "factory runner shutdown grace elapsed; waiting for active runtimes to finish cleanup"
        );
        while let Some(joined) = attempts.join_next().await {
            report_attempt(joined);
        }
    }
}

fn terminal_job(state: JobState) -> bool {
    matches!(
        state,
        JobState::Succeeded | JobState::Failed | JobState::Cancelled
    )
}

async fn control_bounded<T>(
    lease_expires_at: DateTime<Utc>,
    future: impl Future<Output = Result<T>>,
) -> Result<Option<T>> {
    let remaining = lease_deadline_remaining(lease_expires_at).min(JOB_CONTROL_POLL_INTERVAL);
    if remaining.is_zero() {
        return Ok(None);
    }
    match tokio::time::timeout(remaining, future).await {
        Ok(result) => result.map(Some),
        Err(_) => Ok(None),
    }
}

async fn lease_bounded<T>(
    lease_expires_at: DateTime<Utc>,
    future: impl Future<Output = Result<T>>,
) -> Result<Option<T>> {
    let remaining = lease_deadline_remaining(lease_expires_at);
    if remaining.is_zero() {
        return Ok(None);
    }
    match tokio::time::timeout(remaining, future).await {
        Ok(result) => result.map(Some),
        Err(_) => Ok(None),
    }
}

fn lease_deadline_remaining(lease_expires_at: DateTime<Utc>) -> Duration {
    (lease_expires_at - Utc::now())
        .to_std()
        .unwrap_or(Duration::ZERO)
        .saturating_sub(LEASE_SLACK)
}

fn validate_config(config: &RunnerConfig) -> Result<()> {
    if config.slots == 0 {
        return Err(CoordinatorError::InvalidInput(
            "runner slots must be at least one".to_string(),
        ));
    }
    if config.lease_duration.as_secs() < 3
        || config.lease_duration.subsec_nanos() != 0
        || config.lease_duration.as_secs() > u64::from(u32::MAX)
    {
        return Err(CoordinatorError::InvalidInput(
            "runner lease duration must be 3..=u32::MAX whole seconds".to_string(),
        ));
    }
    if config.poll_interval.is_zero() {
        return Err(CoordinatorError::InvalidInput(
            "runner poll interval must be non-zero".to_string(),
        ));
    }
    if config.shutdown_grace.is_zero() {
        return Err(CoordinatorError::InvalidInput(
            "runner shutdown grace must be non-zero".to_string(),
        ));
    }
    if config.shutdown_grace > config.lease_duration.saturating_sub(LEASE_SLACK) {
        return Err(CoordinatorError::InvalidInput(
            "runner shutdown grace must leave at least one second of lease slack".to_string(),
        ));
    }
    if let Some(profile) = &config.execution_profile
        && (profile.provider.trim().is_empty() || profile.model.trim().is_empty())
    {
        return Err(CoordinatorError::InvalidInput(
            "runner execution profile requires a provider and model".to_string(),
        ));
    }
    Ok(())
}

fn lease_seconds(duration: Duration) -> u32 {
    u32::try_from(duration.as_secs()).expect("runner config validates lease duration")
}
