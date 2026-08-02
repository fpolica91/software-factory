use std::collections::BTreeMap;

use serde_json::Value;
use serde_json::json;
use uuid::Uuid;

use crate::responses::ProviderState;
use crate::responses::TranslationError;
use crate::responses::encode_provider_state;
use crate::tools::ToolBinding;
use crate::tools::ToolCatalog;
use crate::tools::ToolKind;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cached_input_tokens: u64,
    pub reasoning_tokens: u64,
}

#[derive(Debug)]
struct ToolOutput {
    item_id: String,
    call_id: String,
    wire_name: String,
    arguments: String,
    output_index: Option<usize>,
}

pub(crate) struct ResponsesStream {
    response_id: String,
    model: String,
    catalog: ToolCatalog,
    reasoning_id: String,
    message_id: String,
    reasoning: String,
    text: String,
    reasoning_output_index: Option<usize>,
    message_output_index: Option<usize>,
    tools: BTreeMap<usize, ToolOutput>,
    next_output_index: usize,
    finish_reason: Option<String>,
    usage: Usage,
}

impl ResponsesStream {
    pub fn new(model: impl Into<String>, catalog: ToolCatalog) -> Self {
        let suffix = Uuid::new_v4().simple().to_string();
        Self {
            response_id: format!("resp_{suffix}"),
            model: model.into(),
            catalog,
            reasoning_id: format!("rs_{suffix}"),
            message_id: format!("msg_{suffix}"),
            reasoning: String::new(),
            text: String::new(),
            reasoning_output_index: None,
            message_output_index: None,
            tools: BTreeMap::new(),
            next_output_index: 0,
            finish_reason: None,
            usage: Usage::default(),
        }
    }

    pub fn response_id(&self) -> &str {
        &self.response_id
    }

    pub fn created(&self) -> Value {
        json!({
            "type": "response.created",
            "response": {
                "id": self.response_id,
                "object": "response",
                "status": "in_progress",
                "model": self.model,
                "output": []
            }
        })
    }

    pub fn reasoning_delta(&mut self, delta: &str) -> Vec<Value> {
        if delta.is_empty() {
            return Vec::new();
        }
        let mut events = Vec::new();
        let output_index = *self.reasoning_output_index.get_or_insert_with(|| {
            let index = self.next_output_index;
            self.next_output_index += 1;
            events.push(json!({
                "type": "response.output_item.added",
                "output_index": index,
                "item": {
                    "id": self.reasoning_id,
                    "type": "reasoning",
                    "summary": [],
                    "content": null,
                    "encrypted_content": null
                }
            }));
            events.push(json!({
                "type": "response.reasoning_summary_part.added",
                "item_id": self.reasoning_id,
                "output_index": index,
                "summary_index": 0,
                "part": {"type": "summary_text", "text": ""}
            }));
            index
        });
        self.reasoning.push_str(delta);
        events.push(json!({
            "type": "response.reasoning_summary_text.delta",
            "item_id": self.reasoning_id,
            "output_index": output_index,
            "summary_index": 0,
            "delta": delta
        }));
        events
    }

    pub fn text_delta(&mut self, delta: &str) -> Vec<Value> {
        if delta.is_empty() {
            return Vec::new();
        }
        let mut events = Vec::new();
        let output_index = *self.message_output_index.get_or_insert_with(|| {
            let index = self.next_output_index;
            self.next_output_index += 1;
            events.push(json!({
                "type": "response.output_item.added",
                "output_index": index,
                "item": {
                    "id": self.message_id,
                    "type": "message",
                    "role": "assistant",
                    "status": "in_progress",
                    "content": []
                }
            }));
            index
        });
        self.text.push_str(delta);
        events.push(json!({
            "type": "response.output_text.delta",
            "item_id": self.message_id,
            "output_index": output_index,
            "content_index": 0,
            "delta": delta
        }));
        events
    }

