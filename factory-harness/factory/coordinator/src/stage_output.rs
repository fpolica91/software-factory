use crate::AttemptId;
use crate::DurableJob;
use crate::JobEventRecord;
use crate::OperationId;
use crate::OperationState;
use std::collections::HashMap;
use std::error::Error;
use std::fmt;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum StageOutputTurnRole {
    Stage,
    Remediation,
    Review,
}

impl StageOutputTurnRole {
    fn heading(self) -> &'static str {
        match self {
            Self::Stage => "Stage",
            Self::Remediation => "Remediation",
            Self::Review => "Review",
        }
    }
}

/// One exact model turn that runtime validation accepted.
#[derive(Clone, Debug, Eq, PartialEq)]
struct ValidatedAgentTurn {
    attempt_id: AttemptId,
    turn_id: String,
    role: StageOutputTurnRole,
    review_cycle: u32,
}

/// Exact validated turns that constitute one successful Factory stage.
#[derive(Clone, Debug, Eq, PartialEq)]
struct ValidatedStageOutput {
    operation_id: OperationId,
    stage: String,
    turns: Vec<ValidatedAgentTurn>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CompletedStageOutput {
    pub operation_id: OperationId,
    pub stage: String,
    pub markdown: String,
    pub findings: Option<serde_json::Value>,
}

const APPROVED_NO_REMEDIATION: &str =
    "No remediation was required because review approved the result.";

#[derive(Debug)]
pub struct StageOutputReductionError {
    message: String,
}

impl fmt::Display for StageOutputReductionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl Error for StageOutputReductionError {}

#[derive(Debug, Default)]
struct AgentMessage {
    streamed: String,
    complete: String,
    phase: Option<String>,
    completed_sequence: Option<u64>,
}

/// Event-to-Markdown reducer shared by runtime materialization and
/// CLI reads. Callers must provide the exact attempt and turn identities that
/// runtime validation accepted; unrelated, failed, and replaced turns are
/// never inferred from operation recency.
fn reduce_completed_stage_outputs(
    validated: &[ValidatedStageOutput],
    events: &[JobEventRecord],
) -> Result<Vec<CompletedStageOutput>, StageOutputReductionError> {
    validated
        .iter()
        .map(|stage| {
            if stage.turns.is_empty() {
                return Err(reduction_error(format!(
                    "validated {} stage has no agent turns",
                    stage.stage
                )));
            }
            let mut turns = Vec::with_capacity(stage.turns.len());
            for turn in &stage.turns {
                let markdown = reduce_turn(events, &stage.operation_id, turn).ok_or_else(|| {
                    reduction_error(format!(
                        "validated {} turn {} from attempt {} has no completed agent output",
                        stage.stage, turn.turn_id, turn.attempt_id
                    ))
                })?;
                turns.push((turn, markdown));
            }
            let markdown = if stage.stage == "remediate" {
                render_remediation_transcript(&turns)
            } else if turns.len() == 1 {
                turns.remove(0).1
            } else {
                return Err(reduction_error(format!(
                    "single-turn {} stage has {} validated turns",
                    stage.stage,
                    turns.len()
                )));
            };
            Ok(CompletedStageOutput {
                operation_id: stage.operation_id.clone(),
                stage: stage.stage.clone(),
                markdown,
                findings: None,
            })
        })
        .collect()
}

/// Reconstructs currently settled output from durable completion markers.
/// Accepted turn IDs may refer to output from an older attempt when a crash
/// occurred after validation but before settlement.
pub fn reduce_settled_job_outputs(
    job: &DurableJob,
    events: &[JobEventRecord],
) -> Result<Vec<CompletedStageOutput>, StageOutputReductionError> {
    let mut outputs = Vec::new();
    let mut operations = job
        .operations
        .iter()
        .filter(|operation| operation.state == OperationState::Succeeded)
        .collect::<Vec<_>>();
    operations.sort_by_key(|operation| operation.ordinal);
    for operation in operations {
        let stage = operation
            .kind
            .strip_prefix("codex.")
            .unwrap_or(&operation.kind)
            .to_string();
        let completion = events
            .iter()
            .filter(|event| {
                event.operation_id.as_ref() == Some(&operation.operation_id)
                    && event.kind == "stage.completed"
            })
            .max_by_key(|event| event.sequence)
            .ok_or_else(|| {
                reduction_error(format!("settled {stage} stage has no completion event"))
            })?;
        let findings = settled_findings(completion)?;

        let markers = if stage == "remediate" {
            remediation_markers(events, &operation.operation_id, completion)?
        } else {
            vec![completion]
        };
        let mut turns = markers
            .iter()
            .map(|marker| {
                accepted_turn(events, marker, &operation.operation_id, completion.sequence)
            })
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .flatten()
            .collect::<Vec<_>>();
        turns.sort_by_key(|turn| (turn.review_cycle, role_rank(turn.role)));

        let approved_without_remediation = stage == "remediate"
            && turns.is_empty()
            && completion
                .payload
                .get("role")
                .and_then(|value| value.as_str())
                .and_then(parse_turn_role)
                == Some(StageOutputTurnRole::Review);
        if approved_without_remediation {
            outputs.push(CompletedStageOutput {
                operation_id: operation.operation_id.clone(),
                stage: stage.clone(),
                markdown: APPROVED_NO_REMEDIATION.to_string(),
                findings,
            });
            continue;
        }

        if turns.len() != markers.len() {
            return Err(reduction_error(format!(
                "settled {stage} stage has an accepted turn without completed agent output"
            )));
        }

        let mut reduced = reduce_completed_stage_outputs(
            &[ValidatedStageOutput {
                operation_id: operation.operation_id.clone(),
                stage: stage.clone(),
                turns: turns.clone(),
            }],
            events,
        )?;
        for output in &mut reduced {
            output.findings = findings.clone();
        }
        outputs.append(&mut reduced);
    }
    Ok(outputs)
}

pub fn render_job_result(outputs: &[CompletedStageOutput]) -> String {
    let mut markdown = String::from("# Result\n");
    for output in outputs {
        markdown.push_str("\n## ");
        let mut characters = output.stage.chars();
        if let Some(first) = characters.next() {
            markdown.extend(first.to_uppercase());
            markdown.extend(characters);
        }
        markdown.push_str("\n\n");
        markdown.push_str(&output.markdown);
        if !markdown.ends_with('\n') {
            markdown.push('\n');
        }
    }
    markdown
}

pub(crate) fn render_job_findings(
    outputs: &[CompletedStageOutput],
) -> Result<Option<Vec<u8>>, StageOutputReductionError> {
    outputs
        .last()
        .and_then(|output| output.findings.as_ref())
        .map(serde_json::to_vec_pretty)
        .transpose()
        .map_err(|error| reduction_error(format!("serialize settled findings: {error}")))
}

fn settled_findings(
    completion: &JobEventRecord,
) -> Result<Option<serde_json::Value>, StageOutputReductionError> {
    let Some(findings) = completion.payload.get("findings").cloned() else {
        return Ok(None);
    };
    if !findings.is_array() {
        return Err(reduction_error(
            "settled stage findings payload is not an array".to_string(),
        ));
    }
    Ok(Some(findings))
}

fn accepted_turn(
    events: &[JobEventRecord],
    marker: &JobEventRecord,
    operation_id: &OperationId,
    upper_sequence: u64,
) -> Result<Option<ValidatedAgentTurn>, StageOutputReductionError> {
    let turn_id = marker
        .payload
        .get("turnId")
        .and_then(|value| value.as_str())
        .ok_or_else(|| reduction_error("validated turn marker has no turnId".to_string()))?;
    let role = marker
        .payload
        .get("role")
        .and_then(|value| value.as_str())
        .and_then(parse_turn_role)
        .ok_or_else(|| reduction_error("validated turn marker has no valid role".to_string()))?;
    let review_cycle = marker
        .payload
        .get("reviewCycle")
        .and_then(|value| value.as_u64())
        .and_then(|value| u32::try_from(value).ok())
        .ok_or_else(|| reduction_error("validated turn marker has no valid cycle".to_string()))?;
    let attempt_id = events
        .iter()
        .filter(|event| {
            event.sequence <= upper_sequence
                && event.operation_id.as_ref() == Some(operation_id)
                && event.kind == "agent.message.completed"
                && event.payload.get("turnId").and_then(|value| value.as_str()) == Some(turn_id)
        })
        .max_by_key(|event| event.sequence)
        .and_then(|event| event.attempt_id.clone());
    Ok(attempt_id.map(|attempt_id| ValidatedAgentTurn {
        attempt_id,
        turn_id: turn_id.to_string(),
        role,
        review_cycle,
    }))
}

fn remediation_markers<'a>(
    events: &'a [JobEventRecord],
    operation_id: &OperationId,
    completion: &'a JobEventRecord,
) -> Result<Vec<&'a JobEventRecord>, StageOutputReductionError> {
    let terminal_role = completion
        .payload
        .get("role")
        .and_then(|value| value.as_str())
        .and_then(parse_turn_role)
        .ok_or_else(|| reduction_error("completion marker has no valid role".to_string()))?;
    let terminal_cycle = completion
        .payload
        .get("reviewCycle")
        .and_then(|value| value.as_u64())
        .and_then(|value| u32::try_from(value).ok())
        .ok_or_else(|| reduction_error("completion marker has no valid cycle".to_string()))?;
    let mut latest = HashMap::<(u32, StageOutputTurnRole), &JobEventRecord>::new();
    for marker in events.iter().filter(|event| {
        event.sequence <= completion.sequence
            && event.operation_id.as_ref() == Some(operation_id)
            && event.kind == "review_cycle.turn_completed"
    }) {
        let Some(role) = marker
            .payload
            .get("role")
            .and_then(|value| value.as_str())
            .and_then(parse_turn_role)
        else {
            continue;
        };
        let Some(cycle) = marker
            .payload
            .get("reviewCycle")
            .and_then(|value| value.as_u64())
            .and_then(|value| u32::try_from(value).ok())
        else {
            continue;
        };
        if cycle > terminal_cycle
            || (cycle == terminal_cycle && role_rank(role) > role_rank(terminal_role))
        {
            continue;
        }
        let slot = latest.entry((cycle, role)).or_insert(marker);
        if marker.sequence > slot.sequence {
            *slot = marker;
        }
    }
    latest.insert((terminal_cycle, terminal_role), completion);
    let mut markers = latest.into_values().collect::<Vec<_>>();
    markers.sort_by_key(|marker| {
        let role = marker
            .payload
            .get("role")
            .and_then(|value| value.as_str())
            .and_then(parse_turn_role)
            .expect("validated marker role");
        let cycle = marker
            .payload
            .get("reviewCycle")
            .and_then(|value| value.as_u64())
            .expect("validated marker cycle");
        (cycle, role_rank(role))
    });
    Ok(markers)
}

