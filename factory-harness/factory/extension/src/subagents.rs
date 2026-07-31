use chrono::SecondsFormat;
use chrono::Utc;
use codex_extension_api::ExtensionData;
use codex_extension_api::ExtensionFuture;
use codex_extension_api::TurnItemContributor;
use codex_protocol::items::CollabAgentTool;
use codex_protocol::items::CollabAgentToolCallItem;
use codex_protocol::items::CollabAgentToolCallStatus;
use codex_protocol::items::SubAgentActivityItem;
use codex_protocol::items::TurnItem;
use codex_protocol::protocol::AgentStatus;
use codex_protocol::protocol::SubAgentActivityKind;

use crate::FactoryExtension;
use crate::FactorySubagentActivity;
use crate::FactorySubagentState;
use crate::FactorySubagentStatus;
use crate::FactorySubagentTool;
use crate::FactorySubagentToolCallStatus;
use crate::FactoryThreadState;
use crate::thread_state;

impl TurnItemContributor for FactoryExtension {
    fn contribute<'a>(
        &'a self,
        thread_store: &'a ExtensionData,
        turn_store: &'a ExtensionData,
        item: &'a mut TurnItem,
    ) -> ExtensionFuture<'a, Result<(), String>> {
        Box::pin(async move {
            let Some(thread_state) = thread_state(thread_store) else {
                return Ok(());
            };
            match item {
                TurnItem::CollabAgentToolCall(item) => {
                    record_collab_activity(&thread_state, turn_store.level_id(), item).await
                }
                TurnItem::SubAgentActivity(item) => {
                    record_native_activity(&thread_state, turn_store.level_id(), item).await
                }
                _ => Ok(()),
            }
        })
    }
}

async fn record_collab_activity(
    thread_state: &FactoryThreadState,
    turn_id: &str,
    item: &CollabAgentToolCallItem,
) -> Result<(), String> {
    let observed_at = Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true);
    let activity = FactorySubagentActivity {
        call_id: item.id.clone(),
        turn_id: turn_id.to_string(),
        sender_thread_id: item.sender_thread_id.to_string(),
        receiver_thread_ids: item
            .receiver_thread_ids
            .iter()
            .map(ToString::to_string)
            .collect(),
        tool: map_tool(item.tool),
        prompt: item.prompt.clone(),
        status: map_call_status(item.status),
        agents: map_agent_states(item),
        created_at: observed_at.clone(),
        updated_at: observed_at,
    };
    upsert_activity(thread_state, activity).await
}

async fn record_native_activity(
    thread_state: &FactoryThreadState,
    turn_id: &str,
    item: &SubAgentActivityItem,
) -> Result<(), String> {
    let observed_at = Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true);
    let (tool, agents) = match item.kind {
        SubAgentActivityKind::Started => (
            FactorySubagentTool::SpawnAgent,
            vec![FactorySubagentState {
                thread_id: item.agent_thread_id.to_string(),
                status: FactorySubagentStatus::Running,
                terminal: false,
                message: None,
            }],
        ),
        SubAgentActivityKind::Interacted => (FactorySubagentTool::Interact, Vec::new()),
        SubAgentActivityKind::Interrupted => (
            FactorySubagentTool::InterruptAgent,
            vec![FactorySubagentState {
                thread_id: item.agent_thread_id.to_string(),
                status: FactorySubagentStatus::Interrupted,
                terminal: false,
                message: None,
            }],
        ),
    };
    upsert_activity(
        thread_state,
        FactorySubagentActivity {
            call_id: item.id.clone(),
            turn_id: turn_id.to_string(),
            sender_thread_id: thread_state.thread_id().to_string(),
            receiver_thread_ids: vec![item.agent_thread_id.to_string()],
            tool,
            prompt: None,
            status: FactorySubagentToolCallStatus::Completed,
            agents,
            created_at: observed_at.clone(),
            updated_at: observed_at,
        },
    )
    .await
}

async fn upsert_activity(
    thread_state: &FactoryThreadState,
    mut activity: FactorySubagentActivity,
) -> Result<(), String> {
    thread_state
        .update(move |state| {
            if let Some(existing) = state
                .subagents
                .iter_mut()
                .find(|existing| existing.call_id == activity.call_id)
            {
                activity.created_at.clone_from(&existing.created_at);
                *existing = activity;
            } else {
                state.subagents.push(activity);
            }
            Ok(())
        })
        .await
        .map(|_| ())
        .map_err(|error| error.to_string())
}

fn map_tool(tool: CollabAgentTool) -> FactorySubagentTool {
    match tool {
        CollabAgentTool::SpawnAgent => FactorySubagentTool::SpawnAgent,
        CollabAgentTool::SendInput => FactorySubagentTool::SendInput,
        CollabAgentTool::ResumeAgent => FactorySubagentTool::ResumeAgent,
        CollabAgentTool::Wait => FactorySubagentTool::Wait,
        CollabAgentTool::CloseAgent => FactorySubagentTool::CloseAgent,
    }
}

fn map_call_status(status: CollabAgentToolCallStatus) -> FactorySubagentToolCallStatus {
    match status {
        CollabAgentToolCallStatus::InProgress => FactorySubagentToolCallStatus::InProgress,
        CollabAgentToolCallStatus::Completed => FactorySubagentToolCallStatus::Completed,
        CollabAgentToolCallStatus::Failed => FactorySubagentToolCallStatus::Failed,
    }
}

fn map_agent_states(item: &CollabAgentToolCallItem) -> Vec<FactorySubagentState> {
    let mut states = item
        .agents_states
        .iter()
        .map(|(thread_id, status)| {
            let (status, terminal, message) = match status {
                AgentStatus::PendingInit => (FactorySubagentStatus::PendingInit, false, None),
                AgentStatus::Running => (FactorySubagentStatus::Running, false, None),
                AgentStatus::Interrupted => (FactorySubagentStatus::Interrupted, false, None),
                AgentStatus::Completed(message) => {
                    (FactorySubagentStatus::Completed, true, message.clone())
                }
                AgentStatus::Errored(message) => {
                    (FactorySubagentStatus::Errored, true, Some(message.clone()))
                }
                AgentStatus::Shutdown => (FactorySubagentStatus::Shutdown, true, None),
                AgentStatus::NotFound => (FactorySubagentStatus::NotFound, true, None),
            };
            FactorySubagentState {
                thread_id: thread_id.to_string(),
                status,
                terminal,
                message,
            }
        })
        .collect::<Vec<_>>();
    states.sort_by(|left, right| left.thread_id.cmp(&right.thread_id));
    states
}
