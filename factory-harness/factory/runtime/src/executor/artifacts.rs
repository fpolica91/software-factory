use factory_coordinator::ArtifactProjectionWarning;
use factory_coordinator::AttemptState;
use factory_coordinator::CoordinatorError;
use factory_coordinator::JobEventRecord;
use factory_coordinator::JobId;
use factory_coordinator::NewJobEvent;
use factory_coordinator::OperationExecutionContext;
use factory_coordinator::OperationState;
use factory_coordinator::reduce_settled_job_outputs;
use serde_json::json;

use super::CodexOperationExecutor;

impl CodexOperationExecutor {
    /// Publishes only from state already committed by the durable runner. The
    /// hook re-reads the attempt, operation, and completion events instead of
    /// trusting any pre-settlement in-memory result.
    pub(super) async fn publish_settled_artifacts(
        &self,
        context: &OperationExecutionContext,
    ) -> factory_coordinator::Result<()> {
        match self.publish_settled_artifacts_inner(context).await {
            Ok(()) => Ok(()),
            Err(error) => {
                let message = format!("post-settlement artifact publication failed: {error}");
                self.emit_settled_artifact_warning(&context.job().job_id, message.clone())
                    .await;
                Err(CoordinatorError::Workspace(message))
            }
        }
    }

    async fn publish_settled_artifacts_inner(
        &self,
        context: &OperationExecutionContext,
    ) -> factory_coordinator::Result<()> {
        let attempt = self
            .store
            .load_attempt(&context.lease().fence.attempt_id)
            .await?;
        let workspace = self
            .store
            .load_workspace(&context.job().job_id)
            .await?
            .ok_or_else(|| CoordinatorError::Workspace("artifact workspace is missing".into()))?;
        let paths = self
            .artifacts
            .job_paths(&context.job().job_id, &workspace.repository_id);
        let guard = self.artifacts.acquire_publication_guard(&paths).await?;
        let job = self.store.load_job(&context.job().job_id).await?;
        let operation = job
            .operations
            .iter()
            .find(|operation| operation.operation_id == context.operation().operation_id)
            .ok_or_else(|| {
                CoordinatorError::InvalidInput("settled artifact operation is missing".to_string())
            })?;
        require_settled_publication(attempt.state, operation.state)?;
        for warning in self
            .artifacts
            .initialize_job_files(&job, Some(&workspace))
            .await?
        {
            self.emit_projection_warning(&job.job.job_id, warning).await;
        }
        let events = self.load_all_job_events(&job.job.job_id).await?;
        let outputs = reduce_settled_job_outputs(&job, &events).map_err(|error| {
            CoordinatorError::InvalidInput(format!("reconstruct settled stage output: {error}"))
        })?;
        if !outputs
            .iter()
            .any(|output| output.operation_id == operation.operation_id)
        {
            return Err(CoordinatorError::InvalidInput(format!(
                "settled {} stage has no validated output",
                operation.kind
            )));
        }
        let warnings = self
            .artifacts
            .publish_settled_outputs(&paths, &outputs, &guard)
            .await?;
        for warning in warnings {
            self.emit_projection_warning(&job.job.job_id, warning).await;
        }
        Ok(())
    }

    async fn load_all_job_events(
        &self,
        job_id: &JobId,
    ) -> factory_coordinator::Result<Vec<JobEventRecord>> {
        let mut after = 0;
        let mut events = Vec::new();
        loop {
            let page = self.store.list_job_events(job_id, after, 1_000).await?;
            let count = page.events.len();
            after = page.next_cursor;
            events.extend(page.events);
            if count < 1_000 {
                return Ok(events);
            }
        }
    }

    async fn emit_projection_warning(&self, job_id: &JobId, warning: ArtifactProjectionWarning) {
        let message = format!(
            "the workspace projection of {} could not be refreshed: {}",
            warning.file().file_name(),
            warning.message()
        );
        eprintln!("factory-worker: {message}");
        self.emit_settled_artifact_warning(job_id, message).await;
    }

    async fn emit_settled_artifact_warning(&self, job_id: &JobId, message: String) {
        if let Err(error) = self
            .store
            .append_job_event(NewJobEvent {
                job_id: job_id.clone(),
                kind: "artifact.warning".to_string(),
                payload: json!({"message": message}),
            })
            .await
        {
            eprintln!("factory-worker: could not persist artifact warning: {error}");
        }
    }
}

fn require_settled_publication(
    attempt_state: AttemptState,
    operation_state: OperationState,
) -> factory_coordinator::Result<()> {
    if attempt_state != AttemptState::Succeeded {
        return Err(CoordinatorError::InvalidInput(
            "artifact publication requires a settled successful attempt".to_string(),
        ));
    }
    if operation_state != OperationState::Succeeded {
        return Err(CoordinatorError::InvalidInput(
            "artifact publication requires a settled successful operation".to_string(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn publication_rejects_unsettled_attempt_or_operation() {
        assert!(
            require_settled_publication(AttemptState::Running, OperationState::Running).is_err()
        );
        assert!(
            require_settled_publication(AttemptState::Succeeded, OperationState::Running).is_err()
        );
        assert!(
            require_settled_publication(AttemptState::Succeeded, OperationState::Succeeded).is_ok()
        );
    }
}