const fn role_rank(role: StageOutputTurnRole) -> u8 {
    match role {
        StageOutputTurnRole::Remediation => 0,
        StageOutputTurnRole::Review => 1,
        StageOutputTurnRole::Stage => 2,
    }
}

fn parse_turn_role(value: &str) -> Option<StageOutputTurnRole> {
    match value {
        "stage" => Some(StageOutputTurnRole::Stage),
        "remediation" => Some(StageOutputTurnRole::Remediation),
        "review" => Some(StageOutputTurnRole::Review),
        _ => None,
    }
}

fn reduce_turn(
    events: &[JobEventRecord],
    operation_id: &OperationId,
    validated: &ValidatedAgentTurn,
) -> Option<String> {
    let mut messages = HashMap::<String, AgentMessage>::new();
    for event in events.iter().filter(|event| {
        event.operation_id.as_ref() == Some(operation_id)
            && event.attempt_id.as_ref() == Some(&validated.attempt_id)
            && event.payload.get("turnId").and_then(|value| value.as_str())
                == Some(validated.turn_id.as_str())
    }) {
        if !matches!(
            event.kind.as_str(),
            "agent.message" | "agent.message.started" | "agent.message.completed"
        ) {
            continue;
        }
        let Some(item_id) = event.payload.get("itemId").and_then(|value| value.as_str()) else {
            continue;
        };
        let message = messages.entry(item_id.to_string()).or_default();
        if let Some(phase) = event.payload.get("phase").and_then(|value| value.as_str()) {
            message.phase = Some(phase.to_string());
        }
        if event.kind == "agent.message" {
            if let Some(text) = event.payload.get("text").and_then(|value| value.as_str()) {
                message.streamed.push_str(text);
            }
        } else if let Some(text) = event.payload.get("text").and_then(|value| value.as_str()) {
            message.complete = text.to_string();
        }
        if event.kind == "agent.message.completed" {
            message.completed_sequence = Some(event.sequence);
        }
    }

    let choose = |allow_commentary: bool| {
        messages
            .values()
            .filter(|message| message.completed_sequence.is_some())
            .filter(|message| allow_commentary || message.phase.as_deref() != Some("commentary"))
            .filter_map(|message| {
                let markdown = if message.complete.is_empty() {
                    &message.streamed
                } else {
                    &message.complete
                };
                (!markdown.trim().is_empty()).then_some((
                    message.completed_sequence.expect("filtered completion"),
                    markdown.clone(),
                ))
            })
            .max_by_key(|(sequence, _)| *sequence)
            .map(|(_, markdown)| markdown)
    };
    choose(false).or_else(|| choose(true))
}

