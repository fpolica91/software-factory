use std::path::PathBuf;

use codex_protocol::models::ResponseItem;
use serde_json::Value;
use serde_json::json;

use crate::AdapterConfig;
use crate::CodexProviderSelection;
use crate::GENERATED_MODEL_CATALOG_PATH;
use crate::anthropic;
use crate::anthropic::AnthropicStreamTranslator;
use crate::chat;
use crate::chat::ChatStreamTranslator;
use crate::profiles::provider_profile;
use crate::profiles::provider_profiles;
use crate::responses::ProviderState;
use crate::responses::ToolCatalog;
use crate::responses::decode_provider_state;
use crate::responses::encode_provider_state;
use crate::server::anthropic_messages_url;

#[test]
fn profile_catalog_is_canonical_and_lightweight() {
    let profiles = provider_profiles();
    assert_eq!(
        profiles
            .iter()
            .map(|profile| profile.id)
            .collect::<Vec<_>>(),
        ["openai", "anthropic", "deepseek", "zai"]
    );
    for (index, profile) in profiles.iter().enumerate() {
        assert!(
            profiles[..index]
                .iter()
                .all(|candidate| candidate.id != profile.id)
        );
        assert!(profile.models.contains(&profile.default_model));
    }
    let anthropic = provider_profile("anthropic").unwrap();
    assert_eq!(
        anthropic.models,
        [
            "claude-haiku-4-5",
            "claude-sonnet-5",
            "claude-opus-5",
            "claude-fable-5"
        ]
    );
    assert!(provider_profile("claude").is_none());
}

#[test]
fn anthropic_request_uses_current_adaptive_thinking_contract() {
    let mut request = base_request();
    request["model"] = json!("claude-sonnet-5");
    request["reasoning"]["summary"] = json!("auto");
    let prepared = anthropic::prepare_request(&request, &config("anthropic")).unwrap();

    assert_eq!(
        prepared.body["thinking"],
        json!({"type": "adaptive", "display": "summarized"})
    );
    assert_eq!(prepared.body["output_config"]["effort"], "high");
    assert!(prepared.body.get("effort").is_none());
}

#[test]
fn anthropic_haiku_request_omits_unsupported_thinking_controls() {
    let mut request = base_request();
    request["model"] = json!("claude-haiku-4-5");
    request.as_object_mut().unwrap().remove("reasoning");
    let prepared = anthropic::prepare_request(&request, &config("anthropic")).unwrap();

    assert!(prepared.body.get("thinking").is_none());
    assert!(prepared.body.get("output_config").is_none());
    assert!(prepared.body.get("effort").is_none());
    assert_eq!(prepared.body["max_tokens"], 64_000);
}

#[test]
fn anthropic_haiku_preserves_a_lower_configured_output_limit() {
    let mut request = base_request();
    request["model"] = json!("claude-haiku-4-5-20251001");
    request.as_object_mut().unwrap().remove("reasoning");
    let mut adapter = config("anthropic");
    adapter.max_tokens = 4_096;
    let prepared = anthropic::prepare_request(&request, &adapter).unwrap();

    assert_eq!(prepared.body["max_tokens"], 4_096);
}

#[test]
fn anthropic_sonnet_45_aliases_clamp_the_default_output_limit() {
    for model in ["claude-sonnet-4-5", "claude-sonnet-4-5-20250929"] {
        let mut request = base_request();
        request["model"] = json!(model);
        request.as_object_mut().unwrap().remove("reasoning");
        let prepared = anthropic::prepare_request(&request, &config("anthropic")).unwrap();

        assert_eq!(prepared.body["max_tokens"], 64_000);
        assert!(prepared.body.get("thinking").is_none());
        assert!(prepared.body.get("output_config").is_none());
    }
}

