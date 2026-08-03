use std::collections::HashMap;
use std::io::Write;

use anyhow::Result;
use factory_coordinator::AttemptRecord;
use factory_coordinator::DurableJob;
use factory_coordinator::FactoryTaskInput;
use factory_coordinator::JobEventRecord;
use factory_coordinator::JobId;
use factory_coordinator::JobState;
use factory_coordinator::OperationState;
use factory_coordinator::StageCheckpointRecord;
use factory_providers::provider_profile;
use serde_json::Value;
use serde_json::json;

use crate::artifacts::ArtifactMetadata;
use crate::artifacts::FullResult;
use crate::transcript::compact_event_line;
use crate::transcript::compact_lines;
use crate::transcript::event_detail;

pub fn print_created(job: &DurableJob, json_output: bool) -> Result<()> {
    let stdout = std::io::stdout();
    let mut output = stdout.lock();
    write_created(&mut output, &job.job.job_id, job.job.state, json_output)
}

fn write_created(
    output: &mut impl Write,
    job_id: &JobId,
    state: JobState,
    json_output: bool,
) -> Result<()> {
    if json_output {
        writeln!(
            output,
            "{}",
            serde_json::to_string(&json!({
                "type": "created",
                "jobId": job_id,
                "state": state,
            }))?
        )?;
    } else {
        writeln!(output, "Created durable job {job_id}.")?;
        writeln!(output, "Status: factory status {job_id}")?;
        writeln!(output, "Attach: factory attach {job_id}")?;
        writeln!(output, "Stop: factory stop {job_id}")?;
        writeln!(output, "Preparing managed workspace...")?;
    }
    output.flush()?;
    Ok(())
}

pub fn print_workspace_ready(
    job: &DurableJob,
    workspace_root: &str,
    detach: bool,
    json_output: bool,
) -> Result<()> {
    if json_output {
        println!(
            "{}",
            serde_json::to_string(&json!({
                "type": "workspaceReady",
                "jobId": job.job.job_id,
                "state": job.job.state,
                "workspaceRoot": workspace_root,
                "detached": detach,
            }))?
        );
    } else {
        println!("Workspace ready: {workspace_root}");
        if detach {
            println!("Attach with: factory attach {}", job.job.job_id);
            println!("Apply after success: factory apply {}", job.job.job_id);
            println!("Export after success: factory export {}", job.job.job_id);
        } else {
            println!("Ctrl-C detaches; it does not stop the job.");
        }
    }
    std::io::stdout().flush()?;
    Ok(())
}

pub fn print_detached(job_id: &JobId, json_output: bool) -> Result<()> {
    if json_output {
        println!(
            "{}",
            serde_json::to_string(&json!({
                "type": "detached",
                "jobId": job_id,
            }))?
        );
    } else {
        println!("\nDetached. Job {job_id} is still running.");
        println!("Reattach with: factory attach {job_id}");
    }
    Ok(())
}

pub fn job_snapshot(job: &DurableJob) -> Result<String> {
    serde_json::to_string(&(
        job.job.state,
        job.operations
            .iter()
            .map(|operation| (operation.operation_id.as_str(), operation.state))
            .collect::<Vec<_>>(),
    ))
    .map_err(Into::into)
}

pub fn print_progress(job: &DurableJob) {
    println!(
        "\nJob {} · {}",
        job.job.job_id,
        job_state_label(job.job.state)
    );
    if job.job.kind == "factory.task" {
        match serde_json::from_value::<FactoryTaskInput>(job.job.input.clone()) {
            Ok(input) => match input.execution_profile {
                Some(profile) => {
                    println!("  profile    {} / {}", profile.provider, profile.model);
                    let current_provider = std::env::var("FACTORY_PROVIDER_ADAPTER")
                        .ok()
                        .and_then(|id| provider_profile(&id).map(|profile| profile.id.to_string()));
                    let current_model = std::env::var("FACTORY_MODEL").ok();
                    if !terminal(job.job.state)
                        && (current_provider.as_deref() != Some(profile.provider.as_str())
                            || current_model.as_deref() != Some(profile.model.as_str()))
                    {
                        println!(
                            "  waiting    start a worker configured for {} / {}",
                            profile.provider, profile.model
                        );
                    }
                }
                None => println!(
                    "  waiting    this job predates provider/model pinning; rerun the task"
                ),
            },
            Err(_) => println!("  input      invalid factory.task data"),
        }
    }
    let mut operations = job.operations.iter().collect::<Vec<_>>();
    operations.sort_by_key(|operation| operation.ordinal);
    for operation in operations {
        let label = operation
            .kind
            .strip_prefix("codex.")
            .unwrap_or(&operation.kind);
        if operation.state == OperationState::RetryWait {
            println!(
                "  {label:<10} {} until {}",
                operation_state_label(operation.state),
                operation.next_eligible_at
            );
        } else {
            println!("  {label:<10} {}", operation_state_label(operation.state));
        }
    }
}

