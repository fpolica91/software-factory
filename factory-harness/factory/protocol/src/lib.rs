//! Factory-owned Protocol V1.
//!
//! Public modules contain only Factory-owned wire and correlation types. The
//! pinned Codex app-server protocol appears only in [`adapter::app_server_v2`].

pub mod adapter;
pub mod correlation;
pub mod envelope;
pub mod error;
pub mod event;
pub mod ids;
pub mod item;
pub mod server_request;
pub mod thread;
pub mod turn;
pub mod version;

use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use ts_rs::TS;

pub use correlation::FactoryCorrelation;
pub use envelope::FactoryJsonlMessage;
pub use envelope::FactoryMethod;
pub use envelope::FactoryNotificationEnvelope;
pub use envelope::FactoryPendingRequest;
pub use envelope::FactoryRequest;
pub use envelope::FactoryRequestEnvelope;
pub use envelope::FactoryResponse;
pub use envelope::FactoryResponseEnvelope;
pub use envelope::FactoryResponseOutcome;
pub use error::FactoryErrorEnvelope;
pub use event::FactoryEvent;
pub use server_request::FactoryDecodedServerRequest;
pub use server_request::FactoryDecodedServerResponse;
pub use server_request::FactoryKnownServerRequestMethod;
pub use server_request::FactoryMethodNotSupportedResolution;
pub use server_request::FactoryRawServerRequest;
pub use server_request::FactoryRawServerResponse;
pub use server_request::FactoryServerErrorResponse;
pub use server_request::FactoryServerPayloadDecodeError;
pub use server_request::FactoryServerRequest;
pub use server_request::FactoryServerRequestIdentity;
pub use server_request::FactoryServerRequestMethod;
pub use server_request::FactoryServerResponse;
pub use server_request::FactoryServerResponsePairingError;
pub use server_request::FactoryUnknownServerRequest;
pub use server_request::FactoryUnknownServerResponse;
pub use version::FACTORY_PROTOCOL_SCHEMA_SHA256;
pub use version::FACTORY_PROTOCOL_VERSION;
pub use version::ProtocolManifest;
pub use version::ProtocolRange;
pub use version::ProtocolVersion;
pub use version::SOURCE_CODEX_REVISION;

/// Export root used to generate the checked-in JSON Schema and TypeScript.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct FactoryProtocolSchema {
    pub jsonl_message: FactoryJsonlMessage,
    pub request: FactoryRequestEnvelope,
    pub response: FactoryResponseEnvelope,
    pub pending_request: FactoryPendingRequest,
    pub response_outcome: FactoryResponseOutcome,
    pub notification: FactoryNotificationEnvelope,
    pub error: FactoryErrorEnvelope,
    pub event: FactoryEvent,
    pub server_request: FactoryServerRequest,
    pub server_response: FactoryServerResponse,
    pub raw_server_request: FactoryRawServerRequest,
    pub decoded_server_request: FactoryDecodedServerRequest,
    pub unknown_server_request: FactoryUnknownServerRequest,
    pub raw_server_response: FactoryRawServerResponse,
    pub decoded_server_response: FactoryDecodedServerResponse,
    pub unknown_server_response: FactoryUnknownServerResponse,
    pub server_error_response: FactoryServerErrorResponse,
    pub method_not_supported_resolution: FactoryMethodNotSupportedResolution,
    pub server_request_identity: FactoryServerRequestIdentity,
    pub server_request_method: FactoryServerRequestMethod,
    pub known_server_request_method: FactoryKnownServerRequestMethod,
    pub server_response_pairing_error: FactoryServerResponsePairingError,
    pub server_payload_decode_error: FactoryServerPayloadDecodeError,
    pub correlation: FactoryCorrelation,
    pub protocol_version: ProtocolVersion,
    pub protocol_range: ProtocolRange,
    pub manifest: ProtocolManifest,
}
