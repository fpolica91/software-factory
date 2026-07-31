use crate::ids::ThreadId;
use crate::ids::TurnId;
use crate::item::FactoryItem;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;
use std::path::PathBuf;
use ts_rs::TS;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "lowercase")]
pub enum FactoryImageDetail {
    Auto,
    Low,
    High,
    Original,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
#[ts(rename_all_fields = "camelCase")]
pub enum FactoryUserInput {
    Text {
        text: String,
    },
    Image {
        url: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[schemars(with = "FactoryImageDetail")]
        #[ts(optional)]
        detail: Option<FactoryImageDetail>,
    },
    LocalImage {
        path: PathBuf,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[schemars(with = "FactoryImageDetail")]
        #[ts(optional)]
        detail: Option<FactoryImageDetail>,
    },
    Audio {
        url: String,
    },
    LocalAudio {
        path: PathBuf,
    },
    Skill {
        name: String,
        path: PathBuf,
    },
    Mention {
        name: String,
        path: String,
    },
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub enum FactoryTurnMode {
    #[default]
    Normal,
    Plan,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct TurnStartRequest {
    pub thread_id: ThreadId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "String")]
    #[ts(optional)]
    pub client_user_message_id: Option<String>,
    pub input: Vec<FactoryUserInput>,
    #[serde(default)]
    pub mode: FactoryTurnMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "String")]
    #[ts(optional)]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "PathBuf")]
    #[ts(optional)]
    pub cwd: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "Value")]
    #[ts(optional)]
    pub output_schema: Option<Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct TurnSteerRequest {
    pub thread_id: ThreadId,
    pub expected_turn_id: TurnId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "String")]
    #[ts(optional)]
    pub client_user_message_id: Option<String>,
    pub input: Vec<FactoryUserInput>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct TurnInterruptRequest {
    pub thread_id: ThreadId,
    pub turn_id: TurnId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub enum FactoryTurnStatus {
    Completed,
    Interrupted,
    Failed,
    InProgress,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct FactoryTurnError {
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "Value")]
    #[ts(optional)]
    pub codex_error_info: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "String")]
    #[ts(optional)]
    pub additional_details: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct FactoryTurn {
    pub id: TurnId,
    pub items: Vec<FactoryItem>,
    pub status: FactoryTurnStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "FactoryTurnError")]
    #[ts(optional)]
    pub error: Option<FactoryTurnError>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "i64")]
    #[ts(optional, type = "number")]
    pub started_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "i64")]
    #[ts(optional, type = "number")]
    pub completed_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "i64")]
    #[ts(optional, type = "number")]
    pub duration_ms: Option<i64>,
    /// Complete pinned app-server turn payload, including the original items
    /// and item-view metadata.
    pub raw: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct TurnStartResponse {
    pub turn: FactoryTurn,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct TurnSteerResponse {
    pub turn_id: TurnId,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
pub struct TurnInterruptResponse {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct FactoryPlanStep {
    pub step: String,
    pub status: FactoryPlanStepStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub enum FactoryPlanStepStatus {
    Pending,
    InProgress,
    Completed,
}