#[test]
fn anthropic_summary_none_requests_omitted_thinking_display() {
    let mut request = base_request();
    request["model"] = json!("claude-sonnet-5");
    request["reasoning"]["summary"] = json!("none");
    let prepared = anthropic::prepare_request(&request, &config("anthropic")).unwrap();

    assert_eq!(
        prepared.body["thinking"],
        json!({"type": "adaptive", "display": "omitted"})
    );
}

#[test]
fn anthropic_disabled_thinking_never_includes_a_display() {
    let mut request = base_request();
    request["model"] = json!("claude-sonnet-5");
    request["reasoning"] = json!({"effort": "none", "summary": "auto"});
    let prepared = anthropic::prepare_request(&request, &config("anthropic")).unwrap();

    assert_eq!(prepared.body["thinking"], json!({"type": "disabled"}));
}

#[test]
fn anthropic_opus_45_snapshot_uses_effort_without_adaptive_thinking() {
    let mut request = base_request();
    request["model"] = json!("claude-opus-4-5-20251101");
    let prepared = anthropic::prepare_request(&request, &config("anthropic")).unwrap();

    assert!(prepared.body.get("thinking").is_none());
    assert_eq!(prepared.body["output_config"]["effort"], "high");
}

#[test]
fn anthropic_rejects_effort_levels_the_selected_model_does_not_support() {
    let mut request = base_request();
    request["model"] = json!("claude-sonnet-4-6");
    request["reasoning"]["effort"] = json!("xhigh");

    let error = match anthropic::prepare_request(&request, &config("anthropic")) {
        Ok(_) => panic!("Sonnet 4.6 must reject xhigh effort"),
        Err(error) => error,
    };
    assert!(
        error
            .to_string()
            .contains("does not support Anthropic effort xhigh")
    );
}

#[test]
fn anthropic_unknown_custom_model_remains_usable_without_effort() {
    for effort in [None, Some("none")] {
        let mut request = base_request();
        request["model"] = json!("claude-private-model");
        match effort {
            Some(effort) => request["reasoning"]["effort"] = json!(effort),
            None => {
                request.as_object_mut().unwrap().remove("reasoning");
            }
        }
        let prepared = anthropic::prepare_request(&request, &config("anthropic")).unwrap();

        assert!(prepared.body.get("thinking").is_none());
        assert!(prepared.body.get("output_config").is_none());
    }
}

#[test]
fn anthropic_rejects_explicit_effort_for_an_unknown_custom_model() {
    let mut request = base_request();
    request["model"] = json!("claude-private-model");

    let error = match anthropic::prepare_request(&request, &config("anthropic")) {
        Ok(_) => panic!("an unknown custom model must not silently discard explicit effort"),
        Err(error) => error,
    };
    assert!(error.to_string().contains(
        "cannot determine whether custom Anthropic model claude-private-model supports effort high"
    ));
}

#[test]
fn anthropic_rejects_disabling_thinking_on_always_adaptive_models() {
    let mut request = base_request();
    request["model"] = json!("claude-fable-5");
    request["reasoning"]["effort"] = json!("none");

    let error = match anthropic::prepare_request(&request, &config("anthropic")) {
        Ok(_) => panic!("Fable 5 must reject disabled thinking"),
        Err(error) => error,
    };
    assert!(
        error
            .to_string()
            .contains("does not support disabling adaptive thinking")
    );
}

#[test]
fn anthropic_messages_endpoint_removes_at_most_one_v1_suffix() {
    assert_eq!(
        anthropic_messages_url("https://api.anthropic.com"),
        "https://api.anthropic.com/v1/messages"
    );
    assert_eq!(
        anthropic_messages_url("https://api.anthropic.com/v1/"),
        "https://api.anthropic.com/v1/messages"
    );
    assert_eq!(
        anthropic_messages_url("https://gateway.example/tenant/v1/v1"),
        "https://gateway.example/tenant/v1/v1/messages"
    );
}

