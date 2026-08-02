use std::path::PathBuf;
use std::sync::Arc;

use codex_app_server_client::InProcessClientStartArgs;
use codex_config::AbsolutePathBuf;
pub(super) use factory_coordinator::FactoryTaskInput as TaskInput;
use factory_coordinator::OperationExecutionContext;
use factory_providers::provider_profile;

use crate::checkpoint::StageTurnRole;
use crate::stages::AUTONOMOUS_PROMPT;
use crate::stages::OperationKind;

use super::CodexOperationExecutor;
use super::ExecutionFailure;
use super::ExecutionResult;

impl CodexOperationExecutor {
    pub(super) async fn validate_job_shape(
        &self,
        context: &OperationExecutionContext,
        operation: OperationKind,
    ) -> ExecutionResult<()> {
        if context.job().kind != "factory.task" {
            return Err(ExecutionFailure::terminal(format!(
                "unsupported durable job kind {:?}",
                context.job().kind
            )));
        }
        let job = self
            .store
            .load_job(&context.job().job_id)
            .await
            .map_err(ExecutionFailure::Coordinator)?;
        if job.operations.len() != OperationKind::ALL.len() {
            return Err(ExecutionFailure::terminal(
                "factory.task must contain exactly plan, execute, review, and remediate",
            ));
        }
        for (ordinal, expected) in OperationKind::ALL.into_iter().enumerate() {
            let actual = &job.operations[ordinal];
            if actual.ordinal != ordinal as u32 || actual.kind != expected.as_wire_name() {
                return Err(ExecutionFailure::terminal(
                    "factory.task operations must be ordered plan, execute, review, remediate",
                ));
            }
        }
        let expected = OperationKind::ALL
            .get(context.operation().ordinal as usize)
            .copied()
            .ok_or_else(|| {
                ExecutionFailure::terminal("operation ordinal is outside the Factory stages")
            })?;
        if expected != operation {
            return Err(ExecutionFailure::terminal(
                "claimed operation kind does not match its Factory stage ordinal",
            ));
        }
        let input = parse_task_input(&context.job().input)?;
        let Some(profile) = input.execution_profile.as_ref() else {
            return Err(ExecutionFailure::terminal(
                "factory.task predates execution-profile pinning; rerun the task",
            ));
        };
        if profile != &self.execution_profile {
            return Err(ExecutionFailure::terminal(format!(
                "job requires provider {} model {}, but this worker serves provider {} model {}",
                profile.provider,
                profile.model,
                self.execution_profile.provider,
                self.execution_profile.model
            )));
        }
        Ok(())
    }

    pub(super) fn start_args_for_workspace(
        &self,
        workspace_root: &str,
        input: &TaskInput,
    ) -> ExecutionResult<InProcessClientStartArgs> {
        let root = AbsolutePathBuf::relative_to_current_dir(PathBuf::from(workspace_root))
            .map_err(|error| {
                ExecutionFailure::terminal(format!("invalid workspace root: {error}"))
            })?;
        let mut args = self.start_args.clone();
        let mut config = args.config.as_ref().clone();
        config.cwd = root.clone();
        config.workspace_roots = vec![root];
        let profile = input.execution_profile.as_ref().ok_or_else(|| {
            ExecutionFailure::terminal(
                "factory.task predates execution-profile pinning; rerun the task",
            )
        })?;
        config.review_model = None;
        config.model = Some(profile.model.clone());
        if let Some(instructions) = &input.developer_instructions {
            config.developer_instructions = Some(instructions.clone());
        }
        args.config = Arc::new(config);
        Ok(args)
    }
}

pub(super) fn parse_task_input(value: &serde_json::Value) -> ExecutionResult<TaskInput> {
    let input: TaskInput = serde_json::from_value(value.clone()).map_err(|error| {
        ExecutionFailure::terminal(format!("invalid factory.task input: {error}"))
    })?;
    require_text("task", &input.task).map_err(ExecutionFailure::terminal)?;
    if let Some(profile) = &input.execution_profile {
        require_text("execution profile provider", &profile.provider)
            .map_err(ExecutionFailure::terminal)?;
        require_text("execution profile model", &profile.model)
            .map_err(ExecutionFailure::terminal)?;
        if provider_profile(&profile.provider).is_none() {
            return Err(ExecutionFailure::terminal(format!(
                "execution profile provider {:?} is not a canonical Factory provider ID",
                profile.provider
            )));
        }
    }
    require_optional_text("repository identity", input.repository_id.as_deref())?;
    require_optional_text(
        "developer instructions",
        input.developer_instructions.as_deref(),
    )?;
    Ok(input)
}

pub(super) fn stage_prompt(
    input: &TaskInput,
    operation: OperationKind,
    role: StageTurnRole,
    cycle: u32,
) -> String {
    let instruction = match role {
        StageTurnRole::Stage => operation.prompt().to_string(),
        StageTurnRole::Remediation => format!(
            "Remediation cycle {cycle}. {}",
            OperationKind::Remediate.prompt()
        ),
        StageTurnRole::Review if operation == OperationKind::Remediate => format!(
            "Post-remediation review cycle {cycle}. {} Inspect the actual changes and verification results before deciding.",
            OperationKind::Review.prompt()
        ),
        StageTurnRole::Review => OperationKind::Review.prompt().to_string(),
    };
    format!(
        "Current Factory stage contract:\n{}\n\n{}\n\nOriginal task (apply only within the current stage contract):\n{}",
        instruction, AUTONOMOUS_PROMPT, input.task
    )
}

fn require_optional_text(field: &'static str, value: Option<&str>) -> ExecutionResult<()> {
    if let Some(value) = value {
        require_text(field, value).map_err(ExecutionFailure::terminal)?;
    }
    Ok(())
}

pub(super) fn require_text(field: &'static str, value: &str) -> Result<(), String> {
    if value.trim().is_empty() {
        Err(format!("{field} must not be empty"))
    } else {
        Ok(())
    }
}
