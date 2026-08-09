use std::future::Future;
use std::pin::Pin;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;
use std::time::SystemTime;

use codex_app_server_client::InProcessClientStartArgs;
use factory_coordinator::ArtifactManager;
use factory_coordinator::AttemptFailure;
use factory_coordinator::CancellationHandle;
use factory_coordinator::CoordinatorError;
use factory_coordinator::CoordinatorStore;
use factory_coordinator::ExecutionEnvironmentRecord;
use factory_coordinator::ExecutionProfile;
use factory_coordinator::OperationExecutionContext;
use factory_coordinator::OperationExecutionResult;
use factory_coordinator::OperationExecutor;
use factory_coordinator::OperationOutcome;
use factory_coordinator::WorkspaceManager;
use factory_coordinator::WorkspaceRecord;
use factory_coordinator::WorkspaceSnapshot;
use factory_coordinator::WorkspaceState;
use factory_extension::FactoryState;
use factory_extension::FactoryStateBackend;
use factory_extension::FactoryStateFence;
use factory_extension::FactorydStateBackend;
use serde_json::json;

use crate::execution_environment::ExecutionEnvironmentProvisionRequest;
use crate::execution_environment::ExecutionEnvironmentProvisioner;
use crate::execution_environment::ExecutionEnvironmentReleaseRequest;
use crate::execution_environment::ProvisionedExecutionEnvironment;
use crate::session::AutonomousSession;
use crate::session::ParentThread;
use crate::stages::OperationKind;

mod artifacts;
mod resume;
mod stage_loop;
mod task;

use resume::ResumePoint;
use task::TaskInput;
use task::parse_task_input;
use task::require_text;

const DEFAULT_MAX_REVIEW_CYCLES: u32 = 5;

struct SingleStageRun<'a> {
    context: &'a OperationExecutionContext,
    cancellation: &'a CancellationHandle,
    input: &'a TaskInput,
    operation: OperationKind,
    resume: &'a ResumePoint,
    backend: &'a dyn FactoryStateBackend,
}

/// Runs the four durable Factory stages through the native Codex app-server.
///
/// The executor owns no agent loop. It resumes one Codex parent thread, starts
/// normal or native review turns, and records only the durable facts needed to
/// recover those turns after process loss.
pub struct CodexOperationExecutor {
    store: CoordinatorStore,
    start_args: InProcessClientStartArgs,
    factoryd_base_url: String,
    max_review_cycles: u32,
    workspaces: WorkspaceManager,
    artifacts: ArtifactManager,
    execution_profile: ExecutionProfile,
    execution_environment_provisioner: Arc<dyn ExecutionEnvironmentProvisioner>,
}

struct SelectedExecutionEnvironment {
    environment_id: String,
    provisioned: ProvisionedExecutionEnvironment,
}

impl CodexOperationExecutor {
    pub fn new(
        store: CoordinatorStore,
        start_args: InProcessClientStartArgs,
        factoryd_base_url: impl Into<String>,
        execution_profile: ExecutionProfile,
        execution_environment_provisioner: Arc<dyn ExecutionEnvironmentProvisioner>,
    ) -> Result<Self, String> {
        Self::with_max_review_cycles(
            store,
            start_args,
            factoryd_base_url,
            execution_profile,
            execution_environment_provisioner,
            DEFAULT_MAX_REVIEW_CYCLES,
        )
    }

    pub fn with_max_review_cycles(
        store: CoordinatorStore,
        start_args: InProcessClientStartArgs,
        factoryd_base_url: impl Into<String>,
        execution_profile: ExecutionProfile,
        execution_environment_provisioner: Arc<dyn ExecutionEnvironmentProvisioner>,
        max_review_cycles: u32,
    ) -> Result<Self, String> {
        let factoryd_base_url = factoryd_base_url.into();
        require_text("factoryd base URL", &factoryd_base_url)?;
        if max_review_cycles == 0 {
            return Err("maximum review cycles must be positive".to_string());
        }
        let workspaces = WorkspaceManager::from_env().map_err(|error| error.to_string())?;
        let artifacts = ArtifactManager::from_env().map_err(|error| error.to_string())?;
        Ok(Self {
            store,
            start_args,
            factoryd_base_url,
            max_review_cycles,
            workspaces,
            artifacts,
            execution_profile,
            execution_environment_provisioner,
        })
    }

