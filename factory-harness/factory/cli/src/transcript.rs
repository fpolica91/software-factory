use std::collections::BTreeMap;
use std::collections::HashMap;

use factory_coordinator::DurableJob;
use factory_coordinator::JobEventRecord;
use serde_json::Value;

const PREVIEW_CHARS: usize = 140;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum EventFamily {
    Agent,
    Context,
    File,
    Plan,
    Reasoning,
    Stage,
    Tool,
    Turn,
    Other,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum EventTone {
    Complete,
    Error,
    Muted,
    Running,
    Warning,
}

#[derive(Clone, Debug)]
struct EventHeader {
    sequence: u64,
    kind: String,
    created_at: String,
}

#[derive(Clone, Debug)]
pub(crate) struct TranscriptRow {
    family: EventFamily,
    events: Vec<EventHeader>,
    operation_id: Option<String>,
    stage: Option<String>,
    phase: Option<String>,
    tool_type: Option<String>,
    last_detail: String,
    preview: String,
    primary_text: String,
    streamed_text: String,
    reasoning_parts: BTreeMap<i64, String>,
}

impl TranscriptRow {
    fn new(family: EventFamily, event: &JobEventRecord) -> Self {
        let mut row = Self {
            family,
            events: Vec::new(),
            operation_id: None,
            stage: None,
            phase: None,
            tool_type: None,
            last_detail: String::new(),
            preview: String::new(),
            primary_text: String::new(),
            streamed_text: String::new(),
            reasoning_parts: BTreeMap::new(),
        };
        row.update(event);
        row
    }

    fn update(&mut self, event: &JobEventRecord) {
        if let Some(stage) = event_stage(event) {
            self.stage = Some(stage);
        }
        if let Some(operation_id) = &event.operation_id {
            self.operation_id = Some(operation_id.to_string());
        }
        if let Some(phase) = event.payload.get("phase").and_then(Value::as_str) {
            self.phase = Some(phase.to_string());
        }
        if let Some(tool_type) = event.payload.get("type").and_then(Value::as_str) {
            self.tool_type = Some(tool_type.to_string());
        }
        self.record_indexed_reasoning(event);

        let detail = event_detail(&event.payload)
            .map(compact_lines)
            .unwrap_or_default();
        if !detail.is_empty() {
            self.last_detail.clone_from(&detail);
        }
        if is_stream_event(&event.kind) {
            if let Some(text) = event.payload.get("text").and_then(Value::as_str) {
                let part = event.payload.get("partIndex").and_then(Value::as_i64);
                if self.family == EventFamily::Reasoning
                    && let Some(part) = part
                {
                    self.reasoning_parts.entry(part).or_default().push_str(text);
                } else {
                    self.streamed_text.push_str(text);
                }
            } else if !detail.is_empty() {
                self.streamed_text.push_str(&detail);
            }
        } else if !detail.is_empty() {
            match self.family {
                EventFamily::Agent => {
                    self.primary_text = detail.clone();
                }
                EventFamily::Reasoning if event.payload.get("summaryIndexes").is_none() => {
                    self.primary_text = detail.clone();
                }
                EventFamily::Tool if event.kind == "tool.output" => {
                    self.streamed_text.push_str(&detail);
                }
                _ => {}
            }
        }

        let content = self.content();
        if matches!(self.family, EventFamily::Agent | EventFamily::Reasoning) && !content.is_empty()
        {
            self.preview = one_line(&content, PREVIEW_CHARS);
        } else if event.kind == "turn.plan" {
            self.preview = plan_preview(&event.payload);
        } else if !detail.is_empty() && event.kind != "tool.output" {
            self.preview = one_line(&detail, PREVIEW_CHARS);
        } else if self.preview.is_empty() {
            self.preview = fallback_preview(event);
        }
        self.events.push(EventHeader {
            sequence: event.sequence,
            kind: event.kind.clone(),
            created_at: event.created_at.to_rfc3339(),
        });
    }

    fn record_indexed_reasoning(&mut self, event: &JobEventRecord) {
        if self.family != EventFamily::Reasoning {
            return;
        }
        let Some(indexes) = event
            .payload
            .get("summaryIndexes")
            .and_then(Value::as_array)
        else {
            return;
        };
        let Some(parts) = event.payload.get("summary").and_then(Value::as_array) else {
            return;
        };
        for (index, part) in indexes.iter().zip(parts) {
            if let (Some(index), Some(part)) = (index.as_i64(), part.as_str()) {
                self.reasoning_parts.insert(index, part.to_string());
            }
        }
    }

    pub(crate) fn label(&self) -> &'static str {
        match self.family {
            EventFamily::Agent => match self.phase.as_deref() {
                Some("commentary") => "Update",
                _ => "Answer",
            },
            EventFamily::Context => "Context",
            EventFamily::File => "Files",
            EventFamily::Plan => "Plan",
            EventFamily::Reasoning => "Thinking",
            EventFamily::Stage => "Stage",
            EventFamily::Tool => {
                if self.tool_type.as_deref() == Some("subagent") {
                    "Subagent"
                } else {
                    "Tool"
                }
            }
            EventFamily::Turn => "Turn",
            EventFamily::Other => "Event",
        }
    }

    pub(crate) fn stage(&self) -> Option<&str> {
        self.stage.as_deref()
    }

    pub(crate) fn tone(&self) -> EventTone {
        let kind = self
            .events
            .last()
            .map(|event| event.kind.as_str())
            .unwrap_or_default();
        if kind.contains("error") || kind.contains("failed") {
            EventTone::Error
        } else if kind.contains("warning") || kind.contains("retry") {
            EventTone::Warning
        } else if kind.ends_with("completed") {
            EventTone::Complete
        } else if kind.ends_with("started") || is_stream_event(kind) || kind.ends_with("progress") {
            EventTone::Running
        } else {
            EventTone::Muted
        }
    }

    pub(crate) fn preview(&self, max_chars: usize) -> String {
        one_line(&self.preview, max_chars)
    }

    pub(crate) fn detail(&self) -> String {
        let mut output = match self.family {
            EventFamily::Agent | EventFamily::Reasoning => self.content(),
            EventFamily::Tool => {
                let streamed = compact_lines(self.streamed_text.clone());
                if streamed.is_empty() {
                    self.preview.clone()
                } else {
                    format!("{}\n\n{streamed}", self.preview)
                }
            }
            _ if self.last_detail.is_empty() => self.preview.clone(),
            _ => self.last_detail.clone(),
        };
        if !self.events.is_empty() {
            output.push_str("\n\nEvents");
            for event in &self.events {
                output.push_str(&format!(
                    "\n#{} {} · {}",
                    event.sequence, event.kind, event.created_at
                ));
            }
        }
        output
    }

    fn content(&self) -> String {
        if self.family == EventFamily::Reasoning && !self.reasoning_parts.is_empty() {
            return self
                .reasoning_parts
                .values()
                .map(|part| compact_lines(part.clone()))
                .collect::<Vec<_>>()
                .join("\n\n");
        }
        let streamed = compact_lines(self.streamed_text.clone());
        if !self.primary_text.is_empty() {
            self.primary_text.clone()
        } else {
            streamed
        }
    }

    fn resolve_stage(&mut self, operations: &HashMap<String, String>) {
        if self.stage.is_some() {
            return;
        }
        self.stage = self
            .operation_id
            .as_ref()
            .and_then(|operation_id| operations.get(operation_id))
            .cloned();
    }
}

