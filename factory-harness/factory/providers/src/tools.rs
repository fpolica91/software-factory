use std::collections::BTreeMap;
use std::collections::BTreeSet;

use serde_json::Value;
use serde_json::json;

use crate::responses::TranslationError;

const APPLY_PATCH_TOOL_NAME: &str = "apply_patch";
const APPLY_PATCH_CHAT_DESCRIPTION: &str = r#"Apply a patch to files using Codex apply_patch grammar, not standard unified-diff grammar. Pass the complete raw patch in `patch`. For an update, follow this exact form:
*** Begin Patch
*** Update File: path/to/file
@@
-old line
+new line
*** End Patch
The `-` and `+` must be the first character of each changed line. Use a bare `@@` hunk header, or `@@` followed by literal source context. Never use numbered unified-diff range headers such as `@@ -1 +1 @@`. The patch must contain at least one add, delete, or update hunk."#;
const APPLY_PATCH_CHAT_INPUT_DESCRIPTION: &str = "Complete raw Codex apply_patch text using the exact grammar example in the tool description. Update hunks use bare `@@` (or `@@` plus literal source context), never numbered unified-diff ranges such as `@@ -1 +1 @@`; `-` and `+` are the first character of changed lines.";

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum ToolKind {
    Function {
        name: String,
        namespace: Option<String>,
    },
    Custom {
        name: String,
        namespace: Option<String>,
    },
}

impl ToolKind {
    pub fn name(&self) -> &str {
        match self {
            Self::Function { name, .. } | Self::Custom { name, .. } => name,
        }
    }

    pub fn namespace(&self) -> Option<&str> {
        match self {
            Self::Function { namespace, .. } | Self::Custom { namespace, .. } => {
                namespace.as_deref()
            }
        }
    }

    fn is_apply_patch(&self) -> bool {
        matches!(
            self,
            Self::Custom {
                name,
                namespace: None
            } if name == APPLY_PATCH_TOOL_NAME
        )
    }
}

#[derive(Debug, Clone)]
pub(crate) struct ToolBinding {
    pub wire_name: String,
    pub kind: ToolKind,
    pub description: String,
    pub input_schema: Value,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct ToolCatalog {
    bindings: Vec<ToolBinding>,
}

impl ToolBinding {
    fn chat_description(&self) -> &str {
        if self.kind.is_apply_patch() {
            APPLY_PATCH_CHAT_DESCRIPTION
        } else {
            &self.description
        }
    }

    fn chat_input_schema(&self) -> Value {
        if self.kind.is_apply_patch() {
            json!({
                "type": "object",
                "properties": {
                    "patch": {
                        "type": "string",
                        "description": APPLY_PATCH_CHAT_INPUT_DESCRIPTION
                    }
                },
                "required": ["patch"],
                "additionalProperties": false
            })
        } else {
            self.input_schema.clone()
        }
    }

    pub fn normalize_custom_input(&self, arguments: &str) -> Result<String, TranslationError> {
        if self.kind.is_apply_patch() {
            normalize_apply_patch(arguments)
        } else {
            Ok(generic_custom_input(arguments))
        }
    }
}

impl ToolCatalog {
    pub fn from_request(request: &Value) -> Result<Self, TranslationError> {
        let mut bindings = Vec::new();
        let tools = request
            .get("tools")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        for tool in tools {
            let kind = tool.get("type").and_then(Value::as_str).unwrap_or("");
            match kind {
                "function" => push_function(&mut bindings, &tool, None)?,
                "namespace" => {
                    let namespace = required_string(&tool, "name")?;
                    let namespace_description = tool
                        .get("description")
                        .and_then(Value::as_str)
                        .unwrap_or("");
                    let nested = tool.get("tools").and_then(Value::as_array).ok_or_else(|| {
                        TranslationError::InvalidRequest(format!(
                            "namespace {namespace} is missing tools"
                        ))
                    })?;
                    for nested_tool in nested {
                        let mut nested_tool = nested_tool.clone();
                        if nested_tool
                            .get("description")
                            .and_then(Value::as_str)
                            .is_none_or(str::is_empty)
                            && let Some(object) = nested_tool.as_object_mut()
                        {
                            object.insert(
                                "description".to_string(),
                                Value::String(namespace_description.to_string()),
                            );
                        }
                        push_function(&mut bindings, &nested_tool, Some(namespace.clone()))?;
                    }
                }
                "custom" => {
                    let name = required_string(&tool, "name")?;
                    let description = tool
                        .get("description")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string();
                    let input_schema = json!({
                        "type": "object",
                        "properties": {
                            "input": {
                                "type": "string",
                                "description": "Raw freeform input for this tool"
                            }
                        },
                        "required": ["input"],
                        "additionalProperties": false
                    });
                    bindings.push(ToolBinding {
                        wire_name: String::new(),
                        kind: ToolKind::Custom {
                            name,
                            namespace: None,
                        },
                        description,
                        input_schema,
                    });
                }
                // Web search is a hosted Responses API tool. Chat-based providers cannot
                // execute it, so do not advertise it to those models.
                "web_search" => continue,
                "tool_search" => {
                    return Err(TranslationError::UnsupportedItem(kind.to_string()));
                }
                other => {
                    return Err(TranslationError::UnsupportedItem(format!(
                        "tool type {other}"
                    )));
                }
            }
        }
        assign_wire_names(&mut bindings);
        Ok(Self { bindings })
    }