#[test]
fn codex_selection_uses_keys_only_for_direct_responses() {
    let openai = provider_profile("openai").unwrap();
    let direct = CodexProviderSelection::for_profile(
        openai,
        openai.base_urls[0].url,
        openai.default_model,
        None,
    );
    assert_eq!(
        direct.config["model_providers.factory-provider"]["env_key"],
        "OPENAI_API_KEY"
    );

    let zai = provider_profile("zai").unwrap();
    let adapter = CodexProviderSelection::for_profile(
        zai,
        "http://127.0.0.1:10101/v1",
        zai.default_model,
        None,
    );
    assert!(
        adapter.config["model_providers.factory-provider"]
            .get("env_key")
            .is_none()
    );
}

#[test]
fn adapter_profiles_default_legacy_blank_catalog_and_honor_explicit_override() {
    let deepseek = provider_profile("deepseek").unwrap();
    assert_eq!(deepseek.context_window, 128_000);

    let legacy_blank = CodexProviderSelection::for_profile(
        deepseek,
        "http://deepseek-provider:10101/v1",
        deepseek.default_model,
        Some(PathBuf::new()),
    );
    assert_eq!(
        legacy_blank.model_catalog_json,
        Some(PathBuf::from(GENERATED_MODEL_CATALOG_PATH))
    );
    assert_eq!(
        legacy_blank.config["model_catalog_json"],
        GENERATED_MODEL_CATALOG_PATH
    );

    let override_path = PathBuf::from("/tmp/factory-explicit-models.json");
    let explicit = CodexProviderSelection::for_profile(
        deepseek,
        "http://deepseek-provider:10101/v1",
        deepseek.default_model,
        Some(override_path.clone()),
    );
    assert_eq!(explicit.model_catalog_json, Some(override_path));

    let openai = provider_profile("openai").unwrap();
    let direct = CodexProviderSelection::for_profile(
        openai,
        openai.base_urls[0].url,
        openai.default_model,
        None,
    );
    assert_eq!(direct.model_catalog_json, None);
    assert!(!direct.config.contains_key("model_catalog_json"));
}

#[test]
fn chat_request_maps_namespaces_freeform_results_and_reasoning_replay() {
    let config = config("zai");
    let state = encode_provider_state(&ProviderState::Chat {
        reasoning_content: "prior reasoning".to_string(),
    });
    let request = request_fixture(state);
    let prepared = chat::prepare_request(&request, &config).unwrap();

    assert_eq!(prepared.body["model"], "glm-5.2");
    assert_eq!(prepared.body["tool_stream"], true);
    assert_eq!(prepared.body["thinking"]["clear_thinking"], false);
    let tools = prepared.body["tools"].as_array().unwrap();
    assert_eq!(tools.len(), 3);
    assert!(tools.iter().any(|tool| {
        tool["function"]["name"] == "web__run" && tool["function"]["parameters"]["type"] == "object"
    }));
    assert!(tools.iter().any(|tool| {
        tool["function"]["name"] == "apply_patch"
            && tool["function"]["parameters"]["required"] == json!(["patch"])
            && tool["function"]["parameters"]["additionalProperties"] == false
            && tool["function"]["description"]
                .as_str()
                .unwrap()
                .contains("*** Update File: path/to/file\n@@\n-old line\n+new line")
            && tool["function"]["description"]
                .as_str()
                .unwrap()
                .contains("Never use numbered unified-diff range headers")
            && tool["function"]["parameters"]["properties"]["patch"]["description"]
                .as_str()
                .unwrap()
                .contains("never numbered unified-diff ranges")
    }));
    let messages = prepared.body["messages"].as_array().unwrap();
    let assistant = messages
        .iter()
        .find(|message| message["role"] == "assistant")
        .unwrap();
    assert_eq!(assistant["reasoning_content"], "prior reasoning");
    assert_eq!(assistant["tool_calls"][0]["function"]["name"], "web__run");
    assert!(messages.iter().any(|message| {
        message["role"] == "tool"
            && message["tool_call_id"] == "call_old"
            && message["content"] == "old result"
    }));
}

