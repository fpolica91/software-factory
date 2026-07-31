use crate::error::FactoryRpcError;
use crate::ids::FactoryRpcRequestId;
use crate::ids::ItemId;
use crate::ids::ThreadId;
use crate::ids::TurnId;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeMap;
use ts_rs::TS;

pub const FACTORY_METHOD_NOT_SUPPORTED_CODE: i64 = -32601;

/// A server-request method selected by the pinned Factory Protocol adapter.
///
/// This enum is deliberately closed over the nine non-deprecated app-server
/// request pairs that Factory Protocol V1 supports. Use
/// [`FactoryServerRequestMethod`] when an arbitrary upstream method must be
/// retained losslessly.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema, TS,
)]
pub enum FactoryKnownServerRequestMethod {
    #[serde(rename = "item/commandExecution/requestApproval")]
    #[ts(rename = "item/commandExecution/requestApproval")]
    CommandExecutionRequestApproval,
    #[serde(rename = "item/fileChange/requestApproval")]
    #[ts(rename = "item/fileChange/requestApproval")]
    FileChangeRequestApproval,
    #[serde(rename = "item/tool/requestUserInput")]
    #[ts(rename = "item/tool/requestUserInput")]
    ToolRequestUserInput,
    #[serde(rename = "mcpServer/elicitation/request")]
    #[ts(rename = "mcpServer/elicitation/request")]
    McpServerElicitationRequest,
    #[serde(rename = "item/permissions/requestApproval")]
    #[ts(rename = "item/permissions/requestApproval")]
    PermissionsRequestApproval,
    #[serde(rename = "item/tool/call")]
    #[ts(rename = "item/tool/call")]
    DynamicToolCall,
    #[serde(rename = "account/chatgptAuthTokens/refresh")]
    #[ts(rename = "account/chatgptAuthTokens/refresh")]
    ChatgptAuthTokensRefresh,
    #[serde(rename = "attestation/generate")]
    #[ts(rename = "attestation/generate")]
    AttestationGenerate,
    #[serde(rename = "currentTime/read")]
    #[ts(rename = "currentTime/read")]
    CurrentTimeRead,
}

impl FactoryKnownServerRequestMethod {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::CommandExecutionRequestApproval => "item/commandExecution/requestApproval",
            Self::FileChangeRequestApproval => "item/fileChange/requestApproval",
            Self::ToolRequestUserInput => "item/tool/requestUserInput",
            Self::McpServerElicitationRequest => "mcpServer/elicitation/request",
            Self::PermissionsRequestApproval => "item/permissions/requestApproval",
            Self::DynamicToolCall => "item/tool/call",
            Self::ChatgptAuthTokensRefresh => "account/chatgptAuthTokens/refresh",
            Self::AttestationGenerate => "attestation/generate",
            Self::CurrentTimeRead => "currentTime/read",
        }
    }

    pub fn from_method(method: &str) -> Option<Self> {
        Some(match method {
            "item/commandExecution/requestApproval" => Self::CommandExecutionRequestApproval,
            "item/fileChange/requestApproval" => Self::FileChangeRequestApproval,
            "item/tool/requestUserInput" => Self::ToolRequestUserInput,
            "mcpServer/elicitation/request" => Self::McpServerElicitationRequest,
            "item/permissions/requestApproval" => Self::PermissionsRequestApproval,
            "item/tool/call" => Self::DynamicToolCall,
            "account/chatgptAuthTokens/refresh" => Self::ChatgptAuthTokensRefresh,
            "attestation/generate" => Self::AttestationGenerate,
            "currentTime/read" => Self::CurrentTimeRead,
            _ => return None,
        })
    }
}

/// Lossless method identifier for raw or typed server requests.
#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema, TS,
)]
#[serde(transparent)]
#[ts(type = "string")]
pub struct FactoryServerRequestMethod(pub String);