pub fn print_progress_compact(job: &DurableJob) {
    let mut operations = job.operations.iter().collect::<Vec<_>>();
    operations.sort_by_key(|operation| operation.ordinal);
    let stages = operations
        .into_iter()
        .map(|operation| {
            let label = operation
                .kind
                .strip_prefix("codex.")
                .unwrap_or(&operation.kind);
            format!("{label}={}", operation_state_label(operation.state))
        })
        .collect::<Vec<_>>()
        .join(" · ");
    if stages.is_empty() {
        println!(
            "Job {} · {}",
            job.job.job_id,
            job_state_label(job.job.state)
        );
    } else {
        println!(
            "Job {} · {} · {stages}",
            job.job.job_id,
            job_state_label(job.job.state)
        );
    }
}

pub fn print_event(event: &JobEventRecord, json_output: bool) -> Result<()> {
    let stdout = std::io::stdout();
    let mut output = stdout.lock();
    write_event(&mut output, event, json_output)
}

pub fn print_compact_event(event: &JobEventRecord) {
    if let Some(line) = compact_event_line(event) {
        println!("  {line}");
    }
}

fn write_event(output: &mut impl Write, event: &JobEventRecord, json_output: bool) -> Result<()> {
    if json_output {
        writeln!(
            output,
            "{}",
            serde_json::to_string(&json!({ "type": "event", "event": event }))?
        )?;
        return Ok(());
    }

    let stage = event
        .payload
        .get("stage")
        .and_then(Value::as_str)
        .or_else(|| event.payload.get("operationKind").and_then(Value::as_str))
        .or_else(|| event.payload.get("operation").and_then(Value::as_str));
    let stage = stage
        .and_then(|value| value.strip_prefix("codex.").or(Some(value)))
        .map(|value| format!(" [{value}]"))
        .unwrap_or_default();
    writeln!(output, "#{} {}{}", event.sequence, event.kind, stage)?;
    if let Some(detail) = event_detail(&event.payload) {
        for line in compact_lines(detail).lines() {
            writeln!(output, "  {line}")?;
        }
    }
    Ok(())
}

pub fn print_final(
    job: &DurableJob,
    stages: &[StageCheckpointRecord],
    attempts: &[AttemptRecord],
    full_result: Option<&FullResult>,
    json_output: bool,
    render_markdown: bool,
) -> Result<()> {
    if json_output {
        println!(
            "{}",
            serde_json::to_string(&json!({
                "type": "result",
                "job": job,
                "stageCheckpoints": stages,
                "attempts": attempts,
                "fullResult": full_result,
                "applyCommand": (job.job.state == JobState::Succeeded)
                    .then(|| format!("factory apply {}", job.job.job_id)),
                "exportCommand": (job.job.state == JobState::Succeeded)
                    .then(|| format!("factory export {}", job.job.job_id)),
            }))?
        );
        return Ok(());
    }

    println!("\nResult: {}", job_state_label(job.job.state));
    if !stages.is_empty() {
        println!("\nStages");
        let mut stages = stages.iter().collect::<Vec<_>>();
        stages.sort_by_key(|stage| stage.ordinal);
        for stage in stages {
            let label = stage
                .operation_kind
                .strip_prefix("codex.")
                .unwrap_or(&stage.operation_kind);
            let turn_id = stage
                .checkpoint
                .payload
                .get("turnId")
                .and_then(Value::as_str);
            let cycle = stage
                .checkpoint
                .payload
                .get("reviewCycle")
                .and_then(Value::as_u64)
                .filter(|cycle| *cycle > 0);
            match (turn_id, cycle) {
                (Some(turn_id), Some(cycle)) => {
                    println!("  {label:<10} succeeded · cycle {cycle} · turn {turn_id}")
                }
                (Some(turn_id), None) => println!("  {label:<10} succeeded · turn {turn_id}"),
                (None, _) => println!("  {label:<10} succeeded"),
            }
        }
    }

    if let Some(result) = full_result {
        print!(
            "\n{}",
            format_full_result(&job.job.job_id, result, render_markdown)
        );
    }

    print_attempt_failures(job, attempts);
    if job.job.state == JobState::Succeeded {
        println!("\nApply:  factory apply {}", job.job.job_id);
        println!("Export: factory export {}", job.job.job_id);
    }
    Ok(())
}

