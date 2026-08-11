use std::collections::HashMap;
use std::collections::HashSet;

use codex_app_server_protocol::ServerNotification;
use codex_app_server_protocol::ThreadItem;
use codex_app_server_protocol::TurnStatus;
use factory_coordinator::AttemptFence;
use factory_coordinator::CoordinatorError;
use factory_coordinator::CoordinatorStore;
use factory_coordinator::NewAttemptEvent;
use serde_json::Map;
use serde_json::Value;
use serde_json::json;

const CHUNK_BYTES: usize = 1_024;

#[derive(Clone, Debug, Eq, PartialEq)]
struct ActiveTurn {
    thread: String,
    turn: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct StreamKey {
    kind: &'static str,
    item: String,
    part: Option<i64>,
}

#[derive(Debug, Eq, PartialEq)]
struct TextChunk {
    key: StreamKey,
    text: String,
}

#[derive(Debug)]
struct TextCoalescer {
    pending: Option<TextChunk>,
    limit: usize,
}

impl TextCoalescer {
    fn new() -> Self {
        Self {
            pending: None,
            limit: CHUNK_BYTES,
        }
    }

    #[cfg(test)]
    fn with_limit(limit: usize) -> Self {
        Self {
            pending: None,
            limit,
        }
    }

    fn push(&mut self, key: StreamKey, delta: &str) -> Vec<TextChunk> {
        let mut ready = Vec::new();
        if self.pending.as_ref().is_some_and(|value| value.key != key) {
            ready.extend(self.flush());
        }
        self.pending
            .get_or_insert_with(|| TextChunk {
                key,
                text: String::new(),
            })
            .text
            .push_str(delta);

        while self
            .pending
            .as_ref()
            .is_some_and(|value| value.text.len() >= self.limit)
        {
            let pending = self.pending.as_mut().expect("pending text");
            let split = utf8_prefix(&pending.text, self.limit);
            let remainder = pending.text.split_off(split);
            let raw = std::mem::replace(&mut pending.text, remainder);
            if !raw.is_empty() {
                ready.push(TextChunk {
                    key: pending.key.clone(),
                    text: raw,
                });
            }
        }
        ready
    }

    fn flush(&mut self) -> Option<TextChunk> {
        let pending = self.pending.take()?;
        (!pending.text.is_empty()).then_some(pending)
    }
}

struct StreamDelta<'a> {
    thread: &'a str,
    turn: &'a str,
    key: StreamKey,
    text: &'a str,
}

struct MappedEvent<'a> {
    thread: &'a str,
    turn: &'a str,
    kind: &'static str,
    payload: Value,
}

struct ModelUsageEvent<'a> {
    thread: &'a str,
    turn: &'a str,
    deduplication_key: String,
    payload: Value,
}

#[derive(Debug, Default)]
struct StreamedContent {
    agent_messages: HashSet<String>,
    reasoning_summary_parts: HashMap<String, HashSet<i64>>,
}

/// Fenced, concise event output for one live Codex attempt.
pub struct AttemptEventWriter {
    store: CoordinatorStore,
    fence: AttemptFence,
    active: Option<ActiveTurn>,
    text: TextCoalescer,
    streamed_content: StreamedContent,
}

impl AttemptEventWriter {
    pub fn new(store: CoordinatorStore, fence: AttemptFence) -> Self {
        Self {
            store,
            fence,
            active: None,
            text: TextCoalescer::new(),
            streamed_content: StreamedContent::default(),
        }
    }

    pub fn activate(&mut self, thread: impl Into<String>, turn: impl Into<String>) {
        debug_assert!(self.text.pending.is_none());
        self.streamed_content = StreamedContent::default();
        self.active = Some(ActiveTurn {
            thread: thread.into(),
            turn: turn.into(),
        });
    }

    pub async fn emit(&mut self, kind: &str, payload: Value) -> Result<(), CoordinatorError> {
        self.flush().await?;
        self.persist(kind, payload).await
    }

