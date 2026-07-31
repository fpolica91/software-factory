use crate::ids::ItemId;
use crate::turn::FactoryUserInput;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;
use ts_rs::TS;

/// Stable Factory projection of a Codex thread item.
///
/// Every known projection carries the complete original payload in `raw`, so
/// consumers can use the stable fields without losing pinned upstream data.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
#[ts(rename_all_fields = "camelCase")]
pub enum FactoryItem {
    #[serde(rename_all = "camelCase")]
    UserMessage {
        id: ItemId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[schemars(with = "String")]
        #[ts(optional)]
        client_id: Option<String>,
        content: Vec<FactoryUserInput>,
        raw: Value,
    },
    #[serde(rename_all = "camelCase")]
    AgentMessage {
        id: ItemId,
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[schemars(with = "String")]
        #[ts(optional)]
        phase: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[schemars(with = "Value")]
        #[ts(optional)]
        memory_citation: Option<Value>,
        raw: Value,
    },
    Plan {
        id: ItemId,
        text: String,
        raw: Value,
    },
    Reasoning {
        id: ItemId,
        summary: Vec<String>,
        content: Vec<String>,
        raw: Value,
    },
    #[serde(rename_all = "camelCase")]
    CommandExecution {
        id: ItemId,
        command: String,
        cwd: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[schemars(with = "String")]
        #[ts(optional)]
        process_id: Option<String>,
        status: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[schemars(with = "String")]
        #[ts(optional)]
        aggregated_output: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[schemars(with = "i32")]
        #[ts(optional)]
        exit_code: Option<i32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[schemars(with = "i64")]
        #[ts(optional, type = "number")]
        duration_ms: Option<i64>,
        raw: Value,
    },
    FileChange {
        id: ItemId,
        changes: Value,
        status: String,
        raw: Value,
    },
    #[serde(rename_all = "camelCase")]
    McpToolCall {
        id: ItemId,
        server: String,
        tool: String,
        status: String,
        arguments: Value,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[schemars(with = "Value")]
        #[ts(optional)]
        result: Option<Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[schemars(with = "Value")]
        #[ts(optional)]
        error: Option<Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[schemars(with = "i64")]
        #[ts(optional, type = "number")]
        duration_ms: Option<i64>,
        raw: Value,
    },
    #[serde(rename_all = "camelCase")]
    DynamicToolCall {
        id: ItemId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[schemars(with = "String")]
        #[ts(optional)]
        namespace: Option<String>,
        tool: String,
        arguments: Value,
        status: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[schemars(with = "Value")]
        #[ts(optional)]
        content_items: Option<Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[schemars(with = "bool")]
        #[ts(optional)]
        success: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[schemars(with = "i64")]
        #[ts(optional, type = "number")]
        duration_ms: Option<i64>,
        raw: Value,
    },
    #[serde(rename_all = "camelCase")]
    CollabAgentToolCall {
        id: ItemId,
        tool: String,
        status: String,
        sender_thread_id: String,
        receiver_thread_ids: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[schemars(with = "String")]
        #[ts(optional)]
        prompt: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[schemars(with = "String")]
        #[ts(optional)]
        model: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[schemars(with = "String")]
        #[ts(optional)]
        reasoning_effort: Option<String>,
        agents_states: Value,
        raw: Value,
    },
    ContextCompaction {
        id: ItemId,
        raw: Value,
    },
    /// Forward-compatible representation for a new or intentionally opaque
    /// upstream item. `value` is the complete original item payload.
    #[serde(rename_all = "camelCase")]
    Unknown {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[schemars(with = "ItemId")]
        #[ts(optional)]
        id: Option<ItemId>,
        upstream_type: String,
        value: Value,
    },
}

impl FactoryItem {
    pub fn id(&self) -> Option<&ItemId> {
        match self {
            Self::UserMessage { id, .. }
            | Self::AgentMessage { id, .. }
            | Self::Plan { id, .. }
            | Self::Reasoning { id, .. }
            | Self::CommandExecution { id, .. }
            | Self::FileChange { id, .. }
            | Self::McpToolCall { id, .. }
            | Self::DynamicToolCall { id, .. }
            | Self::CollabAgentToolCall { id, .. }
            | Self::ContextCompaction { id, .. } => Some(id),
            Self::Unknown { id, .. } => id.as_ref(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
#[ts(rename_all_fields = "camelCase")]
pub enum FactoryItemDelta {
    AgentMessage {
        delta: String,
        raw: Value,
    },
    Plan {
        delta: String,
        raw: Value,
    },
    #[serde(rename_all = "camelCase")]
    ReasoningSummaryText {
        delta: String,
        #[ts(type = "number")]
        summary_index: i64,
        raw: Value,
    },
    #[serde(rename_all = "camelCase")]
    ReasoningSummaryPartAdded {
        #[ts(type = "number")]
        summary_index: i64,
        raw: Value,
    },
    #[serde(rename_all = "camelCase")]
    ReasoningText {
        delta: String,
        #[ts(type = "number")]
        content_index: i64,
        raw: Value,
    },
    CommandExecutionOutput {
        delta: String,
        raw: Value,
    },
    FileChangeOutput {
        delta: String,
        raw: Value,
    },
    FileChangePatchUpdated {
        changes: Value,
        raw: Value,
    },
    McpToolCallProgress {
        message: String,
        raw: Value,
    },
    #[serde(rename_all = "camelCase")]
    TerminalInteraction {
        process_id: String,
        stdin: String,
        raw: Value,
    },
    #[serde(rename_all = "camelCase")]
    Unknown {
        upstream_method: String,
        value: Value,
    },
}