    async fn execute_operation(
        &self,
        context: &OperationExecutionContext,
        cancellation: &CancellationHandle,
    ) -> ExecutionResult<stage_loop::CompletedOperation> {
        if cancellation.is_cancelled() {
            return Err(ExecutionFailure::Cancelled);
        }

        let operation = OperationKind::from_str(&context.operation().kind)
            .map_err(|error| ExecutionFailure::terminal(error.to_string()))?;
        self.validate_job_shape(context, operation).await?;
        let input = parse_task_input(&context.job().input)?;
        let workspace = context
            .workspace()
            .ok_or_else(|| ExecutionFailure::terminal("durable job has no managed worktree"))?;
        if workspace.state != WorkspaceState::Active {
            return Err(ExecutionFailure::terminal(
                "durable job worktree is not active",
            ));
        }
        require_text("workspace root", &workspace.root).map_err(ExecutionFailure::terminal)?;
        require_text("workspace revision", &workspace.revision)
            .map_err(ExecutionFailure::terminal)?;
        let workspace_guard = tokio::select! {
            _ = cancellation.cancelled() => return Err(ExecutionFailure::Cancelled),
            guard = self.store.acquire_workspace_execution(&workspace.job_id) => {
                guard.map_err(ExecutionFailure::Coordinator)?
            }
        };
        let result = async {
            if operation == OperationKind::Plan {
                self.prepare_plan_workspace(workspace).await?;
            }
            let execution_environment = self
                .ensure_execution_environment(context, workspace)
                .await?;
            let start_args = self.start_args_for_workspace(
                workspace.root.as_str(),
                &input,
                &execution_environment,
            )?;
            self.execute_with_workspace_guard(
                context,
                cancellation,
                operation,
                input,
                workspace,
                start_args,
            )
            .await
        }
        .await;
        let release = workspace_guard.release().await;
        match (result, release) {
            (Ok(completed), Ok(())) => Ok(completed),
            (Err(error), _) => Err(error),
            (Ok(_), Err(error)) => Err(ExecutionFailure::retryable(format!(
                "release managed workspace execution lock: {error}"
            ))),
        }
    }

