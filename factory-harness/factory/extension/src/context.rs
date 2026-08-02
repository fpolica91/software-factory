use std::collections::HashSet;

use codex_extension_api::ContextContributor;
use codex_extension_api::ExtensionFuture;
use codex_extension_api::PromptFragment;
use codex_extension_api::TurnContextContributionInput;
use serde::Serialize;

use crate::FactoryExtension;
use crate::FactoryProgressStatus;
use crate::FactoryRemediationRecord;
use crate::FactoryReviewReport;
use crate::FactoryState;
use crate::FactoryStateDurability;
use crate::FactorySubagentActivity;
use crate::FactorySubagentHistory;
use crate::FactoryWorkUnit;
use crate::limits::MAX_CONTEXT_CHARS;
use crate::limits::MAX_CONTEXT_FIELD_CHARS;
use crate::limits::MAX_FINDINGS;
use crate::limits::MAX_IDENTIFIER_CHARS;
use crate::limits::MAX_WORK_UNITS;
use crate::limits::truncate_chars;
use crate::stage::FactoryThreadScope;
use crate::stage::thread_scope;
use crate::thread_state;

const MAX_CONTEXT_REMEDIATIONS: usize = 20;
const MAX_CONTEXT_SUBAGENT_ACTIVITIES: usize = 24;
const MIN_CONTEXT_ITEMS: usize = 4;
const MIN_CONTEXT_FIELD_CHARS: usize = 128;

#[derive(Serialize)]
struct FactoryContextSnapshot<'a> {
    source: &'static str,
    thread_id: &'a str,
    durability: &'static str,
    state: FactoryContextState,
}

#[derive(Serialize)]
struct FactoryContextState {
    revision: u64,
    work_units: Vec<FactoryWorkUnit>,
    review: Option<FactoryReviewReport>,
    remediations: Vec<FactoryRemediationRecord>,
    subagents: Vec<FactorySubagentActivity>,
    subagent_history: Option<FactorySubagentHistory>,
    omitted_work_units: usize,
    omitted_findings: usize,
    omitted_remediations: usize,
    omitted_subagent_activities: usize,
    review_omitted: bool,
}

impl ContextContributor for FactoryExtension {
    fn contribute_turn_context<'a>(
        &'a self,
        input: TurnContextContributionInput<'a>,
    ) -> ExtensionFuture<'a, Vec<PromptFragment>> {
        Box::pin(async move {
            if thread_scope(input.thread_store) == FactoryThreadScope::Subagent {
                return Vec::new();
            }
            let Some(thread_state) = thread_state(input.thread_store) else {
                return Vec::new();
            };
            let snapshot = match thread_state.snapshot().await {
                Ok(snapshot) => snapshot,
                Err(error) => {
                    return vec![PromptFragment::separate_developer(format!(
                        "<factory_state_error>{error}</factory_state_error>"
                    ))];
                }
            };
            let durability = match thread_state.durability() {
                FactoryStateDurability::ProcessMemory => "process_memory",
                FactoryStateDurability::Durable => "durable",
            };
            let body = bounded_context_body(thread_state.thread_id(), durability, &snapshot);
            vec![PromptFragment::separate_developer(format!(
                "The following Factory state is the authoritative current state for this Codex thread. Use Factory tools to change it. Delegate independent runnable work units through native Codex subagents, avoid duplicate assignments, and reconcile child results into Factory progress, review, and remediation before closing them.\n<factory_state>{body}</factory_state>"
            ))]
        })
    }
}

fn bounded_context_body(thread_id: &str, durability: &'static str, state: &FactoryState) -> String {
    let mut item_limit = MAX_WORK_UNITS;
    let mut text_limit = MAX_CONTEXT_FIELD_CHARS;
    loop {
        let snapshot = FactoryContextSnapshot {
            source: "factory-native-extension",
            thread_id,
            durability,
            state: projected_context_state(state, item_limit, text_limit),
        };
        let body = serde_json::to_string(&snapshot)
            .unwrap_or_else(|error| format!("{{\"serialization_error\":{error:?}}}"));
        if body.chars().count() <= MAX_CONTEXT_CHARS {
            return body;
        }
        if text_limit > MIN_CONTEXT_FIELD_CHARS {
            text_limit = (text_limit / 2).max(MIN_CONTEXT_FIELD_CHARS);
        } else if item_limit > MIN_CONTEXT_ITEMS {
            item_limit = (item_limit / 2).max(MIN_CONTEXT_ITEMS);
        } else {
            return minimal_context_body(thread_id, durability, state);
        }
    }
}