fn render_remediation_transcript(turns: &[(&ValidatedAgentTurn, String)]) -> String {
    let mut markdown = String::new();
    for (index, (turn, output)) in turns.iter().enumerate() {
        if index > 0 {
            markdown.push('\n');
        }
        markdown.push_str(&format!(
            "### Cycle {}: {}\n\nAttempt: `{}`  \nTurn: `{}`\n\n{}",
            turn.review_cycle,
            turn.role.heading(),
            turn.attempt_id,
            turn.turn_id,
            output
        ));
        if !markdown.ends_with('\n') {
            markdown.push('\n');
        }
    }
    markdown
}

fn reduction_error(message: String) -> StageOutputReductionError {
    StageOutputReductionError { message }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::JobId;
    use chrono::Utc;
    use serde_json::json;

    fn event(
        sequence: u64,
        operation_id: &OperationId,
        attempt_id: &AttemptId,
        turn_id: &str,
        kind: &str,
        payload: serde_json::Value,
    ) -> JobEventRecord {
        let mut payload = payload.as_object().cloned().unwrap();
        payload.insert("turnId".to_string(), json!(turn_id));
        JobEventRecord {
            sequence,
            job_id: JobId::new("job-one"),
            operation_id: Some(operation_id.clone()),
            attempt_id: Some(attempt_id.clone()),
            kind: kind.to_string(),
            payload: payload.into(),
            created_at: Utc::now(),
        }
    }

    fn turn(
        attempt: &str,
        turn: &str,
        role: StageOutputTurnRole,
        cycle: u32,
    ) -> ValidatedAgentTurn {
        ValidatedAgentTurn {
            attempt_id: AttemptId::new(attempt),
            turn_id: turn.to_string(),
            role,
            review_cycle: cycle,
        }
    }

    fn settled_job(operation_id: &str, kind: &str) -> DurableJob {
        serde_json::from_value(json!({
            "job": {
                "jobId": "job-one",
                "kind": "factory.task",
                "input": {"task": "review", "repositoryId": "local:test"},
                "state": "succeeded",
                "createdAt": "2026-08-02T00:00:00Z",
                "updatedAt": "2026-08-02T00:00:00Z"
            },
            "operations": [{
                "operationId": operation_id,
                "jobId": "job-one",
                "ordinal": 0,
                "kind": kind,
                "input": {},
                "state": "succeeded",
                "maxAttempts": 3,
                "nextEligibleAt": "2026-08-02T00:00:00Z",
                "createdAt": "2026-08-02T00:00:00Z",
                "updatedAt": "2026-08-02T00:00:00Z"
            }]
        }))
        .unwrap()
    }

    #[test]
    fn reconstructs_multiline_without_duplicating_full_completion() {
        let operation_id = OperationId::new("execute");
        let attempt_id = AttemptId::new("attempt-good");
        let events = vec![
            event(
                1,
                &operation_id,
                &attempt_id,
                "turn-good",
                "agent.message",
                json!({"itemId":"answer","text":"First\n\n"}),
            ),
            event(
                2,
                &operation_id,
                &attempt_id,
                "turn-good",
                "agent.message",
                json!({"itemId":"answer","text":"Second"}),
            ),
            event(
                3,
                &operation_id,
                &attempt_id,
                "turn-good",
                "agent.message.completed",
                json!({"itemId":"answer","phase":"final_answer","text":"First\n\nSecond"}),
            ),
        ];
        let output = reduce_completed_stage_outputs(
            &[ValidatedStageOutput {
                operation_id,
                stage: "execute".to_string(),
                turns: vec![turn(
                    "attempt-good",
                    "turn-good",
                    StageOutputTurnRole::Stage,
                    0,
                )],
            }],
            &events,
        )
        .unwrap();
        assert_eq!(output[0].markdown, "First\n\nSecond");
    }

    #[test]
    fn settled_job_fallback_uses_exact_completion_selector() {
        let operation_id = OperationId::new("review-op");
        let attempt_id = AttemptId::new("review-attempt");
        let events = vec![
            event(
                1,
                &operation_id,
                &attempt_id,
                "review-turn",
                "agent.message.completed",
                json!({
                    "itemId": "review-answer",
                    "phase": "final_answer",
                    "text": "Approved.\n\n- Finding remains visible"
                }),
            ),
            event(
                2,
                &operation_id,
                &attempt_id,
                "review-turn",
                "stage.completed",
                json!({"role": "stage", "reviewCycle": 0}),
            ),
        ];

        let outputs =
            reduce_settled_job_outputs(&settled_job("review-op", "codex.review"), &events).unwrap();
        assert_eq!(outputs.len(), 1);
        assert_eq!(
            outputs[0].markdown,
            "Approved.\n\n- Finding remains visible"
        );
        assert_eq!(
            render_job_result(&outputs),
            "# Result\n\n## Review\n\nApproved.\n\n- Finding remains visible\n"
        );
        assert_eq!(outputs[0].findings, None);
        assert_eq!(render_job_findings(&outputs).unwrap(), None);
    }

    #[test]
    fn explicit_empty_findings_remain_known_and_exact() {
        let operation_id = OperationId::new("review-op");
        let attempt_id = AttemptId::new("review-attempt");
        let events = vec![
            event(
                1,
                &operation_id,
                &attempt_id,
                "review-turn",
                "agent.message.completed",
                json!({"itemId":"answer","phase":"final_answer","text":"Approved"}),
            ),
            event(
                2,
                &operation_id,
                &attempt_id,
                "review-turn",
                "stage.completed",
                json!({"role":"stage","reviewCycle":0,"findings":[]}),
            ),
        ];

        let outputs =
            reduce_settled_job_outputs(&settled_job("review-op", "codex.review"), &events).unwrap();
        assert_eq!(outputs[0].findings, Some(json!([])));
        let rendered = render_job_findings(&outputs).unwrap().unwrap();
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&rendered).unwrap(),
            json!([])
        );
    }

    #[test]
    fn settled_findings_ignore_later_unaccepted_state() {
        let operation_id = OperationId::new("review-op");
        let attempt_id = AttemptId::new("review-attempt");
        let accepted = json!([{"id":"accepted","title":"Accepted finding"}]);
        let events = vec![
            event(
                1,
                &operation_id,
                &attempt_id,
                "review-turn",
                "agent.message.completed",
                json!({"itemId":"answer","phase":"final_answer","text":"Review output"}),
            ),
            event(
                2,
                &operation_id,
                &attempt_id,
                "review-turn",
                "stage.completed",
                json!({"role":"stage","reviewCycle":0,"findings":accepted}),
            ),
            event(
                3,
                &operation_id,
                &attempt_id,
                "unaccepted-turn",
                "review.state.changed",
                json!({"findings":[{"id":"later","title":"Unaccepted finding"}]}),
            ),
        ];
        let outputs =
            reduce_settled_job_outputs(&settled_job("review-op", "codex.review"), &events).unwrap();
        assert_eq!(outputs[0].findings, Some(accepted.clone()));
        let findings: serde_json::Value =
            serde_json::from_slice(&render_job_findings(&outputs).unwrap().unwrap()).unwrap();
        assert_eq!(findings, accepted);
    }

    #[test]
    fn crash_recovery_follows_accepted_turn_id_across_attempts() {
        let operation_id = OperationId::new("execute-op");
        let validated_attempt = AttemptId::new("attempt-before-crash");
        let settlement_attempt = AttemptId::new("attempt-after-recovery");
        let events = vec![
            event(
                1,
                &operation_id,
                &validated_attempt,
                "accepted-turn",
                "agent.message.completed",
                json!({"itemId":"answer","phase":"final_answer","text":"Accepted before crash"}),
            ),
            event(
                2,
                &operation_id,
                &settlement_attempt,
                "accepted-turn",
                "stage.completed",
                json!({"role":"stage","reviewCycle":0}),
            ),
        ];

        let outputs =
            reduce_settled_job_outputs(&settled_job("execute-op", "codex.execute"), &events)
                .unwrap();
        assert_eq!(outputs[0].markdown, "Accepted before crash");
    }

    #[test]
    fn settled_approved_remediation_without_own_turn_is_factory_output() {
        let operation_id = OperationId::new("remediate-op");
        let attempt_id = AttemptId::new("remediate-attempt");
        let events = vec![event(
            1,
            &operation_id,
            &attempt_id,
            "prior-review-turn",
            "stage.completed",
            json!({"role": "review", "reviewCycle": 1}),
        )];

        let outputs =
            reduce_settled_job_outputs(&settled_job("remediate-op", "codex.remediate"), &events)
                .unwrap();
        assert_eq!(outputs[0].markdown, APPROVED_NO_REMEDIATION);
    }

    #[test]
    fn retry_selector_excludes_failed_and_replaced_turns() {
        let operation_id = OperationId::new("review");
        let failed = AttemptId::new("attempt-failed");
        let good = AttemptId::new("attempt-good");
        let events = vec![
            event(
                1,
                &operation_id,
                &failed,
                "turn-failed",
                "agent.message.completed",
                json!({"itemId":"bad","phase":"final_answer","text":"Wrong output"}),
            ),
            event(
                2,
                &operation_id,
                &good,
                "turn-good",
                "agent.message.completed",
                json!({"itemId":"good","phase":"final_answer","text":"Approved output"}),
            ),
        ];
        let output = reduce_completed_stage_outputs(
            &[ValidatedStageOutput {
                operation_id,
                stage: "review".to_string(),
                turns: vec![turn(
                    "attempt-good",
                    "turn-good",
                    StageOutputTurnRole::Review,
                    0,
                )],
            }],
            &events,
        )
        .unwrap();
        assert_eq!(output[0].markdown, "Approved output");
    }

    #[test]
    fn settled_remediation_excludes_cycle_markers_from_failed_attempt() {
        let operation_id = OperationId::new("remediate-op");
        let failed = AttemptId::new("attempt-failed");
        let succeeded = AttemptId::new("attempt-succeeded");
        let events = vec![
            event(
                1,
                &operation_id,
                &failed,
                "failed-fix",
                "agent.message.completed",
                json!({"itemId":"failed","phase":"final_answer","text":"Failed attempt output"}),
            ),
            event(
                2,
                &operation_id,
                &failed,
                "failed-fix",
                "review_cycle.turn_completed",
                json!({"role":"remediation","reviewCycle":1}),
            ),
            event(
                3,
                &operation_id,
                &succeeded,
                "good-fix",
                "agent.message.completed",
                json!({"itemId":"good","phase":"final_answer","text":"Successful remediation"}),
            ),
            event(
                4,
                &operation_id,
                &succeeded,
                "good-fix",
                "review_cycle.turn_completed",
                json!({"role":"remediation","reviewCycle":1}),
            ),
            event(
                5,
                &operation_id,
                &succeeded,
                "good-review",
                "agent.message.completed",
                json!({"itemId":"review","phase":"final_answer","text":"Approved"}),
            ),
            event(
                6,
                &operation_id,
                &succeeded,
                "good-review",
                "stage.completed",
                json!({"role":"review","reviewCycle":1}),
            ),
        ];

        let outputs =
            reduce_settled_job_outputs(&settled_job("remediate-op", "codex.remediate"), &events)
                .unwrap();
        assert!(outputs[0].markdown.contains("Successful remediation"));
        assert!(outputs[0].markdown.contains("Approved"));
        assert!(!outputs[0].markdown.contains("Failed attempt output"));
        assert!(!outputs[0].markdown.contains("failed-fix"));
    }

    #[test]
    fn failed_attempt_extra_cycle_is_bounded_by_terminal_review_cycle() {
        let operation_id = OperationId::new("remediate-op");
        let good = AttemptId::new("attempt-good");
        let failed = AttemptId::new("attempt-failed-extra-cycle");
        let events = vec![
            event(
                1,
                &operation_id,
                &good,
                "fix-1",
                "agent.message.completed",
                json!({"itemId":"fix","phase":"final_answer","text":"Accepted fix"}),
            ),
            event(
                2,
                &operation_id,
                &good,
                "fix-1",
                "review_cycle.turn_completed",
                json!({"role":"remediation","reviewCycle":1}),
            ),
            event(
                3,
                &operation_id,
                &failed,
                "discarded-cycle-2",
                "agent.message.completed",
                json!({"itemId":"discarded","phase":"final_answer","text":"Discarded extra cycle"}),
            ),
            event(
                4,
                &operation_id,
                &failed,
                "discarded-cycle-2",
                "review_cycle.turn_completed",
                json!({"role":"remediation","reviewCycle":2}),
            ),
            event(
                5,
                &operation_id,
                &good,
                "review-1",
                "agent.message.completed",
                json!({"itemId":"review","phase":"final_answer","text":"Approved cycle one"}),
            ),
            event(
                6,
                &operation_id,
                &good,
                "review-1",
                "stage.completed",
                json!({"role":"review","reviewCycle":1}),
            ),
        ];

        let outputs =
            reduce_settled_job_outputs(&settled_job("remediate-op", "codex.remediate"), &events)
                .unwrap();
        assert!(outputs[0].markdown.contains("Accepted fix"));
        assert!(outputs[0].markdown.contains("Approved cycle one"));
        assert!(!outputs[0].markdown.contains("Discarded extra cycle"));
    }

    #[test]
    fn remediation_renders_every_validated_cycle_in_order() {
        let operation_id = OperationId::new("remediate");
        let attempt = AttemptId::new("attempt-one");
        let events = vec![
            event(
                1,
                &operation_id,
                &attempt,
                "fix-1",
                "agent.message.completed",
                json!({"itemId":"a","phase":"final_answer","text":"Fixed first issue."}),
            ),
            event(
                2,
                &operation_id,
                &attempt,
                "review-1",
                "agent.message.completed",
                json!({"itemId":"b","phase":"final_answer","text":"One issue remains."}),
            ),
            event(
                3,
                &operation_id,
                &attempt,
                "fix-2",
                "agent.message.completed",
                json!({"itemId":"c","phase":"final_answer","text":"Fixed remaining issue."}),
            ),
            event(
                4,
                &operation_id,
                &attempt,
                "review-2",
                "agent.message.completed",
                json!({"itemId":"d","phase":"final_answer","text":"Approved."}),
            ),
        ];
        let output = reduce_completed_stage_outputs(
            &[ValidatedStageOutput {
                operation_id,
                stage: "remediate".to_string(),
                turns: vec![
                    turn("attempt-one", "fix-1", StageOutputTurnRole::Remediation, 1),
                    turn("attempt-one", "review-1", StageOutputTurnRole::Review, 1),
                    turn("attempt-one", "fix-2", StageOutputTurnRole::Remediation, 2),
                    turn("attempt-one", "review-2", StageOutputTurnRole::Review, 2),
                ],
            }],
            &events,
        )
        .unwrap();
        assert_eq!(
            output[0].markdown,
            "### Cycle 1: Remediation\n\nAttempt: `attempt-one`  \nTurn: `fix-1`\n\nFixed first issue.\n\n### Cycle 1: Review\n\nAttempt: `attempt-one`  \nTurn: `review-1`\n\nOne issue remains.\n\n### Cycle 2: Remediation\n\nAttempt: `attempt-one`  \nTurn: `fix-2`\n\nFixed remaining issue.\n\n### Cycle 2: Review\n\nAttempt: `attempt-one`  \nTurn: `review-2`\n\nApproved.\n"
        );
    }
}
