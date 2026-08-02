use factory_coordinator::CancellationHandle;
use factory_coordinator::CoordinatorError;
use factory_coordinator::CorrelationRecordId;
use factory_coordinator::NewAttemptEvent;
use factory_coordinator::OperationCheckpoint;
use factory_coordinator::OperationExecutionContext;
use factory_extension::FactoryReviewVerdict;
use factory_extension::FactoryStateBackend;
use serde_json::json;

use crate::checkpoint::FactoryStateBaselines;
use crate::checkpoint::StageCheckpoint;
use crate::checkpoint::StageCheckpointPhase;
use crate::checkpoint::StageLineage;
use crate::checkpoint::StageTurnRole;
use crate::events::AttemptEventWriter;
use crate::session::AutonomousSession;
use crate::stages::OperationKind;
use crate::stages::ReviewBaseline;

use super::CodexOperationExecutor;
use super::ExecutionFailure;
use super::ExecutionResult;
use super::SingleStageRun;
use super::resume::ResumePoint;
use super::resume::checkpoint_baselines;
use super::resume::encode_checkpoint;
use super::resume::final_checkpoint;
use super::resume::load_state;
use super::resume::review_baselines;
use super::resume::validate_checkpoint_state;
use super::resume::workspace_metadata;
use super::task::TaskInput;
use super::task::stage_prompt;

impl CodexOperationExecutor {
    pub(super) async fn run_single_stage(
        &self,
        run: SingleStageRun<'_>,
        session: &mut AutonomousSession,
    ) -> ExecutionResult<CompletedOperation> {
        let SingleStageRun {
            context,
            cancellation,
            input,
            operation,
            resume,
            backend,
        } = run;
        let current = resume.current();
        if let Some((checkpoint, correlation_id)) = current {
            let may_promote = checkpoint.phase != StageCheckpointPhase::AwaitingTurn
                || self.persisted_turn_completed(session, checkpoint).await?;
            if may_promote {
                let state = load_state(backend, session.parent_thread_id()).await?;
                match validate_checkpoint_state(&state, checkpoint) {
                    Ok(()) => {
                        if checkpoint.turn_role == StageTurnRole::Review {
                            self.commit_detached_review(backend, session.parent_thread_id())
                                .await?;
                        }
                        if operation == OperationKind::Plan
                            && let Err(error) = self
                                .workspaces
                                .validate_pristine(
                                    context.workspace().expect("validated managed worktree"),
                                )
                                .await
                        {
                            let message = format!("plan modified its managed worktree: {error}");
                            self.emit_stage_error(context, checkpoint, &message).await?;
                            return Err(ExecutionFailure::PlanValidation(message));
                        }
                        return self.complete_stage(
                            checkpoint,
                            final_checkpoint(
                                checkpoint,
                                workspace_metadata(context),
                                correlation_id.clone(),
                            )?,
                        );
                    }
                    Err(error) if checkpoint.phase == StageCheckpointPhase::Completed => {
                        if checkpoint.turn_role == StageTurnRole::Review {
                            self.rollback_detached_review(backend, session.parent_thread_id())
                                .await?;
                        }
                        return Err(ExecutionFailure::terminal(format!(
                            "completed stage state is no longer valid: {error}"
                        )));
                    }
                    Err(_) => {}
                }
            }
        }

        let (turn_role, review_cycle, baselines) = match current {
            Some((checkpoint, _)) => (
                checkpoint.turn_role,
                checkpoint.review_cycle,
                if operation == OperationKind::Plan {
                    FactoryStateBaselines::new(0, None, None)
                } else {
                    checkpoint_baselines(checkpoint)
                },
            ),
            None => {
                let state = load_state(backend, session.parent_thread_id()).await?;
                if operation == OperationKind::Review {
                    let parent_turn_id = resume.preceding_turn_id().ok_or_else(|| {
                        ExecutionFailure::terminal(
                            "review stage has no completed execute turn to review",
                        )
                    })?;
                    let baseline =
                        ReviewBaseline::capture(&state, session.parent_thread_id(), parent_turn_id)
                            .map_err(|error| ExecutionFailure::terminal(error.to_string()))?;
                    (
                        StageTurnRole::Review,
                        0,
                        review_baselines(&state, &baseline),
                    )
                } else {
                    (
                        StageTurnRole::Stage,
                        0,
                        FactoryStateBaselines::new(state.revision, None, None),
                    )
                }
            }
        };

        // A recovered Plan may already be a valid business result; the block
        // above gets the first chance to promote it. Only once we have proven
        // that a replacement turn is required do we reset Factory state and
        // the disposable worktree. The current attempt's thread correlation
        // was established by the outer executor before entering this method.
        if operation == OperationKind::Plan {
            self.restore_plan_baseline(
                backend,
                session.parent_thread_id(),
                context.workspace().expect("validated managed worktree"),
            )
            .await
            .map_err(ExecutionFailure::retryable)?;
        } else if current
            .is_some_and(|(checkpoint, _)| checkpoint.turn_role == StageTurnRole::Review)
        {
            self.rollback_detached_review(backend, session.parent_thread_id())
                .await?;
        }

        let completed = self
            .run_turn(
                context,
                cancellation,
                input,
                operation,
                turn_role,
                review_cycle,
                baselines,
                backend,
                session,
            )
            .await?;
        let state = load_state(backend, session.parent_thread_id()).await?;
        if let Err(error) = validate_checkpoint_state(&state, &completed.checkpoint) {
            if completed.checkpoint.turn_role == StageTurnRole::Review {
                self.rollback_detached_review(backend, session.parent_thread_id())
                    .await?;
            }
            self.emit_stage_error(context, &completed.checkpoint, &error)
                .await?;
            return Err(if operation == OperationKind::Plan {
                ExecutionFailure::PlanValidation(format!(
                    "plan produced execution state instead of a pure decomposition: {error}"
                ))
            } else {
                ExecutionFailure::retryable(error)
            });
        }
        if completed.checkpoint.turn_role == StageTurnRole::Review {
            self.commit_detached_review(backend, session.parent_thread_id())
                .await?;
        }
        if operation == OperationKind::Plan
            && let Err(error) = self
                .workspaces
                .validate_pristine(context.workspace().expect("validated managed worktree"))
                .await
        {
            let message = format!("plan modified its managed worktree: {error}");
            self.emit_stage_error(context, &completed.checkpoint, &message)
                .await?;
            return Err(ExecutionFailure::PlanValidation(message));
        }
        self.complete_stage(
            &completed.checkpoint,
            final_checkpoint(
                &completed.checkpoint,
                workspace_metadata(context),
                Some(completed.correlation_id),
            )?,
        )
    }