#[derive(Debug, Default)]
pub(crate) struct Transcript {
    rows: Vec<TranscriptRow>,
    row_indexes: HashMap<String, usize>,
}

impl Transcript {
    pub(crate) fn ingest(&mut self, event: &JobEventRecord) {
        let family = event_family(&event.kind);
        if matches!(family, EventFamily::Stage | EventFamily::Turn)
            && !event.kind.contains("error")
            && !event.kind.contains("warning")
            && !event.kind.contains("retry")
        {
            return;
        }
        let key = event_group_key(event, family);
        if let Some(index) = self.row_indexes.get(&key).copied() {
            self.rows[index].update(event);
        } else {
            let index = self.rows.len();
            self.rows.push(TranscriptRow::new(family, event));
            self.row_indexes.insert(key, index);
        }
    }

    pub(crate) fn rows(&self) -> &[TranscriptRow] {
        &self.rows
    }

    pub(crate) fn correlate_job(&mut self, job: &DurableJob) {
        let operations = job
            .operations
            .iter()
            .map(|operation| {
                (
                    operation.operation_id.to_string(),
                    operation
                        .kind
                        .strip_prefix("codex.")
                        .unwrap_or(&operation.kind)
                        .to_string(),
                )
            })
            .collect::<HashMap<_, _>>();
        for row in &mut self.rows {
            row.resolve_stage(&operations);
        }
    }
}

