use codex_app_server_protocol::TurnStatus;
use factory_coordinator::Correlation;
use factory_coordinator::CorrelationRecordId;
use factory_coordinator::ItemId;
use factory_coordinator::OperationCheckpoint;
use factory_coordinator::OperationExecutionContext;
use factory_coordinator::RequestId;
use factory_coordinator::ResumeStrategy;
use factory_coordinator::ThreadId;
use factory_coordinator::TurnId;
use factory_extension::FactoryReviewVerdict;
use factory_extension::FactoryState;
use factory_extension::FactoryStateBackend;

use crate::checkpoint::FactoryStateBaselines;
use crate::checkpoint::StageCheckpoint;
use crate::checkpoint::StageCheckpointPhase;
use crate::checkpoint::StageTurnRole;
use crate::session::AutonomousSession;
use crate::session::ThreadCorrelation;
use crate::session::TurnCorrelation;
use crate::stages::OperationKind;
use crate::stages::ReviewBaseline;

use super::CodexOperationExecutor;
use super::ExecutionFailure;
use super::ExecutionResult;

impl CodexOperationExecutor {
    pub(super) async fn append_thread_correlation(
        &self,
        context: &OperationExecutionContext,
        thread: &ThreadCorrelation,
    ) -> ExecutionResult<CorrelationRecordId> {
        let record = self
            .store
            .append_correlation(
                &context.lease().fence,
                &durable_correlation(
                    context,
                    thread.request_id().to_string(),
                    Some(thread.thread_id()),
                    None,
                ),
            )
            .await
            .map_err(ExecutionFailure::Coordinator)?;
        Ok(record.correlation_id)
    }

    pub(super) async fn append_turn_correlation(
        &self,
        context: &OperationExecutionContext,
        turn: &TurnCorrelation,
    ) -> ExecutionResult<factory_coordinator::DurableCorrelationRecord> {
        self.store
            .append_correlation(
                &context.lease().fence,
                &durable_correlation(
                    context,
                    turn.request_id().to_string(),
                    Some(turn.thread_id()),
                    Some(turn.turn_id()),
                ),
            )
            .await
            .map_err(ExecutionFailure::Coordinator)
    }

    pub(super) async fn persisted_turn_completed(
        &self,
        session: &mut AutonomousSession,
        checkpoint: &StageCheckpoint,
    ) -> ExecutionResult<bool> {
        session
            .persisted_turn_status(&checkpoint.active_thread_id, &checkpoint.turn_id)
            .await
            .map(|status| status == Some(TurnStatus::Completed))
            .map_err(|error| ExecutionFailure::retryable(error.to_string()))
    }

    /// Re-attaches a recovered turn to the newly claimed attempt. A retry has
    /// a new attempt ID, so its final checkpoint cannot point at the source
    /// attempt's correlation record even though the Codex IDs are unchanged.
    pub(super) async fn append_resumed_turn_correlation(
        &self,
        context: &OperationExecutionContext,
        resume: &ResumePoint,
    ) -> ExecutionResult<Option<CorrelationRecordId>> {
        let Some((checkpoint, _)) = resume.current() else {
            return Ok(None);
        };
        self.append_checkpoint_turn_correlation(context, checkpoint)
            .await
            .map(Some)
    }

    /// Re-attaches an exact checkpoint turn correlation to the current
    /// attempt. This is used for both a recovered current stage and a
    /// no-op remediation that completes from the preceding approved review.
    pub(super) async fn append_checkpoint_turn_correlation(
        &self,
        context: &OperationExecutionContext,
        checkpoint: &StageCheckpoint,
    ) -> ExecutionResult<CorrelationRecordId> {
        let source = context
            .lease()
            .selection
            .checkpoint_correlation
            .as_ref()
            .ok_or_else(|| {
                ExecutionFailure::terminal("checkpoint has no exact turn correlation")
            })?;
        if source.correlation.thread_id.as_ref().map(ThreadId::as_str)
            != Some(checkpoint.active_thread_id.as_str())
            || source.correlation.turn_id.as_ref().map(TurnId::as_str)
                != Some(checkpoint.turn_id.as_str())
        {
            return Err(ExecutionFailure::terminal(
                "checkpoint correlation does not match its Codex turn",
            ));
        }
        let record = self
            .store
            .append_correlation(
                &context.lease().fence,
                &durable_correlation(
                    context,
                    source.correlation.request_id.as_str().to_string(),
                    Some(checkpoint.active_thread_id.as_str()),
                    Some(checkpoint.turn_id.as_str()),
                ),
            )
            .await
            .map_err(ExecutionFailure::Coordinator)?;
        Ok(record.correlation_id)
    }
}