    pub(super) async fn run_remediation_loop(
        &self,
        context: &OperationExecutionContext,
        cancellation: &CancellationHandle,
        input: &TaskInput,
        resume: &ResumePoint,
        backend: &dyn FactoryStateBackend,
        session: &mut AutonomousSession,
    ) -> ExecutionResult<CompletedOperation> {
        let mut step = match resume.current() {
            Some((checkpoint, correlation_id)) => match checkpoint.phase {
                StageCheckpointPhase::AwaitingTurn => ReviewLoopStep::Evaluate {
                    checkpoint: checkpoint.clone(),
                    correlation_id: correlation_id.clone(),
                    may_rerun: true,
                },
                StageCheckpointPhase::TurnCompleted => ReviewLoopStep::Evaluate {
                    checkpoint: checkpoint.clone(),
                    correlation_id: correlation_id.clone(),
                    may_rerun: true,
                },
                StageCheckpointPhase::Completed => ReviewLoopStep::Evaluate {
                    checkpoint: checkpoint.clone(),
                    correlation_id: correlation_id.clone(),
                    may_rerun: false,
                },
            },
            None => {
                let state = load_state(backend, session.parent_thread_id()).await?;
                if OperationKind::Remediate.validate(&state, None).is_ok()
                    && state
                        .review
                        .as_ref()
                        .is_some_and(|review| review.verdict == FactoryReviewVerdict::Approve)
                {
                    let review = resume.preceding_checkpoint().ok_or_else(|| {
                        ExecutionFailure::terminal(
                            "approved remediation has no completed review checkpoint",
                        )
                    })?;
                    let completed = StageCheckpoint::completed(
                        OperationKind::Remediate,
                        review.lineage(),
                        StageTurnRole::Review,
                        1,
                        checkpoint_baselines(review),
                    )
                    .map_err(|error| ExecutionFailure::terminal(error.to_string()))?;
                    validate_checkpoint_state(&state, &completed)
                        .map_err(ExecutionFailure::terminal)?;
                    let correlation_id = self
                        .append_checkpoint_turn_correlation(context, review)
                        .await?;
                    return self.complete_stage(
                        &completed,
                        encode_checkpoint(
                            &completed,
                            workspace_metadata(context),
                            Some(correlation_id),
                        )?,
                    );
                }
                ReviewLoopStep::Run {
                    role: StageTurnRole::Remediation,
                    cycle: 1,
                    baselines: FactoryStateBaselines::new(state.revision, None, None),
                }
            }
        };

        loop {
            if cancellation.is_cancelled() {
                return Err(ExecutionFailure::Cancelled);
            }
            step = match step {
                ReviewLoopStep::Run {
                    role,
                    cycle,
                    baselines,
                } => {
                    let completed = self
                        .run_turn(
                            context,
                            cancellation,
                            input,
                            OperationKind::Remediate,
                            role,
                            cycle,
                            baselines,
                            backend,
                            session,
                        )
                        .await?;
                    ReviewLoopStep::Evaluate {
                        checkpoint: completed.checkpoint,
                        correlation_id: Some(completed.correlation_id),
                        may_rerun: false,
                    }
                }
                ReviewLoopStep::Evaluate {
                    checkpoint,
                    correlation_id,
                    may_rerun,
                } => {
                    if checkpoint.phase == StageCheckpointPhase::AwaitingTurn
                        && !self.persisted_turn_completed(session, &checkpoint).await?
                    {
                        if checkpoint.turn_role == StageTurnRole::Review {
                            self.rollback_detached_review(backend, session.parent_thread_id())
                                .await?;
                        }
                        ReviewLoopStep::Run {
                            role: checkpoint.turn_role,
                            cycle: checkpoint.review_cycle,
                            baselines: checkpoint_baselines(&checkpoint),
                        }
                    } else {
                        let state = load_state(backend, session.parent_thread_id()).await?;
                        if let Err(error) = validate_checkpoint_state(&state, &checkpoint) {
                            if checkpoint.turn_role == StageTurnRole::Review {
                                self.rollback_detached_review(backend, session.parent_thread_id())
                                    .await?;
                            }
                            if may_rerun {
                                ReviewLoopStep::Run {
                                    role: checkpoint.turn_role,
                                    cycle: checkpoint.review_cycle,
                                    baselines: checkpoint_baselines(&checkpoint),
                                }
                            } else if checkpoint.phase == StageCheckpointPhase::Completed {
                                return Err(ExecutionFailure::terminal(format!(
                                    "completed remediation state is no longer valid: {error}"
                                )));
                            } else {
                                return Err(ExecutionFailure::retryable(error));
                            }
                        } else {
                            if checkpoint.turn_role == StageTurnRole::Review {
                                self.commit_detached_review(backend, session.parent_thread_id())
                                    .await?;
                            }
                            let review = state.review.as_ref().ok_or_else(|| {
                                ExecutionFailure::retryable("Factory state has no review")
                            })?;
                            match checkpoint.turn_role {
                                StageTurnRole::Remediation => {
                                    if review.verdict == FactoryReviewVerdict::Approve {
                                        return self.complete_stage(
                                            &checkpoint,
                                            final_checkpoint(
                                                &checkpoint,
                                                workspace_metadata(context),
                                                correlation_id,
                                            )?,
                                        );
                                    }
                                    self.emit_review_cycle_completed(context, &checkpoint)
                                        .await?;
                                    let baseline = ReviewBaseline::capture(
                                        &state,
                                        session.parent_thread_id(),
                                        checkpoint.turn_id.clone(),
                                    )
                                    .map_err(|error| {
                                        ExecutionFailure::terminal(error.to_string())
                                    })?;
                                    ReviewLoopStep::Run {
                                        role: StageTurnRole::Review,
                                        cycle: checkpoint.review_cycle,
                                        baselines: review_baselines(&state, &baseline),
                                    }
                                }
                                StageTurnRole::Review => {
                                    if review.verdict == FactoryReviewVerdict::Approve {
                                        return self.complete_stage(
                                            &checkpoint,
                                            final_checkpoint(
                                                &checkpoint,
                                                workspace_metadata(context),
                                                correlation_id,
                                            )?,
                                        );
                                    }
                                    self.emit_review_cycle_completed(context, &checkpoint)
                                        .await?;
                                    if checkpoint.review_cycle >= self.max_review_cycles {
                                        return Err(ExecutionFailure::terminal(format!(
                                            "review still requires changes after {} remediation cycles",
                                            self.max_review_cycles
                                        )));
                                    }
                                    ReviewLoopStep::Run {
                                        role: StageTurnRole::Remediation,
                                        cycle: checkpoint.review_cycle + 1,
                                        baselines: FactoryStateBaselines::new(
                                            state.revision,
                                            None,
                                            None,
                                        ),
                                    }
                                }
                                StageTurnRole::Stage => {
                                    return Err(ExecutionFailure::terminal(
                                        "remediation checkpoint cannot use the stage turn role",
                                    ));
                                }
                            }
                        }
                    }
                }
            };
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn run_turn(
        &self,
        context: &OperationExecutionContext,
        cancellation: &CancellationHandle,
        input: &TaskInput,
        operation: OperationKind,
        role: StageTurnRole,
        cycle: u32,
        baselines: FactoryStateBaselines,
        backend: &dyn FactoryStateBackend,
        session: &mut AutonomousSession,
    ) -> ExecutionResult<CompletedStageTurn> {
        if cancellation.is_cancelled() {
            return Err(ExecutionFailure::Cancelled);
        }
        let mut events = AttemptEventWriter::new(self.store.clone(), context.lease().fence.clone());
        events
            .emit(
                if operation == OperationKind::Remediate {
                    "review_cycle.turn_started"
                } else {
                    "stage.started"
                },
                json!({
                    "stage": operation.as_wire_name(),
                    "role": role,
                    "reviewCycle": cycle,
                }),
            )
            .await
            .map_err(ExecutionFailure::Coordinator)?;
        let prompt = stage_prompt(input, operation, role, cycle);
        let review_snapshot = if role == StageTurnRole::Review {
            Some(
                self.prepare_detached_review(
                    backend,
                    session.parent_thread_id(),
                    context.workspace().expect("validated managed worktree"),
                )
                .await?,
            )
        } else {
            None
        };

        let turn_execution: ExecutionResult<CompletedStageTurn> = async {
            let turn_result = match role {
                StageTurnRole::Review => {
                    let parent_turn_id = baselines
                        .expected_review_parent_turn_id
                        .as_deref()
                        .ok_or_else(|| {
                            ExecutionFailure::terminal(
                                "review turn has no exact parent turn baseline",
                            )
                        })?;
                    let durable_state_key = session.parent_thread_id().to_string();
                    session
                        .start_review(prompt, parent_turn_id, durable_state_key)
                        .await
                }
                StageTurnRole::Stage | StageTurnRole::Remediation => {
                    session
                        .start_text_turn(
                            prompt,
                            operation.sandbox_policy(),
                            operation.collaboration_mode(),
                            operation.turn_stage(),
                        )
                        .await
                }
            };
            let turn = match turn_result {
                Ok(turn) => turn,
                Err(error) => {
                    let message = error.to_string();
                    events
                        .emit(
                            "stage.error",
                            json!({
                                "stage": operation.as_wire_name(),
                                "role": role,
                                "reviewCycle": cycle,
                                "message": message,
                            }),
                        )
                        .await
                        .map_err(ExecutionFailure::Coordinator)?;
                    return Err(ExecutionFailure::retryable(message));
                }
            };
            events.activate(turn.thread_id(), turn.turn_id());
            let correlation = self.append_turn_correlation(context, &turn).await?;
            let lineage =
                StageLineage::new(session.parent_thread_id(), turn.thread_id(), turn.turn_id())
                    .map_err(|error| ExecutionFailure::terminal(error.to_string()))?;
            let awaiting = StageCheckpoint::awaiting_turn(
                operation,
                lineage.clone(),
                role,
                cycle,
                baselines.clone(),
            )
            .map_err(|error| ExecutionFailure::terminal(error.to_string()))?;
            context
                .checkpoints()
                .write(encode_checkpoint(
                    &awaiting,
                    workspace_metadata(context),
                    Some(correlation.correlation_id.clone()),
                )?)
                .await
                .map_err(ExecutionFailure::Coordinator)?;

            if let Err(error) = session.wait_for_completion(cancellation, &mut events).await {
                if matches!(
                    error.downcast_ref::<CoordinatorError>(),
                    Some(CoordinatorError::AttemptLeaseUnavailable(_))
                ) {
                    return Err(ExecutionFailure::Coordinator(
                        CoordinatorError::AttemptLeaseUnavailable(
                            context.lease().fence.attempt_id.clone(),
                        ),
                    ));
                }
                if cancellation.is_cancelled() {
                    return Err(ExecutionFailure::Cancelled);
                }
                let message = error.to_string();
                events
                    .emit(
                        "stage.error",
                        json!({
                            "stage": operation.as_wire_name(),
                            "role": role,
                            "reviewCycle": cycle,
                            "message": message,
                        }),
                    )
                    .await
                    .map_err(ExecutionFailure::Coordinator)?;
                return Err(ExecutionFailure::retryable(message));
            }
            let completed =
                StageCheckpoint::turn_completed(operation, lineage, role, cycle, baselines)
                    .map_err(|error| ExecutionFailure::terminal(error.to_string()))?;
            context
                .checkpoints()
                .write(encode_checkpoint(
                    &completed,
                    workspace_metadata(context),
                    Some(correlation.correlation_id.clone()),
                )?)
                .await
                .map_err(ExecutionFailure::Coordinator)?;
            Ok(CompletedStageTurn {
                checkpoint: completed,
                correlation_id: correlation.correlation_id,
            })
        }
        .await;

        if let Some(snapshot) = review_snapshot {
            let changed = self
                .restore_detached_review_workspace(
                    context.workspace().expect("validated managed worktree"),
                    snapshot,
                )
                .await?;
            if changed {
                self.rollback_detached_review(backend, session.parent_thread_id())
                    .await?;
                self.acknowledge_detached_review_mutation(
                    context.workspace().expect("validated managed worktree"),
                )
                .await?;
                if turn_execution.is_ok() {
                    let message =
                        "detached review modified its managed worktree; changes were restored";
                    events
                        .emit(
                            "stage.error",
                            json!({
                                "stage": operation.as_wire_name(),
                                "role": role,
                                "reviewCycle": cycle,
                                "message": message,
                            }),
                        )
                        .await
                        .map_err(ExecutionFailure::Coordinator)?;
                    return Err(ExecutionFailure::retryable(message));
                }
            } else if turn_execution.is_err() {
                self.rollback_detached_review(backend, session.parent_thread_id())
                    .await?;
            }
        }

        turn_execution
    }

    fn complete_stage(
        &self,
        checkpoint: &StageCheckpoint,
        durable_checkpoint: OperationCheckpoint,
    ) -> ExecutionResult<CompletedOperation> {
        Ok(CompletedOperation {
            checkpoint: durable_checkpoint,
            completion_event: NewAttemptEvent {
                kind: "stage.completed".to_string(),
                payload: json!({
                    "stage": checkpoint.operation.as_wire_name(),
                    "role": checkpoint.turn_role,
                    "reviewCycle": checkpoint.review_cycle,
                    "threadId": checkpoint.active_thread_id,
                    "turnId": checkpoint.turn_id,
                }),
                deduplication_key: None,
            },
        })
    }

    async fn emit_review_cycle_completed(
        &self,
        context: &OperationExecutionContext,
        checkpoint: &StageCheckpoint,
    ) -> ExecutionResult<()> {
        let mut events = AttemptEventWriter::new(self.store.clone(), context.lease().fence.clone());
        events.activate(&checkpoint.active_thread_id, &checkpoint.turn_id);
        events
            .emit(
                "review_cycle.turn_completed",
                json!({
                    "stage": checkpoint.operation.as_wire_name(),
                    "role": checkpoint.turn_role,
                    "reviewCycle": checkpoint.review_cycle,
                }),
            )
            .await
            .map_err(ExecutionFailure::Coordinator)
    }

    async fn emit_stage_error(
        &self,
        context: &OperationExecutionContext,
        checkpoint: &StageCheckpoint,
        message: &str,
    ) -> ExecutionResult<()> {
        let mut events = AttemptEventWriter::new(self.store.clone(), context.lease().fence.clone());
        events.activate(&checkpoint.active_thread_id, &checkpoint.turn_id);
        events
            .emit(
                "stage.error",
                json!({
                    "stage": checkpoint.operation.as_wire_name(),
                    "role": checkpoint.turn_role,
                    "reviewCycle": checkpoint.review_cycle,
                    "message": message,
                }),
            )
            .await
            .map_err(ExecutionFailure::Coordinator)
    }
}

struct CompletedStageTurn {
    checkpoint: StageCheckpoint,
    correlation_id: CorrelationRecordId,
}

pub(super) struct CompletedOperation {
    pub(super) checkpoint: OperationCheckpoint,
    pub(super) completion_event: NewAttemptEvent,
}

enum ReviewLoopStep {
    Run {
        role: StageTurnRole,
        cycle: u32,
        baselines: FactoryStateBaselines,
    },
    Evaluate {
        checkpoint: StageCheckpoint,
        correlation_id: Option<CorrelationRecordId>,
        may_rerun: bool,
    },
}
