use std::collections::HashSet;
use std::fmt::Write;

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
use sha2::Digest;
use sha2::Sha256;

use crate::FactoryEventReference;
use crate::FactoryExtension;
use crate::FactoryState;
use crate::FactorySubagentActivity;
use crate::FactorySubagentHistory;
use crate::FactorySubagentHistorySource;
use crate::FactorySubagentState;
use crate::FactorySubagentStatus;
use crate::FactorySubagentTool;
use crate::FactorySubagentToolCallStatus;
use crate::FactoryThreadState;
use crate::thread_state;

const SUBAGENT_EVENT_KIND: &str = "factory.subagent.activity";
const MAX_PROJECTED_SUBAGENT_ACTIVITIES: usize = 24;

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
    let snapshot = thread_state
        .snapshot()
        .await
        .map_err(|error| error.to_string())?;
    if let Some(existing) = snapshot
        .subagents
        .iter()
        .find(|existing| same_logical_activity(existing, &activity))
    {
        activity.created_at.clone_from(&existing.created_at);
    }
    let durable_archive = archive_activities(thread_state, &snapshot, &activity).await?;
    thread_state
        .update(move |state| {
            apply_activity(state, activity, durable_archive);
            Ok(())
        })
        .await
        .map(|_| ())
        .map_err(|error| error.to_string())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct FactoryEventArchive {
    latest_sequence: u64,
}

async fn archive_activities(
    thread_state: &FactoryThreadState,
    snapshot: &FactoryState,
    activity: &FactorySubagentActivity,
) -> Result<Option<FactoryEventArchive>, String> {
    let mut archive = None;
    if snapshot.subagent_history.is_none() {
        for existing in &snapshot.subagents {
            let Some(reference) = archive_activity(thread_state, existing).await? else {
                return Ok(None);
            };
            extend_archive(&mut archive, reference);
        }
    }
    let Some(reference) = archive_activity(thread_state, activity).await? else {
        return Ok(None);
    };
    extend_archive(&mut archive, reference);
    Ok(archive)
}

async fn archive_activity(
    thread_state: &FactoryThreadState,
    activity: &FactorySubagentActivity,
) -> Result<Option<FactoryEventReference>, String> {
    let (payload, deduplication_key) = activity_event(activity)?;
    thread_state
        .append_event(SUBAGENT_EVENT_KIND, payload, &deduplication_key)
        .await
        .map_err(|error| error.to_string())
}

fn activity_event(
    activity: &FactorySubagentActivity,
) -> Result<(serde_json::Value, String), String> {
    let mut payload = serde_json::to_value(activity)
        .map_err(|error| format!("encode subagent activity event: {error}"))?;
    let object = payload
        .as_object_mut()
        .ok_or_else(|| "encoded subagent activity is not an object".to_string())?;
    object.remove("created_at");
    object.remove("updated_at");
    let bytes = serde_json::to_vec(&payload)
        .map_err(|error| format!("encode subagent activity identity: {error}"))?;
    let digest = Sha256::digest(bytes);
    let mut key = String::with_capacity(SUBAGENT_EVENT_KIND.len() + 1 + digest.len() * 2);
    key.push_str(SUBAGENT_EVENT_KIND);
    key.push(':');
    for byte in digest {
        write!(&mut key, "{byte:02x}").expect("writing to a String cannot fail");
    }
    Ok((payload, key))
}

fn extend_archive(archive: &mut Option<FactoryEventArchive>, reference: FactoryEventReference) {
    let archive = archive.get_or_insert(FactoryEventArchive {
        latest_sequence: reference.sequence,
    });
    archive.latest_sequence = archive.latest_sequence.max(reference.sequence);
}