    pub fn tool_delta(
        &mut self,
        index: usize,
        call_id: Option<&str>,
        wire_name: Option<&str>,
        arguments: Option<&str>,
    ) -> Result<Vec<Value>, TranslationError> {
        let suffix = Uuid::new_v4().simple().to_string();
        let tool = self.tools.entry(index).or_insert_with(|| ToolOutput {
            item_id: format!("fc_{suffix}"),
            call_id: String::new(),
            wire_name: String::new(),
            arguments: String::new(),
            output_index: None,
        });
        if let Some(call_id) = call_id {
            tool.call_id.push_str(call_id);
        }
        if let Some(wire_name) = wire_name {
            tool.wire_name.push_str(wire_name);
        }
        if let Some(arguments) = arguments {
            tool.arguments.push_str(arguments);
        }

        let mut events = Vec::new();
        if tool.output_index.is_none()
            && !tool.call_id.is_empty()
            && self.catalog.by_wire_name(&tool.wire_name).is_some()
            && (arguments.is_some() || !tool.arguments.is_empty())
        {
            let output_index = self.next_output_index;
            self.next_output_index += 1;
            tool.output_index = Some(output_index);
            let binding = self
                .catalog
                .by_wire_name(&tool.wire_name)
                .expect("binding was checked");
            events.push(output_item_added(output_index, tool, binding));
        }
        Ok(events)
    }

    pub fn set_finish_reason(&mut self, reason: Option<&str>) {
        if let Some(reason) = reason {
            self.finish_reason = Some(reason.to_string());
        }
    }

    pub fn set_usage(&mut self, usage: Usage) {
        self.usage = usage;
    }

    pub fn finish(
        mut self,
        provider_state: Option<ProviderState>,
    ) -> Result<Vec<Value>, TranslationError> {
        let mut events = Vec::new();
        if let Some(output_index) = self.reasoning_output_index {
            let encrypted_content = provider_state.as_ref().map(encode_provider_state);
            events.push(json!({
                "type": "response.reasoning_summary_text.done",
                "item_id": self.reasoning_id,
                "output_index": output_index,
                "summary_index": 0,
                "text": self.reasoning
            }));
            events.push(json!({
                "type": "response.output_item.done",
                "output_index": output_index,
                "item": reasoning_item(&self.reasoning_id, &self.reasoning, encrypted_content)
            }));
        } else if let Some(provider_state) = provider_state {
            let output_index = self.next_output_index;
            self.next_output_index += 1;
            events.push(json!({
                "type": "response.output_item.added",
                "output_index": output_index,
                "item": reasoning_item(&self.reasoning_id, "", None)
            }));
            events.push(json!({
                "type": "response.output_item.done",
                "output_index": output_index,
                "item": reasoning_item(
                    &self.reasoning_id,
                    "",
                    Some(encode_provider_state(&provider_state))
                )
            }));
        }
        if let Some(output_index) = self.message_output_index {
            events.push(json!({
                "type": "response.output_text.done",
                "item_id": self.message_id,
                "output_index": output_index,
                "content_index": 0,
                "text": self.text
            }));
            events.push(json!({
                "type": "response.output_item.done",
                "output_index": output_index,
                "item": {
                    "id": self.message_id,
                    "type": "message",
                    "role": "assistant",
                    "status": "completed",
                    "content": [{
                        "type": "output_text",
                        "text": self.text,
                        "annotations": []
                    }]
                }
            }));
        }
        for tool in self.tools.values_mut() {
            let binding = self
                .catalog
                .by_wire_name(&tool.wire_name)
                .ok_or_else(|| TranslationError::UnknownTool(tool.wire_name.clone()))?;
            let output_index = match tool.output_index {
                Some(index) => index,
                None => {
                    let index = self.next_output_index;
                    self.next_output_index += 1;
                    events.push(output_item_added(index, tool, binding));
                    index
                }
            };
            let custom_input = if matches!(&binding.kind, ToolKind::Custom { .. }) {
                Some(binding.normalize_custom_input(&tool.arguments)?)
            } else {
                None
            };
            if let Some(input) = custom_input.as_deref() {
                events.push(json!({
                    "type": "response.custom_tool_call_input.delta",
                    "item_id": tool.item_id,
                    "call_id": tool.call_id,
                    "output_index": output_index,
                    "delta": input
                }));
            }
            events.push(json!({
                "type": "response.output_item.done",
                "output_index": output_index,
                "item": completed_tool_item(tool, binding, custom_input.as_deref())
            }));
        }

        match self.finish_reason.as_deref() {
            Some("length" | "max_tokens") => events.push(json!({
                "type": "response.incomplete",
                "response": {
                    "id": self.response_id,
                    "status": "incomplete",
                    "incomplete_details": {"reason": "max_output_tokens"}
                }
            })),
            Some(
                "content_filter" | "sensitive" | "network_error" | "insufficient_system_resource",
            ) => {
                events.push(failed_event(
                    &self.response_id,
                    self.finish_reason.as_deref().unwrap_or("provider_error"),
                ));
            }
            _ => events.push(json!({
                "type": "response.completed",
                "response": {
                    "id": self.response_id,
                    "object": "response",
                    "status": "completed",
                    "model": self.model,
                    "end_turn": true,
                    "usage": {
                        "input_tokens": self.usage.input_tokens,
                        "output_tokens": self.usage.output_tokens,
                        "total_tokens": self.usage.input_tokens + self.usage.output_tokens,
                        "input_tokens_details": {
                            "cached_tokens": self.usage.cached_input_tokens
                        },
                        "output_tokens_details": {
                            "reasoning_tokens": self.usage.reasoning_tokens
                        }
                    }
                }
            })),
        }
        Ok(events)
    }
}