    pub async fn observe(
        &mut self,
        notification: &ServerNotification,
    ) -> Result<(), CoordinatorError> {
        if let Some(delta) = stream_delta(notification) {
            if self.targets(delta.thread, delta.turn) {
                for chunk in self.text.push(delta.key, delta.text) {
                    self.persist_chunk(chunk).await?;
                }
            }
            return Ok(());
        }
        if let ServerNotification::ItemCompleted(value) = notification {
            if self.targets(&value.thread_id, &value.turn_id) {
                self.flush().await?;
                if let Some((kind, payload)) =
                    completed_item_event(&value.item, &mut self.streamed_content)
                {
                    self.persist(kind, payload).await?;
                }
            }
            return Ok(());
        }
        if let Some(event) = model_usage_event(notification) {
            if self.targets(event.thread, event.turn) {
                self.flush().await?;
                self.persist_with_deduplication(
                    "model.usage",
                    event.payload,
                    Some(event.deduplication_key),
                )
                .await?;
            }
            return Ok(());
        }
        let Some(event) = map_event(notification) else {
            return Ok(());
        };
        if self.targets(event.thread, event.turn) {
            self.emit(event.kind, event.payload).await?;
        }
        Ok(())
    }

    fn targets(&self, thread: &str, turn: &str) -> bool {
        targets(self.active.as_ref(), thread, turn)
    }

    async fn flush(&mut self) -> Result<(), CoordinatorError> {
        if let Some(chunk) = self.text.flush() {
            self.persist_chunk(chunk).await?;
        }
        Ok(())
    }

    async fn persist_chunk(&mut self, chunk: TextChunk) -> Result<(), CoordinatorError> {
        let key = chunk.key;
        let payload = json!({ "itemId": key.item, "partIndex": key.part, "text": chunk.text });
        self.persist(key.kind, payload).await?;
        record_streamed_content(&mut self.streamed_content, &key);
        Ok(())
    }

    async fn persist(&mut self, kind: &str, payload: Value) -> Result<(), CoordinatorError> {
        self.persist_with_deduplication(kind, payload, None).await
    }

    async fn persist_with_deduplication(
        &mut self,
        kind: &str,
        payload: Value,
        deduplication_key: Option<String>,
    ) -> Result<(), CoordinatorError> {
        let event = NewAttemptEvent {
            kind: kind.to_string(),
            payload: correlate(payload, self.active.as_ref()),
            deduplication_key,
        };
        self.store
            .append_attempt_event(&self.fence, event)
            .await
            .map(|_| ())
    }
}

fn model_usage_event(notification: &ServerNotification) -> Option<ModelUsageEvent<'_>> {
    let ServerNotification::RawResponseCompleted(value) = notification else {
        return None;
    };
    let usage = value.usage.as_ref()?;
    Some(ModelUsageEvent {
        thread: &value.thread_id,
        turn: &value.turn_id,
        deduplication_key: format!("model.usage:{}", value.response_id),
        payload: json!({
            "totalTokens": usage.total_tokens,
            "inputTokens": usage.input_tokens,
            "cachedInputTokens": usage.cached_input_tokens,
            "cacheWriteInputTokens": usage.cache_write_input_tokens,
            "outputTokens": usage.output_tokens,
            "reasoningOutputTokens": usage.reasoning_output_tokens,
        }),
    })
}

fn stream_delta(notification: &ServerNotification) -> Option<StreamDelta<'_>> {
    let value = match notification {
        ServerNotification::AgentMessageDelta(value) => StreamDelta {
            thread: &value.thread_id,
            turn: &value.turn_id,
            key: stream_key("agent.message", &value.item_id, None),
            text: &value.delta,
        },
        ServerNotification::ReasoningSummaryTextDelta(value) => StreamDelta {
            thread: &value.thread_id,
            turn: &value.turn_id,
            key: stream_key(
                "reasoning.summary",
                &value.item_id,
                Some(value.summary_index),
            ),
            text: &value.delta,
        },
        ServerNotification::CommandExecutionOutputDelta(value) => StreamDelta {
            thread: &value.thread_id,
            turn: &value.turn_id,
            key: stream_key("tool.output", &value.item_id, None),
            text: &value.delta,
        },
        _ => return None,
    };
    Some(value)
}