impl FactoryServerRequestMethod {
    pub fn new(method: impl Into<String>) -> Self {
        Self(method.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn into_inner(self) -> String {
        self.0
    }

    pub fn known(&self) -> Option<FactoryKnownServerRequestMethod> {
        FactoryKnownServerRequestMethod::from_method(self.as_str())
    }

    pub fn is_supported(&self) -> bool {
        self.known().is_some()
    }
}

impl From<FactoryKnownServerRequestMethod> for FactoryServerRequestMethod {
    fn from(method: FactoryKnownServerRequestMethod) -> Self {
        Self(method.as_str().to_string())
    }
}

impl From<String> for FactoryServerRequestMethod {
    fn from(method: String) -> Self {
        Self(method)
    }
}

impl From<&str> for FactoryServerRequestMethod {
    fn from(method: &str) -> Self {
        Self(method.to_string())
    }
}

impl std::fmt::Display for FactoryServerRequestMethod {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Stable identity used to pair an app-server initiated request with exactly
/// one Factory response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct FactoryServerRequestIdentity {
    pub request_id: FactoryRpcRequestId,
    pub method: FactoryServerRequestMethod,
}

/// Alias retained for callers that describe the pairing key as metadata.
pub type FactoryServerRequestMetadata = FactoryServerRequestIdentity;

impl FactoryServerRequestIdentity {
    pub fn new(
        request_id: impl Into<FactoryRpcRequestId>,
        method: impl Into<FactoryServerRequestMethod>,
    ) -> Self {
        Self {
            request_id: request_id.into(),
            method: method.into(),
        }
    }

    pub fn validate_response_identity(
        &self,
        response: &Self,
    ) -> Result<(), FactoryServerResponsePairingError> {
        if self.request_id != response.request_id {
            return Err(FactoryServerResponsePairingError::RequestIdMismatch {
                expected: self.request_id.clone(),
                actual: response.request_id.clone(),
            });
        }
        if self.method != response.method {
            return Err(FactoryServerResponsePairingError::MethodMismatch {
                expected: self.method.clone(),
                actual: response.method.clone(),
            });
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS, thiserror::Error)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
#[ts(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum FactoryServerResponsePairingError {
    #[error("server response request id mismatch: expected {expected}, got {actual}")]
    RequestIdMismatch {
        expected: FactoryRpcRequestId,
        actual: FactoryRpcRequestId,
    },
    #[error("server response method mismatch: expected {expected}, got {actual}")]
    MethodMismatch {
        expected: FactoryServerRequestMethod,
        actual: FactoryServerRequestMethod,
    },
}

/// Exact raw app-server request shape. Keeping this beside a typed projection
/// prevents newly-added or deliberately unprojected params from being lost.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
pub struct FactoryRawServerRequest {
    #[serde(rename = "id")]
    #[ts(rename = "id")]
    pub request_id: FactoryRpcRequestId,
    pub method: FactoryServerRequestMethod,
    pub params: Value,
}

impl FactoryRawServerRequest {
    pub fn identity(&self) -> FactoryServerRequestIdentity {
        FactoryServerRequestIdentity {
            request_id: self.request_id.clone(),
            method: self.method.clone(),
        }
    }

    pub fn metadata(&self) -> FactoryServerRequestMetadata {
        self.identity()
    }

    pub fn validate_response(
        &self,
        response: &FactoryRawServerResponse,
    ) -> Result<(), FactoryServerResponsePairingError> {
        self.identity()
            .validate_response_identity(&response.identity())
    }

    /// Decode a supported request while retaining this exact raw record in the
    /// returned value. Unsupported methods are classified rather than treated
    /// as malformed known payloads.
    pub fn decode(self) -> Result<FactoryDecodedServerRequest, FactoryServerPayloadDecodeError> {
        if !self.method.is_supported() {
            return Ok(FactoryDecodedServerRequest::Unknown {
                request: FactoryUnknownServerRequest(self),
            });
        }

        let identity = self.identity();
        let value = serde_json::to_value(&self).map_err(|error| {
            FactoryServerPayloadDecodeError::InvalidKnownRequest {
                identity: identity.clone(),
                message: error.to_string(),
            }
        })?;
        let request = serde_json::from_value(value).map_err(|error| {
            FactoryServerPayloadDecodeError::InvalidKnownRequest {
                identity,
                message: error.to_string(),
            }
        })?;
        Ok(FactoryDecodedServerRequest::Known { request, raw: self })
    }
}

/// Raw request whose method is outside Factory Protocol V1's supported set.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(transparent)]
pub struct FactoryUnknownServerRequest(pub FactoryRawServerRequest);

impl FactoryUnknownServerRequest {
    pub fn identity(&self) -> FactoryServerRequestIdentity {
        self.0.identity()
    }

    pub fn method_not_supported(&self) -> FactoryMethodNotSupportedResolution {
        FactoryMethodNotSupportedResolution::new(self.identity())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
#[ts(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum FactoryDecodedServerRequest {
    Known {
        request: FactoryServerRequest,
        raw: FactoryRawServerRequest,
    },
    Unknown {
        request: FactoryUnknownServerRequest,
    },
}

/// Exact raw typed-response shape. The raw response remains available even
/// when the known response projection does not model a newly-added field.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
pub struct FactoryRawServerResponse {
    #[serde(rename = "id")]
    #[ts(rename = "id")]
    pub request_id: FactoryRpcRequestId,
    pub method: FactoryServerRequestMethod,
    pub response: Value,
}

impl FactoryRawServerResponse {
    pub fn identity(&self) -> FactoryServerRequestIdentity {
        FactoryServerRequestIdentity {
            request_id: self.request_id.clone(),
            method: self.method.clone(),
        }
    }

    pub fn metadata(&self) -> FactoryServerRequestMetadata {
        self.identity()
    }

    pub fn validate_request(
        &self,
        request: &FactoryRawServerRequest,
    ) -> Result<(), FactoryServerResponsePairingError> {
        request.validate_response(self)
    }

    pub fn decode(self) -> Result<FactoryDecodedServerResponse, FactoryServerPayloadDecodeError> {
        if !self.method.is_supported() {
            return Ok(FactoryDecodedServerResponse::Unknown {
                response: FactoryUnknownServerResponse(self),
            });
        }

        let identity = self.identity();
        let value = serde_json::to_value(&self).map_err(|error| {
            FactoryServerPayloadDecodeError::InvalidKnownResponse {
                identity: identity.clone(),
                message: error.to_string(),
            }
        })?;
        let response = serde_json::from_value(value).map_err(|error| {
            FactoryServerPayloadDecodeError::InvalidKnownResponse {
                identity,
                message: error.to_string(),
            }
        })?;
        Ok(FactoryDecodedServerResponse::Known {
            response,
            raw: self,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(transparent)]
pub struct FactoryUnknownServerResponse(pub FactoryRawServerResponse);

impl FactoryUnknownServerResponse {
    pub fn identity(&self) -> FactoryServerRequestIdentity {
        self.0.identity()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
#[ts(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum FactoryDecodedServerResponse {
    Known {
        response: FactoryServerResponse,
        raw: FactoryRawServerResponse,
    },
    Unknown {
        response: FactoryUnknownServerResponse,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS, thiserror::Error)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
#[ts(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum FactoryServerPayloadDecodeError {
    #[error("invalid payload for known server request {identity:?}: {message}")]
    InvalidKnownRequest {
        identity: FactoryServerRequestIdentity,
        message: String,
    },
    #[error("invalid payload for known server response {identity:?}: {message}")]
    InvalidKnownResponse {
        identity: FactoryServerRequestIdentity,
        message: String,
    },
}

/// Owned JSON-RPC error response for an app-server initiated request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct FactoryServerErrorResponse {
    pub identity: FactoryServerRequestIdentity,
    pub error: FactoryRpcError,
}

impl FactoryServerErrorResponse {
    pub fn method_not_supported(identity: FactoryServerRequestIdentity) -> Self {
        let method = identity.method.clone();
        Self {
            identity,
            error: FactoryRpcError {
                code: FACTORY_METHOD_NOT_SUPPORTED_CODE,
                message: format!("server request method is not supported: {method}"),
                data: Some(serde_json::json!({ "method": method.as_str() })),
            },
        }
    }
}

/// Required resolution for an unknown app-server request: reply with JSON-RPC
/// method-not-supported and terminate the affected operation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct FactoryMethodNotSupportedResolution {
    pub response: FactoryServerErrorResponse,
    pub terminate_operation: bool,
}

impl FactoryMethodNotSupportedResolution {
    pub fn new(identity: FactoryServerRequestIdentity) -> Self {
        Self {
            response: FactoryServerErrorResponse::method_not_supported(identity),
            terminate_operation: true,
        }
    }
}

/// A typed request initiated by Codex app-server and delivered to the Factory
/// host. The request id must be echoed unchanged in the matching response.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[allow(clippy::large_enum_variant)]
#[serde(
    tag = "method",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
#[ts(
    tag = "method",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum FactoryServerRequest {
    #[serde(rename = "item/commandExecution/requestApproval")]
    #[ts(rename = "item/commandExecution/requestApproval")]
    CommandExecutionRequestApproval {
        #[serde(rename = "id")]
        #[ts(rename = "id")]
        request_id: FactoryRpcRequestId,
        params: FactoryCommandExecutionRequestApprovalParams,
    },
    #[serde(rename = "item/fileChange/requestApproval")]
    #[ts(rename = "item/fileChange/requestApproval")]
    FileChangeRequestApproval {
        #[serde(rename = "id")]
        #[ts(rename = "id")]
        request_id: FactoryRpcRequestId,
        params: FactoryFileChangeRequestApprovalParams,
    },
    #[serde(rename = "item/tool/requestUserInput")]
    #[ts(rename = "item/tool/requestUserInput")]
    ToolRequestUserInput {
        #[serde(rename = "id")]
        #[ts(rename = "id")]
        request_id: FactoryRpcRequestId,
        params: FactoryToolRequestUserInputParams,
    },
    #[serde(rename = "mcpServer/elicitation/request")]
    #[ts(rename = "mcpServer/elicitation/request")]
    McpServerElicitationRequest {
        #[serde(rename = "id")]
        #[ts(rename = "id")]
        request_id: FactoryRpcRequestId,
        params: FactoryMcpServerElicitationRequestParams,
    },
    #[serde(rename = "item/permissions/requestApproval")]
    #[ts(rename = "item/permissions/requestApproval")]
    PermissionsRequestApproval {
        #[serde(rename = "id")]
        #[ts(rename = "id")]
        request_id: FactoryRpcRequestId,
        params: FactoryPermissionsRequestApprovalParams,
    },
    #[serde(rename = "item/tool/call")]
    #[ts(rename = "item/tool/call")]
    DynamicToolCall {
        #[serde(rename = "id")]
        #[ts(rename = "id")]
        request_id: FactoryRpcRequestId,
        params: FactoryDynamicToolCallParams,
    },
    #[serde(rename = "account/chatgptAuthTokens/refresh")]
    #[ts(rename = "account/chatgptAuthTokens/refresh")]
    ChatgptAuthTokensRefresh {
        #[serde(rename = "id")]
        #[ts(rename = "id")]
        request_id: FactoryRpcRequestId,
        params: FactoryChatgptAuthTokensRefreshParams,
    },
    #[serde(rename = "attestation/generate")]
    #[ts(rename = "attestation/generate")]
    AttestationGenerate {
        #[serde(rename = "id")]
        #[ts(rename = "id")]
        request_id: FactoryRpcRequestId,
        params: FactoryAttestationGenerateParams,
    },
    #[serde(rename = "currentTime/read")]
    #[ts(rename = "currentTime/read")]
    CurrentTimeRead {
        #[serde(rename = "id")]
        #[ts(rename = "id")]
        request_id: FactoryRpcRequestId,
        params: FactoryCurrentTimeReadParams,
    },
}

impl FactoryServerRequest {
    pub fn id(&self) -> &FactoryRpcRequestId {
        match self {
            Self::CommandExecutionRequestApproval { request_id, .. }
            | Self::FileChangeRequestApproval { request_id, .. }
            | Self::ToolRequestUserInput { request_id, .. }
            | Self::McpServerElicitationRequest { request_id, .. }
            | Self::PermissionsRequestApproval { request_id, .. }
            | Self::DynamicToolCall { request_id, .. }
            | Self::ChatgptAuthTokensRefresh { request_id, .. }
            | Self::AttestationGenerate { request_id, .. }
            | Self::CurrentTimeRead { request_id, .. } => request_id,
        }
    }

    pub const fn method(&self) -> &'static str {
        match self {
            Self::CommandExecutionRequestApproval { .. } => "item/commandExecution/requestApproval",
            Self::FileChangeRequestApproval { .. } => "item/fileChange/requestApproval",
            Self::ToolRequestUserInput { .. } => "item/tool/requestUserInput",
            Self::McpServerElicitationRequest { .. } => "mcpServer/elicitation/request",
            Self::PermissionsRequestApproval { .. } => "item/permissions/requestApproval",
            Self::DynamicToolCall { .. } => "item/tool/call",
            Self::ChatgptAuthTokensRefresh { .. } => "account/chatgptAuthTokens/refresh",
            Self::AttestationGenerate { .. } => "attestation/generate",
            Self::CurrentTimeRead { .. } => "currentTime/read",
        }
    }

    pub const fn known_method(&self) -> FactoryKnownServerRequestMethod {
        match self {
            Self::CommandExecutionRequestApproval { .. } => {
                FactoryKnownServerRequestMethod::CommandExecutionRequestApproval
            }
            Self::FileChangeRequestApproval { .. } => {
                FactoryKnownServerRequestMethod::FileChangeRequestApproval
            }
            Self::ToolRequestUserInput { .. } => {
                FactoryKnownServerRequestMethod::ToolRequestUserInput
            }
            Self::McpServerElicitationRequest { .. } => {
                FactoryKnownServerRequestMethod::McpServerElicitationRequest
            }
            Self::PermissionsRequestApproval { .. } => {
                FactoryKnownServerRequestMethod::PermissionsRequestApproval
            }
            Self::DynamicToolCall { .. } => FactoryKnownServerRequestMethod::DynamicToolCall,
            Self::ChatgptAuthTokensRefresh { .. } => {
                FactoryKnownServerRequestMethod::ChatgptAuthTokensRefresh
            }
            Self::AttestationGenerate { .. } => {
                FactoryKnownServerRequestMethod::AttestationGenerate
            }
            Self::CurrentTimeRead { .. } => FactoryKnownServerRequestMethod::CurrentTimeRead,
        }
    }

    pub fn identity(&self) -> FactoryServerRequestIdentity {
        FactoryServerRequestIdentity {
            request_id: self.id().clone(),
            method: self.known_method().into(),
        }
    }

    pub fn metadata(&self) -> FactoryServerRequestMetadata {
        self.identity()
    }

    pub fn validate_response(
        &self,
        response: &FactoryServerResponse,
    ) -> Result<(), FactoryServerResponsePairingError> {
        self.identity()
            .validate_response_identity(&response.identity())
    }

    pub fn to_raw(&self) -> Result<FactoryRawServerRequest, serde_json::Error> {
        serde_json::from_value(serde_json::to_value(self)?)
    }
}

/// A typed response to a [`FactoryServerRequest`]. The method and id allow the
/// adapter to verify that a response is paired with the correct request before
/// converting its payload to the app-server result value.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[allow(clippy::large_enum_variant)]
#[serde(
    tag = "method",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
#[ts(
    tag = "method",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum FactoryServerResponse {
    #[serde(rename = "item/commandExecution/requestApproval")]
    #[ts(rename = "item/commandExecution/requestApproval")]
    CommandExecutionRequestApproval {
        #[serde(rename = "id")]
        #[ts(rename = "id")]
        request_id: FactoryRpcRequestId,
        response: FactoryCommandExecutionRequestApprovalResponse,
    },
    #[serde(rename = "item/fileChange/requestApproval")]
    #[ts(rename = "item/fileChange/requestApproval")]
    FileChangeRequestApproval {
        #[serde(rename = "id")]
        #[ts(rename = "id")]
        request_id: FactoryRpcRequestId,
        response: FactoryFileChangeRequestApprovalResponse,
    },
    #[serde(rename = "item/tool/requestUserInput")]
    #[ts(rename = "item/tool/requestUserInput")]
    ToolRequestUserInput {
        #[serde(rename = "id")]
        #[ts(rename = "id")]
        request_id: FactoryRpcRequestId,
        response: FactoryToolRequestUserInputResponse,
    },
    #[serde(rename = "mcpServer/elicitation/request")]
    #[ts(rename = "mcpServer/elicitation/request")]
    McpServerElicitationRequest {
        #[serde(rename = "id")]
        #[ts(rename = "id")]
        request_id: FactoryRpcRequestId,
        response: FactoryMcpServerElicitationRequestResponse,
    },
    #[serde(rename = "item/permissions/requestApproval")]
    #[ts(rename = "item/permissions/requestApproval")]
    PermissionsRequestApproval {
        #[serde(rename = "id")]
        #[ts(rename = "id")]
        request_id: FactoryRpcRequestId,
        response: FactoryPermissionsRequestApprovalResponse,
    },
    #[serde(rename = "item/tool/call")]
    #[ts(rename = "item/tool/call")]
    DynamicToolCall {
        #[serde(rename = "id")]
        #[ts(rename = "id")]
        request_id: FactoryRpcRequestId,
        response: FactoryDynamicToolCallResponse,
    },
    #[serde(rename = "account/chatgptAuthTokens/refresh")]
    #[ts(rename = "account/chatgptAuthTokens/refresh")]
    ChatgptAuthTokensRefresh {
        #[serde(rename = "id")]
        #[ts(rename = "id")]
        request_id: FactoryRpcRequestId,
        response: FactoryChatgptAuthTokensRefreshResponse,
    },
    #[serde(rename = "attestation/generate")]
    #[ts(rename = "attestation/generate")]
    AttestationGenerate {
        #[serde(rename = "id")]
        #[ts(rename = "id")]
        request_id: FactoryRpcRequestId,
        response: FactoryAttestationGenerateResponse,
    },
    #[serde(rename = "currentTime/read")]
    #[ts(rename = "currentTime/read")]
    CurrentTimeRead {
        #[serde(rename = "id")]
        #[ts(rename = "id")]
        request_id: FactoryRpcRequestId,
        response: FactoryCurrentTimeReadResponse,
    },
}

impl FactoryServerResponse {
    pub fn id(&self) -> &FactoryRpcRequestId {
        match self {
            Self::CommandExecutionRequestApproval { request_id, .. }
            | Self::FileChangeRequestApproval { request_id, .. }
            | Self::ToolRequestUserInput { request_id, .. }
            | Self::McpServerElicitationRequest { request_id, .. }
            | Self::PermissionsRequestApproval { request_id, .. }
            | Self::DynamicToolCall { request_id, .. }
            | Self::ChatgptAuthTokensRefresh { request_id, .. }
            | Self::AttestationGenerate { request_id, .. }
            | Self::CurrentTimeRead { request_id, .. } => request_id,
        }
    }

    pub const fn method(&self) -> &'static str {
        match self {
            Self::CommandExecutionRequestApproval { .. } => "item/commandExecution/requestApproval",
            Self::FileChangeRequestApproval { .. } => "item/fileChange/requestApproval",
            Self::ToolRequestUserInput { .. } => "item/tool/requestUserInput",
            Self::McpServerElicitationRequest { .. } => "mcpServer/elicitation/request",
            Self::PermissionsRequestApproval { .. } => "item/permissions/requestApproval",
            Self::DynamicToolCall { .. } => "item/tool/call",
            Self::ChatgptAuthTokensRefresh { .. } => "account/chatgptAuthTokens/refresh",
            Self::AttestationGenerate { .. } => "attestation/generate",
            Self::CurrentTimeRead { .. } => "currentTime/read",
        }
    }

    pub const fn known_method(&self) -> FactoryKnownServerRequestMethod {
        match self {
            Self::CommandExecutionRequestApproval { .. } => {
                FactoryKnownServerRequestMethod::CommandExecutionRequestApproval
            }
            Self::FileChangeRequestApproval { .. } => {
                FactoryKnownServerRequestMethod::FileChangeRequestApproval
            }
            Self::ToolRequestUserInput { .. } => {
                FactoryKnownServerRequestMethod::ToolRequestUserInput
            }
            Self::McpServerElicitationRequest { .. } => {
                FactoryKnownServerRequestMethod::McpServerElicitationRequest
            }
            Self::PermissionsRequestApproval { .. } => {
                FactoryKnownServerRequestMethod::PermissionsRequestApproval
            }
            Self::DynamicToolCall { .. } => FactoryKnownServerRequestMethod::DynamicToolCall,
            Self::ChatgptAuthTokensRefresh { .. } => {
                FactoryKnownServerRequestMethod::ChatgptAuthTokensRefresh
            }
            Self::AttestationGenerate { .. } => {
                FactoryKnownServerRequestMethod::AttestationGenerate
            }
            Self::CurrentTimeRead { .. } => FactoryKnownServerRequestMethod::CurrentTimeRead,
        }
    }

    pub fn identity(&self) -> FactoryServerRequestIdentity {
        FactoryServerRequestIdentity {
            request_id: self.id().clone(),
            method: self.known_method().into(),
        }
    }

    pub fn metadata(&self) -> FactoryServerRequestMetadata {
        self.identity()
    }

    pub fn validate_request(
        &self,
        request: &FactoryServerRequest,
    ) -> Result<(), FactoryServerResponsePairingError> {
        request.validate_response(self)
    }

    pub fn to_raw(&self) -> Result<FactoryRawServerResponse, serde_json::Error> {
        serde_json::from_value(serde_json::to_value(self)?)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct FactoryCommandExecutionRequestApprovalParams {
    pub thread_id: ThreadId,
    pub turn_id: TurnId,
    pub item_id: ItemId,
    #[ts(type = "number")]
    pub started_at_ms: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "String")]
    #[ts(optional)]
    pub approval_id: Option<String>,
    #[serde(default)]
    pub environment_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "String")]
    #[ts(optional)]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "Value")]
    #[ts(optional)]
    pub network_approval_context: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "String")]
    #[ts(optional)]
    pub command: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "String")]
    #[ts(optional)]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "Vec<Value>")]
    #[ts(optional)]
    pub command_actions: Option<Vec<Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "Value")]
    #[ts(optional)]
    pub additional_permissions: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "FactoryExecPolicyAmendment")]
    #[ts(optional)]
    pub proposed_execpolicy_amendment: Option<FactoryExecPolicyAmendment>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "Vec<FactoryNetworkPolicyAmendment>")]
    #[ts(optional)]
    pub proposed_network_policy_amendments: Option<Vec<FactoryNetworkPolicyAmendment>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "Vec<FactoryCommandExecutionApprovalDecision>")]
    #[ts(optional)]
    pub available_decisions: Option<Vec<FactoryCommandExecutionApprovalDecision>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