fn projected_context_state(
    state: &FactoryState,
    item_limit: usize,
    text_limit: usize,
) -> FactoryContextState {
    let work_units = bounded_work_units(&state.work_units, item_limit, text_limit);
    let (review, omitted_findings) = bounded_review(
        state.review.as_ref(),
        item_limit.min(MAX_FINDINGS),
        text_limit,
    );
    let remediation_limit = item_limit.min(MAX_CONTEXT_REMEDIATIONS);
    let remediation_start = state.remediations.len().saturating_sub(remediation_limit);
    let remediations = state.remediations[remediation_start..]
        .iter()
        .cloned()
        .map(|mut record| {
            record.finding_id = truncate_chars(&record.finding_id, MAX_IDENTIFIER_CHARS);
            record.unit_id = truncate_chars(&record.unit_id, MAX_IDENTIFIER_CHARS);
            record.rationale = truncate_chars(&record.rationale, text_limit);
            record
        })
        .collect();
    let subagent_limit = item_limit.min(MAX_CONTEXT_SUBAGENT_ACTIVITIES);
    let subagents = bounded_subagents(&state.subagents, subagent_limit, text_limit);
    FactoryContextState {
        revision: state.revision,
        omitted_work_units: state.work_units.len().saturating_sub(work_units.len()),
        omitted_findings,
        omitted_remediations: remediation_start,
        omitted_subagent_activities: state.subagents.len().saturating_sub(subagents.len()),
        work_units,
        review,
        remediations,
        subagents,
        subagent_history: bounded_subagent_history(state.subagent_history.as_ref()),
        review_omitted: false,
    }
}

fn minimal_context_body(thread_id: &str, durability: &'static str, state: &FactoryState) -> String {
    serde_json::to_string(&FactoryContextSnapshot {
        source: "factory-native-extension",
        thread_id,
        durability,
        state: FactoryContextState {
            revision: state.revision,
            work_units: Vec::new(),
            review: None,
            remediations: Vec::new(),
            subagents: Vec::new(),
            subagent_history: bounded_subagent_history(state.subagent_history.as_ref()),
            omitted_work_units: state.work_units.len(),
            omitted_findings: state
                .review
                .as_ref()
                .map_or(0, |review| review.findings.len()),
            omitted_remediations: state.remediations.len(),
            omitted_subagent_activities: state.subagents.len(),
            review_omitted: state.review.is_some(),
        },
    })
    .expect("minimal Factory context is serializable")
}

fn bounded_work_units(
    units: &[FactoryWorkUnit],
    limit: usize,
    text_limit: usize,
) -> Vec<FactoryWorkUnit> {
    let mut selected = units
        .iter()
        .enumerate()
        .filter(|(_, unit)| unit.status != FactoryProgressStatus::Completed)
        .map(|(index, _)| index)
        .take(limit)
        .collect::<Vec<_>>();
    for index in (0..units.len()).rev() {
        if selected.len() == limit {
            break;
        }
        if !selected.contains(&index) {
            selected.push(index);
        }
    }
    selected.sort_unstable();
    selected
        .into_iter()
        .map(|index| {
            let mut unit = units[index].clone();
            unit.id = truncate_chars(&unit.id, MAX_IDENTIFIER_CHARS);
            unit.title = truncate_chars(&unit.title, text_limit);
            unit.description = truncate_chars(&unit.description, text_limit);
            unit.depends_on = unit
                .depends_on
                .into_iter()
                .take(limit)
                .map(|dependency| truncate_chars(&dependency, MAX_IDENTIFIER_CHARS))
                .collect();
            unit.progress_summary = unit
                .progress_summary
                .as_deref()
                .map(|summary| truncate_chars(summary, text_limit));
            unit
        })
        .collect()
}

fn bounded_review(
    review: Option<&FactoryReviewReport>,
    finding_limit: usize,
    text_limit: usize,
) -> (Option<FactoryReviewReport>, usize) {
    let Some(review) = review else {
        return (None, 0);
    };
    let mut review = review.clone();
    let omitted = review.findings.len().saturating_sub(finding_limit);
    review.summary = truncate_chars(&review.summary, text_limit);
    review.recorded_turn_id = bounded_optional_id(review.recorded_turn_id);
    review.recorded_thread_id = bounded_optional_id(review.recorded_thread_id);
    review.recorded_parent_thread_id = bounded_optional_id(review.recorded_parent_thread_id);
    review.recorded_parent_turn_id = bounded_optional_id(review.recorded_parent_turn_id);
    review.recorded_subagent_kind = bounded_optional_id(review.recorded_subagent_kind);
    review.findings.truncate(finding_limit);
    for finding in &mut review.findings {
        finding.id = truncate_chars(&finding.id, MAX_IDENTIFIER_CHARS);
        finding.unit_id = truncate_chars(&finding.unit_id, MAX_IDENTIFIER_CHARS);
        finding.title = truncate_chars(&finding.title, text_limit);
        finding.evidence = truncate_chars(&finding.evidence, text_limit);
        finding.recommendation = truncate_chars(&finding.recommendation, text_limit);
    }
    (Some(review), omitted)
}

