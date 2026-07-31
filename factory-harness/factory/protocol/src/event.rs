use crate::error::FactoryRuntimeError;
use crate::ids::FactoryRpcRequestId;
use crate::ids::ItemId;
use crate::ids::ThreadId;
use crate::ids::TurnId;
use crate::item::FactoryItem;
use crate::item::FactoryItemDelta;
use crate::server_request::FactoryDecodedServerRequest;
use crate::thread::FactoryThread;
use crate::thread::FactoryThreadStatus;
use crate::turn::FactoryPlanStep;
use crate::turn::FactoryTurn;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;
use ts_rs::TS;

/// Normalized event stream emitted by a Factory harness connection.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
#[ts(rename_all_fields = "camelCase")]
pub enum FactoryEvent {
    ThreadStarted {
        thread: FactoryThread,
    },
    #[serde(rename_all = "camelCase")]
    ThreadStatusChanged {
        thread_id: ThreadId,
        status: FactoryThreadStatus,
    },
    #[serde(rename_all = "camelCase")]
    ThreadCompacted {
        thread_id: ThreadId,
        turn_id: TurnId,
    },
    #[serde(rename_all = "camelCase")]
    TurnStarted {
        thread_id: ThreadId,
        turn: FactoryTurn,
    },
    /// The completed turn status is authoritative for operation outcome.
    #[serde(rename_all = "camelCase")]
    TurnCompleted {
        thread_id: ThreadId,
        turn: FactoryTurn,
    },
    #[serde(rename_all = "camelCase")]
    TurnPlanUpdated {
        thread_id: ThreadId,
        turn_id: TurnId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[schemars(with = "String")]
        #[ts(optional)]
        explanation: Option<String>,
        plan: Vec<FactoryPlanStep>,
    },
    #[serde(rename_all = "camelCase")]
    ItemStarted {
        thread_id: ThreadId,
        turn_id: TurnId,
        item: FactoryItem,
        #[ts(type = "number")]
        started_at_ms: i64,
    },
    #[serde(rename_all = "camelCase")]
    ItemDelta {
        thread_id: ThreadId,
        turn_id: TurnId,
        item_id: ItemId,
        delta: FactoryItemDelta,
    },
    #[serde(rename_all = "camelCase")]
    ItemCompleted {
        thread_id: ThreadId,
        turn_id: TurnId,
        item: FactoryItem,
        #[ts(type = "number")]
        completed_at_ms: i64,
    },
    ServerRequest {
        request: FactoryDecodedServerRequest,
    },
    #[serde(rename_all = "camelCase")]
    ServerRequestResolved {
        thread_id: ThreadId,
        request_id: FactoryRpcRequestId,
    },
    RuntimeError {
        error: FactoryRuntimeError,
    },
    /// Any notification added by a newer pinned Codex revision remains
    /// observable instead of being dropped during decoding.
    UnknownNotification {
        method: String,
        params: Value,
    },
    ConnectionClosed {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[schemars(with = "String")]
        #[ts(optional)]
        reason: Option<String>,
    },
}