fn map_event(notification: &ServerNotification) -> Option<MappedEvent<'_>> {
    let event = match notification {
        ServerNotification::TurnStarted(value) => MappedEvent {
            thread: &value.thread_id,
            turn: &value.turn.id,
            kind: "turn.started",
            payload: json!({ "status": value.turn.status }),
        },
        ServerNotification::TurnCompleted(value) => MappedEvent {
            thread: &value.thread_id,
            turn: &value.turn.id,
            kind: if value.turn.status == TurnStatus::Completed {
                "turn.completed"
            } else {
                "turn.error"
            },
            payload: json!({
                "status": value.turn.status,
                "message": value.turn.error.as_ref().map(ToString::to_string),
            }),
        },
        ServerNotification::TurnPlanUpdated(value) => MappedEvent {
            thread: &value.thread_id,
            turn: &value.turn_id,
            kind: "turn.plan",
            payload: json!({
                "summary": value.explanation.as_deref().and_then(compact).map(|v| brief(&v)),
                "steps": value.plan.iter().map(|step| json!({
                    "step": brief(&step.step), "status": step.status,
                })).collect::<Vec<_>>(),
            }),
        },
        ServerNotification::ItemStarted(value) => {
            let (kind, payload) = item_event(&value.item, false)?;
            MappedEvent {
                thread: &value.thread_id,
                turn: &value.turn_id,
                kind,
                payload,
            }
        }
        ServerNotification::FileChangePatchUpdated(value) => MappedEvent {
            thread: &value.thread_id,
            turn: &value.turn_id,
            kind: "file.updated",
            payload: json!({
                "itemId": value.item_id,
                "paths": value.changes.iter().map(|change| &change.path).collect::<Vec<_>>(),
            }),
        },
        ServerNotification::McpToolCallProgress(value) => MappedEvent {
            thread: &value.thread_id,
            turn: &value.turn_id,
            kind: "tool.progress",
            payload: json!({ "itemId": value.item_id, "message": brief(&value.message) }),
        },
        ServerNotification::Error(value) => MappedEvent {
            thread: &value.thread_id,
            turn: &value.turn_id,
            kind: if value.will_retry {
                "turn.warning"
            } else {
                "turn.error"
            },
            payload: json!({ "message": brief(&value.error.to_string()), "willRetry": value.will_retry }),
        },
        ServerNotification::ModelRerouted(value) => MappedEvent {
            thread: &value.thread_id,
            turn: &value.turn_id,
            kind: "turn.warning",
            payload: json!({
                "message": format!("model rerouted from {} to {}", value.from_model, value.to_model),
            }),
        },
        ServerNotification::ContextCompacted(value) => MappedEvent {
            thread: &value.thread_id,
            turn: &value.turn_id,
            kind: "context.compacted",
            payload: json!({}),
        },
        _ => return None,
    };
    Some(event)
}

fn record_streamed_content(streamed_content: &mut StreamedContent, key: &StreamKey) {
    match (key.kind, key.part) {
        ("agent.message", _) => {
            streamed_content.agent_messages.insert(key.item.clone());
        }
        ("reasoning.summary", Some(part)) => {
            streamed_content
                .reasoning_summary_parts
                .entry(key.item.clone())
                .or_default()
                .insert(part);
        }
        _ => {}
    }
}

