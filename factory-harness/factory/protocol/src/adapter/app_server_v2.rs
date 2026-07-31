//! Translation between Factory Protocol V1 and the pinned app-server protocol.
//!
//! This is the only module in `factory-protocol` permitted to import Codex
//! app-server protocol types.

use crate::envelope::FactoryMethod;
use crate::envelope::FactoryPendingRequest;
use crate::envelope::FactoryRequest;
use crate::envelope::FactoryRequestEnvelope;
use crate::envelope::FactoryResponse;
use crate::envelope::FactoryResponseEnvelope;
use crate::envelope::FactoryResponseOutcome;
use crate::error::FactoryErrorEnvelope;
use crate::error::FactoryRpcError;
use crate::event::FactoryEvent;
use crate::ids::ItemId;
use crate::ids::ThreadId;
use crate::ids::TurnId;
use crate::item::FactoryItem;
use crate::item::FactoryItemDelta;
use crate::server_request::FactoryDecodedServerRequest;
use crate::server_request::FactoryMethodNotSupportedResolution;
use crate::server_request::FactoryRawServerRequest;
use crate::server_request::FactoryServerRequest;
use crate::server_request::FactoryServerResponse;
use crate::server_request::FactoryUnknownServerRequest;
use crate::thread::FactoryThread;
use crate::thread::FactoryThreadStatus;
use crate::turn::FactoryPlanStep;
use crate::turn::FactoryTurn;
use codex_app_server_protocol as app_server;
use serde::de::DeserializeOwned;
use serde_json::Map;
use serde_json::Value;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum AdapterError {
    #[error("Factory plan mode requires an explicit model")]
    PlanModeRequiresModel,
    #[error("missing protocol field `{0}`")]
    MissingField(&'static str),
    #[error("invalid protocol field `{0}`")]
    InvalidField(&'static str),
    #[error("app-server response id does not match pending Factory request `{expected}`")]
    ResponseIdMismatch { expected: String },
    #[error("expected an app-server response or error, received `{0}`")]
    UnexpectedResponseMessage(&'static str),
    #[error("server request/response pairing failed: {0}")]
    ServerResponsePairing(#[from] crate::server_request::FactoryServerResponsePairingError),
    #[error("server request payload decode failed: {0}")]
    ServerPayloadDecode(#[from] crate::server_request::FactoryServerPayloadDecodeError),
    #[error("protocol translation failed: {0}")]
    Serde(#[from] serde_json::Error),
}

/// Convert an owned Factory request into the pinned app-server request enum.
///
/// In-process callers must special-case `initialize`: app-server initialization
/// is performed by `InProcessAppServerClient::start*`, which also emits
/// `initialized`, so forwarding it again would initialize the connection twice.
pub fn request_to_app_server_json(
    envelope: &FactoryRequestEnvelope,
) -> Result<Value, AdapterError> {
    let mut value = serde_json::to_value(envelope)?;

    if let FactoryRequest::TurnStart(params) = &envelope.request {
        let request_params = value
            .get_mut("params")
            .and_then(Value::as_object_mut)
            .ok_or(AdapterError::MissingField("params"))?;
        request_params.remove("mode");

        match params.mode {
            crate::turn::FactoryTurnMode::Normal => {
                request_params.remove("collaborationMode");
            }
            crate::turn::FactoryTurnMode::Plan => {
                let model = params
                    .model
                    .clone()
                    .ok_or(AdapterError::PlanModeRequiresModel)?;
                request_params.insert(
                    "collaborationMode".to_string(),
                    serde_json::json!({
                        "mode": "plan",
                        "settings": {
                            "model": model,
                            "reasoning_effort": null,
                            "developer_instructions": null
                        }
                    }),
                );
            }
        }
    }

    let request: app_server::ClientRequest = serde_json::from_value(value)?;
    Ok(serde_json::to_value(request)?)
}

/// Decode a raw app-server JSON-RPC response with the pending request that
/// selects the result schema. This is the only supported response decoder;
/// Factory responses are never inferred from untagged enum order.
pub fn decode_app_server_response(
    pending: &FactoryPendingRequest,
    raw: Value,
) -> Result<FactoryResponseOutcome, AdapterError> {
    match serde_json::from_value::<app_server::JSONRPCMessage>(raw)? {
        app_server::JSONRPCMessage::Response(response) => {
            ensure_response_id(&response.id, pending)?;
            let response = response_for_method(pending.method, response.result)?;
            Ok(FactoryResponseOutcome::Success {
                response: FactoryResponseEnvelope {
                    id: pending.id.clone(),
                    response,
                },
            })
        }
        app_server::JSONRPCMessage::Error(error) => {
            ensure_response_id(&error.id, pending)?;
            Ok(FactoryResponseOutcome::Error {
                error: FactoryErrorEnvelope {
                    id: pending.id.clone(),
                    method: pending.method,
                    error: FactoryRpcError {
                        code: error.error.code,
                        message: error.error.message,
                        data: error.error.data,
                    },
                },
            })
        }
        app_server::JSONRPCMessage::Request(_) => {
            Err(AdapterError::UnexpectedResponseMessage("request"))
        }
        app_server::JSONRPCMessage::Notification(_) => {
            Err(AdapterError::UnexpectedResponseMessage("notification"))
        }
    }
}

fn ensure_response_id(
    id: &app_server::RequestId,
    pending: &FactoryPendingRequest,
) -> Result<(), AdapterError> {
    if matches!(id, app_server::RequestId::String(value) if value == pending.id.as_str()) {
        Ok(())
    } else {
        Err(AdapterError::ResponseIdMismatch {
            expected: pending.id.to_string(),
        })
    }
}

fn response_for_method(
    method: FactoryMethod,
    mut result: Value,
) -> Result<FactoryResponse, AdapterError> {
    Ok(match method {
        FactoryMethod::Initialize => FactoryResponse::Initialize(decode(result)?),
        FactoryMethod::ThreadStart => {
            normalize_thread_response(&mut result)?;
            FactoryResponse::ThreadStart(decode(result)?)
        }
        FactoryMethod::ThreadResume => {
            normalize_thread_response(&mut result)?;
            FactoryResponse::ThreadResume(decode(result)?)
        }
        FactoryMethod::ThreadFork => {
            normalize_thread_response(&mut result)?;
            FactoryResponse::ThreadFork(decode(result)?)
        }
        FactoryMethod::ThreadCompactStart => FactoryResponse::ThreadCompactStart(decode(result)?),
        FactoryMethod::TurnStart => {
            normalize_turn_at(&mut result, "turn")?;
            FactoryResponse::TurnStart(decode(result)?)
        }
        FactoryMethod::TurnSteer => FactoryResponse::TurnSteer(decode(result)?),
        FactoryMethod::TurnInterrupt => FactoryResponse::TurnInterrupt(decode(result)?),
    })
}

/// Decode a raw app-server notification before constructing its typed enum.
/// This two-stage path is intentional: the upstream enum has no catch-all
/// variant, while Factory must retain notifications added by newer revisions.
pub fn notification_from_json(method: impl Into<String>, params: Value) -> FactoryEvent {
    let method = method.into();
    match notification_from_known_json(&method, params.clone()) {
        Ok(event) => event,
        Err(_) => FactoryEvent::UnknownNotification { method, params },
    }
}

/// Decode an app-server initiated request from its raw JSON-RPC wire object.
/// Supported methods are validated against the pinned upstream types and keep
/// both a typed Factory projection and the exact raw request. Unknown methods
/// remain observable and must follow the method-not-supported path below.
pub fn server_request_from_app_server_json(
    raw: Value,
) -> Result<FactoryDecodedServerRequest, AdapterError> {
    let wire: app_server::JSONRPCRequest = serde_json::from_value(raw.clone())?;
    let request_id = match wire.id.clone() {
        app_server::RequestId::String(value) => crate::ids::FactoryRpcRequestId::String(value),
        app_server::RequestId::Integer(value) => crate::ids::FactoryRpcRequestId::Integer(value),
    };
    let raw_request = FactoryRawServerRequest {
        request_id,
        method: wire.method.clone().into(),
        params: wire.params.clone().unwrap_or(Value::Object(Map::new())),
    };

    if raw_request.method.is_supported() {
        // The owned shape is not the source of truth for pinned request
        // validity. Always validate a known method with the upstream enum.
        let _: app_server::ServerRequest = app_server::ServerRequest::try_from(wire)?;
    }

    Ok(raw_request.decode()?)
}

pub fn server_request_event_from_app_server_json(raw: Value) -> Result<FactoryEvent, AdapterError> {
    Ok(FactoryEvent::ServerRequest {
        request: server_request_from_app_server_json(raw)?,
    })
}

/// Encode a typed Factory response for a supported server request. Method and
/// request-id equality are checked before the pinned upstream response type is
/// used to validate the result payload.
pub fn server_response_to_app_server_json(
    request: &FactoryServerRequest,
    response: &FactoryServerResponse,
) -> Result<Value, AdapterError> {
    request.validate_response(response)?;

    let upstream_request: app_server::ServerRequest =
        serde_json::from_value(serde_json::to_value(request)?)?;
    let raw_response = response.to_raw()?;
    let result = raw_response.response;
    let _validated = upstream_request.response_from_result(result.clone())?;

    let wire = app_server::JSONRPCResponse {
        id: rpc_id_to_app_server(response.id()),
        result,
    };
    Ok(serde_json::to_value(wire)?)
}

/// Encode the required JSON-RPC `-32601` reply for an unknown server request.
/// `terminate_operation` remains part of the owned resolution and must be
/// acted on by the transport after this response is delivered.
pub fn method_not_supported_to_app_server_json(
    resolution: &FactoryMethodNotSupportedResolution,
) -> Result<Value, AdapterError> {
    let response = &resolution.response;
    let wire = app_server::JSONRPCError {
        id: rpc_id_to_app_server(&response.identity.request_id),
        error: app_server::JSONRPCErrorError {
            code: response.error.code,
            message: response.error.message.clone(),
            data: response.error.data.clone(),
        },
    };
    Ok(serde_json::to_value(wire)?)
}

pub fn reject_unknown_server_request_to_app_server_json(
    request: &FactoryUnknownServerRequest,
) -> Result<(Value, bool), AdapterError> {
    let resolution = request.method_not_supported();
    let terminate_operation = resolution.terminate_operation;
    Ok((
        method_not_supported_to_app_server_json(&resolution)?,
        terminate_operation,
    ))
}

fn rpc_id_to_app_server(id: &crate::ids::FactoryRpcRequestId) -> app_server::RequestId {
    match id {
        crate::ids::FactoryRpcRequestId::String(value) => {
            app_server::RequestId::String(value.clone())
        }
        crate::ids::FactoryRpcRequestId::Integer(value) => app_server::RequestId::Integer(*value),
    }
}

fn notification_from_known_json(
    method: &str,
    mut params: Value,
) -> Result<FactoryEvent, AdapterError> {
    let raw_params = params.clone();
    let object = params
        .as_object_mut()
        .ok_or(AdapterError::InvalidField("params"))?;
    match method {
        "thread/started" => Ok(FactoryEvent::ThreadStarted {
            thread: project_thread(
                object
                    .remove("thread")
                    .ok_or(AdapterError::MissingField("thread"))?,
            )?,
        }),
        "thread/status/changed" => Ok(FactoryEvent::ThreadStatusChanged {
            thread_id: ThreadId::new(take_string(object, "threadId")?),
            status: take_thread_status(object, "status")?,
        }),
        "turn/started" | "turn/completed" => {
            let thread_id = ThreadId::new(take_string(object, "threadId")?);
            let mut turn = object
                .remove("turn")
                .ok_or(AdapterError::MissingField("turn"))?;
            normalize_turn(&mut turn)?;
            let turn: FactoryTurn = decode(turn)?;
            if method == "turn/started" {
                Ok(FactoryEvent::TurnStarted { thread_id, turn })
            } else {
                Ok(FactoryEvent::TurnCompleted { thread_id, turn })
            }
        }
        "turn/plan/updated" => Ok(FactoryEvent::TurnPlanUpdated {
            thread_id: ThreadId::new(take_string(object, "threadId")?),
            turn_id: TurnId::new(take_string(object, "turnId")?),
            explanation: take_optional_string(object, "explanation")?,
            plan: take_decode::<Vec<FactoryPlanStep>>(object, "plan")?,
        }),
        "thread/compacted" => Ok(FactoryEvent::ThreadCompacted {
            thread_id: ThreadId::new(take_string(object, "threadId")?),
            turn_id: TurnId::new(take_string(object, "turnId")?),
        }),
        "item/started" | "item/completed" => {
            let thread_id = ThreadId::new(take_string(object, "threadId")?);
            let turn_id = TurnId::new(take_string(object, "turnId")?);
            let item = project_item(
                object
                    .remove("item")
                    .ok_or(AdapterError::MissingField("item"))?,
            );
            if method == "item/started" {
                Ok(FactoryEvent::ItemStarted {
                    thread_id,
                    turn_id,
                    item,
                    started_at_ms: take_i64(object, "startedAtMs")?,
                })
            } else {
                Ok(FactoryEvent::ItemCompleted {
                    thread_id,
                    turn_id,
                    item,
                    completed_at_ms: take_i64(object, "completedAtMs")?,
                })
            }
        }
        "serverRequest/resolved" => Ok(FactoryEvent::ServerRequestResolved {
            thread_id: ThreadId::new(take_string(object, "threadId")?),
            request_id: decode(
                object
                    .remove("requestId")
                    .ok_or(AdapterError::MissingField("requestId"))?,
            )?,
        }),
        "error" => {
            let error = object
                .remove("error")
                .and_then(|value| value.as_object().cloned())
                .ok_or(AdapterError::MissingField("error"))?;
            Ok(FactoryEvent::RuntimeError {
                error: crate::error::FactoryRuntimeError {
                    message: value_string(&error, "message")?,
                    codex_error_info: error.get("codexErrorInfo").cloned().filter(not_null),
                    additional_details: value_optional_string(&error, "additionalDetails")?,
                    will_retry: take_bool(object, "willRetry")?,
                    thread_id: ThreadId::new(take_string(object, "threadId")?),
                    turn_id: TurnId::new(take_string(object, "turnId")?),
                },
            })
        }
        _ => item_delta_from_json(method, object, raw_params),
    }
}

fn item_delta_from_json(
    method: &str,
    object: &mut Map<String, Value>,
    raw_params: Value,
) -> Result<FactoryEvent, AdapterError> {
    let delta = match method {
        "item/agentMessage/delta" => FactoryItemDelta::AgentMessage {
            delta: take_string(object, "delta")?,
            raw: raw_params.clone(),
        },
        "item/plan/delta" => FactoryItemDelta::Plan {
            delta: take_string(object, "delta")?,
            raw: raw_params.clone(),
        },
        "item/reasoning/summaryTextDelta" => FactoryItemDelta::ReasoningSummaryText {
            delta: take_string(object, "delta")?,
            summary_index: take_i64(object, "summaryIndex")?,
            raw: raw_params.clone(),
        },
        "item/reasoning/summaryPartAdded" => FactoryItemDelta::ReasoningSummaryPartAdded {
            summary_index: take_i64(object, "summaryIndex")?,
            raw: raw_params.clone(),
        },
        "item/reasoning/textDelta" => FactoryItemDelta::ReasoningText {
            delta: take_string(object, "delta")?,
            content_index: take_i64(object, "contentIndex")?,
            raw: raw_params.clone(),
        },
        "item/commandExecution/outputDelta" => FactoryItemDelta::CommandExecutionOutput {
            delta: take_string(object, "delta")?,
            raw: raw_params.clone(),
        },
        "item/fileChange/outputDelta" => FactoryItemDelta::FileChangeOutput {
            delta: take_string(object, "delta")?,
            raw: raw_params.clone(),
        },
        "item/fileChange/patchUpdated" => FactoryItemDelta::FileChangePatchUpdated {
            changes: object
                .remove("changes")
                .ok_or(AdapterError::MissingField("changes"))?,
            raw: raw_params.clone(),
        },
        "item/mcpToolCall/progress" => FactoryItemDelta::McpToolCallProgress {
            message: take_string(object, "message")?,
            raw: raw_params.clone(),
        },
        "item/commandExecution/terminalInteraction" => FactoryItemDelta::TerminalInteraction {
            process_id: take_string(object, "processId")?,
            stdin: take_string(object, "stdin")?,
            raw: raw_params.clone(),
        },
        _ => FactoryItemDelta::Unknown {
            upstream_method: method.to_string(),
            value: raw_params,
        },
    };

    Ok(FactoryEvent::ItemDelta {
        thread_id: ThreadId::new(take_string(object, "threadId")?),
        turn_id: TurnId::new(take_string(object, "turnId")?),
        item_id: ItemId::new(take_string(object, "itemId")?),
        delta,
    })
}

fn normalize_turn_at(value: &mut Value, field: &'static str) -> Result<(), AdapterError> {
    let turn = value
        .get_mut(field)
        .ok_or(AdapterError::MissingField(field))?;
    normalize_turn(turn)
}

fn normalize_turn(turn: &mut Value) -> Result<(), AdapterError> {
    *turn = serde_json::to_value(project_turn(turn.take())?)?;
    Ok(())
}

fn project_item(value: Value) -> FactoryItem {
    let raw = value.clone();
    let mut projected = value.clone();
    if let Some(object) = projected.as_object_mut() {
        object.insert("raw".to_string(), raw.clone());
    }
    if let Ok(item) = serde_json::from_value::<FactoryItem>(projected) {
        return item;
    }

    let upstream_type = value
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("<unknown>")
        .to_string();
    let id = value.get("id").and_then(Value::as_str).map(ItemId::new);
    FactoryItem::Unknown {
        id,
        upstream_type,
        value,
    }
}

fn project_turn(mut value: Value) -> Result<FactoryTurn, AdapterError> {
    let raw = value.clone();
    let object = value
        .as_object_mut()
        .ok_or(AdapterError::InvalidField("turn"))?;
    let items = object
        .get_mut("items")
        .and_then(Value::as_array_mut)
        .ok_or(AdapterError::MissingField("turn.items"))?;
    for item in items {
        *item = serde_json::to_value(project_item(item.take()))?;
    }
    object.insert("raw".to_string(), raw);
    decode(value)
}

fn project_thread(mut value: Value) -> Result<FactoryThread, AdapterError> {
    let raw = value.clone();
    let object = value
        .as_object_mut()
        .ok_or(AdapterError::InvalidField("thread"))?;
    let status = object
        .remove("status")
        .ok_or(AdapterError::MissingField("thread.status"))?;
    object.insert(
        "status".to_string(),
        serde_json::to_value(project_thread_status(status))?,
    );
    object.insert("raw".to_string(), raw);
    decode(value)
}

fn normalize_thread_response(value: &mut Value) -> Result<(), AdapterError> {
    let raw = value.clone();
    let object = value
        .as_object_mut()
        .ok_or(AdapterError::InvalidField("thread response"))?;
    let thread = object
        .remove("thread")
        .ok_or(AdapterError::MissingField("thread"))?;
    object.insert(
        "thread".to_string(),
        serde_json::to_value(project_thread(thread)?)?,
    );
    object.insert("raw".to_string(), raw);
    Ok(())
}

fn project_thread_status(mut value: Value) -> FactoryThreadStatus {
    let raw = value.clone();
    if let Some(object) = value.as_object_mut() {
        object.insert("raw".to_string(), raw.clone());
    }
    match serde_json::from_value(value) {
        Ok(status) => status,
        Err(_) => FactoryThreadStatus::Unknown {
            upstream_status: raw
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or("<unknown>")
                .to_string(),
            raw,
        },
    }
}

fn take_thread_status(
    object: &mut Map<String, Value>,
    field: &'static str,
) -> Result<FactoryThreadStatus, AdapterError> {
    let value = object
        .remove(field)
        .ok_or(AdapterError::MissingField(field))?;
    Ok(project_thread_status(value))
}

fn decode<T: DeserializeOwned>(value: Value) -> Result<T, AdapterError> {
    Ok(serde_json::from_value(value)?)
}

fn take_decode<T: DeserializeOwned>(
    object: &mut Map<String, Value>,
    field: &'static str,
) -> Result<T, AdapterError> {
    decode(
        object
            .remove(field)
            .ok_or(AdapterError::MissingField(field))?,
    )
}

fn take_string(
    object: &mut Map<String, Value>,
    field: &'static str,
) -> Result<String, AdapterError> {
    object
        .remove(field)
        .and_then(|value| value.as_str().map(str::to_owned))
        .ok_or(AdapterError::InvalidField(field))
}

fn take_optional_string(
    object: &mut Map<String, Value>,
    field: &'static str,
) -> Result<Option<String>, AdapterError> {
    match object.remove(field) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => Ok(Some(value)),
        Some(_) => Err(AdapterError::InvalidField(field)),
    }
}

fn take_i64(object: &mut Map<String, Value>, field: &'static str) -> Result<i64, AdapterError> {
    object
        .remove(field)
        .and_then(|value| value.as_i64())
        .ok_or(AdapterError::InvalidField(field))
}

fn take_bool(object: &mut Map<String, Value>, field: &'static str) -> Result<bool, AdapterError> {
    object
        .remove(field)
        .and_then(|value| value.as_bool())
        .ok_or(AdapterError::InvalidField(field))
}

fn value_string(object: &Map<String, Value>, field: &'static str) -> Result<String, AdapterError> {
    object
        .get(field)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or(AdapterError::InvalidField(field))
}

fn value_optional_string(
    object: &Map<String, Value>,
    field: &'static str,
) -> Result<Option<String>, AdapterError> {
    match object.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => Ok(Some(value.clone())),
        Some(_) => Err(AdapterError::InvalidField(field)),
    }
}

fn not_null(value: &Value) -> bool {
    !value.is_null()
}
