use std::collections::BTreeMap;

use serde_json::Map;
use serde_json::Value;
use serde_json::json;

use crate::config::AdapterConfig;
use crate::profiles::anthropic_model_capabilities;
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

pub(crate) struct PreparedAnthropicRequest {
    pub body: Value,
    pub tools: ToolCatalog,
}

pub(crate) fn prepare_request(
    request: &Value,
    config: &AdapterConfig,
) -> Result<PreparedAnthropicRequest, TranslationError> {
    let model = request_model(request)?;
    let tools = ToolCatalog::from_request(request)?;
    let (system, messages) = build_messages(request_input(request)?, &tools, request)?;
    let capabilities = anthropic_model_capabilities(model);
    let mut body = Map::new();
    body.insert("model".to_string(), Value::String(model.to_string()));
    body.insert("messages".to_string(), Value::Array(messages));
    let max_tokens = capabilities
        .map(|capabilities| config.max_tokens.min(capabilities.max_output_tokens))
        .unwrap_or(config.max_tokens);
    body.insert("max_tokens".to_string(), json!(max_tokens));
    body.insert("stream".to_string(), Value::Bool(true));
    if capabilities.is_some_and(|capabilities| capabilities.supports_adaptive_thinking) {
        body.insert("thinking".to_string(), json!({"type": "adaptive"}));
    }
    if !system.is_empty() {
        body.insert("system".to_string(), Value::String(system));
    }
    if let Some(effort) = request.pointer("/reasoning/effort").and_then(Value::as_str) {
        match effort {
            "none"
                if capabilities
                    .is_some_and(|capabilities| capabilities.adaptive_thinking_is_required) =>
            {
                return Err(TranslationError::InvalidRequest(format!(
                    "model {model} does not support disabling adaptive thinking"
                )));
            }
            "none"
                if capabilities
                    .is_some_and(|capabilities| capabilities.supports_adaptive_thinking) =>
            {
                body.insert("thinking".to_string(), json!({"type": "disabled"}));
            }
            "none" => {}
            supported
                if capabilities
                    .is_some_and(|capabilities| capabilities.effort.supports(supported)) =>
            {
                body.insert("output_config".to_string(), json!({"effort": effort}));
            }
            unsupported if capabilities.is_some() => {
                return Err(TranslationError::InvalidRequest(format!(
                    "model {model} does not support Anthropic effort {unsupported}"
                )));
            }
            requested => {
                return Err(TranslationError::InvalidRequest(format!(
                    "cannot determine whether custom Anthropic model {model} supports effort {requested}; omit the reasoning effort or select a known model"
                )));
            }
        }
    }
    if body
        .get("thinking")
        .and_then(|thinking| thinking.get("type"))
        .and_then(Value::as_str)
        == Some("adaptive")
        && capabilities.is_some_and(|capabilities| capabilities.supports_thinking_display)
        && let Some(summary) = request
            .pointer("/reasoning/summary")
            .and_then(Value::as_str)
    {
        let display = match summary {
            "auto" | "concise" | "detailed" => "summarized",
            "none" => "omitted",
            unsupported => {
                return Err(TranslationError::InvalidRequest(format!(
                    "unsupported reasoning summary {unsupported} for Anthropic model {model}"
                )));
            }
        };
        body["thinking"]["display"] = Value::String(display.to_string());
    }
    let anthropic_tools = tools.anthropic_tools();
    if !anthropic_tools.is_empty() {
        body.insert("tools".to_string(), Value::Array(anthropic_tools));
    }
    Ok(PreparedAnthropicRequest {
        body: Value::Object(body),
        tools,
    })
}