    async fn execute_with_workspace_guard(
        &self,
        context: &OperationExecutionContext,
        cancellation: &CancellationHandle,
        operation: OperationKind,
        input: TaskInput,
        workspace: &WorkspaceRecord,
        start_args: InProcessClientStartArgs,
    ) -> ExecutionResult<stage_loop::CompletedOperation> {
        let mut resume =
            ResumePoint::decode(context, operation, &workspace.root, &workspace.revision)?;
        let parent = match &resume {
            ResumePoint::Fresh => ParentThread::New,
            ResumePoint::Previous { checkpoint, .. } | ResumePoint::Current { checkpoint, .. } => {
                ParentThread::Resume {
                    thread_id: checkpoint.parent_execution_thread_id.clone(),
                }
            }
        };

        let fence = context.lease().fence.clone();
        let state_fence = FactoryStateFence::new(
            fence.attempt_id.as_str(),
            fence.owner_instance_id.as_str(),
            fence.lease_epoch,
        )
        .map_err(|error| ExecutionFailure::terminal(error.to_string()))?;
        let backend: Arc<dyn FactoryStateBackend> = Arc::new(
            FactorydStateBackend::new(&self.factoryd_base_url, state_fence)
                .map_err(|error| ExecutionFailure::terminal(error.to_string()))?,
        );
        if let Some(input_repository_id) = input.repository_id.as_deref()
            && input_repository_id != workspace.repository_id
        {
            return Err(ExecutionFailure::terminal(format!(
                "job repository identity {:?} does not match its managed workspace identity {:?}",
                input_repository_id, workspace.repository_id
            )));
        }
        let repository_id =
            factory_extension::FactoryRepositoryId::new(workspace.repository_id.clone())
                .map_err(|error| ExecutionFailure::terminal(error.to_string()))?;
        let (mut session, thread) = match AutonomousSession::start(
            start_args,
            Arc::clone(&backend),
            repository_id,
            operation.turn_stage(),
            parent,
        )
        .await
        {
            Ok(started) => started,
            Err(error) => {
                return Err(ExecutionFailure::retryable(format!(
                    "start native Codex runtime: {error}"
                )));
            }
        };

        let result = async {
            self.append_thread_correlation(context, &thread).await?;
            if let Some(correlation_id) = self
                .append_resumed_turn_correlation(context, &resume)
                .await?
            {
                resume.set_current_correlation(correlation_id);
            }
            if matches!(operation, OperationKind::Review | OperationKind::Remediate)
                && self
                    .workspaces
                    .recover_review_snapshot(workspace)
                    .await
                    .map_err(|error| {
                        ExecutionFailure::retryable(format!(
                            "recover detached-review workspace snapshot: {error}"
                        ))
                    })?
            {
                self.rollback_detached_review(backend.as_ref(), session.parent_thread_id())
                    .await?;
                self.acknowledge_detached_review_mutation(workspace).await?;
            }
            match operation {
                OperationKind::Plan
                | OperationKind::Execute
                | OperationKind::Review
                | OperationKind::Iterate => {
                    self.run_single_stage(
                        SingleStageRun {
                            context,
                            cancellation,
                            input: &input,
                            operation,
                            resume: &resume,
                            backend: backend.as_ref(),
                        },
                        &mut session,
                    )
                    .await
                }
                OperationKind::Remediate => {
                    self.run_remediation_loop(
                        context,
                        cancellation,
                        &input,
                        &resume,
                        backend.as_ref(),
                        &mut session,
                    )
                    .await
                }
            }
        }
        .await;

        let parent_thread_id = session.parent_thread_id().to_string();
        let shutdown = session.shutdown().await;
        let cancelled = cancellation.is_cancelled();
        let mut final_result = match (result, shutdown) {
            (Ok(completed), Ok(())) if !cancelled => Ok(completed),
            (_, _) if cancelled => Err(ExecutionFailure::Cancelled),
            (Ok(_), Ok(())) => Err(ExecutionFailure::Cancelled),
            (Err(ExecutionFailure::PlanValidation(message)), Ok(())) => {
                Err(ExecutionFailure::retryable(message))
            }
            (Err(error), Ok(())) => Err(error),
            (Err(error), Err(_)) => Err(error),
            (Ok(_), Err(error)) => Err(ExecutionFailure::retryable(format!(
                "Codex shutdown failed after stage completion: {error}"
            ))),
        };

        if operation == OperationKind::Plan && final_result.is_err() {
            let cleanup = self
                .restore_plan_baseline(backend.as_ref(), &parent_thread_id, workspace)
                .await;
            if let Err(cleanup) = cleanup
                && !matches!(&final_result, Err(ExecutionFailure::Cancelled))
            {
                final_result = Err(ExecutionFailure::retryable(format!(
                    "Plan failed and rollback failed: {cleanup}"
                )));
            }
        } else if matches!(operation, OperationKind::Review | OperationKind::Remediate)
            && final_result.is_err()
        {
            let cleanup = self
                .cleanup_detached_review(backend.as_ref(), &parent_thread_id, workspace)
                .await;
            if !matches!(&final_result, Err(ExecutionFailure::Cancelled))
                && let Err(error) = cleanup
            {
                final_result = Err(error);
            }
        }
        final_result
    }

    async fn ensure_execution_environment(
        &self,
        context: &OperationExecutionContext,
        workspace: &WorkspaceRecord,
    ) -> ExecutionResult<SelectedExecutionEnvironment> {
        let provisioner = &self.execution_environment_provisioner;
        let fence = &context.lease().fence;
        let repository_metadata_root = self
            .workspaces
            .repository_metadata_root(&workspace.repository_id)
            .map_err(ExecutionFailure::Coordinator)?;
        let mut environment = self
            .store
            .ensure_execution_environment(fence, provisioner.backend())
            .await
            .map_err(ExecutionFailure::Coordinator)?;
        if let Some(locator) = provisioner.durable_locator(&environment).map_err(|error| {
            ExecutionFailure::terminal(format!(
                "derive {} execution environment locator: {error:#}",
                provisioner.backend()
            ))
        })? {
            environment = self
                .store
                .reserve_execution_environment_locator(fence, environment.generation, &locator)
                .await
                .map_err(ExecutionFailure::Coordinator)?;
        }
        let provisioned = match provisioner
            .ensure(ExecutionEnvironmentProvisionRequest {
                environment: environment.clone(),
                workspace_root: workspace.root.clone(),
                repository_metadata_root,
            })
            .await
        {
            Ok(provisioned) => provisioned,
            Err(error) => {
                let message = format!(
                    "provision {} execution environment {} generation {}: {error:#}",
                    provisioner.backend(),
                    environment.environment_id,
                    environment.generation
                );
                self.store
                    .mark_execution_environment_failed(fence, environment.generation, &message)
                    .await
                    .map_err(ExecutionFailure::Coordinator)?;
                return Err(ExecutionFailure::retryable(message));
            }
        };
        if let Err(message) = require_text(
            "execution environment backend reference",
            &provisioned.backend_ref,
        )
        .and_then(|()| require_text("execution environment URL", &provisioned.url))
        {
            self.store
                .mark_execution_environment_failed(fence, environment.generation, &message)
                .await
                .map_err(ExecutionFailure::Coordinator)?;
            return Err(ExecutionFailure::retryable(message));
        }
        let ready = self
            .store
            .mark_execution_environment_ready(
                fence,
                environment.generation,
                &provisioned.backend_ref,
                &provisioned.url,
            )
            .await
            .map_err(ExecutionFailure::Coordinator)?;
        Ok(SelectedExecutionEnvironment {
            environment_id: ready.environment_id.as_str().to_string(),
            provisioned,
        })
    }