    pub fn chat_tools(&self) -> Vec<Value> {
        self.bindings
            .iter()
            .map(|binding| {
                json!({
                    "type": "function",
                    "function": {
                        "name": binding.wire_name,
                        "description": binding.chat_description(),
                        "parameters": binding.chat_input_schema()
                    }
                })
            })
            .collect()
    }

    pub fn anthropic_tools(&self) -> Vec<Value> {
        self.bindings
            .iter()
            .map(|binding| {
                json!({
                    "name": binding.wire_name,
                    "description": binding.description,
                    "input_schema": binding.input_schema
                })
            })
            .collect()
    }

    pub fn by_wire_name(&self, name: &str) -> Option<&ToolBinding> {
        let mut matches = self
            .bindings
            .iter()
            .filter(|binding| binding.wire_name == name || binding.kind.name() == name);
        let binding = matches.next()?;
        matches.next().is_none().then_some(binding)
    }

    pub fn wire_name(&self, namespace: Option<&str>, name: &str) -> Option<&str> {
        self.bindings
            .iter()
            .find(|binding| binding.kind.name() == name && binding.kind.namespace() == namespace)
            .map(|binding| binding.wire_name.as_str())
    }

    /// Returns the provider-facing name for a tool call already present in
    /// conversation history. A detached thread can inherit calls to tools that
    /// are intentionally unavailable in its current turn, so replay must not
    /// require a live binding for every historical call.
    pub fn historical_wire_name(&self, namespace: Option<&str>, name: &str) -> String {
        self.wire_name(namespace, name)
            .map(str::to_string)
            .unwrap_or_else(|| {
                let mut wire_name = sanitized_wire_name(namespace, name);
                if wire_name.is_empty() {
                    return "factory_historical_tool".to_string();
                }
                wire_name.truncate(64);
                wire_name
            })
    }