pub fn print_full_result(job_id: &JobId, result: &FullResult, json_output: bool) -> Result<()> {
    if json_output {
        println!(
            "{}",
            serde_json::to_string(&full_result_json(job_id, result))?
        );
        return Ok(());
    }
    print!("{}", format_full_result(job_id, result, true));
    Ok(())
}

fn full_result_json(job_id: &JobId, result: &FullResult) -> Value {
    json!({
        "type": "result",
        "jobId": job_id,
        "fullResult": result,
    })
}

pub fn print_artifacts(
    job_id: &JobId,
    artifacts: &ArtifactMetadata,
    json_output: bool,
) -> Result<()> {
    if json_output {
        println!(
            "{}",
            serde_json::to_string(&json!({
                "type": "artifacts",
                "jobId": job_id,
                "artifacts": artifacts,
            }))?
        );
        return Ok(());
    }
    print!("{}", format_artifacts(artifacts));
    Ok(())
}

fn format_artifacts(artifacts: &ArtifactMetadata) -> String {
    let mut output = String::from("Artifacts\n");
    for file in &artifacts.files {
        let state = if file.local_available {
            "available locally"
        } else if file.available {
            "available via coordinator"
        } else {
            "not materialized"
        };
        output.push_str(&format!("  {:<14} {state}\n", file.name));
    }
    if let Some(directory) = &artifacts.local_directory {
        output.push_str(&format!("\nDirectory: {directory}\n"));
    }
    output.push_str(&format!("\n{}\n", artifacts.message));
    output
}

fn format_full_result(job_id: &JobId, result: &FullResult, render_markdown: bool) -> String {
    let mut output = String::new();
    if render_markdown {
        output.push_str(&result.markdown);
        if !output.ends_with('\n') {
            output.push('\n');
        }
    }
    if let Some(path) = &result.artifacts.result_path {
        output.push_str(&format!("\nFull result: {path}\n"));
    }
    output.push_str(&format!("\n{}\n", result.artifacts.message));
    output.push_str(&format!("Inspect: factory result {job_id}\n"));
    output
}

pub fn print_status_json(
    job: &DurableJob,
    stages: &[StageCheckpointRecord],
    attempts: &[AttemptRecord],
    full_result: Option<&FullResult>,
) -> Result<()> {
    println!(
        "{}",
        serde_json::to_string(&json!({
            "job": job,
            "stageCheckpoints": stages,
            "attempts": attempts,
            "fullResult": full_result,
        }))?
    );
    Ok(())
}

pub fn print_stopped(job: &DurableJob, json_output: bool) -> Result<()> {
    if json_output {
        println!("{}", serde_json::to_string(&json!({ "job": job }))?);
    } else {
        let stdout = std::io::stdout();
        let mut output = stdout.lock();
        write_stop_confirmation(&mut output, &job.job.job_id, job.job.state)?;
        drop(output);
        print_progress(job);
    }
    Ok(())
}

fn write_stop_confirmation(output: &mut impl Write, job_id: &JobId, state: JobState) -> Result<()> {
    if state == JobState::Cancelling {
        writeln!(output, "Cancellation requested for job {job_id}.")?;
        writeln!(output, "Status: factory status {job_id}")?;
        writeln!(output, "Attach: factory attach {job_id}")?;
    } else if state == JobState::Cancelled {
        writeln!(output, "Stopped job {job_id}.")?;
    } else {
        writeln!(output, "Job {job_id} is {}.", job_state_label(state))?;
    }
    output.flush()?;
    Ok(())
}

fn print_attempt_failures(job: &DurableJob, attempts: &[AttemptRecord]) {
    let operation_labels = job
        .operations
        .iter()
        .map(|operation| {
            (
                operation.operation_id.as_str(),
                operation
                    .kind
                    .strip_prefix("codex.")
                    .unwrap_or(&operation.kind),
            )
        })
        .collect::<HashMap<_, _>>();
    let failures = attempts
        .iter()
        .filter(|attempt| attempt.failure.is_some())
        .collect::<Vec<_>>();
    if failures.is_empty() {
        return;
    }
    println!("\nAttempt failures");
    for attempt in failures {
        let label = operation_labels
            .get(attempt.operation_id.as_str())
            .copied()
            .unwrap_or("operation");
        let failure = attempt.failure.as_ref().expect("filtered above");
        let detail = event_detail(failure).unwrap_or_else(|| failure.to_string());
        println!("  {label} attempt {}: {detail}", attempt.attempt_number);
    }
}