    /// Repairs only the exceptional missing/corrupt Plan worktree before any
    /// Codex session starts. A backend that still mounts the old directory is
    /// removed first; ordinary provisioning then recreates the same durable
    /// environment identity and generation against the repaired root.
    async fn prepare_plan_workspace(&self, workspace: &WorkspaceRecord) -> ExecutionResult<()> {
        match self.workspaces.restore(workspace).await {
            Ok(()) => Ok(()),
            Err(CoordinatorError::WorkspaceRebindRequired { .. }) => {
                if let Some(environment) = self
                    .store
                    .load_execution_environment(&workspace.job_id)
                    .await
                    .map_err(ExecutionFailure::Coordinator)?
                {
                    release_execution_environment_backend(
                        self.execution_environment_provisioner.as_ref(),
                        &environment,
                    )
                    .await
                    .map_err(ExecutionFailure::Coordinator)?;
                }
                self.workspaces
                    .recreate(workspace)
                    .await
                    .map_err(ExecutionFailure::Coordinator)
            }
            Err(error) => Err(ExecutionFailure::Coordinator(error)),
        }
    }

    async fn reconcile_releasing_execution_environments(&self) -> factory_coordinator::Result<()> {
        reconcile_releasing_environments(
            &self.store,
            self.execution_environment_provisioner.as_ref(),
        )
        .await
    }

    async fn release_cancelled_environment(
        &self,
        context: &OperationExecutionContext,
    ) -> factory_coordinator::Result<()> {
        let Some(environment) = self
            .store
            .request_cancelling_execution_environment_release(&context.lease().fence)
            .await?
        else {
            return Ok(());
        };
        release_execution_environment(
            &self.store,
            self.execution_environment_provisioner.as_ref(),
            environment,
        )
        .await
    }

    async fn cleanup_cancelled_operation(
        &self,
        context: &OperationExecutionContext,
    ) -> ExecutionResult<()> {
        let operation = OperationKind::from_str(&context.operation().kind)
            .map_err(|error| ExecutionFailure::terminal(error.to_string()))?;
        let workspace = context
            .workspace()
            .ok_or_else(|| ExecutionFailure::terminal("durable job has no managed worktree"))?;
        let workspace_guard = self
            .store
            .acquire_workspace_execution(&workspace.job_id)
            .await
            .map_err(ExecutionFailure::Coordinator)?;
        let cleanup = async {
            let resume =
                ResumePoint::decode(context, operation, &workspace.root, &workspace.revision)?;
            if let Some(checkpoint) = resume.correlation_checkpoint() {
                self.append_checkpoint_turn_correlation(context, checkpoint)
                    .await?;
            }
            let fence = &context.lease().fence;
            let state_fence = FactoryStateFence::new(
                fence.attempt_id.as_str(),
                fence.owner_instance_id.as_str(),
                fence.lease_epoch,
            )
            .map_err(|error| ExecutionFailure::terminal(error.to_string()))?;
            let backend = FactorydStateBackend::new(&self.factoryd_base_url, state_fence)
                .map_err(|error| ExecutionFailure::terminal(error.to_string()))?;
            match operation {
                OperationKind::Plan => {
                    if let Some(parent_thread_id) = resume.parent_thread_id() {
                        restore_factory_plan_state(&backend, parent_thread_id)
                            .await
                            .map_err(ExecutionFailure::retryable)?;
                    }
                    self.prepare_plan_workspace(workspace).await?;
                }
                OperationKind::Review | OperationKind::Remediate => {
                    if let Some(parent_thread_id) = resume.parent_thread_id() {
                        self.cleanup_detached_review(&backend, parent_thread_id, workspace)
                            .await?;
                    }
                }
                OperationKind::Execute | OperationKind::Iterate => {}
            }
            Ok(())
        }
        .await;
        let release = workspace_guard.release().await;
        match (cleanup, release) {
            (Err(error), _) => Err(error),
            (Ok(()), Err(error)) => Err(ExecutionFailure::Coordinator(error)),
            (Ok(()), Ok(())) => Ok(()),
        }
    }