pub struct FactoryCommandExecutionRequestApprovalResponse {
    pub decision: FactoryCommandExecutionApprovalDecision,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub enum FactoryCommandExecutionApprovalDecision {
    Accept,
    AcceptForSession,
    #[serde(rename_all = "camelCase")]
    #[ts(rename_all = "camelCase")]
    AcceptWithExecpolicyAmendment {
        execpolicy_amendment: FactoryExecPolicyAmendment,
    },
    #[serde(rename_all = "camelCase")]
    #[ts(rename_all = "camelCase")]
    ApplyNetworkPolicyAmendment {
        network_policy_amendment: FactoryNetworkPolicyAmendment,
    },
    Decline,
    Cancel,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(transparent)]
#[ts(type = "Array<string>")]
pub struct FactoryExecPolicyAmendment(pub Vec<String>);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct FactoryNetworkPolicyAmendment {
    pub host: String,
    pub action: FactoryNetworkPolicyRuleAction,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub enum FactoryNetworkPolicyRuleAction {
    Allow,
    Deny,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct FactoryFileChangeRequestApprovalParams {
    pub thread_id: ThreadId,
    pub turn_id: TurnId,
    pub item_id: ItemId,
    #[ts(type = "number")]
    pub started_at_ms: i64,
    pub reason: Option<String>,
    pub grant_root: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
pub struct FactoryFileChangeRequestApprovalResponse {
    pub decision: FactoryFileChangeApprovalDecision,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub enum FactoryFileChangeApprovalDecision {
    Accept,
    AcceptForSession,
    Decline,
    Cancel,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct FactoryToolRequestUserInputOption {
    pub label: String,
    pub description: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct FactoryToolRequestUserInputQuestion {
    pub id: String,
    pub header: String,
    pub question: String,
    #[serde(default)]
    pub is_other: bool,
    #[serde(default)]
    pub is_secret: bool,
    pub options: Option<Vec<FactoryToolRequestUserInputOption>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct FactoryToolRequestUserInputParams {
    pub thread_id: ThreadId,
    pub turn_id: TurnId,
    pub item_id: ItemId,
    pub questions: Vec<FactoryToolRequestUserInputQuestion>,
    #[serde(default)]
    #[ts(type = "number | null")]
    pub auto_resolution_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct FactoryToolRequestUserInputAnswer {
    pub answers: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct FactoryToolRequestUserInputResponse {
    pub answers: BTreeMap<String, FactoryToolRequestUserInputAnswer>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct FactoryMcpServerElicitationRequestParams {
    pub thread_id: ThreadId,
    pub turn_id: Option<TurnId>,
    pub server_name: String,
    #[serde(flatten)]
    pub request: FactoryMcpServerElicitationRequest,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(tag = "mode", rename_all = "camelCase")]
#[ts(tag = "mode", rename_all = "camelCase")]
pub enum FactoryMcpServerElicitationRequest {
    #[serde(rename_all = "camelCase")]
    #[ts(rename_all = "camelCase")]
    Form {
        #[serde(rename = "_meta")]
        #[ts(rename = "_meta")]
        meta: Option<Value>,
        message: String,
        requested_schema: Value,
    },
    #[serde(rename = "openai/form", rename_all = "camelCase")]
    #[ts(rename = "openai/form", rename_all = "camelCase")]
    OpenAiForm {
        #[serde(rename = "_meta")]
        #[ts(rename = "_meta")]
        meta: Option<Value>,
        message: String,
        requested_schema: Value,
    },
    #[serde(rename_all = "camelCase")]
    #[ts(rename_all = "camelCase")]
    Url {
        #[serde(rename = "_meta")]
        #[ts(rename = "_meta")]
        meta: Option<Value>,
        message: String,
        url: String,
        elicitation_id: String,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct FactoryMcpServerElicitationRequestResponse {
    pub action: FactoryMcpServerElicitationAction,
    pub content: Option<Value>,
    #[serde(rename = "_meta")]
    #[ts(rename = "_meta")]
    pub meta: Option<Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub enum FactoryMcpServerElicitationAction {
    Accept,
    Decline,
    Cancel,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct FactoryPermissionsRequestApprovalParams {
    pub thread_id: ThreadId,
    pub turn_id: TurnId,
    pub item_id: ItemId,
    #[serde(default)]
    pub environment_id: Option<String>,
    #[ts(type = "number")]
    pub started_at_ms: i64,
    pub cwd: String,
    pub reason: Option<String>,
    pub permissions: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct FactoryPermissionsRequestApprovalResponse {
    pub permissions: Value,
    #[serde(default)]
    pub scope: FactoryPermissionGrantScope,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "bool")]
    #[ts(optional)]
    pub strict_auto_review: Option<bool>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub enum FactoryPermissionGrantScope {
    #[default]
    Turn,
    Session,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct FactoryDynamicToolCallParams {
    pub thread_id: ThreadId,
    pub turn_id: TurnId,
    pub call_id: String,
    pub namespace: Option<String>,
    pub tool: String,
    pub arguments: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct FactoryDynamicToolCallResponse {
    pub content_items: Vec<FactoryDynamicToolCallOutputContentItem>,
    pub success: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(tag = "type", rename_all = "camelCase")]
#[ts(tag = "type", rename_all = "camelCase")]
pub enum FactoryDynamicToolCallOutputContentItem {
    #[serde(rename_all = "camelCase")]
    #[ts(rename_all = "camelCase")]
    InputText { text: String },
    #[serde(rename_all = "camelCase")]
    #[ts(rename_all = "camelCase")]
    InputImage { image_url: String },
    #[serde(rename_all = "camelCase")]
    #[ts(rename_all = "camelCase")]
    InputAudio { audio_url: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct FactoryChatgptAuthTokensRefreshParams {
    pub reason: FactoryChatgptAuthTokensRefreshReason,
    pub previous_account_id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub enum FactoryChatgptAuthTokensRefreshReason {
    Unauthorized,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct FactoryChatgptAuthTokensRefreshResponse {
    pub access_token: String,
    pub chatgpt_account_id: String,
    pub chatgpt_plan_type: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct FactoryAttestationGenerateParams {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct FactoryAttestationGenerateResponse {
    pub token: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct FactoryCurrentTimeReadParams {
    pub thread_id: ThreadId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct FactoryCurrentTimeReadResponse {
    #[ts(type = "number")]
    pub current_time_at: i64,
}