pub(crate) fn compact_event_line(event: &JobEventRecord) -> Option<String> {
    let important = event.kind.contains("error")
        || event.kind.contains("warning")
        || event.kind.contains("retry")
        || event.kind.contains("recover")
        || event.kind.contains("cancel")
        || event.kind == "context.compacted"
        || event.kind == "turn.plan";
    if !important {
        return None;
    }
    let family = event_family(&event.kind);
    let row = TranscriptRow::new(family, event);
    let stage = row
        .stage()
        .map(|stage| format!("[{stage}] "))
        .unwrap_or_default();
    Some(format!(
        "{}{}: {}",
        stage,
        row.label(),
        row.preview(PREVIEW_CHARS)
    ))
}

pub(crate) fn event_detail(value: &Value) -> Option<String> {
    if let Some(steps) = value.get("steps").and_then(Value::as_array) {
        let mut details = value
            .get("summary")
            .and_then(Value::as_str)
            .filter(|summary| !summary.trim().is_empty())
            .map(|summary| vec![summary.to_string()])
            .unwrap_or_default();
        details.extend(steps.iter().filter_map(|item| {
            let step = item.get("step")?.as_str()?.trim();
            if step.is_empty() {
                return None;
            }
            Some(match item.get("status").and_then(Value::as_str) {
                Some(status) => format!("[{status}] {step}"),
                None => step.to_string(),
            })
        }));
        if !details.is_empty() {
            return Some(details.join("\n"));
        }
    }
    for key in ["message", "text", "summary", "method", "detail"] {
        match value.get(key) {
            Some(Value::String(text)) if !text.trim().is_empty() => return Some(text.clone()),
            Some(Value::Array(items)) => {
                let text = items
                    .iter()
                    .filter_map(Value::as_str)
                    .filter(|text| !text.trim().is_empty())
                    .collect::<Vec<_>>()
                    .join("\n\n");
                if !text.is_empty() {
                    return Some(text);
                }
            }
            Some(other) if !other.is_null() && !other.is_object() => {
                return Some(other.to_string());
            }
            _ => {}
        }
    }
    if let Some(tool) = value.get("tool").and_then(Value::as_str) {
        return Some(match value.get("type").and_then(Value::as_str) {
            Some("subagent") => format!("subagent: {tool}"),
            _ => tool.to_string(),
        });
    }
    if let Some(paths) = value.get("paths").and_then(Value::as_array) {
        let paths = paths
            .iter()
            .filter_map(Value::as_str)
            .filter(|path| !path.trim().is_empty())
            .collect::<Vec<_>>()
            .join(", ");
        if !paths.is_empty() {
            return Some(match value.get("status").and_then(Value::as_str) {
                Some(status) => format!("{paths} ({status})"),
                None => paths,
            });
        }
    }
    if let Some(kind) = value.get("type").and_then(Value::as_str) {
        return Some(match kind {
            "webSearch" => "web search".to_string(),
            _ => kind.to_string(),
        });
    }
    if let Some(status) = value.get("status") {
        if let Some(status) = status.as_str().filter(|status| !status.trim().is_empty()) {
            return Some(status.to_string());
        }
        if !status.is_null() && !status.is_object() {
            return Some(status.to_string());
        }
    }
    let item = value.get("item")?;
    if let Some(text) = item.get("text").and_then(Value::as_str) {
        return Some(text.to_string());
    }
    item.get("type")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
}