    async fn restore_plan_baseline(
        &self,
        backend: &dyn FactoryStateBackend,
        parent_thread_id: &str,
        workspace: &WorkspaceRecord,
    ) -> Result<(), String> {
        restore_factory_plan_state(backend, parent_thread_id).await?;
        self.workspaces
            .restore(workspace)
            .await
            .map_err(|error| format!("restore managed plan worktree: {error}"))
    }

    async fn prepare_detached_review(
        &self,
        backend: &dyn FactoryStateBackend,
        parent_thread_id: &str,
        workspace: &WorkspaceRecord,
    ) -> ExecutionResult<WorkspaceSnapshot> {
        let mut state = backend
            .load(parent_thread_id)
            .await
            .map_err(|error| ExecutionFailure::retryable(error.to_string()))?
            .unwrap_or_default();
        if state.rollback_review_recovery() {
            backend
                .save(parent_thread_id, state.clone())
                .await
                .map_err(|error| ExecutionFailure::retryable(error.to_string()))?;
        }
        let snapshot = self
            .workspaces
            .capture_review_snapshot(workspace)
            .await
            .map_err(|error| ExecutionFailure::retryable(error.to_string()))?;
        state.prepare_review_recovery();
        if let Err(error) = backend.save(parent_thread_id, state).await {
            let _ = self
                .workspaces
                .restore_review_snapshot(workspace, snapshot.clone())
                .await;
            return Err(ExecutionFailure::retryable(error.to_string()));
        }
        Ok(snapshot)
    }

    async fn restore_detached_review_workspace(
        &self,
        workspace: &WorkspaceRecord,
        snapshot: WorkspaceSnapshot,
    ) -> ExecutionResult<bool> {
        self.workspaces
            .restore_review_snapshot(workspace, snapshot)
            .await
            .map_err(|error| ExecutionFailure::retryable(error.to_string()))
    }

    async fn commit_detached_review(
        &self,
        backend: &dyn FactoryStateBackend,
        parent_thread_id: &str,
    ) -> ExecutionResult<()> {
        update_review_recovery(backend, parent_thread_id, true)
            .await
            .map(|_| ())
    }

    async fn rollback_detached_review(
        &self,
        backend: &dyn FactoryStateBackend,
        parent_thread_id: &str,
    ) -> ExecutionResult<bool> {
        update_review_recovery(backend, parent_thread_id, false).await
    }

    async fn acknowledge_detached_review_mutation(
        &self,
        workspace: &WorkspaceRecord,
    ) -> ExecutionResult<()> {
        self.workspaces
            .acknowledge_review_mutation(workspace)
            .await
            .map_err(|error| ExecutionFailure::retryable(error.to_string()))
    }

    async fn cleanup_detached_review(
        &self,
        backend: &dyn FactoryStateBackend,
        parent_thread_id: &str,
        workspace: &WorkspaceRecord,
    ) -> ExecutionResult<()> {
        let recovered = self
            .workspaces
            .recover_review_snapshot(workspace)
            .await
            .map_err(|error| {
                ExecutionFailure::retryable(format!("restore detached-review workspace: {error}"))
            });
        let state = self
            .rollback_detached_review(backend, parent_thread_id)
            .await;
        let recovered = match (recovered, state) {
            (Ok(recovered), Ok(_)) => recovered,
            (Err(error), _) | (_, Err(error)) => return Err(error),
        };
        if recovered {
            self.acknowledge_detached_review_mutation(workspace).await?;
        }
        Ok(())
    }
}

async fn restore_factory_plan_state(
    backend: &dyn FactoryStateBackend,
    parent_thread_id: &str,
) -> Result<(), String> {
    backend
        .save(parent_thread_id, FactoryState::default())
        .await
        .map_err(|error| format!("restore Factory planning state: {error}"))
}

async fn update_review_recovery(
    backend: &dyn FactoryStateBackend,
    parent_thread_id: &str,
    commit: bool,
) -> ExecutionResult<bool> {
    let mut state = backend
        .load(parent_thread_id)
        .await
        .map_err(|error| ExecutionFailure::retryable(error.to_string()))?
        .unwrap_or_default();
    let changed = if commit {
        state.commit_review_recovery()
    } else {
        state.rollback_review_recovery()
    };
    if changed {
        backend
            .save(parent_thread_id, state)
            .await
            .map_err(|error| ExecutionFailure::retryable(error.to_string()))?;
    }
    Ok(changed)
}

