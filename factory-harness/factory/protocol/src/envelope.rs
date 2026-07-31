use crate::error::FactoryErrorEnvelope;
use crate::event::FactoryEvent;
use crate::ids::FactoryRequestId;
use crate::server_request::FactoryServerErrorResponse;
use crate::server_request::FactoryServerResponse;
use crate::thread::ThreadCompactRequest;
use crate::thread::ThreadCompactResponse;
use crate::thread::ThreadForkRequest;
use crate::thread::ThreadForkResponse;
use crate::thread::ThreadResumeRequest;
use crate::thread::ThreadResumeResponse;
use crate::thread::ThreadStartRequest;
use crate::thread::ThreadStartResponse;
use crate::turn::TurnInterruptRequest;
use crate::turn::TurnInterruptResponse;
use crate::turn::TurnStartRequest;
use crate::turn::TurnStartResponse;
use crate::turn::TurnSteerRequest;
use crate::turn::TurnSteerResponse;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use ts_rs::TS;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct FactoryClientInfo {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "String")]
    #[ts(optional)]
    pub title: Option<String>,
    pub version: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct FactoryInitializeCapabilities {
    #[serde(default)]
    pub experimental_api: bool,
    #[serde(default)]
    pub request_attestation: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub mcp_server_openai_form_elicitation: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "Vec<String>")]
    #[ts(optional)]
    pub opt_out_notification_methods: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct InitializeRequest {
    pub client_info: FactoryClientInfo,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "FactoryInitializeCapabilities")]
    #[ts(optional)]
    pub capabilities: Option<FactoryInitializeCapabilities>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct InitializeResponse {
    pub user_agent: String,
    pub codex_home: String,
    pub platform_family: String,
    pub platform_os: String,
}

/// Stable operation discriminator used to correlate a response with the
/// request that selected its result schema.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema, TS)]
pub enum FactoryMethod {
    #[serde(rename = "initialize")]
    Initialize,
    #[serde(rename = "thread/start")]
    ThreadStart,
    #[serde(rename = "thread/resume")]
    ThreadResume,
    #[serde(rename = "thread/fork")]
    ThreadFork,
    #[serde(rename = "thread/compact/start")]
    ThreadCompactStart,
    #[serde(rename = "turn/start")]
    TurnStart,
    #[serde(rename = "turn/steer")]
    TurnSteer,
    #[serde(rename = "turn/interrupt")]
    TurnInterrupt,
}

impl FactoryMethod {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Initialize => "initialize",
            Self::ThreadStart => "thread/start",
            Self::ThreadResume => "thread/resume",
            Self::ThreadFork => "thread/fork",
            Self::ThreadCompactStart => "thread/compact/start",
            Self::TurnStart => "turn/start",
            Self::TurnSteer => "turn/steer",
            Self::TurnInterrupt => "turn/interrupt",
        }
    }
}

/// Factory-owned request surface. Method names intentionally match the stable
/// app-server V2 operations selected for Protocol V1.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(tag = "method", content = "params")]
pub enum FactoryRequest {
    #[serde(rename = "initialize")]
    Initialize(InitializeRequest),
    #[serde(rename = "thread/start")]
    ThreadStart(ThreadStartRequest),
    #[serde(rename = "thread/resume")]
    ThreadResume(ThreadResumeRequest),
    #[serde(rename = "thread/fork")]
    ThreadFork(ThreadForkRequest),
    #[serde(rename = "thread/compact/start")]
    ThreadCompactStart(ThreadCompactRequest),
    #[serde(rename = "turn/start")]
    TurnStart(TurnStartRequest),
    #[serde(rename = "turn/steer")]
    TurnSteer(TurnSteerRequest),
    #[serde(rename = "turn/interrupt")]
    TurnInterrupt(TurnInterruptRequest),
}

impl FactoryRequest {
    pub const fn method(&self) -> FactoryMethod {
        match self {
            Self::Initialize(_) => FactoryMethod::Initialize,
            Self::ThreadStart(_) => FactoryMethod::ThreadStart,
            Self::ThreadResume(_) => FactoryMethod::ThreadResume,
            Self::ThreadFork(_) => FactoryMethod::ThreadFork,
            Self::ThreadCompactStart(_) => FactoryMethod::ThreadCompactStart,
            Self::TurnStart(_) => FactoryMethod::TurnStart,
            Self::TurnSteer(_) => FactoryMethod::TurnSteer,
            Self::TurnInterrupt(_) => FactoryMethod::TurnInterrupt,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(tag = "method", content = "result")]
pub enum FactoryResponse {
    #[serde(rename = "initialize")]
    Initialize(InitializeResponse),
    #[serde(rename = "thread/start")]
    ThreadStart(ThreadStartResponse),
    #[serde(rename = "thread/resume")]
    ThreadResume(ThreadResumeResponse),
    #[serde(rename = "thread/fork")]
    ThreadFork(ThreadForkResponse),
    #[serde(rename = "thread/compact/start")]
    ThreadCompactStart(ThreadCompactResponse),
    #[serde(rename = "turn/start")]
    TurnStart(TurnStartResponse),
    #[serde(rename = "turn/steer")]
    TurnSteer(TurnSteerResponse),
    #[serde(rename = "turn/interrupt")]
    TurnInterrupt(TurnInterruptResponse),
}

impl FactoryResponse {
    pub const fn method(&self) -> FactoryMethod {
        match self {
            Self::Initialize(_) => FactoryMethod::Initialize,
            Self::ThreadStart(_) => FactoryMethod::ThreadStart,
            Self::ThreadResume(_) => FactoryMethod::ThreadResume,
            Self::ThreadFork(_) => FactoryMethod::ThreadFork,
            Self::ThreadCompactStart(_) => FactoryMethod::ThreadCompactStart,
            Self::TurnStart(_) => FactoryMethod::TurnStart,
            Self::TurnSteer(_) => FactoryMethod::TurnSteer,
            Self::TurnInterrupt(_) => FactoryMethod::TurnInterrupt,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct FactoryRequestEnvelope {
    pub id: FactoryRequestId,
    #[serde(flatten)]
    pub request: FactoryRequest,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct FactoryPendingRequest {
    pub id: FactoryRequestId,
    pub method: FactoryMethod,
}

impl From<&FactoryRequestEnvelope> for FactoryPendingRequest {
    fn from(envelope: &FactoryRequestEnvelope) -> Self {
        Self {
            id: envelope.id.clone(),
            method: envelope.request.method(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct FactoryResponseEnvelope {
    pub id: FactoryRequestId,
    #[serde(flatten)]
    pub response: FactoryResponse,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
#[ts(rename_all_fields = "camelCase")]
pub enum FactoryResponseOutcome {
    Success { response: FactoryResponseEnvelope },
    Error { error: FactoryErrorEnvelope },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(tag = "method", content = "params")]
pub enum FactoryNotification {
    #[serde(rename = "initialized")]
    Initialized,
    #[serde(rename = "factory/event")]
    Event(FactoryEvent),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
pub struct FactoryNotificationEnvelope {
    #[serde(flatten)]
    pub notification: FactoryNotification,
}

/// One JSON value per line. Deliberately follows app-server JSON-RPC
/// semantics without adding a `jsonrpc` member.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(untagged)]
pub enum FactoryJsonlMessage {
    Request(FactoryRequestEnvelope),
    Response(FactoryResponseEnvelope),
    Error(FactoryErrorEnvelope),
    Notification(FactoryNotificationEnvelope),
    ServerResponse(FactoryServerResponse),
    ServerError(FactoryServerErrorResponse),
}
