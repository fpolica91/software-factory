use std::collections::HashSet;

use codex_extension_api::ContextContributor;
use codex_extension_api::ExtensionFuture;
use codex_extension_api::PromptFragment;
use codex_extension_api::TurnContextContributionInput;
use serde::Serialize;

use crate::FactoryExtension;
use crate::FactoryRemediationRecord;
use crate::FactoryReviewReport;
use crate::FactoryState;
use crate::FactoryStateDurability;
use crate::FactorySubagentActivity;
use crate::FactoryWorkUnit;
use crate::thread_state;

const MAX_CONTEXT_REMEDIATIONS: usize = 20;
const MAX_CONTEXT_SUBAGENT_ACTIVITIES: usize = 24;
const MAX_CONTEXT_ACTIVITY_TEXT_CHARS: usize = 2_000;

#[derive(Serialize)]
struct FactoryContextSnapshot<'a> {
    source: &'static str,
    thread_id: &'a str,
    durability: &'static str,
    state: FactoryContextState<'a>,
}

#[derive(Serialize)]
struct FactoryContextState<'a> {
    revision: u64,
    work_units: &'a [FactoryWorkUnit],
    review: Option<&'a FactoryReviewReport>,
    remediations: &'a [FactoryRemediationRecord],
    subagents: Vec<FactorySubagentActivity>,
    omitted_remediations: usize,
    omitted_subagent_activities: usize,
}

impl ContextContributor for FactoryExtension {
    fn contribute_turn_context<'a>(
        &'a self,
        input: TurnContextContributionInput<'a>,
    ) -> ExtensionFuture<'a, Vec<PromptFragment>> {
        Box::pin(async move {
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
            let body = serde_json::to_string(&FactoryContextSnapshot {
                source: "factory-native-extension",
                thread_id: thread_state.thread_id(),
                durability,
                state: bounded_context_state(&snapshot),
            })
            .unwrap_or_else(|error| format!("{{\"serialization_error\":{error:?}}}"));
            vec![PromptFragment::separate_developer(format!(
                "The following Factory state is the authoritative current state for this Codex thread. Use Factory tools to change it. Delegate independent runnable work units through native Codex subagents, avoid duplicate assignments, and reconcile child results into Factory progress, review, and remediation before closing them.\n<factory_state>{body}</factory_state>"
            ))]
        })
    }
}

fn bounded_context_state(state: &FactoryState) -> FactoryContextState<'_> {
    let remediation_start = state
        .remediations
        .len()
        .saturating_sub(MAX_CONTEXT_REMEDIATIONS);
    let subagents = bounded_subagents(&state.subagents);
    FactoryContextState {
        revision: state.revision,
        work_units: &state.work_units,
        review: state.review.as_ref(),
        remediations: &state.remediations[remediation_start..],
        subagents,
        omitted_remediations: remediation_start,
        omitted_subagent_activities: state
            .subagents
            .len()
            .saturating_sub(MAX_CONTEXT_SUBAGENT_ACTIVITIES),
    }
}

fn bounded_subagents(activities: &[FactorySubagentActivity]) -> Vec<FactorySubagentActivity> {
    let mut selected = Vec::new();
    let mut seen_agents = HashSet::new();
    for (index, activity) in activities.iter().enumerate().rev() {
        let represents_current_agent = activity
            .receiver_thread_ids
            .iter()
            .any(|thread_id| seen_agents.insert(thread_id.clone()));
        if represents_current_agent {
            selected.push(index);
            if selected.len() == MAX_CONTEXT_SUBAGENT_ACTIVITIES {
                break;
            }
        }
    }
    for index in (0..activities.len()).rev() {
        if selected.len() == MAX_CONTEXT_SUBAGENT_ACTIVITIES {
            break;
        }
        if !selected.contains(&index) {
            selected.push(index);
        }
    }
    selected.sort_unstable();
    selected
        .into_iter()
        .map(|index| bounded_activity(&activities[index]))
        .collect()
}

fn bounded_activity(activity: &FactorySubagentActivity) -> FactorySubagentActivity {
    let mut activity = activity.clone();
    activity.prompt = activity
        .prompt
        .as_deref()
        .map(|prompt| truncate_chars(prompt, MAX_CONTEXT_ACTIVITY_TEXT_CHARS));
    for agent in &mut activity.agents {
        agent.message = agent
            .message
            .as_deref()
            .map(|message| truncate_chars(message, MAX_CONTEXT_ACTIVITY_TEXT_CHARS));
    }
    activity
}

fn truncate_chars(value: &str, limit: usize) -> String {
    let mut chars = value.chars();
    let truncated = chars.by_ref().take(limit).collect::<String>();
    if chars.next().is_some() {
        format!("{truncated}…")
    } else {
        truncated
    }
}