fn completed_item_event(
    item: &ThreadItem,
    streamed_content: &mut StreamedContent,
) -> Option<(&'static str, Value)> {
    match item {
        ThreadItem::AgentMessage { id, phase, .. }
            if streamed_content.agent_messages.remove(id.as_str()) =>
        {
            Some((
                "agent.message.completed",
                json!({ "itemId": id, "phase": phase }),
            ))
        }
        ThreadItem::Reasoning { id, summary, .. } => {
            let Some(streamed_parts) = streamed_content.reasoning_summary_parts.remove(id) else {
                return item_event(item, true);
            };
            let mut remaining_summary = Vec::new();
            let mut remaining_indexes = Vec::new();
            for (index, part) in summary.iter().enumerate() {
                let index = index as i64;
                if !streamed_parts.contains(&index)
                    && let Some(part) = compact(part)
                {
                    remaining_summary.push(part);
                    remaining_indexes.push(index);
                }
            }
            Some((
                "reasoning.summary.completed",
                json!({
                    "itemId": id,
                    "summary": remaining_summary,
                    "summaryIndexes": remaining_indexes,
                }),
            ))
        }
        _ => item_event(item, true),
    }
}

fn item_event(item: &ThreadItem, completed: bool) -> Option<(&'static str, Value)> {
    let (kind, detail) = match item {
        ThreadItem::AgentMessage { text, phase, .. } => {
            let text = compact(text)?;
            (
                if completed {
                    "agent.message.completed"
                } else {
                    "agent.message.started"
                },
                json!({ "text": text, "phase": phase }),
            )
        }
        ThreadItem::Reasoning { summary, .. } => {
            let summary = summary
                .iter()
                .filter_map(|part| compact(part))
                .collect::<Vec<_>>();
            if summary.is_empty() {
                return None;
            }
            (
                if completed {
                    "reasoning.summary.completed"
                } else {
                    "reasoning.summary.started"
                },
                json!({ "summary": summary }),
            )
        }
        ThreadItem::CommandExecution {
            command,
            status,
            exit_code,
            ..
        } => (
            if completed {
                "tool.completed"
            } else {
                "tool.started"
            },
            json!({ "type": "command", "message": brief(command), "status": status, "exitCode": exit_code }),
        ),
        ThreadItem::McpToolCall {
            server,
            tool,
            status,
            ..
        } => (
            if completed {
                "tool.completed"
            } else {
                "tool.started"
            },
            json!({ "type": "mcp", "message": format!("{server}.{tool}"), "status": status }),
        ),
        ThreadItem::DynamicToolCall { tool, status, .. } => {
            let kind = if completed {
                "tool.completed"
            } else {
                "tool.started"
            };
            (
                kind,
                json!({ "type": "dynamic", "message": tool, "status": status }),
            )
        }
        ThreadItem::CollabAgentToolCall { tool, status, .. } => {
            let kind = if completed {
                "tool.completed"
            } else {
                "tool.started"
            };
            (
                kind,
                json!({ "type": "subagent", "tool": tool, "status": status }),
            )
        }
        ThreadItem::WebSearch(_) => (
            if completed {
                "tool.completed"
            } else {
                "tool.started"
            },
            json!({ "type": "webSearch" }),
        ),
        ThreadItem::FileChange {
            changes, status, ..
        } => (
            if completed {
                "file.completed"
            } else {
                "file.started"
            },
            json!({
                "paths": changes.iter().map(|change| &change.path).collect::<Vec<_>>(),
                "status": status,
            }),
        ),
        ThreadItem::ContextCompaction { .. } => (
            if completed {
                "context.compaction.completed"
            } else {
                "context.compaction.started"
            },
            json!({}),
        ),
        _ => return None,
    };
    let mut payload = detail.as_object().cloned().unwrap_or_default();
    payload.insert("itemId".to_string(), Value::String(item.id().to_string()));
    Some((kind, Value::Object(payload)))
}

fn stream_key(kind: &'static str, item: &str, part: Option<i64>) -> StreamKey {
    StreamKey {
        kind,
        item: item.to_string(),
        part,
    }
}