pub(super) enum ResumePoint {
    Fresh,
    Previous {
        checkpoint: StageCheckpoint,
    },
    Current {
        checkpoint: StageCheckpoint,
        correlation_id: Option<CorrelationRecordId>,
    },
}

impl ResumePoint {
    pub(super) fn decode(
        context: &OperationExecutionContext,
        operation: OperationKind,
        workspace_root: &str,
        workspace_revision: &str,
    ) -> ExecutionResult<Self> {
        let record = match &context.lease().selection.resume {
            ResumeStrategy::Fresh => {
                if operation != OperationKind::Plan || context.operation().ordinal != 0 {
                    return Err(ExecutionFailure::terminal(
                        "non-plan stage has no completed predecessor checkpoint",
                    ));
                }
                return Ok(Self::Fresh);
            }
            ResumeStrategy::FromCheckpoint(record) => record,
        };
        if record.workspace_root.as_deref() != Some(workspace_root)
            || record.workspace_revision.as_deref() != Some(workspace_revision)
        {
            return Err(ExecutionFailure::terminal(
                "resume checkpoint does not belong to the current managed worktree",
            ));
        }
        let checkpoint = StageCheckpoint::decode(record)
            .map_err(|error| ExecutionFailure::terminal(error.to_string()))?;
        if checkpoint.operation == operation {
            return Ok(Self::Current {
                checkpoint,
                correlation_id: record.correlation_id.clone(),
            });
        }
        let predecessor = context
            .operation()
            .ordinal
            .checked_sub(1)
            .map(OperationKind::at_ordinal)
            .ok_or_else(|| {
                ExecutionFailure::terminal("plan stage resumed from another operation")
            })?;
        if checkpoint.operation != predecessor
            || checkpoint.phase != StageCheckpointPhase::Completed
        {
            return Err(ExecutionFailure::terminal(
                "resume checkpoint is not the completed immediately preceding Factory stage",
            ));
        }
        Ok(Self::Previous { checkpoint })
    }

    pub(super) fn current(&self) -> Option<(&StageCheckpoint, &Option<CorrelationRecordId>)> {
        match self {
            Self::Current {
                checkpoint,
                correlation_id,
            } => Some((checkpoint, correlation_id)),
            Self::Fresh | Self::Previous { .. } => None,
        }
    }

    pub(super) fn preceding_turn_id(&self) -> Option<&str> {
        match self {
            Self::Previous { checkpoint, .. } => Some(checkpoint.turn_id.as_str()),
            Self::Fresh | Self::Current { .. } => None,
        }
    }

    pub(super) fn preceding_checkpoint(&self) -> Option<&StageCheckpoint> {
        match self {
            Self::Previous { checkpoint } => Some(checkpoint),
            Self::Fresh | Self::Current { .. } => None,
        }
    }

    pub(super) fn correlation_checkpoint(&self) -> Option<&StageCheckpoint> {
        match self {
            Self::Previous { checkpoint } | Self::Current { checkpoint, .. } => Some(checkpoint),
            Self::Fresh => None,
        }
    }

    pub(super) fn parent_thread_id(&self) -> Option<&str> {
        match self {
            Self::Previous { checkpoint } | Self::Current { checkpoint, .. } => {
                Some(checkpoint.parent_execution_thread_id.as_str())
            }
            Self::Fresh => None,
        }
    }

    pub(super) fn set_current_correlation(&mut self, value: CorrelationRecordId) {
        if let Self::Current { correlation_id, .. } = self {
            *correlation_id = Some(value);
        }
    }
}

fn durable_correlation(
    context: &OperationExecutionContext,
    request_id: String,
    thread_id: Option<&str>,
    turn_id: Option<&str>,
) -> Correlation {
    Correlation {
        job_id: context.job().job_id.clone(),
        operation_id: context.operation().operation_id.clone(),
        attempt_id: context.lease().fence.attempt_id.clone(),
        request_id: RequestId::new(request_id),
        thread_id: thread_id.map(ThreadId::new),
        turn_id: turn_id.map(TurnId::new),
        item_id: None::<ItemId>,
    }
}

pub(super) async fn load_state(
    backend: &dyn FactoryStateBackend,
    parent_thread_id: &str,
) -> ExecutionResult<FactoryState> {
    backend
        .load(parent_thread_id)
        .await
        .map(|state| state.unwrap_or_default())
        .map_err(|error| ExecutionFailure::retryable(error.to_string()))
}