fn build_messages(
    input: &[Value],
    tools: &ToolCatalog,
    request: &Value,
) -> Result<(String, Vec<Value>), TranslationError> {
    let mut system = request
        .get("instructions")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let mut messages = Vec::new();
    for item in input {
        let item_type = item.get("type").and_then(Value::as_str).unwrap_or("");
        match item_type {
            "message" => {
                let role = item.get("role").and_then(Value::as_str).unwrap_or("user");
                let text = item_text(item);
                if matches!(role, "developer" | "system") {
                    if !system.is_empty() {
                        system.push_str("\n\n");
                    }
                    system.push_str(&text);
                } else {
                    let role = if role == "assistant" {
                        "assistant"
                    } else {
                        "user"
                    };
                    push_block(&mut messages, role, json!({"type": "text", "text": text}));
                }
            }
            "reasoning" => {
                if let Some(ProviderState::Anthropic { thinking_blocks }) = reasoning_state(item) {
                    for block in thinking_blocks {
                        push_block(&mut messages, "assistant", block);
                    }
                }
            }
            "function_call" | "custom_tool_call" => {
                let name = required(item, "name")?;
                let namespace = item.get("namespace").and_then(Value::as_str);
                let wire_name = tools.historical_wire_name(namespace, name);
                let input = if item_type == "custom_tool_call" {
                    json!({"input": item.get("input").and_then(Value::as_str).unwrap_or("")})
                } else {
                    serde_json::from_str(
                        item.get("arguments")
                            .and_then(Value::as_str)
                            .unwrap_or("{}"),
                    )
                    .unwrap_or_else(|_| json!({}))
                };
                push_block(
                    &mut messages,
                    "assistant",
                    json!({
                        "type": "tool_use",
                        "id": required(item, "call_id")?,
                        "name": wire_name,
                        "input": input
                    }),
                );
            }
            "function_call_output" | "custom_tool_call_output" => push_block(
                &mut messages,
                "user",
                json!({
                    "type": "tool_result",
                    "tool_use_id": required(item, "call_id")?,
                    "content": output_text(item.get("output"))
                }),
            ),
            "compaction_trigger" => {}
            other => {
                return Err(TranslationError::UnsupportedItem(format!(
                    "input type {other}"
                )));
            }
        }
    }
    Ok((system, messages))
}

fn push_block(messages: &mut Vec<Value>, role: &str, block: Value) {
    if let Some(last) = messages.last_mut()
        && last.get("role").and_then(Value::as_str) == Some(role)
        && let Some(content) = last.get_mut("content").and_then(Value::as_array_mut)
    {
        content.push(block);
        return;
    }
    messages.push(json!({"role": role, "content": [block]}));
}

fn required<'a>(item: &'a Value, field: &str) -> Result<&'a str, TranslationError> {
    item.get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| TranslationError::InvalidRequest(format!("missing {field}")))
}

enum AnthropicBlock {
    Thinking { thinking: String, signature: String },
    Tool { has_input: bool },
    Text,
    Opaque(Value),
}

pub(crate) struct AnthropicStreamTranslator {
    responses: Option<ResponsesStream>,
    blocks: BTreeMap<usize, AnthropicBlock>,
    thinking_blocks: Vec<Value>,
    usage: Usage,
    saw_stop_reason: bool,
    saw_message_stop: bool,
}