fn correlate(payload: Value, active: Option<&ActiveTurn>) -> Value {
    let mut payload = payload
        .as_object()
        .cloned()
        .unwrap_or_else(|| Map::from_iter([(String::from("detail"), payload)]));
    if let Some(active) = active {
        payload.insert("threadId".to_string(), Value::String(active.thread.clone()));
        payload.insert("turnId".to_string(), Value::String(active.turn.clone()));
    }
    Value::Object(payload)
}

fn targets(active: Option<&ActiveTurn>, thread: &str, turn: &str) -> bool {
    active.is_some_and(|active| active.thread == thread && active.turn == turn)
}

fn brief(value: &str) -> String {
    let value = compact(value).unwrap_or_default();
    let split = utf8_prefix(&value, CHUNK_BYTES.saturating_sub(3));
    if split == value.len() {
        value
    } else {
        format!("{}...", &value[..split])
    }
}

fn compact(value: &str) -> Option<String> {
    let mut output = String::new();
    let mut blank = false;
    for line in value.replace("\r\n", "\n").replace('\r', "\n").lines() {
        let line = line.trim_end();
        if line.is_empty() {
            blank |= !output.is_empty();
        } else {
            if !output.is_empty() {
                output.push('\n');
                if blank {
                    output.push('\n');
                }
            }
            output.push_str(line);
            blank = false;
        }
    }
    (!output.is_empty()).then_some(output)
}

fn utf8_prefix(value: &str, limit: usize) -> usize {
    let mut split = value.len().min(limit);
    while split > 0 && !value.is_char_boundary(split) {
        split -= 1;
    }
    if split == 0 && !value.is_empty() {
        value.chars().next().map_or(0, char::len_utf8)
    } else {
        split
    }
}

#[cfg(test)]
mod tests {
    use codex_app_server_protocol::RawResponseCompletedNotification;
    use codex_app_server_protocol::ReasoningTextDeltaNotification;
    use codex_app_server_protocol::TokenUsageBreakdown;

    use super::*;

    fn key(kind: &'static str, item: &str) -> StreamKey {
        stream_key(kind, item, None)
    }

    #[test]
    fn compacts_whitespace() {
        assert_eq!(
            compact("\nfirst  \n\n \nsecond\t\n"),
            Some("first\n\nsecond".into())
        );
    }

    #[test]
    fn chunks_without_splitting_utf8() {
        let mut text = TextCoalescer::with_limit(5);
        let mut chunks = text.push(key("agent.message", "one"), "aébcdef");
        chunks.extend(text.flush());
        assert_eq!(
            chunks
                .iter()
                .map(|chunk| chunk.text.as_str())
                .collect::<Vec<_>>(),
            ["aébc", "def"]
        );
    }

    #[test]
    fn stream_chunks_preserve_exact_join_boundaries() {
        let mut text = TextCoalescer::with_limit(5);
        let mut chunks = text.push(key("agent.message", "one"), "abcd efgh");
        chunks.extend(text.flush());
        assert_eq!(
            chunks
                .iter()
                .map(|chunk| chunk.text.as_str())
                .collect::<String>(),
            "abcd efgh"
        );
    }

    #[test]
    fn stream_change_flushes_pending_text() {
        let mut text = TextCoalescer::with_limit(100);
        assert!(
            text.push(key("reasoning.summary", "one"), "thinking")
                .is_empty()
        );
        let chunks = text.push(key("agent.message", "two"), "answer");
        assert_eq!(chunks[0].text, "thinking");
        assert_eq!(text.flush().expect("answer").text, "answer");
    }

    #[test]
    fn filter_requires_exact_thread_and_turn() {
        let active = ActiveTurn {
            thread: "t1".into(),
            turn: "u1".into(),
        };
        assert!(targets(Some(&active), "t1", "u1"));
        assert!(!targets(Some(&active), "t2", "u1"));
        assert!(!targets(Some(&active), "t1", "u2"));
    }