    pub fn chat_custom_arguments(
        &self,
        namespace: Option<&str>,
        name: &str,
        input: &str,
    ) -> String {
        if namespace.is_none() && name == APPLY_PATCH_TOOL_NAME {
            json!({"patch": input}).to_string()
        } else {
            json!({"input": input}).to_string()
        }
    }
}

fn normalize_apply_patch(arguments: &str) -> Result<String, TranslationError> {
    let trimmed = arguments.trim();
    let candidate = match serde_json::from_str::<Value>(trimmed) {
        Ok(Value::String(patch)) => patch,
        Ok(Value::Object(object)) => {
            if object.len() != 1 {
                return Err(invalid_apply_patch_arguments(
                    "expected exactly one string field named `patch` (or legacy `input`)"
                        .to_string(),
                ));
            }
            let (field, value) = object.into_iter().next().expect("object has one entry");
            if !matches!(field.as_str(), "patch" | "input") {
                return Err(invalid_apply_patch_arguments(format!(
                    "unsupported field `{field}`; expected `patch` or legacy `input`"
                )));
            }
            value.as_str().map(str::to_string).ok_or_else(|| {
                invalid_apply_patch_arguments(format!("field `{field}` must be a string"))
            })?
        }
        Ok(_) => {
            return Err(invalid_apply_patch_arguments(
                "expected raw patch text, a JSON string, or an object with one `patch` field"
                    .to_string(),
            ));
        }
        Err(_) if looks_like_raw_patch(trimmed) => trimmed.to_string(),
        Err(error) => {
            return Err(invalid_apply_patch_arguments(format!(
                "arguments are neither raw patch text nor supported JSON: {error}"
            )));
        }
    };

    let parsed = codex_apply_patch::parse_patch(&candidate)
        .map_err(|error| invalid_apply_patch_arguments(error.to_string()))?;
    if parsed.hunks.is_empty() {
        return Err(invalid_apply_patch_arguments(
            "patch contains no file hunks".to_string(),
        ));
    }
    Ok(parsed.patch)
}

fn looks_like_raw_patch(value: &str) -> bool {
    value.starts_with("*** Begin Patch")
        || value.starts_with("<<EOF")
        || value.starts_with("<<'EOF'")
        || value.starts_with("<<\"EOF\"")
}

fn invalid_apply_patch_arguments(detail: String) -> TranslationError {
    TranslationError::InvalidToolArguments {
        tool: APPLY_PATCH_TOOL_NAME.to_string(),
        detail,
    }
}

fn generic_custom_input(arguments: &str) -> String {
    serde_json::from_str::<Value>(arguments)
        .ok()
        .and_then(|value| {
            value
                .get("input")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .unwrap_or_else(|| arguments.to_string())
}

fn push_function(
    bindings: &mut Vec<ToolBinding>,
    tool: &Value,
    namespace: Option<String>,
) -> Result<(), TranslationError> {
    if tool.get("type").and_then(Value::as_str) != Some("function") {
        return Err(TranslationError::UnsupportedItem(
            "non-function namespace member".to_string(),
        ));
    }
    let name = required_string(tool, "name")?;
    let description = tool
        .get("description")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let input_schema = tool
        .get("parameters")
        .cloned()
        .unwrap_or_else(|| json!({"type": "object", "properties": {}}));
    bindings.push(ToolBinding {
        wire_name: String::new(),
        kind: ToolKind::Function { name, namespace },
        description,
        input_schema,
    });
    Ok(())
}

fn required_string(value: &Value, field: &str) -> Result<String, TranslationError> {
    value
        .get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| TranslationError::InvalidRequest(format!("missing {field}")))
}

fn assign_wire_names(bindings: &mut [ToolBinding]) {
    let mut original_owners = BTreeMap::<String, Option<usize>>::new();
    for (index, binding) in bindings.iter().enumerate() {
        original_owners
            .entry(binding.kind.name().to_string())
            .and_modify(|owner| *owner = None)
            .or_insert(Some(index));
    }

    let mut used = BTreeSet::<String>::new();
    let mut synthetic_index = 0;
    for (index, binding) in bindings.iter_mut().enumerate() {
        let sanitized = sanitized_wire_name(binding.kind.namespace(), binding.kind.name());
        let conflicts_with_alias = |candidate: &str| {
            original_owners
                .get(candidate)
                .is_some_and(|owner| *owner != Some(index))
        };
        let wire_name = if sanitized.is_empty()
            || sanitized.len() > 64
            || used.contains(&sanitized)
            || conflicts_with_alias(&sanitized)
        {
            loop {
                let candidate = format!("factory_tool_{synthetic_index}");
                synthetic_index += 1;
                if !used.contains(&candidate) && !conflicts_with_alias(&candidate) {
                    break candidate;
                }
            }
        } else {
            sanitized
        };
        used.insert(wire_name.clone());
        binding.wire_name = wire_name;
    }
}

fn sanitized_wire_name(namespace: Option<&str>, name: &str) -> String {
    let candidate = match namespace {
        Some(namespace) => format!("{namespace}__{name}"),
        None => name.to_string(),
    };
    candidate
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '_' | '-') {
                character
            } else {
                '_'
            }
        })
        .collect::<String>()
}
