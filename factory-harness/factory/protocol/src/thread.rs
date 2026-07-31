use crate::ids::ThreadId;
use crate::ids::TurnId;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeMap;
use ts_rs::TS;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "kebab-case")]
pub enum FactoryApprovalPolicy {
    #[serde(rename = "untrusted")]
    Untrusted,
    OnRequest,
    Never,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "kebab-case")]
pub enum FactorySandboxMode {
    ReadOnly,
    WorkspaceWrite,
    DangerFullAccess,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "lowercase")]
pub enum FactoryPersonality {
    None,
    Friendly,
    Pragmatic,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct ThreadStartRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "String")]
    #[ts(optional)]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "String")]
    #[ts(optional)]
    pub model_provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "String")]
    #[ts(optional)]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "FactoryApprovalPolicy")]
    #[ts(optional)]
    pub approval_policy: Option<FactoryApprovalPolicy>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "FactorySandboxMode")]
    #[ts(optional)]
    pub sandbox: Option<FactorySandboxMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "BTreeMap<String, Value>")]
    #[ts(optional)]
    pub config: Option<BTreeMap<String, Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "String")]
    #[ts(optional)]
    pub service_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "String")]
    #[ts(optional)]
    pub base_instructions: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "String")]
    #[ts(optional)]
    pub developer_instructions: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "FactoryPersonality")]
    #[ts(optional)]
    pub personality: Option<FactoryPersonality>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "bool")]
    #[ts(optional)]
    pub ephemeral: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct ThreadResumeRequest {
    pub thread_id: ThreadId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct ThreadForkRequest {
    pub thread_id: ThreadId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "TurnId")]
    #[ts(optional)]
    pub last_turn_id: Option<TurnId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct ThreadCompactRequest {
    pub thread_id: ThreadId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
#[ts(rename_all_fields = "camelCase")]
pub enum FactoryThreadStatus {
    NotLoaded {
        raw: Value,
    },
    Idle {
        raw: Value,
    },
    SystemError {
        raw: Value,
    },
    #[serde(rename_all = "camelCase")]
    Active {
        active_flags: Vec<FactoryThreadActiveFlag>,
        raw: Value,
    },
    #[serde(rename_all = "camelCase")]
    Unknown {
        upstream_status: String,
        raw: Value,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub enum FactoryThreadActiveFlag {
    WaitingOnApproval,
    WaitingOnUserInput,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct FactoryThread {
    pub id: ThreadId,
    pub session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "ThreadId")]
    #[ts(optional)]
    pub forked_from_id: Option<ThreadId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "ThreadId")]
    #[ts(optional)]
    pub parent_thread_id: Option<ThreadId>,
    pub preview: String,
    pub ephemeral: bool,
    pub model_provider: String,
    #[ts(type = "number")]
    pub created_at: i64,
    #[ts(type = "number")]
    pub updated_at: i64,
    pub status: FactoryThreadStatus,
    pub cwd: String,
    /// Complete pinned app-server thread payload, including turns, items,
    /// pagination-related fields, and fields not projected in Protocol V1.
    pub raw: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct ThreadStartResponse {
    pub thread: FactoryThread,
    pub model: String,
    pub model_provider: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "String")]
    #[ts(optional)]
    pub service_tier: Option<String>,
    pub cwd: String,
    /// Complete pinned app-server response payload.
    pub raw: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct ThreadResumeResponse {
    pub thread: FactoryThread,
    /// Complete response payload, including initial turn pages and cursors.
    pub raw: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct ThreadForkResponse {
    pub thread: FactoryThread,
    /// Complete response payload, including fork history and loaded items.
    pub raw: Value,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
pub struct ThreadCompactResponse {}
