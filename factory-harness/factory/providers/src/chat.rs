use serde_json::Map;
use serde_json::Value;
use serde_json::json;

use crate::config::AdapterConfig;
use crate::responses::ProviderState;
use crate::responses::ResponsesStream;
use crate::responses::ToolCatalog;
use crate::responses::TranslationError;
use crate::responses::Usage;
use crate::responses::item_text;
use crate::responses::output_text;
use crate::responses::reasoning_state;
use crate::responses::request_input;
use crate::responses::request_model;

pub(crate) struct PreparedChatRequest {
    pub body: Value,
    pub tools: ToolCatalog,
}

pub(crate) fn prepare_request(
    request: &Value,
    config: &AdapterConfig,
) -> Result<PreparedChatRequest, TranslationError> {
    let model = request_model(request)?;
    let tools = ToolCatalog::from_request(request)?;
    let messages = build_messages(request_input(request)?, &tools, request)?;
    let mut body = Map::new();
    body.insert("model".to_string(), Value::String(model.to_string()));
    body.insert("messages".to_string(), Value::Array(messages));
    body.insert("stream".to_string(), Value::Bool(true));
    body.insert("stream_options".to_string(), json!({"include_usage": true}));
    body.insert("thinking".to_string(), json!({"type": "enabled"}));
    if let Some(effort) = request.pointer("/reasoning/effort").and_then(Value::as_str) {
        body.insert(
            "reasoning_effort".to_string(),
            Value::String(effort.to_string()),
        );
    }
    let chat_tools = tools.chat_tools();
    if !chat_tools.is_empty() {
        body.insert("tools".to_string(), Value::Array(chat_tools));
        body.insert(
            "parallel_tool_calls".to_string(),
            request
                .get("parallel_tool_calls")
                .cloned()
                .unwrap_or(Value::Bool(true)),
        );
    }
    match config.profile.id {
        "zai" => {
            body.insert("tool_stream".to_string(), Value::Bool(true));
            body.insert(
                "thinking".to_string(),
                json!({"type": "enabled", "clear_thinking": false}),
            );
            if body.contains_key("tools") {
                body.insert("tool_choice".to_string(), Value::String("auto".to_string()));
            }
        }
        "deepseek" => {
            // DeepSeek's thinking/tool loop is driven by the complete assistant
            // message replay. It does not need a forced tool choice.
        }
        _ => {}
    }
    Ok(PreparedChatRequest {
        body: Value::Object(body),
        tools,
    })
}

#[derive(Default)]
struct AssistantMessage {
    content: String,
    reasoning_content: String,
    tool_calls: Vec<Value>,
}

fn build_messages(
    input: &[Value],
    tools: &ToolCatalog,
    request: &Value,
) -> Result<Vec<Value>, TranslationError> {
    let mut messages = Vec::new();
    if let Some(instructions) = request.get("instructions").and_then(Value::as_str)
        && !instructions.is_empty()
    {
        messages.push(json!({"role": "system", "content": instructions}));
    }
    let mut assistant = AssistantMessage::default();
    for item in input {
        let item_type = item.get("type").and_then(Value::as_str).unwrap_or("");
        match item_type {
            "message" => {
                let role = item.get("role").and_then(Value::as_str).unwrap_or("user");
                let text = item_text(item);
                if role == "assistant" {
                    assistant.content.push_str(&text);
                } else {
                    flush_assistant(&mut messages, &mut assistant);
                    let role = if matches!(role, "developer" | "system") {
                        "system"
                    } else {
                        "user"
                    };
                    messages.push(json!({"role": role, "content": text}));
                }
            }
            "reasoning" => {
                if let Some(ProviderState::Chat { reasoning_content }) = reasoning_state(item) {
                    assistant.reasoning_content.push_str(&reasoning_content);
                } else {
                    assistant.reasoning_content.push_str(&reasoning_text(item));
                }
            }
            "function_call" | "custom_tool_call" => {
                let name = required(item, "name")?;
                let namespace = item.get("namespace").and_then(Value::as_str);
                let wire_name = tools.historical_wire_name(namespace, name);
                let call_id = required(item, "call_id")?;
                let arguments = if item_type == "custom_tool_call" {
                    tools.chat_custom_arguments(
                        namespace,
                        name,
                        item.get("input").and_then(Value::as_str).unwrap_or(""),
                    )
                } else {
                    item.get("arguments")
                        .and_then(Value::as_str)
                        .unwrap_or("{}")
                        .to_string()
                };
                assistant.tool_calls.push(json!({
                    "id": call_id,
                    "type": "function",
                    "function": {"name": wire_name, "arguments": arguments}
                }));
            }
            "function_call_output" | "custom_tool_call_output" => {
                flush_assistant(&mut messages, &mut assistant);
                messages.push(json!({
                    "role": "tool",
                    "tool_call_id": required(item, "call_id")?,
                    "content": output_text(item.get("output"))
                }));
            }
            "compaction_trigger" => {}
            other => {
                return Err(TranslationError::UnsupportedItem(format!(
                    "input type {other}"
                )));
            }
        }
    }
    flush_assistant(&mut messages, &mut assistant);
    Ok(messages)
}