impl OperationExecutor for CodexOperationExecutor {
    fn execute(
        &self,
        context: OperationExecutionContext,
        cancellation: CancellationHandle,
    ) -> Pin<Box<dyn Future<Output = OperationExecutionResult> + Send + '_>> {
        Box::pin(async move {
            match self.execute_operation(&context, &cancellation).await {
                Ok(completed) => Ok(OperationOutcome::Complete {
                    checkpoint: Some(completed.checkpoint),
                    completion_event: Some(completed.completion_event),
                }),
                Err(ExecutionFailure::Coordinator(error)) => Err(error),
                Err(ExecutionFailure::Cancelled) => Err(CoordinatorError::AttemptLeaseUnavailable(
                    context.lease().fence.attempt_id.clone(),
                )),
                Err(ExecutionFailure::Terminal(message)) => Ok(OperationOutcome::Fail {
                    checkpoint: None,
                    failure: AttemptFailure::Terminal {
                        detail: json!({
                            "cause": "stageExecutionFailed",
                            "message": message,
                        }),
                    },
                }),
                Err(ExecutionFailure::Retryable(message)) => Ok(OperationOutcome::Fail {
                    checkpoint: None,
                    failure: AttemptFailure::RetryAt {
                        retry_at: (SystemTime::now() + Duration::from_secs(5)).into(),
                        detail: json!({
                            "cause": "stageExecutionRetry",
                            "message": message,
                        }),
                    },
                }),
                Err(ExecutionFailure::PlanValidation(message)) => Ok(OperationOutcome::Fail {
                    checkpoint: None,
                    failure: AttemptFailure::RetryAt {
                        retry_at: (SystemTime::now() + Duration::from_secs(5)).into(),
                        detail: json!({
                            "cause": "stageExecutionRetry",
                            "message": message,
                        }),
                    },
                }),
            }
        })
    }

    fn cleanup_cancelled(
        &self,
        context: OperationExecutionContext,
    ) -> Pin<Box<dyn Future<Output = factory_coordinator::Result<()>> + Send + '_>> {
        Box::pin(async move {
            self.cleanup_cancelled_operation(&context)
                .await
                .map_err(cancel_cleanup_error)
        })
    }

    fn release_cancelled_execution_environment(
        &self,
        context: OperationExecutionContext,
    ) -> Pin<Box<dyn Future<Output = factory_coordinator::Result<()>> + Send + '_>> {
        Box::pin(async move { self.release_cancelled_environment(&context).await })
    }

    fn reconcile_execution_environments(
        &self,
    ) -> Pin<Box<dyn Future<Output = factory_coordinator::Result<()>> + Send + '_>> {
        Box::pin(async move { self.reconcile_releasing_execution_environments().await })
    }

    fn after_successful_settlement(
        &self,
        context: OperationExecutionContext,
    ) -> Pin<Box<dyn Future<Output = factory_coordinator::Result<()>> + Send + '_>> {
        Box::pin(async move { self.publish_settled_artifacts(&context).await })
    }
}

async fn release_execution_environment(
    store: &CoordinatorStore,
    provisioner: &dyn ExecutionEnvironmentProvisioner,
    environment: ExecutionEnvironmentRecord,
) -> factory_coordinator::Result<()> {
    release_execution_environment_backend(provisioner, &environment).await?;
    match store
        .mark_execution_environment_released(&environment.job_id, environment.generation)
        .await
    {
        Ok(_) => Ok(()),
        Err(CoordinatorError::ExecutionEnvironmentGenerationStale { .. }) => Ok(()),
        Err(error) => Err(error),
    }
}

async fn release_execution_environment_backend(
    provisioner: &dyn ExecutionEnvironmentProvisioner,
    environment: &ExecutionEnvironmentRecord,
) -> factory_coordinator::Result<()> {
    if environment.backend != provisioner.backend() {
        return Err(CoordinatorError::InvalidInput(format!(
            "execution environment {} uses backend {:?}, but this worker serves {:?}",
            environment.environment_id,
            environment.backend,
            provisioner.backend()
        )));
    }
    provisioner
        .release(ExecutionEnvironmentReleaseRequest {
            environment: environment.clone(),
        })
        .await
        .map_err(|error| {
            CoordinatorError::InvalidInput(format!(
                "release {} execution environment {} generation {}: {error:#}",
                provisioner.backend(),
                environment.environment_id,
                environment.generation
            ))
        })
}

