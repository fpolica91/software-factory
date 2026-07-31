use crate::envelope::FactoryMethod;
use crate::ids::FactoryRequestId;
use crate::ids::ThreadId;
use crate::ids::TurnId;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;
use ts_rs::TS;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct FactoryRpcError {
    #[ts(type = "number")]
    pub code: i64,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "Value")]
    #[ts(optional)]
    pub data: Option<Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct FactoryErrorEnvelope {
    pub id: FactoryRequestId,
    pub method: FactoryMethod,
    pub error: FactoryRpcError,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct FactoryRuntimeError {
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "Value")]
    #[ts(optional)]
    pub codex_error_info: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "String")]
    #[ts(optional)]
    pub additional_details: Option<String>,
    pub will_retry: bool,
    pub thread_id: ThreadId,
    pub turn_id: TurnId,
}