fn flush_assistant(messages: &mut Vec<Value>, assistant: &mut AssistantMessage) {
    if assistant.content.is_empty()
        && assistant.reasoning_content.is_empty()
        && assistant.tool_calls.is_empty()
    {
        return;
    }
    let mut message = Map::new();
    message.insert("role".to_string(), Value::String("assistant".to_string()));
    message.insert(
        "content".to_string(),
        if assistant.content.is_empty() {
            Value::Null
        } else {
            Value::String(std::mem::take(&mut assistant.content))
        },
    );
    if !assistant.reasoning_content.is_empty() {
        message.insert(
            "reasoning_content".to_string(),
            Value::String(std::mem::take(&mut assistant.reasoning_content)),
        );
    }
    if !assistant.tool_calls.is_empty() {
        message.insert(
            "tool_calls".to_string(),
            Value::Array(std::mem::take(&mut assistant.tool_calls)),
        );
    }
    messages.push(Value::Object(message));
}

fn reasoning_text(item: &Value) -> String {
    item.get("content")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .chain(
            item.get("summary")
                .and_then(Value::as_array)
                .into_iter()
                .flatten(),
        )
        .filter_map(|part| part.get("text").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join("\n")
}

fn required<'a>(item: &'a Value, field: &str) -> Result<&'a str, TranslationError> {
    item.get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| TranslationError::InvalidRequest(format!("missing {field}")))
}

pub(crate) struct ChatStreamTranslator {
    responses: Option<ResponsesStream>,
    reasoning: String,
    saw_finish_reason: bool,
}

impl ChatStreamTranslator {
    pub fn new(model: impl Into<String>, tools: ToolCatalog) -> Self {
        Self {
            responses: Some(ResponsesStream::new(model, tools)),
            reasoning: String::new(),
            saw_finish_reason: false,
        }
    }

    pub fn created(&self) -> Value {
        self.responses
            .as_ref()
            .expect("translator is active")
            .created()
    }

    pub fn response_id(&self) -> &str {
        self.responses
            .as_ref()
            .expect("translator is active")
            .response_id()
    }

    pub fn push(&mut self, chunk: &Value) -> Result<Vec<Value>, TranslationError> {
        let responses = self.responses.as_mut().expect("translator is active");
        if let Some(error) = chunk.get("error") {
            return Err(TranslationError::InvalidRequest(format!(
                "upstream provider error: {error}"
            )));
        }
        if let Some(usage) = chunk.get("usage").filter(|usage| !usage.is_null()) {
            responses.set_usage(chat_usage(usage));
        }
        let Some(choice) = chunk
            .get("choices")
            .and_then(Value::as_array)
            .and_then(|choices| choices.first())
        else {
            return Ok(Vec::new());
        };
        let finish_reason = choice
            .get("finish_reason")
            .and_then(Value::as_str)
            .filter(|reason| !reason.is_empty());
        self.saw_finish_reason |= finish_reason.is_some();
        responses.set_finish_reason(finish_reason);
        let Some(delta) = choice.get("delta") else {
            return Ok(Vec::new());
        };
        let mut events = Vec::new();
        if let Some(reasoning) = delta.get("reasoning_content").and_then(Value::as_str) {
            self.reasoning.push_str(reasoning);
            events.extend(responses.reasoning_delta(reasoning));
        }
        if let Some(content) = delta.get("content").and_then(Value::as_str) {
            events.extend(responses.text_delta(content));
        }
        if let Some(tool_calls) = delta.get("tool_calls").and_then(Value::as_array) {
            for tool in tool_calls {
                let index = tool.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
                let function = tool.get("function").unwrap_or(&Value::Null);
                events.extend(responses.tool_delta(
                    index,
                    tool.get("id").and_then(Value::as_str),
                    function.get("name").and_then(Value::as_str),
                    function.get("arguments").and_then(Value::as_str),
                )?);
            }
        }
        Ok(events)
    }

    pub fn finish(mut self, saw_end_marker: bool) -> Result<Vec<Value>, TranslationError> {
        if !self.saw_finish_reason {
            return Err(TranslationError::InvalidRequest(
                "truncated chat stream: no terminal finish_reason".to_string(),
            ));
        }
        if !saw_end_marker {
            return Err(TranslationError::InvalidRequest(
                "truncated chat stream: missing [DONE] marker".to_string(),
            ));
        }
        let state = (!self.reasoning.is_empty()).then_some(ProviderState::Chat {
            reasoning_content: self.reasoning,
        });
        self.responses
            .take()
            .expect("translator is active")
            .finish(state)
    }
}

fn chat_usage(value: &Value) -> Usage {
    Usage {
        input_tokens: value
            .get("prompt_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        output_tokens: value
            .get("completion_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        cached_input_tokens: value
            .get("prompt_cache_hit_tokens")
            .or_else(|| value.pointer("/prompt_tokens_details/cached_tokens"))
            .and_then(Value::as_u64)
            .unwrap_or(0),
        reasoning_tokens: value
            .pointer("/completion_tokens_details/reasoning_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0),
    }
}