async fn reconcile_releasing_environments(
    store: &CoordinatorStore,
    provisioner: &dyn ExecutionEnvironmentProvisioner,
) -> factory_coordinator::Result<()> {
    let mut failures = 0usize;
    for environment in store
        .list_releasing_execution_environments(provisioner.backend())
        .await?
    {
        let identity = format!(
            "{} generation {}",
            environment.environment_id, environment.generation
        );
        if let Err(error) = release_execution_environment(store, provisioner, environment).await {
            eprintln!(
                "factory execution-environment reconciliation failed for {identity}: {error}"
            );
            failures += 1;
        }
    }
    if failures != 0 {
        return Err(CoordinatorError::InvalidInput(format!(
            "{failures} execution environment release(s) remain pending"
        )));
    }
    Ok(())
}

fn cancel_cleanup_error(error: ExecutionFailure) -> CoordinatorError {
    match error {
        ExecutionFailure::Coordinator(error) => error,
        ExecutionFailure::Terminal(message)
        | ExecutionFailure::Retryable(message)
        | ExecutionFailure::PlanValidation(message) => CoordinatorError::InvalidInput(format!(
            "cancelled Factory operation cleanup failed: {message}"
        )),
        ExecutionFailure::Cancelled => CoordinatorError::InvalidInput(
            "cancelled Factory operation cleanup was itself cancelled".to_string(),
        ),
    }
}

type ExecutionResult<T> = Result<T, ExecutionFailure>;

enum ExecutionFailure {
    Coordinator(CoordinatorError),
    Terminal(String),
    Retryable(String),
    PlanValidation(String),
    Cancelled,
}

impl ExecutionFailure {
    fn terminal(message: impl Into<String>) -> Self {
        Self::Terminal(message.into())
    }

    fn retryable(message: impl Into<String>) -> Self {
        Self::Retryable(message.into())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use factory_coordinator::AttemptSettlement;
    use factory_coordinator::ClaimRequest;
    use factory_coordinator::CoordinatorInstanceId;
    use factory_coordinator::ExecutionEnvironmentStatus;
    use factory_coordinator::JobDefinition;
    use factory_coordinator::OperationDefinition;
    use factory_extension::FactoryBackendFuture;
    use factory_extension::FactoryStateDurability;

    use super::*;

    struct RecordingBackend {
        state: Mutex<Option<FactoryState>>,
    }

    struct ReconciliationProvisioner {
        released: Mutex<Vec<String>>,
        fail_environment: Mutex<Option<String>>,
    }

    impl ExecutionEnvironmentProvisioner for ReconciliationProvisioner {
        fn backend(&self) -> &'static str {
            "reconciliation-test"
        }

        fn ensure(
            &self,
            _request: ExecutionEnvironmentProvisionRequest,
        ) -> crate::execution_environment::ProvisionFuture<'_> {
            Box::pin(async {
                Ok(ProvisionedExecutionEnvironment {
                    backend_ref: "fixture".to_string(),
                    url: "ws://fixture:4500".to_string(),
                })
            })
        }