#[test]
fn chat_request_replays_apply_patch_with_the_chat_schema() {
    let mut request = base_request();
    request["input"] = json!([
        {
            "type": "message",
            "role": "user",
            "content": [{"type": "input_text", "text": "Make the change."}]
        },
        {
            "type": "custom_tool_call",
            "name": "apply_patch",
            "input": valid_patch(),
            "call_id": "call_patch"
        },
        {
            "type": "custom_tool_call_output",
            "call_id": "call_patch",
            "output": "Done!"
        }
    ]);

    let prepared = chat::prepare_request(&request, &config("deepseek")).unwrap();
    let assistant = prepared.body["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|message| message["role"] == "assistant")
        .unwrap();
    let arguments: Value = serde_json::from_str(
        assistant["tool_calls"][0]["function"]["arguments"]
            .as_str()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(arguments, json!({"patch": valid_patch()}));
}

#[test]
fn anthropic_keeps_the_generic_custom_tool_schema() {
    let prepared = anthropic::prepare_request(&anthropic_request(), &config("anthropic")).unwrap();
    let apply_patch = prepared.body["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|tool| tool["name"] == "apply_patch")
        .unwrap();
    assert_eq!(apply_patch["input_schema"]["required"], json!(["input"]));
}

#[test]
fn chat_request_replays_a_tool_hidden_from_the_current_turn() {
    let request = request_with_hidden_goal_history();
    let prepared = chat::prepare_request(&request, &config("deepseek")).unwrap();
    let messages = prepared.body["messages"].as_array().unwrap();
    let assistant = messages
        .iter()
        .find(|message| message["role"] == "assistant")
        .unwrap();
    assert_eq!(
        assistant["tool_calls"][0]["function"]["name"],
        "goals__get_goal"
    );
    assert!(messages.iter().any(|message| {
        message["role"] == "tool"
            && message["tool_call_id"] == "call_goal"
            && message["content"] == "goal result"
    }));
}

#[test]
fn chat_stream_emits_parallel_namespace_and_freeform_calls_with_state() {
    let config = config("zai");
    let prepared = chat::prepare_request(&base_request(), &config).unwrap();
    let mut stream = ChatStreamTranslator::new("glm-5.2", prepared.tools);
    let arguments = json!({"patch": valid_patch()}).to_string();
    let split = arguments.len() / 2;
    let mut events = vec![stream.created()];
    events.extend(
        stream
            .push(&json!({
                "choices": [{
                    "delta": {
                        "reasoning_content": "inspect first",
                        "tool_calls": [
                            {"index": 0, "id": "call_patch", "function": {"name": "apply_patch", "arguments": &arguments[..split]}},
                            {"index": 1, "id": "call_web", "function": {"name": "web__run", "arguments": "{\"q\":\"rust\"}"}}
                        ]
                    },
                    "finish_reason": null
                }]
            }))
            .unwrap(),
    );
    events.extend(
        stream
            .push(&json!({
                "choices": [{
                    "delta": {
                        "tool_calls": [
                            {"index": 0, "function": {"arguments": &arguments[split..]}}
                        ]
                    },
                    "finish_reason": "tool_calls"
                }],
                "usage": {
                    "prompt_tokens": 20,
                    "completion_tokens": 7,
                    "completion_tokens_details": {"reasoning_tokens": 3}
                }
            }))
            .unwrap(),
    );
    events.extend(stream.finish(true).unwrap());

    let done_items = output_items(&events);
    assert_codex_items_parse(&done_items);
    let patch = done_items
        .iter()
        .find(|item| item["type"] == "custom_tool_call")
        .unwrap();
    assert_eq!(patch["name"], "apply_patch");
    assert_eq!(patch["input"], valid_patch());
    let web = done_items
        .iter()
        .find(|item| item["type"] == "function_call")
        .unwrap();
    assert_eq!(web["name"], "run");
    assert_eq!(web["namespace"], "web");
    assert_eq!(web["arguments"], "{\"q\":\"rust\"}");

    let reasoning = done_items
        .iter()
        .find(|item| item["type"] == "reasoning")
        .unwrap();
    let state = decode_provider_state(reasoning["encrypted_content"].as_str().unwrap()).unwrap();
    assert_eq!(
        state,
        ProviderState::Chat {
            reasoning_content: "inspect first".to_string()
        }
    );
    let completed = events
        .iter()
        .find(|event| event["type"] == "response.completed")
        .unwrap();
    assert_eq!(completed["response"]["usage"]["total_tokens"], 27);
}

#[test]
fn apply_patch_normalizes_common_provider_argument_shapes() {
    let catalog = ToolCatalog::from_request(&base_request()).unwrap();
    let binding = catalog.by_wire_name("apply_patch").unwrap();
    let heredoc = format!("<<'EOF'\n{}\nEOF\n", valid_patch());
    let arguments = [
        valid_patch().to_string(),
        serde_json::to_string(valid_patch()).unwrap(),
        json!({"patch": valid_patch()}).to_string(),
        json!({"input": valid_patch()}).to_string(),
        heredoc,
    ];

    for argument in arguments {
        assert_eq!(
            binding.normalize_custom_input(&argument).unwrap(),
            valid_patch(),
            "failed to normalize {argument}"
        );
    }
}

#[test]
fn apply_patch_rejects_ambiguous_malformed_and_invalid_arguments() {
    let catalog = ToolCatalog::from_request(&base_request()).unwrap();
    let binding = catalog.by_wire_name("apply_patch").unwrap();
    let invalid_arguments = [
        json!({"patch": valid_patch(), "input": valid_patch()}).to_string(),
        json!({"diff": valid_patch()}).to_string(),
        json!({"patch": 42}).to_string(),
        json!({"patch": "*** Begin Patch\n*** End Patch"}).to_string(),
        "{\"patch\":".to_string(),
    ];

    for argument in invalid_arguments {
        let error = binding.normalize_custom_input(&argument).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("invalid arguments for tool apply_patch"),
            "unexpected error for {argument}: {error}"
        );
    }
}

#[test]
fn chat_stream_rejects_an_invalid_apply_patch_instead_of_forwarding_it() {
    let prepared = chat::prepare_request(&base_request(), &config("deepseek")).unwrap();
    let mut stream = ChatStreamTranslator::new("deepseek-chat", prepared.tools);
    stream
        .push(&json!({
            "choices": [{
                "delta": {
                    "tool_calls": [{
                        "index": 0,
                        "id": "call_patch",
                        "function": {
                            "name": "apply_patch",
                            "arguments": json!({"patch": "*** Begin Patch\n*** End Patch"}).to_string()
                        }
                    }]
                },
                "finish_reason": "tool_calls"
            }]
        }))
        .unwrap();

    let error = stream.finish(true).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("invalid arguments for tool apply_patch")
    );
}

#[test]
fn chat_stream_resolves_an_unambiguous_original_namespace_tool_name() {
    let mut request = base_request();
    request["tools"] = json!([{
        "type": "namespace",
        "name": "goals",
        "description": "Durable goal tools",
        "tools": [{
            "type": "function",
            "name": "get_goal",
            "description": "Read the current goal",
            "strict": false,
            "parameters": {"type": "object", "properties": {}}
        }]
    }]);
    let prepared = chat::prepare_request(&request, &config("zai")).unwrap();
    assert_eq!(
        prepared.body["tools"][0]["function"]["name"],
        "goals__get_goal"
    );

    let mut stream = ChatStreamTranslator::new("glm-5.2", prepared.tools);
    stream
        .push(&json!({
            "choices": [{
                "delta": {
                    "tool_calls": [{
                        "index": 0,
                        "id": "call_goal",
                        "function": {"name": "get_goal", "arguments": "{}"}
                    }]
                },
                "finish_reason": "tool_calls"
            }]
        }))
        .unwrap();
    let events = stream.finish(true).unwrap();
    let item = output_items(&events)
        .into_iter()
        .find(|item| item["type"] == "function_call")
        .unwrap();
    assert_eq!(item["name"], "get_goal");
    assert_eq!(item["namespace"], "goals");
}

#[test]
fn tool_names_never_alias_another_binding() {
    let request = json!({
        "tools": [
            {
                "type": "namespace",
                "name": "goals",
                "tools": [{
                    "type": "function",
                    "name": "get_goal",
                    "parameters": {"type": "object", "properties": {}}
                }]
            },
            {
                "type": "function",
                "name": "goals__get_goal",
                "parameters": {"type": "object", "properties": {}}
            },
            {
                "type": "function",
                "name": "factory_tool_0",
                "parameters": {"type": "object", "properties": {}}
            }
        ]
    });
    let catalog = ToolCatalog::from_request(&request).unwrap();
    let namespaced_wire = catalog.wire_name(Some("goals"), "get_goal").unwrap();
    assert_ne!(namespaced_wire, "goals__get_goal");
    assert_ne!(namespaced_wire, "factory_tool_0");

    let namespaced = catalog.by_wire_name("get_goal").unwrap();
    assert_eq!(namespaced.kind.namespace(), Some("goals"));
    let plain = catalog.by_wire_name("goals__get_goal").unwrap();
    assert_eq!(plain.kind.namespace(), None);
    let synthetic_lookalike = catalog.by_wire_name("factory_tool_0").unwrap();
    assert_eq!(synthetic_lookalike.kind.name(), "factory_tool_0");
    assert_eq!(
        catalog
            .by_wire_name(namespaced_wire)
            .unwrap()
            .kind
            .namespace(),
        Some("goals")
    );
}

#[test]
fn chat_stream_rejects_missing_terminal_signals() {
    let prepared = chat::prepare_request(&base_request(), &config("zai")).unwrap();
    let mut without_reason = ChatStreamTranslator::new("glm-5.2", prepared.tools.clone());
    without_reason
        .push(&json!({
            "choices": [{"delta": {"content": "partial"}, "finish_reason": null}]
        }))
        .unwrap();
    assert!(
        without_reason
            .finish(true)
            .unwrap_err()
            .to_string()
            .contains("finish_reason")
    );

    let mut without_done = ChatStreamTranslator::new("glm-5.2", prepared.tools);
    without_done
        .push(&json!({
            "choices": [{"delta": {"content": "complete"}, "finish_reason": "stop"}]
        }))
        .unwrap();
    assert!(
        without_done
            .finish(false)
            .unwrap_err()
            .to_string()
            .contains("[DONE]")
    );
}

#[test]
fn anthropic_request_replays_signed_thinking_and_tool_results_unchanged() {
    let config = config("anthropic");
    let state = encode_provider_state(&ProviderState::Anthropic {
        thinking_blocks: vec![json!({
            "type": "thinking",
            "thinking": "keep this exactly",
            "signature": "opaque-signature"
        })],
    });
    let mut request = request_fixture(state);
    request["model"] = json!("claude-sonnet-5");
    let prepared = anthropic::prepare_request(&request, &config).unwrap();
    let messages = prepared.body["messages"].as_array().unwrap();
    let assistant = messages
        .iter()
        .find(|message| message["role"] == "assistant")
        .unwrap();
    assert_eq!(assistant["content"][0]["thinking"], "keep this exactly");
    assert_eq!(assistant["content"][0]["signature"], "opaque-signature");
    assert_eq!(assistant["content"][1]["type"], "tool_use");
    let result = messages
        .iter()
        .find(|message| message["role"] == "user" && message["content"][0]["type"] == "tool_result")
        .unwrap();
    assert_eq!(result["content"][0]["content"], "old result");
}

#[test]
fn anthropic_request_replays_a_tool_hidden_from_the_current_turn() {
    let mut request = request_with_hidden_goal_history();
    request["model"] = json!("claude-sonnet-5");
    let prepared = anthropic::prepare_request(&request, &config("anthropic")).unwrap();
    let messages = prepared.body["messages"].as_array().unwrap();
    let assistant = messages
        .iter()
        .find(|message| message["role"] == "assistant")
        .unwrap();
    assert_eq!(assistant["content"][0]["name"], "goals__get_goal");
    let result = messages
        .iter()
        .find(|message| message["role"] == "user" && message["content"][0]["type"] == "tool_result")
        .unwrap();
    assert_eq!(result["content"][0]["content"], "goal result");
}

#[test]
fn anthropic_stream_preserves_signature_and_streamed_tool_arguments() {
    let config = config("anthropic");
    let prepared = anthropic::prepare_request(&anthropic_request(), &config).unwrap();
    let mut stream = AnthropicStreamTranslator::new("claude-sonnet-5", prepared.tools);
    let arguments = json!({"input": valid_patch()}).to_string();
    let split = arguments.len() / 2;
    let mut events = vec![stream.created()];
    for fixture in [
        json!({"type": "message_start", "message": {"usage": {"input_tokens": 11}}}),
        json!({"type": "content_block_start", "index": 0, "content_block": {"type": "thinking", "thinking": "", "signature": ""}}),
        json!({"type": "content_block_delta", "index": 0, "delta": {"type": "thinking_delta", "thinking": "check"}}),
        json!({"type": "content_block_delta", "index": 0, "delta": {"type": "signature_delta", "signature": "signed"}}),
        json!({"type": "content_block_stop", "index": 0}),
        json!({"type": "content_block_start", "index": 1, "content_block": {"type": "tool_use", "id": "call_patch", "name": "apply_patch", "input": {}}}),
        json!({"type": "content_block_delta", "index": 1, "delta": {"type": "input_json_delta", "partial_json": &arguments[..split]}}),
        json!({"type": "content_block_delta", "index": 1, "delta": {"type": "input_json_delta", "partial_json": &arguments[split..]}}),
        json!({"type": "content_block_stop", "index": 1}),
        json!({"type": "message_delta", "delta": {"stop_reason": "tool_use"}, "usage": {"output_tokens": 5}}),
        json!({"type": "message_stop"}),
    ] {
        events.extend(stream.push(&fixture).unwrap());
    }
    events.extend(stream.finish().unwrap());

    let done_items = output_items(&events);
    assert_codex_items_parse(&done_items);
    let patch = done_items
        .iter()
        .find(|item| item["type"] == "custom_tool_call")
        .unwrap();
    assert_eq!(patch["input"], valid_patch());
    let reasoning = done_items
        .iter()
        .find(|item| item["type"] == "reasoning")
        .unwrap();
    let state = decode_provider_state(reasoning["encrypted_content"].as_str().unwrap()).unwrap();
    assert_eq!(
        state,
        ProviderState::Anthropic {
            thinking_blocks: vec![json!({
                "type": "thinking",
                "thinking": "check",
                "signature": "signed"
            })]
        }
    );
}

#[test]
fn anthropic_stream_rejects_open_blocks_and_missing_message_stop() {
    let prepared = anthropic::prepare_request(&anthropic_request(), &config("anthropic")).unwrap();
    let mut open_block = AnthropicStreamTranslator::new("claude-sonnet-5", prepared.tools.clone());
    open_block
        .push(&json!({
            "type": "content_block_start",
            "index": 0,
            "content_block": {"type": "text", "text": "partial"}
        }))
        .unwrap();
    open_block
        .push(&json!({"type": "message_delta", "delta": {"stop_reason": "end_turn"}}))
        .unwrap();
    open_block.push(&json!({"type": "message_stop"})).unwrap();
    assert!(
        open_block
            .finish()
            .unwrap_err()
            .to_string()
            .contains("content block")
    );

    let mut without_stop = AnthropicStreamTranslator::new("claude-sonnet-5", prepared.tools);
    without_stop
        .push(&json!({"type": "message_delta", "delta": {"stop_reason": "end_turn"}}))
        .unwrap();
    assert!(
        without_stop
            .finish()
            .unwrap_err()
            .to_string()
            .contains("message_stop")
    );
}

fn config(id: &'static str) -> AdapterConfig {
    let profile = provider_profile(id).unwrap();
    AdapterConfig {
        bind_host: "127.0.0.1".to_string(),
        port: 0,
        advertised_base_url: "http://127.0.0.1:0/v1".to_string(),
        profile,
        model: profile.default_model.to_string(),
        upstream_base_url: profile.base_urls[0].url.to_string(),
        api_key_env: profile.api_key_env.to_string(),
        api_key: "fixture-key".to_string(),
        state_dir: PathBuf::from("target/provider-fixture"),
        max_tokens: 65_536,
    }
}

fn base_request() -> Value {
    json!({
        "model": "glm-5.2",
        "instructions": "Use the tools.",
        "input": [{
            "type": "message",
            "role": "user",
            "content": [{"type": "input_text", "text": "Change and inspect."}]
        }],
        "tools": [
            {
                "type": "function",
                "name": "shell",
                "description": "Run a command",
                "strict": false,
                "parameters": {"type": "object", "properties": {"cmd": {"type": "string"}}, "required": ["cmd"]}
            },
            {
                "type": "namespace",
                "name": "web",
                "description": "Web tools",
                "tools": [{
                    "type": "function",
                    "name": "run",
                    "description": "Run a web query",
                    "strict": false,
                    "parameters": {"type": "object", "properties": {"q": {"type": "string"}}, "required": ["q"]}
                }]
            },
            {
                "type": "custom",
                "name": "apply_patch",
                "description": "Apply a patch",
                "format": {"type": "grammar", "syntax": "lark", "definition": "start: /[\\s\\S]+/"}
            }
        ],
        "tool_choice": "auto",
        "parallel_tool_calls": true,
        "reasoning": {"effort": "high", "summary": "auto"},
        "store": false,
        "stream": true,
        "include": ["reasoning.encrypted_content"]
    })
}

fn anthropic_request() -> Value {
    let mut request = base_request();
    request["model"] = json!("claude-sonnet-5");
    request
}

fn valid_patch() -> &'static str {
    "*** Begin Patch\n*** Add File: provider-bridge-test.txt\n+translated\n*** End Patch"
}