fn bounded_optional_id(value: Option<String>) -> Option<String> {
    value.map(|value| truncate_chars(&value, MAX_IDENTIFIER_CHARS))
}

fn bounded_subagent_history(
    history: Option<&FactorySubagentHistory>,
) -> Option<FactorySubagentHistory> {
    history.cloned().map(|mut history| {
        history.event_kind = truncate_chars(&history.event_kind, MAX_IDENTIFIER_CHARS);
        history
    })
}

fn bounded_subagents(
    activities: &[FactorySubagentActivity],
    limit: usize,
    text_limit: usize,
) -> Vec<FactorySubagentActivity> {
    let mut selected = Vec::new();
    let mut seen_agents = HashSet::new();
    for (index, activity) in activities.iter().enumerate().rev() {
        let represents_current_agent = activity
            .receiver_thread_ids
            .iter()
            .any(|thread_id| seen_agents.insert(thread_id.clone()));
        if represents_current_agent {
            selected.push(index);
            if selected.len() == limit {
                break;
            }
        }
    }
    for index in (0..activities.len()).rev() {
        if selected.len() == limit {
            break;
        }
        if !selected.contains(&index) {
            selected.push(index);
        }
    }
    selected.sort_unstable();
    selected
        .into_iter()
        .map(|index| bounded_activity(&activities[index], limit, text_limit))
        .collect()
}

fn bounded_activity(
    activity: &FactorySubagentActivity,
    nested_limit: usize,
    text_limit: usize,
) -> FactorySubagentActivity {
    let mut activity = activity.clone();
    activity.call_id = truncate_chars(&activity.call_id, MAX_IDENTIFIER_CHARS);
    activity.turn_id = truncate_chars(&activity.turn_id, MAX_IDENTIFIER_CHARS);
    activity.sender_thread_id = truncate_chars(&activity.sender_thread_id, MAX_IDENTIFIER_CHARS);
    activity.created_at = truncate_chars(&activity.created_at, MAX_IDENTIFIER_CHARS);
    activity.updated_at = truncate_chars(&activity.updated_at, MAX_IDENTIFIER_CHARS);
    activity.receiver_thread_ids = activity
        .receiver_thread_ids
        .into_iter()
        .take(nested_limit)
        .map(|thread_id| truncate_chars(&thread_id, MAX_IDENTIFIER_CHARS))
        .collect();
    activity.prompt = activity
        .prompt
        .as_deref()
        .map(|prompt| truncate_chars(prompt, text_limit));
    activity.agents.truncate(nested_limit);
    for agent in &mut activity.agents {
        agent.thread_id = truncate_chars(&agent.thread_id, MAX_IDENTIFIER_CHARS);
        agent.message = agent
            .message
            .as_deref()
            .map(|message| truncate_chars(message, text_limit));
    }
    activity
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oversized_legacy_state_is_projected_without_mutation() {
        let mut state = FactoryState {
            revision: 7,
            work_units: (0..80)
                .map(|index| FactoryWorkUnit {
                    id: format!("unit-{index}"),
                    title: "t".repeat(5_000),
                    description: "d".repeat(5_000),
                    depends_on: Vec::new(),
                    status: FactoryProgressStatus::Pending,
                    progress_summary: Some("p".repeat(5_000)),
                })
                .collect(),
            ..FactoryState::default()
        };
        state.subagent_history = Some(FactorySubagentHistory {
            source: crate::FactorySubagentHistorySource::CoordinatorJobEvents,
            event_kind: "e".repeat(100_000),
            latest_sequence: 99,
        });
        let original = state.clone();

        let body = bounded_context_body("thread", "durable", &state);

        assert!(body.chars().count() <= MAX_CONTEXT_CHARS);
        assert_eq!(state, original);
        let value: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert!(value["state"]["omitted_work_units"].as_u64().unwrap() > 0);
    }

    #[test]
    fn detached_review_recovery_marker_is_not_model_context() {
        let mut state = FactoryState {
            revision: 3,
            ..FactoryState::default()
        };
        state.prepare_review_recovery();

        let body = bounded_context_body("thread", "durable", &state);
        let value: serde_json::Value = serde_json::from_str(&body).unwrap();

        assert!(state.review_recovery.is_some());
        assert!(value["state"].get("reviewRecovery").is_none());
        assert!(!body.contains("review_recovery"));
    }
}