pub(crate) fn compact_lines(value: String) -> String {
    let mut compact = String::new();
    let mut previous_blank = false;
    for line in value.trim().lines() {
        let blank = line.trim().is_empty();
        if blank && previous_blank {
            continue;
        }
        if !compact.is_empty() {
            compact.push('\n');
        }
        compact.push_str(line.trim_end());
        previous_blank = blank;
    }
    compact
}

fn event_family(kind: &str) -> EventFamily {
    if kind.starts_with("agent.message") {
        EventFamily::Agent
    } else if kind.starts_with("reasoning.summary") {
        EventFamily::Reasoning
    } else if kind.starts_with("tool.") {
        EventFamily::Tool
    } else if kind.starts_with("file.") {
        EventFamily::File
    } else if kind == "turn.plan" {
        EventFamily::Plan
    } else if kind.starts_with("turn.") {
        EventFamily::Turn
    } else if kind.starts_with("stage.") || kind.starts_with("review_cycle.") {
        EventFamily::Stage
    } else if kind.starts_with("context.") {
        EventFamily::Context
    } else {
        EventFamily::Other
    }
}

fn event_group_key(event: &JobEventRecord, family: EventFamily) -> String {
    if event.kind.contains("error") || event.kind.contains("warning") {
        return format!("event:{}", event.sequence);
    }
    let attempt = event
        .attempt_id
        .as_ref()
        .map(ToString::to_string)
        .unwrap_or_default();
    let turn = event
        .payload
        .get("turnId")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let item = event
        .payload
        .get("itemId")
        .and_then(Value::as_str)
        .unwrap_or_default();
    match family {
        EventFamily::Agent | EventFamily::File | EventFamily::Reasoning | EventFamily::Tool
            if !item.is_empty() =>
        {
            format!("{family:?}:{attempt}:{turn}:{item}")
        }
        EventFamily::Plan if !turn.is_empty() => format!("plan:{attempt}:{turn}"),
        EventFamily::Turn if !turn.is_empty() => format!("turn:{attempt}:{turn}"),
        EventFamily::Stage => {
            let stage = event_stage(event).unwrap_or_else(|| "stage".to_string());
            let cycle = event
                .payload
                .get("reviewCycle")
                .and_then(Value::as_u64)
                .unwrap_or_default();
            format!("stage:{attempt}:{stage}:{cycle}")
        }
        _ => format!("event:{}", event.sequence),
    }
}

fn event_stage(event: &JobEventRecord) -> Option<String> {
    event
        .payload
        .get("operationKind")
        .and_then(Value::as_str)
        .or_else(|| event.payload.get("stage").and_then(Value::as_str))
        .or_else(|| event.payload.get("operation").and_then(Value::as_str))
        .map(|stage| stage.strip_prefix("codex.").unwrap_or(stage).to_string())
}

fn is_stream_event(kind: &str) -> bool {
    matches!(kind, "agent.message" | "reasoning.summary" | "tool.output")
}

fn fallback_preview(event: &JobEventRecord) -> String {
    if event.kind == "context.compacted" {
        "Conversation compacted".to_string()
    } else {
        event.kind.replace('.', " ")
    }
}