    #[test]
    fn agent_messages_are_mapped_with_text_and_phase() {
        let item = ThreadItem::AgentMessage {
            id: "message-1".into(),
            text: "Checking the workspace.  \n".into(),
            phase: None,
            memory_citation: None,
        };

        let (kind, payload) = item_event(&item, true).expect("agent message event");

        assert_eq!(kind, "agent.message.completed");
        assert_eq!(
            payload,
            json!({
                "itemId": "message-1",
                "phase": null,
                "text": "Checking the workspace.",
            })
        );
    }

    #[test]
    fn reasoning_maps_only_model_provided_summaries() {
        let item = ThreadItem::Reasoning {
            id: "reasoning-1".into(),
            summary: vec![
                "Inspecting the event mapper.  ".into(),
                "\nVerifying the CLI output.\n".into(),
            ],
            content: vec!["private raw reasoning must not be persisted".into()],
        };

        let (kind, payload) = item_event(&item, true).expect("reasoning summary event");

        assert_eq!(kind, "reasoning.summary.completed");
        assert_eq!(
            payload,
            json!({
                "itemId": "reasoning-1",
                "summary": [
                    "Inspecting the event mapper.",
                    "Verifying the CLI output.",
                ],
            })
        );
        assert!(!payload.to_string().contains("private raw reasoning"));
    }

    #[test]
    fn empty_or_unhandled_items_do_not_emit_generic_item_spam() {
        let empty_message: ThreadItem = serde_json::from_value(json!({
            "type": "agentMessage",
            "id": "message-empty",
            "text": "",
            "phase": "commentary",
        }))
        .expect("empty agent message fixture");
        let raw_reasoning_only = ThreadItem::Reasoning {
            id: "reasoning-raw".into(),
            summary: Vec::new(),
            content: vec!["private".into()],
        };
        let plan = ThreadItem::Plan {
            id: "plan-1".into(),
            text: "already represented by turn.plan".into(),
        };

        assert!(item_event(&empty_message, false).is_none());
        assert!(item_event(&raw_reasoning_only, true).is_none());
        assert!(item_event(&plan, true).is_none());
    }

    #[test]
    fn raw_reasoning_deltas_are_not_user_visible_streams() {
        let notification = ServerNotification::ReasoningTextDelta(ReasoningTextDeltaNotification {
            thread_id: "thread-1".into(),
            turn_id: "turn-1".into(),
            item_id: "reasoning-1".into(),
            delta: "private raw reasoning".into(),
            content_index: 0,
        });

        assert!(stream_delta(&notification).is_none());
    }

    #[test]
    fn exact_response_usage_maps_to_a_deduplicated_aggregate_event() {
        let notification =
            ServerNotification::RawResponseCompleted(RawResponseCompletedNotification {
                thread_id: "thread-1".into(),
                turn_id: "turn-1".into(),
                response_id: "response-1".into(),
                usage: Some(TokenUsageBreakdown {
                    total_tokens: 160,
                    input_tokens: 100,
                    cached_input_tokens: 40,
                    cache_write_input_tokens: 10,
                    output_tokens: 60,
                    reasoning_output_tokens: 20,
                }),
            });

        let event = model_usage_event(&notification).expect("model usage event");

        assert_eq!(event.thread, "thread-1");
        assert_eq!(event.turn, "turn-1");
        assert_eq!(event.deduplication_key, "model.usage:response-1");
        assert_eq!(
            event.payload,
            json!({
                "totalTokens": 160,
                "inputTokens": 100,
                "cachedInputTokens": 40,
                "cacheWriteInputTokens": 10,
                "outputTokens": 60,
                "reasoningOutputTokens": 20,
            })
        );
        assert!(!event.payload.to_string().contains("response-1"));
    }