pub(super) fn validate_checkpoint_state(
    state: &FactoryState,
    checkpoint: &StageCheckpoint,
) -> Result<(), String> {
    match checkpoint.turn_role {
        StageTurnRole::Stage => {
            checkpoint
                .operation
                .validate(state, None)
                .map_err(|error| error.to_string())?;
            // An iterate round starts with every unit already completed and
            // the progress tool rejects rewriting a completed unit, so no
            // Factory state mutation is possible; the round's detached
            // review judges the workspace changes instead.
            if checkpoint.operation != OperationKind::Iterate
                && state.revision <= checkpoint.state_revision_baseline
            {
                return Err(format!(
                    "Factory state revision {} did not advance beyond {}",
                    state.revision, checkpoint.state_revision_baseline
                ));
            }
        }
        StageTurnRole::Remediation => {
            OperationKind::Remediate
                .validate(state, None)
                .map_err(|error| error.to_string())?;
            let review = state
                .review
                .as_ref()
                .ok_or_else(|| "Factory state has no review".to_string())?;
            if review.verdict != FactoryReviewVerdict::Approve
                && state.revision <= checkpoint.state_revision_baseline
            {
                return Err(format!(
                    "Factory state revision {} did not advance beyond {}",
                    state.revision, checkpoint.state_revision_baseline
                ));
            }
        }
        StageTurnRole::Review => {
            let baseline = checkpoint_review_baseline(checkpoint)?;
            OperationKind::Review
                .validate(state, Some(&baseline))
                .map_err(|error| error.to_string())?;
            let review = state
                .review
                .as_ref()
                .ok_or_else(|| "Factory state has no review".to_string())?;
            if review.recorded_thread_id.as_deref() != Some(checkpoint.active_thread_id.as_str()) {
                return Err(format!(
                    "review was recorded by thread {:?}, expected {:?}",
                    review.recorded_thread_id, checkpoint.active_thread_id
                ));
            }
            if review.recorded_turn_id.as_deref() != Some(checkpoint.turn_id.as_str()) {
                return Err(format!(
                    "review was recorded by turn {:?}, expected {:?}",
                    review.recorded_turn_id, checkpoint.turn_id
                ));
            }
        }
    }
    Ok(())
}

fn checkpoint_review_baseline(checkpoint: &StageCheckpoint) -> Result<ReviewBaseline, String> {
    let generation = checkpoint
        .review_generation_baseline
        .ok_or_else(|| "review checkpoint has no review generation baseline".to_string())?;
    let parent_turn_id = checkpoint
        .expected_review_parent_turn_id
        .clone()
        .ok_or_else(|| "review checkpoint has no expected parent turn".to_string())?;
    ReviewBaseline::from_parts(
        generation,
        checkpoint.parent_execution_thread_id.clone(),
        parent_turn_id,
    )
    .map_err(|error| error.to_string())
}

pub(super) fn review_baselines(
    state: &FactoryState,
    baseline: &ReviewBaseline,
) -> FactoryStateBaselines {
    FactoryStateBaselines::new(
        state.revision,
        Some(baseline.generation()),
        Some(baseline.parent_turn_id().to_string()),
    )
}

pub(super) fn checkpoint_baselines(checkpoint: &StageCheckpoint) -> FactoryStateBaselines {
    FactoryStateBaselines::new(
        checkpoint.state_revision_baseline,
        checkpoint.review_generation_baseline,
        checkpoint.expected_review_parent_turn_id.clone(),
    )
}

pub(super) fn final_checkpoint(
    checkpoint: &StageCheckpoint,
    workspace: (&str, &str),
    correlation_id: Option<CorrelationRecordId>,
) -> ExecutionResult<OperationCheckpoint> {
    let completed = StageCheckpoint::completed(
        checkpoint.operation,
        checkpoint.lineage(),
        checkpoint.turn_role,
        checkpoint.review_cycle,
        checkpoint_baselines(checkpoint),
    )
    .map_err(|error| ExecutionFailure::terminal(error.to_string()))?;
    encode_checkpoint(&completed, workspace, correlation_id)
}

pub(super) fn encode_checkpoint(
    checkpoint: &StageCheckpoint,
    workspace: (&str, &str),
    correlation_id: Option<CorrelationRecordId>,
) -> ExecutionResult<OperationCheckpoint> {
    checkpoint
        .encode(
            Some(workspace.0.to_string()),
            Some(workspace.1.to_string()),
            correlation_id,
        )
        .map_err(|error| ExecutionFailure::terminal(error.to_string()))
}

pub(super) fn workspace_metadata(context: &OperationExecutionContext) -> (&str, &str) {
    let workspace = context
        .workspace()
        .expect("executor validates the managed worktree before running");
    (&workspace.root, &workspace.revision)
}