fn plan_preview(payload: &Value) -> String {
    let Some(steps) = payload.get("steps").and_then(Value::as_array) else {
        return payload
            .get("summary")
            .and_then(Value::as_str)
            .map(|summary| one_line(summary, PREVIEW_CHARS))
            .unwrap_or_else(|| "Plan updated".to_string());
    };
    let complete = steps
        .iter()
        .filter(|step| {
            matches!(
                step.get("status").and_then(Value::as_str),
                Some("completed")
            )
        })
        .count();
    let active = steps
        .iter()
        .find(|step| {
            matches!(
                step.get("status").and_then(Value::as_str),
                Some("inProgress" | "in_progress")
            )
        })
        .and_then(|step| step.get("step"))
        .and_then(Value::as_str);
    match active {
        Some(active) => format!(
            "{complete}/{} complete · {}",
            steps.len(),
            one_line(active, 90)
        ),
        None => format!("{complete}/{} complete", steps.len()),
    }
}

fn one_line(value: &str, max_chars: usize) -> String {
    let mut output = String::new();
    let mut whitespace = false;
    for character in value.trim().chars() {
        if character.is_whitespace() {
            whitespace |= !output.is_empty();
            continue;
        }
        if whitespace {
            output.push(' ');
            whitespace = false;
        }
        output.push(character);
        if output.chars().count() >= max_chars {
            output.push('…');
            break;
        }
    }
    output
}

#[cfg(test)]
mod tests {
    use factory_coordinator::JobEventRecord;
    use serde_json::Value;
    use serde_json::json;

    use super::Transcript;

    fn event(sequence: u64, kind: &str, payload: Value) -> JobEventRecord {
        serde_json::from_value(json!({
            "sequence": sequence,
            "jobId": "job-compact-output",
            "operationId": "operation-review",
            "attemptId": "attempt-review-1",
            "kind": kind,
            "payload": payload,
            "createdAt": "2026-08-02T00:00:00Z",
        }))
        .unwrap()
    }

    #[test]
    fn lifecycle_pairs_collapse_into_one_row() {
        let payload = json!({
            "itemId": "tool-1",
            "threadId": "thread-1",
            "turnId": "turn-1",
            "type": "command",
            "message": "cargo check --workspace",
        });
        let mut transcript = Transcript::default();
        transcript.ingest(&event(1, "tool.started", payload.clone()));
        transcript.ingest(&event(2, "tool.completed", payload));

        assert_eq!(transcript.rows().len(), 1);
        assert_eq!(transcript.rows()[0].preview(200), "cargo check --workspace");
        assert!(transcript.rows()[0].detail().contains("tool.started"));
        assert!(transcript.rows()[0].detail().contains("tool.completed"));
    }

    #[test]
    fn errors_are_never_folded_together() {
        let mut transcript = Transcript::default();
        transcript.ingest(&event(
            1,
            "turn.error",
            json!({ "turnId": "turn-1", "message": "first" }),
        ));
        transcript.ingest(&event(
            2,
            "turn.error",
            json!({ "turnId": "turn-1", "message": "second" }),
        ));
        assert_eq!(transcript.rows().len(), 2);
    }

    #[test]
    fn reasoning_parts_render_in_index_order_and_finish() {
        let base = json!({
            "itemId": "reasoning-1",
            "threadId": "thread-1",
            "turnId": "turn-1",
        });
        let mut first = base.clone();
        first["partIndex"] = json!(0);
        first["text"] = json!("zero");
        let mut third = base.clone();
        third["partIndex"] = json!(2);
        third["text"] = json!("two");
        let mut completed = base;
        completed["summary"] = json!(["one"]);
        completed["summaryIndexes"] = json!([1]);

        let mut transcript = Transcript::default();
        transcript.ingest(&event(1, "reasoning.summary", first));
        transcript.ingest(&event(2, "reasoning.summary", third));
        transcript.ingest(&event(3, "reasoning.summary.completed", completed));

        assert_eq!(transcript.rows().len(), 1);
        assert_eq!(transcript.rows()[0].preview(200), "zero one two");
        assert_eq!(transcript.rows()[0].tone(), super::EventTone::Complete);
        assert!(
            transcript.rows()[0]
                .detail()
                .starts_with("zero\n\none\n\ntwo")
        );
    }
}