    #[test]
    fn response_without_usage_does_not_emit_metrics() {
        let notification =
            ServerNotification::RawResponseCompleted(RawResponseCompletedNotification {
                thread_id: "thread-1".into(),
                turn_id: "turn-1".into(),
                response_id: "response-1".into(),
                usage: None,
            });

        assert!(model_usage_event(&notification).is_none());
    }

    #[test]
    fn streamed_delta_keeps_completion_metadata_without_duplicate_text() {
        let item = ThreadItem::AgentMessage {
            id: "message-streamed".into(),
            text: "The complete streamed response.".into(),
            phase: None,
            memory_citation: None,
        };
        let mut streamed_content = StreamedContent::default();
        record_streamed_content(
            &mut streamed_content,
            &stream_key("agent.message", item.id(), None),
        );

        let (kind, payload) =
            completed_item_event(&item, &mut streamed_content).expect("completion metadata");
        assert_eq!(kind, "agent.message.completed");
        assert_eq!(payload["itemId"], "message-streamed");
        assert!(payload["phase"].is_null());
        assert!(payload.get("text").is_none());
        assert!(streamed_content.agent_messages.is_empty());
    }

    #[test]
    fn completion_only_provider_keeps_the_full_structured_event() {
        let item = ThreadItem::Reasoning {
            id: "reasoning-completed-only".into(),
            summary: vec!["Completed-only summary.".into()],
            content: vec!["private raw reasoning".into()],
        };
        let mut streamed_content = StreamedContent::default();

        let (kind, payload) = completed_item_event(&item, &mut streamed_content)
            .expect("completion-only reasoning event");

        assert_eq!(kind, "reasoning.summary.completed");
        assert_eq!(
            payload,
            json!({
                "itemId": "reasoning-completed-only",
                "summary": ["Completed-only summary."],
            })
        );
        assert!(!payload.to_string().contains("private raw reasoning"));
    }

    #[test]
    fn mixed_reasoning_completion_emits_only_unstreamed_summary_parts() {
        let item = ThreadItem::Reasoning {
            id: "reasoning-mixed".into(),
            summary: vec![
                "Already streamed first.".into(),
                "Completion-only middle.".into(),
                "Already streamed last.".into(),
            ],
            content: vec!["private raw reasoning".into()],
        };
        let mut streamed_content = StreamedContent::default();
        record_streamed_content(
            &mut streamed_content,
            &stream_key("reasoning.summary", item.id(), Some(0)),
        );
        record_streamed_content(
            &mut streamed_content,
            &stream_key("reasoning.summary", item.id(), Some(2)),
        );

        let (kind, payload) = completed_item_event(&item, &mut streamed_content)
            .expect("missing reasoning summary part");

        assert_eq!(kind, "reasoning.summary.completed");
        assert_eq!(
            payload,
            json!({
                "itemId": "reasoning-mixed",
                "summary": ["Completion-only middle."],
                "summaryIndexes": [1],
            })
        );
        assert!(streamed_content.reasoning_summary_parts.is_empty());
        assert!(!payload.to_string().contains("private raw reasoning"));
    }

    #[test]
    fn fully_streamed_reasoning_keeps_completion_metadata() {
        let item = ThreadItem::Reasoning {
            id: "reasoning-streamed".into(),
            summary: vec!["First.".into(), "Second.".into()],
            content: vec!["private raw reasoning".into()],
        };
        let mut streamed_content = StreamedContent::default();
        for index in [0, 1] {
            record_streamed_content(
                &mut streamed_content,
                &stream_key("reasoning.summary", item.id(), Some(index)),
            );
        }

        let (kind, payload) = completed_item_event(&item, &mut streamed_content)
            .expect("reasoning completion metadata");

        assert_eq!(kind, "reasoning.summary.completed");
        assert_eq!(
            payload,
            json!({
                "itemId": "reasoning-streamed",
                "summary": [],
                "summaryIndexes": [],
            })
        );
        assert!(streamed_content.reasoning_summary_parts.is_empty());
    }
}
