use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;
use thiserror::Error;

pub(crate) use crate::response_stream::ResponsesStream;
pub(crate) use crate::response_stream::Usage;
pub(crate) use crate::response_stream::failed_event;
pub(crate) use crate::tools::ToolCatalog;

const STATE_PREFIX: &str = "factory-provider-state:";

#[derive(Debug, Error)]
pub(crate) enum TranslationError {
    #[error("invalid Responses request: {0}")]
    InvalidRequest(String),
    #[error("unsupported Responses item: {0}")]
    UnsupportedItem(String),
    #[error("provider returned unknown tool {0}")]
    UnknownTool(String),
    #[error("provider returned invalid arguments for tool {tool}: {detail}")]
    InvalidToolArguments { tool: String, detail: String },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum ProviderState {
    Chat { reasoning_content: String },
    Anthropic { thinking_blocks: Vec<Value> },
}

pub(crate) fn encode_provider_state(state: &ProviderState) -> String {
    format!(
        "{STATE_PREFIX}{}",
        serde_json::to_string(state).expect("provider state is serializable")
    )
}

pub(crate) fn decode_provider_state(value: &str) -> Option<ProviderState> {
    let payload = value.strip_prefix(STATE_PREFIX)?;
    serde_json::from_str(payload).ok()
}

pub(crate) fn request_model(request: &Value) -> Result<&str, TranslationError> {
    request
        .get("model")
        .and_then(Value::as_str)
        .filter(|model| !model.is_empty())
        .ok_or_else(|| TranslationError::InvalidRequest("missing model".to_string()))
}

pub(crate) fn request_input(request: &Value) -> Result<&[Value], TranslationError> {
    request
        .get("input")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .ok_or_else(|| TranslationError::InvalidRequest("missing input array".to_string()))
}

pub(crate) fn item_text(item: &Value) -> String {
    match item.get("content") {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(parts)) => parts
            .iter()
            .filter_map(|part| {
                part.get("text")
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .or_else(|| part.as_str().map(str::to_string))
            })
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

pub(crate) fn output_text(output: Option<&Value>) -> String {
    match output {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(|item| item.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        Some(other) => other.to_string(),
        None => String::new(),
    }
}

pub(crate) fn reasoning_state(item: &Value) -> Option<ProviderState> {
    item.get("encrypted_content")
        .and_then(Value::as_str)
        .and_then(decode_provider_state)
}