pub fn job_state_exit_code(state: JobState) -> i32 {
    match state {
        JobState::Failed => 1,
        JobState::Cancelled => 2,
        JobState::Queued | JobState::Running | JobState::Cancelling | JobState::Succeeded => 0,
    }
}

pub fn terminal(state: JobState) -> bool {
    matches!(
        state,
        JobState::Succeeded | JobState::Failed | JobState::Cancelled
    )
}

fn job_state_label(state: JobState) -> &'static str {
    match state {
        JobState::Queued => "queued",
        JobState::Running => "running",
        JobState::Cancelling => "cancelling",
        JobState::Succeeded => "succeeded",
        JobState::Failed => "failed",
        JobState::Cancelled => "cancelled",
    }
}

fn operation_state_label(state: OperationState) -> &'static str {
    match state {
        OperationState::Ready => "ready",
        OperationState::Running => "running",
        OperationState::RetryWait => "retry wait",
        OperationState::Succeeded => "succeeded",
        OperationState::Failed => "failed",
        OperationState::Cancelled => "cancelled",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(kind: &str, payload: Value) -> JobEventRecord {
        serde_json::from_value(json!({
            "sequence": 17,
            "jobId": "job-output-test",
            "operationId": null,
            "attemptId": null,
            "kind": kind,
            "payload": payload,
            "createdAt": "2026-08-02T00:00:00Z",
        }))
        .expect("job event fixture")
    }

    fn full_result() -> FullResult {
        FullResult {
            markdown: "# Result\n\n## Review\n\nApproved.\n\n- Finding one\n- Finding two\n"
                .to_string(),
            artifacts: ArtifactMetadata {
                ownership: "local",
                files: vec![crate::artifacts::ArtifactFileMetadata {
                    name: "result.md",
                    available: true,
                    local_available: true,
                }],
                local_directory: Some(".factory/jobs/job-final/".to_string()),
                result_path: Some(".factory/jobs/job-final/result.md".to_string()),
                message: "Artifacts are available in the current workspace.".to_string(),
            },
        }
    }

    #[test]
    fn created_job_output_exposes_all_recovery_commands_before_workspace_setup() {
        let job_id = JobId::new("job-visible-before-clone");
        let mut output = Vec::new();

        write_created(&mut output, &job_id, JobState::Queued, false).unwrap();

        let output = String::from_utf8(output).unwrap();
        assert!(output.starts_with("Created durable job job-visible-before-clone.\n"));
        assert!(output.contains("factory status job-visible-before-clone"));
        assert!(output.contains("factory attach job-visible-before-clone"));
        assert!(output.contains("factory stop job-visible-before-clone"));
        assert!(output.ends_with("Preparing managed workspace...\n"));
    }

    #[test]
    fn stop_output_distinguishes_requested_from_terminal_cancellation() {
        let job_id = JobId::new("job-being-cancelled");
        let mut requested = Vec::new();
        write_stop_confirmation(&mut requested, &job_id, JobState::Cancelling).unwrap();
        let requested = String::from_utf8(requested).unwrap();
        assert!(requested.starts_with("Cancellation requested for job job-being-cancelled.\n"));
        assert!(requested.contains("factory status job-being-cancelled"));
        assert!(requested.contains("factory attach job-being-cancelled"));
        assert!(!requested.contains("Stopped job"));

        let mut stopped = Vec::new();
        write_stop_confirmation(&mut stopped, &job_id, JobState::Cancelled).unwrap();
        assert_eq!(
            String::from_utf8(stopped).unwrap(),
            "Stopped job job-being-cancelled.\n"
        );
    }

    #[test]
    fn human_event_output_renders_reasoning_summary_parts_as_progress() {
        let event = event(
            "reasoning.summary.completed",
            json!({
                "operationKind": "codex.review",
                "itemId": "reasoning-1",
                "summary": [
                    "Inspecting the event mapping.",
                    "Verifying attached CLI output.",
                ],
            }),
        );
        let mut output = Vec::new();

        write_event(&mut output, &event, false).unwrap();

        assert_eq!(
            String::from_utf8(output).unwrap(),
            "#17 reasoning.summary.completed [review]\n  Inspecting the event mapping.\n  \n  Verifying attached CLI output.\n"
        );
    }

    #[test]
    fn json_event_output_preserves_structured_agent_content() {
        let event = event(
            "agent.message.completed",
            json!({
                "itemId": "message-1",
                "phase": "commentary",
                "text": "Running the focused checks.",
            }),
        );
        let mut output = Vec::new();

        write_event(&mut output, &event, true).unwrap();

        let output: Value = serde_json::from_slice(&output).expect("JSON event output");
        assert_eq!(output["type"], "event");
        assert_eq!(output["event"]["kind"], "agent.message.completed");
        assert_eq!(
            output["event"]["payload"],
            json!({
                "itemId": "message-1",
                "phase": "commentary",
                "text": "Running the focused checks.",
            })
        );
    }

    #[test]
    fn human_event_output_renders_tool_path_web_and_plan_details() {
        let cases = [
            (
                "tool.completed",
                json!({
                    "itemId": "subagent-1",
                    "type": "subagent",
                    "tool": "spawnAgent",
                    "status": "completed",
                }),
                "#17 tool.completed\n  subagent: spawnAgent\n",
            ),
            (
                "file.completed",
                json!({
                    "itemId": "file-1",
                    "paths": ["runtime/src/events.rs", "cli/src/output.rs"],
                    "status": "completed",
                }),
                "#17 file.completed\n  runtime/src/events.rs, cli/src/output.rs (completed)\n",
            ),
            (
                "tool.completed",
                json!({ "itemId": "web-1", "type": "webSearch" }),
                "#17 tool.completed\n  web search\n",
            ),
            (
                "turn.plan",
                json!({
                    "summary": "Implementation plan",
                    "steps": [
                        { "step": "Inspect event mapping", "status": "completed" },
                        { "step": "Run focused tests", "status": "inProgress" },
                    ],
                }),
                "#17 turn.plan\n  Implementation plan\n  [completed] Inspect event mapping\n  [inProgress] Run focused tests\n",
            ),
        ];

        for (kind, payload, expected) in cases {
            let mut output = Vec::new();
            write_event(&mut output, &event(kind, payload), false).unwrap();
            assert_eq!(String::from_utf8(output).unwrap(), expected);
        }
    }

    #[test]
    fn full_result_keeps_multiline_review_findings_and_local_path() {
        let rendered = format_full_result(&JobId::new("job-final"), &full_result(), true);
        assert!(rendered.contains("## Review\n\nApproved."));
        assert!(rendered.contains("- Finding one\n- Finding two"));
        assert!(rendered.contains("Full result: .factory/jobs/job-final/result.md"));
        assert!(rendered.contains("Inspect: factory result job-final"));
    }

    #[test]
    fn projection_warning_remains_visible_when_local_path_exists() {
        let mut result = full_result();
        result.artifacts.message =
            "1 workspace artifact file could not be refreshed; durable event output is current."
                .to_string();
        let rendered = format_full_result(&JobId::new("job-final"), &result, true);
        assert!(rendered.contains("Full result: .factory/jobs/job-final/result.md"));
        assert!(rendered.contains("could not be refreshed"));
        let inventory = format_artifacts(&result.artifacts);
        assert!(inventory.contains("Directory: .factory/jobs/job-final/"));
        assert!(inventory.contains("could not be refreshed"));
    }

    #[test]
    fn full_result_replaces_compact_preview_and_verbose_does_not_duplicate_body() {
        let compact = format_full_result(&JobId::new("job-final"), &full_result(), true);
        assert!(compact.contains("Finding two"));
        assert!(!compact.contains('…'));

        let verbose_handoff = format_full_result(&JobId::new("job-final"), &full_result(), false);
        assert!(!verbose_handoff.contains("## Review"));
        assert_eq!(
            verbose_handoff.matches("factory result job-final").count(),
            1
        );
    }

    #[test]
    fn json_result_contains_full_markdown_and_artifact_metadata() {
        let payload = full_result_json(&JobId::new("job-final"), &full_result());
        assert_eq!(payload["fullResult"]["markdown"], full_result().markdown);
        assert!(payload["fullResult"].get("source").is_none());
        assert_eq!(
            payload["fullResult"]["artifacts"]["resultPath"],
            ".factory/jobs/job-final/result.md"
        );
        assert!(payload.get("artifacts").is_none());
    }

    #[test]
    fn succeeded_exit_zero_has_a_human_final_handoff() {
        assert_eq!(job_state_exit_code(JobState::Succeeded), 0);
        let rendered = format_full_result(&JobId::new("job-final"), &full_result(), true);
        assert!(rendered.starts_with("# Result"));
        assert!(rendered.ends_with("Inspect: factory result job-final\n"));
    }
}