fn request_fixture(encrypted_state: String) -> Value {
    let mut request = base_request();
    request["input"] = json!([
        {
            "type": "message",
            "role": "user",
            "content": [{"type": "input_text", "text": "Use web."}]
        },
        {
            "type": "reasoning",
            "summary": [],
            "content": null,
            "encrypted_content": encrypted_state
        },
        {
            "type": "function_call",
            "name": "run",
            "namespace": "web",
            "arguments": "{\"q\":\"rust\"}",
            "call_id": "call_old"
        },
        {
            "type": "function_call_output",
            "call_id": "call_old",
            "output": "old result"
        }
    ]);
    request
}

fn request_with_hidden_goal_history() -> Value {
    let mut request = base_request();
    request["input"] = json!([
        {
            "type": "message",
            "role": "user",
            "content": [{"type": "input_text", "text": "Review the completed work."}]
        },
        {
            "type": "function_call",
            "name": "get_goal",
            "namespace": "goals",
            "arguments": "{}",
            "call_id": "call_goal"
        },
        {
            "type": "function_call_output",
            "call_id": "call_goal",
            "output": "goal result"
        }
    ]);
    request
}

fn output_items(events: &[Value]) -> Vec<&Value> {
    events
        .iter()
        .filter(|event| event["type"] == "response.output_item.done")
        .map(|event| &event["item"])
        .collect()
}

fn assert_codex_items_parse(items: &[&Value]) {
    for item in items {
        serde_json::from_value::<ResponseItem>((*item).clone()).unwrap();
    }
}