fn apply_activity(
    state: &mut FactoryState,
    mut activity: FactorySubagentActivity,
    durable_archive: Option<FactoryEventArchive>,
) {
    if let Some(position) = state
        .subagents
        .iter()
        .position(|existing| same_logical_activity(existing, &activity))
    {
        let existing = state.subagents.remove(position);
        activity.created_at.clone_from(&existing.created_at);
    }
    state.subagents.push(activity);
    if let Some(archive) = durable_archive {
        let history = state
            .subagent_history
            .get_or_insert_with(|| FactorySubagentHistory {
                source: FactorySubagentHistorySource::CoordinatorJobEvents,
                event_kind: SUBAGENT_EVENT_KIND.to_string(),
                latest_sequence: archive.latest_sequence,
            });
        history.latest_sequence = history.latest_sequence.max(archive.latest_sequence);
        state.subagents = bounded_projection(&state.subagents);
    }
}

fn same_logical_activity(left: &FactorySubagentActivity, right: &FactorySubagentActivity) -> bool {
    left.call_id == right.call_id
        && left.turn_id == right.turn_id
        && left.sender_thread_id == right.sender_thread_id
}

fn bounded_projection(activities: &[FactorySubagentActivity]) -> Vec<FactorySubagentActivity> {
    if activities.len() <= MAX_PROJECTED_SUBAGENT_ACTIVITIES {
        return activities.to_vec();
    }
    let mut selected = Vec::with_capacity(MAX_PROJECTED_SUBAGENT_ACTIVITIES);
    let mut seen_agents = HashSet::new();
    for (index, activity) in activities.iter().enumerate().rev() {
        let represents_latest_agent_state = activity
            .receiver_thread_ids
            .iter()
            .any(|thread_id| seen_agents.insert(thread_id.clone()));
        if represents_latest_agent_state {
            selected.push(index);
            if selected.len() == MAX_PROJECTED_SUBAGENT_ACTIVITIES {
                break;
            }
        }
    }
    for index in (0..activities.len()).rev() {
        if selected.len() == MAX_PROJECTED_SUBAGENT_ACTIVITIES {
            break;
        }
        if !selected.contains(&index) {
            selected.push(index);
        }
    }
    selected.sort_unstable();
    selected
        .into_iter()
        .map(|index| activities[index].clone())
        .collect()
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

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Arc;
    use std::sync::Mutex;
    use std::sync::RwLock;
    use std::sync::atomic::AtomicUsize;
    use std::sync::atomic::Ordering;

    use super::*;
    use crate::FactoryBackendError;
    use crate::FactoryBackendFuture;
    use crate::FactoryStateBackend;
    use crate::FactoryStateDurability;
    use crate::state::FactoryStateRegistry;

    struct ArchiveBackend {
        state: RwLock<Option<FactoryState>>,
        events: Mutex<HashMap<String, (u64, serde_json::Value)>>,
        append_calls: AtomicUsize,
        fail_at: Option<usize>,
    }

    impl ArchiveBackend {
        fn new(state: FactoryState, fail_at: Option<usize>) -> Self {
            Self {
                state: RwLock::new(Some(state)),
                events: Mutex::new(HashMap::new()),
                append_calls: AtomicUsize::new(0),
                fail_at,
            }
        }
    }

    impl FactoryStateBackend for ArchiveBackend {
        fn load<'a>(
            &'a self,
            _thread_id: &'a str,
        ) -> FactoryBackendFuture<'a, Option<FactoryState>> {
            Box::pin(async move {
                self.state
                    .read()
                    .map(|state| state.clone())
                    .map_err(|_| FactoryBackendError::new("test state lock failed"))
            })
        }

        fn save<'a>(
            &'a self,
            _thread_id: &'a str,
            state: FactoryState,
        ) -> FactoryBackendFuture<'a, ()> {
            Box::pin(async move {
                *self
                    .state
                    .write()
                    .map_err(|_| FactoryBackendError::new("test state lock failed"))? = Some(state);
                Ok(())
            })
        }

        fn append_event<'a>(
            &'a self,
            _kind: &'a str,
            payload: serde_json::Value,
            deduplication_key: &'a str,
        ) -> FactoryBackendFuture<'a, Option<FactoryEventReference>> {
            Box::pin(async move {
                let call = self.append_calls.fetch_add(1, Ordering::SeqCst) + 1;
                if self.fail_at == Some(call) {
                    return Err(FactoryBackendError::new("injected archive failure"));
                }
                let mut events = self
                    .events
                    .lock()
                    .map_err(|_| FactoryBackendError::new("test event lock failed"))?;
                if let Some((sequence, existing)) = events.get(deduplication_key) {
                    if existing != &payload {
                        return Err(FactoryBackendError::new(
                            "deduplication key reused for different test payload",
                        ));
                    }
                    return Ok(Some(FactoryEventReference {
                        sequence: *sequence,
                    }));
                }
                let sequence = events.len() as u64 + 1;
                events.insert(deduplication_key.to_string(), (sequence, payload));
                Ok(Some(FactoryEventReference { sequence }))
            })
        }

        fn durability(&self) -> FactoryStateDurability {
            FactoryStateDurability::Durable
        }
    }

    fn activity(index: u64) -> FactorySubagentActivity {
        FactorySubagentActivity {
            call_id: format!("call-{index}"),
            turn_id: "turn-1".to_string(),
            sender_thread_id: "parent".to_string(),
            receiver_thread_ids: vec![format!("child-{index}")],
            tool: FactorySubagentTool::SpawnAgent,
            prompt: Some(format!("prompt-{index}")),
            status: FactorySubagentToolCallStatus::Completed,
            agents: Vec::new(),
            created_at: format!("created-{index}"),
            updated_at: format!("updated-{index}"),
        }
    }

    #[test]
    fn bounds_only_after_durable_archival() {
        let mut durable = FactoryState::default();
        for sequence in 1..=30 {
            apply_activity(
                &mut durable,
                activity(sequence),
                Some(FactoryEventArchive {
                    latest_sequence: sequence,
                }),
            );
        }
        assert_eq!(durable.subagents.len(), MAX_PROJECTED_SUBAGENT_ACTIVITIES);
        assert_eq!(durable.subagents[0].call_id, "call-7");
        assert_eq!(
            durable.subagent_history,
            Some(FactorySubagentHistory {
                source: FactorySubagentHistorySource::CoordinatorJobEvents,
                event_kind: SUBAGENT_EVENT_KIND.to_string(),
                latest_sequence: 30,
            })
        );

        let mut without_archive = FactoryState::default();
        for sequence in 1..=30 {
            apply_activity(&mut without_archive, activity(sequence), None);
        }
        assert_eq!(without_archive.subagents.len(), 30);
        assert!(without_archive.subagent_history.is_none());
    }

    #[test]
    fn repeated_call_updates_projection_and_preserves_creation_time() {
        let mut state = FactoryState::default();
        let first = activity(1);
        apply_activity(
            &mut state,
            first,
            Some(FactoryEventArchive {
                latest_sequence: 10,
            }),
        );
        let mut completed = activity(1);
        completed.created_at = "incorrect-new-time".to_string();
        completed.updated_at = "completed-time".to_string();
        apply_activity(
            &mut state,
            completed,
            Some(FactoryEventArchive {
                latest_sequence: 11,
            }),
        );

        assert_eq!(state.subagents.len(), 1);
        assert_eq!(state.subagents[0].created_at, "created-1");
        assert_eq!(state.subagents[0].updated_at, "completed-time");
        assert_eq!(state.subagent_history.as_ref().unwrap().latest_sequence, 11);
    }

    #[test]
    fn activity_event_identity_ignores_observation_timestamps() {
        let first = activity(1);
        let mut replay = first.clone();
        replay.created_at = "later-created-time".to_string();
        replay.updated_at = "later-updated-time".to_string();

        let (first_payload, first_key) = activity_event(&first).unwrap();
        let (replay_payload, replay_key) = activity_event(&replay).unwrap();
        assert_eq!(replay_payload, first_payload);
        assert_eq!(replay_key, first_key);

        replay.prompt = Some("different durable detail".to_string());
        let (_, changed_key) = activity_event(&replay).unwrap();
        assert_ne!(changed_key, first_key);

        let mut reused_by_sender = first.clone();
        reused_by_sender.sender_thread_id = "another-parent".to_string();
        let (_, sender_key) = activity_event(&reused_by_sender).unwrap();
        assert_ne!(sender_key, first_key);

        let mut reused_in_turn = first.clone();
        reused_in_turn.turn_id = "turn-2".to_string();
        let (_, turn_key) = activity_event(&reused_in_turn).unwrap();
        assert_ne!(turn_key, first_key);
    }

    #[tokio::test]
    async fn reused_call_id_across_sender_or_turn_is_a_distinct_activity() {
        let initial = FactoryState {
            subagents: vec![activity(1)],
            subagent_history: Some(FactorySubagentHistory {
                source: FactorySubagentHistorySource::CoordinatorJobEvents,
                event_kind: SUBAGENT_EVENT_KIND.to_string(),
                latest_sequence: 1,
            }),
            ..FactoryState::default()
        };
        let backend = Arc::new(ArchiveBackend::new(initial, None));
        let state = FactoryStateRegistry::default()
            .get_or_create("thread-1", backend.clone())
            .await;

        let mut reused_by_sender = activity(1);
        reused_by_sender.sender_thread_id = "another-parent".to_string();
        reused_by_sender.created_at = "sender-created".to_string();
        upsert_activity(&state, reused_by_sender).await.unwrap();

        let mut reused_in_turn = activity(1);
        reused_in_turn.turn_id = "turn-2".to_string();
        reused_in_turn.created_at = "turn-created".to_string();
        upsert_activity(&state, reused_in_turn).await.unwrap();

        let saved = backend.state.read().unwrap().clone().unwrap();
        assert_eq!(saved.subagents.len(), 3);
        assert_eq!(saved.subagents[0].created_at, "created-1");
        assert_eq!(saved.subagents[1].created_at, "sender-created");
        assert_eq!(saved.subagents[2].created_at, "turn-created");
        assert_eq!(backend.events.lock().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn legacy_backfill_and_replay_are_durable_and_idempotent() {
        let initial = FactoryState {
            subagents: (1..=30).map(activity).collect(),
            ..FactoryState::default()
        };
        let backend = Arc::new(ArchiveBackend::new(initial, None));
        let state = FactoryStateRegistry::default()
            .get_or_create("thread-1", backend.clone())
            .await;

        upsert_activity(&state, activity(31)).await.unwrap();
        assert_eq!(backend.events.lock().unwrap().len(), 31);
        let saved = backend.state.read().unwrap().clone().unwrap();
        assert_eq!(saved.subagents.len(), MAX_PROJECTED_SUBAGENT_ACTIVITIES);
        assert_eq!(saved.subagents[0].call_id, "call-8");
        assert_eq!(saved.subagent_history.unwrap().latest_sequence, 31);

        let mut replay = activity(31);
        replay.updated_at = "retry-time".to_string();
        upsert_activity(&state, replay).await.unwrap();
        assert_eq!(backend.events.lock().unwrap().len(), 31);
        assert_eq!(
            backend
                .state
                .read()
                .unwrap()
                .as_ref()
                .unwrap()
                .subagents
                .len(),
            MAX_PROJECTED_SUBAGENT_ACTIVITIES
        );
    }

    #[tokio::test]
    async fn failed_legacy_backfill_never_prunes_state() {
        let initial = FactoryState {
            subagents: (1..=30).map(activity).collect(),
            ..FactoryState::default()
        };
        let backend = Arc::new(ArchiveBackend::new(initial, Some(10)));
        let state = FactoryStateRegistry::default()
            .get_or_create("thread-1", backend.clone())
            .await;

        assert!(upsert_activity(&state, activity(31)).await.is_err());
        let saved = backend.state.read().unwrap().clone().unwrap();
        assert_eq!(saved.subagents.len(), 30);
        assert!(saved.subagent_history.is_none());
    }
}