        fn release(
            &self,
            request: ExecutionEnvironmentReleaseRequest,
        ) -> crate::execution_environment::ReleaseFuture<'_> {
            let environment_id = request.environment.environment_id.as_str().to_string();
            self.released.lock().unwrap().push(environment_id.clone());
            let should_fail =
                self.fail_environment.lock().unwrap().as_deref() == Some(environment_id.as_str());
            Box::pin(async move {
                if should_fail {
                    anyhow::bail!("deliberate release failure")
                }
                Ok(())
            })
        }
    }

    impl FactoryStateBackend for RecordingBackend {
        fn load<'a>(
            &'a self,
            _thread_id: &'a str,
        ) -> FactoryBackendFuture<'a, Option<FactoryState>> {
            Box::pin(async move { Ok(self.state.lock().unwrap().clone()) })
        }

        fn save<'a>(
            &'a self,
            _thread_id: &'a str,
            state: FactoryState,
        ) -> FactoryBackendFuture<'a, ()> {
            Box::pin(async move {
                *self.state.lock().unwrap() = Some(state);
                Ok(())
            })
        }

        fn durability(&self) -> FactoryStateDurability {
            FactoryStateDurability::Durable
        }
    }

    #[tokio::test]
    async fn rejected_plan_state_is_replaced_with_a_clean_baseline() {
        let backend = RecordingBackend {
            state: Mutex::new(Some(FactoryState {
                revision: 9,
                ..FactoryState::default()
            })),
        };

        restore_factory_plan_state(&backend, "thread-1")
            .await
            .unwrap();

        assert_eq!(
            backend.load("thread-1").await.unwrap(),
            Some(FactoryState::default())
        );
    }

    #[tokio::test]
    #[ignore = "requires FACTORY_COORDINATOR_TEST_DATABASE_URL"]
    async fn restart_reconciliation_processes_other_rows_and_retries_failures() {
        let database_url = std::env::var("FACTORY_COORDINATOR_TEST_DATABASE_URL")
            .expect("set a disposable PostgreSQL database");
        let store = CoordinatorStore::connect(&database_url).await.unwrap();
        store.migrate().await.unwrap();
        let first = create_releasing_environment(&store, "first").await;
        let second = create_releasing_environment(&store, "second").await;
        let provisioner = ReconciliationProvisioner {
            released: Mutex::new(Vec::new()),
            fail_environment: Mutex::new(Some(first.environment_id.as_str().to_string())),
        };

        let error = reconcile_releasing_environments(&store, &provisioner)
            .await
            .expect_err("one release remains pending");
        assert!(
            error
                .to_string()
                .contains("1 execution environment release")
        );
        let released_ids = provisioner.released.lock().unwrap().clone();
        assert!(released_ids.contains(&first.environment_id.as_str().to_string()));
        assert!(released_ids.contains(&second.environment_id.as_str().to_string()));
        assert_eq!(
            store
                .load_execution_environment(&first.job_id)
                .await
                .unwrap()
                .unwrap()
                .status,
            ExecutionEnvironmentStatus::Releasing
        );
        assert_eq!(
            store
                .load_execution_environment(&second.job_id)
                .await
                .unwrap()
                .unwrap()
                .status,
            ExecutionEnvironmentStatus::Released
        );

        *provisioner.fail_environment.lock().unwrap() = None;
        reconcile_releasing_environments(&store, &provisioner)
            .await
            .expect("restart retries the persisted release");
        assert_eq!(
            store
                .load_execution_environment(&first.job_id)
                .await
                .unwrap()
                .unwrap()
                .status,
            ExecutionEnvironmentStatus::Released
        );
        assert!(
            store
                .list_releasing_execution_environments(provisioner.backend())
                .await
                .unwrap()
                .is_empty()
        );
        reconcile_releasing_environments(&store, &provisioner)
            .await
            .expect("reconciliation is idempotent when nothing remains");
        store.close().await;
    }

    #[test]
    fn runtime_accepts_only_canonical_persisted_provider_ids() {
        let legacy = json!({
            "task": "retained task",
            "executionProfile": {
                "provider": "claude",
                "model": "claude-sonnet-5"
            }
        });
        assert!(matches!(
            parse_task_input(&legacy),
            Err(ExecutionFailure::Terminal(message))
                if message.contains("is not a canonical Factory provider ID")
        ));

        let canonical = json!({
            "task": "retained task",
            "executionProfile": {
                "provider": "anthropic",
                "model": "claude-sonnet-5"
            }
        });
        let Ok(input) = parse_task_input(&canonical) else {
            panic!("canonical Anthropic task input must pass runtime validation");
        };
        assert_eq!(input.execution_profile.unwrap().provider, "anthropic");
    }

    async fn create_releasing_environment(
        store: &CoordinatorStore,
        label: &str,
    ) -> ExecutionEnvironmentRecord {
        let nonce = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let job = store
            .create_job(JobDefinition {
                kind: format!("runtime-reconciliation-{label}-{nonce}"),
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
                    owner_instance_id: CoordinatorInstanceId::new(format!(
                        "reconciliation-{label}-{nonce}"
                    )),
                    lease_seconds: 60,
                    execution_profile: None,
                },
            )
            .await
            .unwrap()
            .unwrap();
        let environment = store
            .ensure_execution_environment(&lease.fence, "reconciliation-test")
            .await
            .unwrap();
        store
            .mark_execution_environment_ready(
                &lease.fence,
                environment.generation,
                &format!("backend-{}", environment.environment_id),
                "ws://fixture:4500",
            )
            .await
            .unwrap();
        store
            .settle_attempt(&lease.fence, AttemptSettlement::Succeeded, None)
            .await
            .unwrap();
        store
            .load_execution_environment(&job.job.job_id)
            .await
            .unwrap()
            .unwrap()
    }
}
