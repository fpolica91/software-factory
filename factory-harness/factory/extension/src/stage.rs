use codex_extension_api::ExtensionData;
use codex_extension_api::ToolCall;
use serde_json::Value;

/// Factory-owned turn metadata used to authorize stage-specific mutations.
pub const FACTORY_STAGE_METADATA_KEY: &str = "factory_stage";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FactoryTurnStage {
    Plan,
    Execute,
    Review,
    Remediate,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum FactoryThreadScope {
    Parent,
    DetachedReview,
    Subagent,
}

pub(crate) fn thread_scope(thread_store: &ExtensionData) -> FactoryThreadScope {
    thread_store
        .get::<FactoryThreadScope>()
        .as_deref()
        .copied()
        .unwrap_or(FactoryThreadScope::Parent)
}

impl FactoryTurnStage {
    pub const fn as_wire_name(self) -> &'static str {
        match self {
            Self::Plan => "codex.plan",
            Self::Execute => "codex.execute",
            Self::Review => "codex.review",
            Self::Remediate => "codex.remediate",
        }
    }

    fn from_wire_name(value: &str) -> Option<Self> {
        match value {
            "codex.plan" => Some(Self::Plan),
            "codex.execute" => Some(Self::Execute),
            "codex.review" => Some(Self::Review),
            "codex.remediate" => Some(Self::Remediate),
            _ => None,
        }
    }
}

pub(crate) fn require_tool_stage(
    call: &ToolCall,
    tool_name: &str,
    allowed: &[FactoryTurnStage],
) -> Result<(), String> {
    let stage = tool_stage(call).ok_or_else(|| {
        format!("{tool_name} is unavailable because this turn has no valid Factory stage")
    })?;
    if allowed.contains(&stage) {
        Ok(())
    } else {
        Err(format!(
            "{tool_name} is unavailable during {}; allowed stage: {}",
            stage.as_wire_name(),
            allowed
                .iter()
                .map(|stage| stage.as_wire_name())
                .collect::<Vec<_>>()
                .join(", ")
        ))
    }
}

fn tool_stage(call: &ToolCall) -> Option<FactoryTurnStage> {
    let metadata = serde_json::from_str::<Value>(call.codex_turn_metadata.as_deref()?).ok()?;
    metadata
        .get(FACTORY_STAGE_METADATA_KEY)
        .and_then(Value::as_str)
        .and_then(FactoryTurnStage::from_wire_name)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use codex_extension_api::ConversationHistory;
    use codex_extension_api::NoopTurnItemEmitter;
    use codex_extension_api::ToolName;
    use codex_extension_api::ToolPayload;
    use codex_utils_output_truncation::TruncationPolicy;
    use serde_json::json;

    use super::*;

    fn call(stage: Option<&str>) -> ToolCall {
        ToolCall {
            turn_id: "turn-1".to_string(),
            call_id: "call-1".to_string(),
            tool_name: ToolName::plain("factory_test"),
            model: "test-model".to_string(),
            codex_turn_metadata: stage
                .map(|stage| json!({FACTORY_STAGE_METADATA_KEY: stage}).to_string()),
            truncation_policy: TruncationPolicy::Bytes(1024),
            conversation_history: ConversationHistory::default(),
            turn_item_emitter: Arc::new(NoopTurnItemEmitter),
            environments: Vec::new(),
            payload: ToolPayload::Function {
                arguments: "{}".to_string(),
            },
        }
    }

    #[test]
    fn requires_an_exact_allowed_stage() {
        assert!(
            require_tool_stage(
                &call(Some("codex.execute")),
                "factory_test",
                &[FactoryTurnStage::Execute]
            )
            .is_ok()
        );
        assert!(
            require_tool_stage(
                &call(Some("codex.plan")),
                "factory_test",
                &[FactoryTurnStage::Execute]
            )
            .is_err()
        );
        assert!(
            require_tool_stage(&call(None), "factory_test", &[FactoryTurnStage::Execute]).is_err()
        );
        assert!(
            require_tool_stage(
                &call(Some("unknown")),
                "factory_test",
                &[FactoryTurnStage::Execute]
            )
            .is_err()
        );
    }
}