pub(crate) fn failed_event(response_id: &str, message: &str) -> Value {
    json!({
        "type": "response.failed",
        "response": {
            "id": response_id,
            "status": "failed",
            "error": {
                "code": "provider_error",
                "message": message
            }
        }
    })
}

fn reasoning_item(id: &str, text: &str, encrypted_content: Option<String>) -> Value {
    let summary = if text.is_empty() {
        Vec::new()
    } else {
        vec![json!({"type": "summary_text", "text": text})]
    };
    json!({
        "id": id,
        "type": "reasoning",
        "summary": summary,
        "content": null,
        "encrypted_content": encrypted_content
    })
}

fn output_item_added(output_index: usize, tool: &ToolOutput, binding: &ToolBinding) -> Value {
    let item = match &binding.kind {
        ToolKind::Function { name, namespace } => json!({
            "id": tool.item_id,
            "type": "function_call",
            "name": name,
            "namespace": namespace,
            "arguments": "",
            "call_id": tool.call_id
        }),
        ToolKind::Custom { name, namespace } => json!({
            "id": tool.item_id,
            "type": "custom_tool_call",
            "status": "in_progress",
            "name": name,
            "namespace": namespace,
            "input": "",
            "call_id": tool.call_id
        }),
    };
    json!({
        "type": "response.output_item.added",
        "output_index": output_index,
        "item": item
    })
}

fn completed_tool_item(
    tool: &ToolOutput,
    binding: &ToolBinding,
    custom_input: Option<&str>,
) -> Value {
    match &binding.kind {
        ToolKind::Function { name, namespace } => json!({
            "id": tool.item_id,
            "type": "function_call",
            "name": name,
            "namespace": namespace,
            "arguments": tool.arguments,
            "call_id": tool.call_id
        }),
        ToolKind::Custom { name, namespace } => json!({
            "id": tool.item_id,
            "type": "custom_tool_call",
            "status": "completed",
            "name": name,
            "namespace": namespace,
            "input": custom_input.expect("custom tool input was normalized"),
            "call_id": tool.call_id
        }),
    }
}