impl AnthropicStreamTranslator {
    pub fn new(model: impl Into<String>, tools: ToolCatalog) -> Self {
        Self {
            responses: Some(ResponsesStream::new(model, tools)),
            blocks: BTreeMap::new(),
            thinking_blocks: Vec::new(),
            usage: Usage::default(),
            saw_stop_reason: false,
            saw_message_stop: false,
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

    pub fn push(&mut self, event: &Value) -> Result<Vec<Value>, TranslationError> {
        let event_type = event.get("type").and_then(Value::as_str).unwrap_or("");
        let responses = self.responses.as_mut().expect("translator is active");
        let mut output = Vec::new();
        match event_type {
            "message_start" => {
                if let Some(usage) = event.pointer("/message/usage") {
                    update_usage(&mut self.usage, usage);
                }
            }
            "content_block_start" => {
                let index = event.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
                let block = event.get("content_block").cloned().unwrap_or(Value::Null);
                match block.get("type").and_then(Value::as_str).unwrap_or("") {
                    "thinking" => {
                        let thinking = block
                            .get("thinking")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string();
                        let signature = block
                            .get("signature")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string();
                        output.extend(responses.reasoning_delta(&thinking));
                        self.blocks.insert(
                            index,
                            AnthropicBlock::Thinking {
                                thinking,
                                signature,
                            },
                        );
                    }
                    "redacted_thinking" => {
                        self.blocks.insert(index, AnthropicBlock::Opaque(block));
                    }
                    "text" => {
                        let text = block.get("text").and_then(Value::as_str).unwrap_or("");
                        output.extend(responses.text_delta(text));
                        self.blocks.insert(index, AnthropicBlock::Text);
                    }
                    "tool_use" => {
                        let input = block.get("input").cloned().unwrap_or_else(|| json!({}));
                        let has_input = input.as_object().is_none_or(|input| !input.is_empty());
                        let input = has_input.then(|| input.to_string());
                        output.extend(responses.tool_delta(
                            index,
                            block.get("id").and_then(Value::as_str),
                            block.get("name").and_then(Value::as_str),
                            input.as_deref(),
                        )?);
                        self.blocks
                            .insert(index, AnthropicBlock::Tool { has_input });
                    }
                    other => {
                        return Err(TranslationError::UnsupportedItem(format!(
                            "Anthropic content block {other}"
                        )));
                    }
                }
            }
            "content_block_delta" => {
                let index = event.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
                let delta = event.get("delta").unwrap_or(&Value::Null);
                match delta.get("type").and_then(Value::as_str).unwrap_or("") {
                    "thinking_delta" => {
                        let text = delta.get("thinking").and_then(Value::as_str).unwrap_or("");
                        if let Some(AnthropicBlock::Thinking { thinking, .. }) =
                            self.blocks.get_mut(&index)
                        {
                            thinking.push_str(text);
                        }
                        output.extend(responses.reasoning_delta(text));
                    }
                    "signature_delta" => {
                        if let Some(AnthropicBlock::Thinking { signature, .. }) =
                            self.blocks.get_mut(&index)
                            && let Some(delta) = delta.get("signature").and_then(Value::as_str)
                        {
                            signature.push_str(delta);
                        }
                    }
                    "text_delta" => output.extend(
                        responses
                            .text_delta(delta.get("text").and_then(Value::as_str).unwrap_or("")),
                    ),
                    "input_json_delta" => {
                        if let Some(AnthropicBlock::Tool { has_input }) =
                            self.blocks.get_mut(&index)
                        {
                            *has_input = true;
                        }
                        output.extend(responses.tool_delta(
                            index,
                            None,
                            None,
                            delta.get("partial_json").and_then(Value::as_str),
                        )?);
                    }
                    other => {
                        return Err(TranslationError::UnsupportedItem(format!(
                            "Anthropic delta {other}"
                        )));
                    }
                }
            }
            "content_block_stop" => {
                let index = event.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
                match self.blocks.remove(&index) {
                    Some(AnthropicBlock::Thinking {
                        thinking,
                        signature,
                    }) => self.thinking_blocks.push(json!({
                        "type": "thinking",
                        "thinking": thinking,
                        "signature": signature
                    })),
                    Some(AnthropicBlock::Opaque(block)) => self.thinking_blocks.push(block),
                    Some(AnthropicBlock::Tool { has_input: false }) => {
                        output.extend(responses.tool_delta(index, None, None, Some("{}"))?);
                    }
                    _ => {}
                }
            }
            "message_delta" => {
                let stop_reason = event
                    .pointer("/delta/stop_reason")
                    .and_then(Value::as_str)
                    .filter(|reason| !reason.is_empty());
                self.saw_stop_reason |= stop_reason.is_some();
                responses.set_finish_reason(stop_reason);
                if let Some(usage) = event.get("usage") {
                    update_usage(&mut self.usage, usage);
                }
            }
            "error" => {
                let error = event
                    .get("error")
                    .cloned()
                    .unwrap_or_else(|| json!({"message": "unknown Anthropic stream error"}));
                return Err(TranslationError::InvalidRequest(format!(
                    "upstream provider error: {error}"
                )));
            }
            "message_stop" => self.saw_message_stop = true,
            "ping" => {}
            other => {
                return Err(TranslationError::UnsupportedItem(format!(
                    "Anthropic event {other}"
                )));
            }
        }
        responses.set_usage(self.usage.clone());
        Ok(output)
    }

    pub fn finish(mut self) -> Result<Vec<Value>, TranslationError> {
        if !self.blocks.is_empty() {
            return Err(TranslationError::InvalidRequest(
                "truncated Anthropic stream: content block was not closed".to_string(),
            ));
        }
        if !self.saw_stop_reason {
            return Err(TranslationError::InvalidRequest(
                "truncated Anthropic stream: no terminal stop_reason".to_string(),
            ));
        }
        if !self.saw_message_stop {
            return Err(TranslationError::InvalidRequest(
                "truncated Anthropic stream: missing message_stop".to_string(),
            ));
        }
        let state = (!self.thinking_blocks.is_empty()).then_some(ProviderState::Anthropic {
            thinking_blocks: self.thinking_blocks,
        });
        self.responses
            .take()
            .expect("translator is active")
            .finish(state)
    }
}

fn update_usage(target: &mut Usage, value: &Value) {
    if let Some(tokens) = value.get("input_tokens").and_then(Value::as_u64) {
        target.input_tokens = tokens;
    }
    if let Some(tokens) = value.get("output_tokens").and_then(Value::as_u64) {
        target.output_tokens = tokens;
    }
    target.cached_input_tokens = value
        .get("cache_read_input_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(target.cached_input_tokens);
}
