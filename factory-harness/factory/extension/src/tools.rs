use std::collections::HashMap;
use std::collections::HashSet;
use std::sync::Arc;

use codex_extension_api::DetachedReviewThreadContext;
use codex_extension_api::FunctionCallError;
use codex_extension_api::JsonToolOutput;
use codex_extension_api::ResponsesApiTool;
use codex_extension_api::ToolCall;
use codex_extension_api::ToolContributor;
use codex_extension_api::ToolExecutor;
use codex_extension_api::ToolExecutorFuture;
use codex_extension_api::ToolName;
use codex_extension_api::ToolOutput;
use codex_extension_api::ToolSpec;
use codex_extension_api::parse_tool_input_schema;
use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;
use serde_json::json;

use crate::FactoryExtension;
use crate::FactoryProgressStatus;
use crate::FactoryRemediationRecord;
use crate::FactoryReviewFinding;
use crate::FactoryReviewReport;
use crate::FactoryReviewVerdict;
use crate::FactoryState;
use crate::FactoryThreadState;
use crate::FactoryWorkUnit;
use crate::limits::MAX_DEPENDENCIES;
use crate::limits::MAX_DETAIL_CHARS;
use crate::limits::MAX_FINDINGS;
use crate::limits::MAX_IDENTIFIER_CHARS;
use crate::limits::MAX_SUMMARY_CHARS;
use crate::limits::MAX_TITLE_CHARS;
use crate::limits::MAX_WORK_UNITS;
use crate::limits::require_bounded_text;
use crate::stage::FactoryThreadScope;
use crate::stage::FactoryTurnStage;
use crate::stage::require_tool_stage;
use crate::stage::thread_scope;
use crate::thread_state;

const DECOMPOSE_TOOL: &str = "factory_decompose";
const PROGRESS_TOOL: &str = "factory_update_progress";
const REVIEW_TOOL: &str = "factory_record_review";
const REMEDIATION_TOOL: &str = "factory_record_remediation";

#[derive(Clone, Copy)]
enum FactoryToolKind {
    Decompose,
    Progress,
    Review,
    Remediation,
}

#[derive(Clone)]
struct FactoryToolExecutor {
    kind: FactoryToolKind,
    state: Arc<FactoryThreadState>,
    active_thread_id: String,
    detached_review_context: Option<Arc<DetachedReviewThreadContext>>,
}

#[derive(Debug, Deserialize)]
struct DecomposeArgs {
    units: Vec<WorkUnitArgs>,
}

#[derive(Debug, Deserialize)]
struct WorkUnitArgs {
    id: String,
    title: String,
    description: String,
    depends_on: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct ProgressArgs {
    unit_id: String,
    status: FactoryProgressStatus,
    summary: String,
}

#[derive(Debug, Deserialize)]
struct ReviewArgs {
    verdict: FactoryReviewVerdict,
    summary: String,
    findings: Vec<FactoryReviewFinding>,
}

#[derive(Debug, Deserialize)]
struct RemediationArgs {
    dispositions: Vec<FactoryRemediationRecord>,
}

#[derive(Debug, Serialize)]
struct MutationReceipt {
    operation: &'static str,
    revision: u64,
    work_unit_count: usize,
    finding_count: usize,
    remediation_count: usize,
}

impl ToolContributor for FactoryExtension {
    fn tools(
        &self,
        _session_store: &codex_extension_api::ExtensionData,
        thread_store: &codex_extension_api::ExtensionData,
    ) -> Vec<Arc<dyn ToolExecutor<ToolCall>>> {
        let Some(state) = thread_state(thread_store) else {
            return Vec::new();
        };
        let active_thread_id = thread_store.level_id().to_string();
        let detached_review_context = thread_store.get::<DetachedReviewThreadContext>();
        let stage = match thread_scope(thread_store) {
            FactoryThreadScope::Parent => Some(self.stage),
            FactoryThreadScope::DetachedReview => Some(FactoryTurnStage::Review),
            FactoryThreadScope::Subagent => None,
        };
        stage
            .into_iter()
            .flat_map(|stage| stage.tool_kinds().iter().copied())
            .map(|kind| {
                Arc::new(FactoryToolExecutor {
                    kind,
                    state: Arc::clone(&state),
                    active_thread_id: active_thread_id.clone(),
                    detached_review_context: detached_review_context.clone(),
                }) as Arc<dyn ToolExecutor<ToolCall>>
            })
            .collect()
    }

    fn disabled_tools_for_step(
        &self,
        _session_store: &codex_extension_api::ExtensionData,
        _thread_store: &codex_extension_api::ExtensionData,
        _step_store: &codex_extension_api::ExtensionData,
    ) -> Vec<ToolName> {
        if self.stage == FactoryTurnStage::Plan {
            vec![
                ToolName::plain("apply_patch"),
                ToolName::plain("request_permissions"),
            ]
        } else {
            Vec::new()
        }
    }
}

impl ToolExecutor<ToolCall> for FactoryToolExecutor {
    fn tool_name(&self) -> ToolName {
        ToolName::plain(self.kind.name())
    }

    fn spec(&self) -> ToolSpec {
        self.kind.spec()
    }

    fn handle(&self, call: ToolCall) -> ToolExecutorFuture<'_> {
        Box::pin(async move {
            self.kind
                .authorize(&call, self.detached_review_context.is_some())
                .map_err(respond)?;
            match self.kind {
                FactoryToolKind::Decompose => self.decompose(call).await,
                FactoryToolKind::Progress => self.progress(call).await,
                FactoryToolKind::Review => self.review(call).await,
                FactoryToolKind::Remediation => self.remediation(call).await,
            }
        })
    }
}

impl FactoryToolExecutor {
    async fn decompose(&self, call: ToolCall) -> Result<Box<dyn ToolOutput>, FunctionCallError> {
        let args: DecomposeArgs = parse_args(&call)?;
        validate_decomposition(&args.units).map_err(respond)?;
        let units = args
            .units
            .into_iter()
            .map(|unit| FactoryWorkUnit {
                id: unit.id,
                title: unit.title,
                description: unit.description,
                depends_on: unit.depends_on,
                status: FactoryProgressStatus::Pending,
                progress_summary: None,
            })
            .collect();
        let state = self
            .state
            .update(move |state| {
                if !state.work_units.is_empty() {
                    return Err(
                        "factory_decompose may be called only once in a fresh Plan turn"
                            .to_string(),
                    );
                }
                state.work_units = units;
                state.review = None;
                state.remediations.clear();
                state.review_history.clear();
                Ok(())
            })
            .await
            .map_err(|error| respond(error.to_string()))?;
        receipt("decompose", &state)
    }

    async fn progress(&self, call: ToolCall) -> Result<Box<dyn ToolOutput>, FunctionCallError> {
        let args: ProgressArgs = parse_args(&call)?;
        require_bounded_text("unit_id", &args.unit_id, MAX_IDENTIFIER_CHARS).map_err(respond)?;
        require_bounded_text("summary", &args.summary, MAX_SUMMARY_CHARS).map_err(respond)?;
        let state = self
            .state
            .update(move |state| {
                if args.status != FactoryProgressStatus::Completed {
                    return Err(
                        "factory_update_progress accepts only completed status after implementation and verification"
                            .to_string(),
                    );
                }
                let unit_index = state
                    .work_units
                    .iter()
                    .position(|unit| unit.id == args.unit_id)
                    .ok_or_else(|| format!("unknown Factory work unit {}", args.unit_id))?;
                if state.work_units[unit_index].status == FactoryProgressStatus::Completed {
                    return Err(format!(
                        "work unit {} is already completed and cannot be rewritten",
                        args.unit_id
                    ));
                }
                let incomplete = state.work_units[unit_index]
                    .depends_on
                    .iter()
                    .filter(|dependency| {
                        let dependency = dependency.as_str();
                        state.work_units.iter().any(|unit| {
                            unit.id == dependency
                                && unit.status != FactoryProgressStatus::Completed
                        })
                    })
                    .cloned()
                    .collect::<Vec<_>>();
                if !incomplete.is_empty() {
                    return Err(format!(
                        "work unit {} has incomplete dependencies: {}",
                        args.unit_id,
                        incomplete.join(", ")
                    ));
                }
                let unit = &mut state.work_units[unit_index];
                unit.status = FactoryProgressStatus::Completed;
                unit.progress_summary = Some(args.summary);
                Ok(())
            })
            .await
            .map_err(|error| respond(error.to_string()))?;
        receipt("progress", &state)
    }

    async fn review(&self, call: ToolCall) -> Result<Box<dyn ToolOutput>, FunctionCallError> {
        let recorded_turn_id = call.turn_id.clone();
        let turn_metadata = call
            .codex_turn_metadata
            .as_deref()
            .and_then(|value| serde_json::from_str::<Value>(value).ok());
        let metadata_text = |key: &str| {
            turn_metadata
                .as_ref()
                .and_then(|metadata| metadata.get(key))
                .and_then(Value::as_str)
                .map(ToString::to_string)
        };
        let (
            recorded_thread_id,
            recorded_parent_thread_id,
            recorded_parent_turn_id,
            recorded_subagent_kind,
        ) = match &self.detached_review_context {
            Some(context) => (
                Some(self.active_thread_id.clone()),
                Some(context.parent_thread_id.to_string()),
                Some(context.parent_turn_id.clone()),
                Some("review".to_string()),
            ),
            None => (
                metadata_text("thread_id"),
                metadata_text("parent_thread_id"),
                metadata_text("parent_turn_id"),
                metadata_text("subagent_kind"),
            ),
        };
        let args: ReviewArgs = parse_args(&call)?;
        let state = self
            .state
            .update(move |state| {
                validate_review(&args, state)?;
                let ReviewArgs {
                    verdict,
                    summary,
                    findings,
                } = args;
                state.record_review(FactoryReviewReport {
                    generation: 0,
                    recorded_turn_id: Some(recorded_turn_id),
                    recorded_thread_id,
                    recorded_parent_thread_id,
                    recorded_parent_turn_id,
                    recorded_subagent_kind,
                    verdict,
                    summary,
                    findings,
                });
                Ok(())
            })
            .await
            .map_err(|error| respond(error.to_string()))?;
        receipt("review", &state)
    }

    async fn remediation(&self, call: ToolCall) -> Result<Box<dyn ToolOutput>, FunctionCallError> {
        let args: RemediationArgs = parse_args(&call)?;
        let state = self
            .state
            .update(move |state| {
                validate_remediations(&args.dispositions, state)?;
                state.remediations = args.dispositions;
                Ok(())
            })
            .await
            .map_err(|error| respond(error.to_string()))?;
        receipt("remediation", &state)
    }
}

impl FactoryToolKind {
    fn authorize(self, call: &ToolCall, detached_review: bool) -> Result<(), String> {
        match self {
            Self::Decompose => require_tool_stage(call, self.name(), &[FactoryTurnStage::Plan]),
            Self::Progress => require_tool_stage(call, self.name(), &[FactoryTurnStage::Execute]),
            Self::Review if detached_review => Ok(()),
            Self::Review => require_tool_stage(call, self.name(), &[FactoryTurnStage::Review]),
            Self::Remediation => {
                require_tool_stage(call, self.name(), &[FactoryTurnStage::Remediate])
            }
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Decompose => DECOMPOSE_TOOL,
            Self::Progress => PROGRESS_TOOL,
            Self::Review => REVIEW_TOOL,
            Self::Remediation => REMEDIATION_TOOL,
        }
    }

    fn spec(self) -> ToolSpec {
        let (description, schema) = match self {
            Self::Decompose => (
                "During codex.plan only, create the Factory decomposition exactly once with independently trackable work units and explicit dependency IDs.",
                decomposition_schema(),
            ),
            Self::Progress => (
                "During codex.execute only, complete one incomplete Factory work unit exactly once after implementation and verification. All dependencies must already be complete; the summary must contain concise evidence.",
                progress_schema(),
            ),
            Self::Review => (
                "During detached Factory review only, record the current structured review exactly once. Approve only with an empty findings array and put passing evidence in the summary; request_changes or blocked requires findings tied to work-unit IDs.",
                review_schema(),
            ),
            Self::Remediation => (
                "During codex.remediate only, record dispositions exactly once for findings in the current Factory review. Each finding ID and work-unit ID must match current Factory state.",
                remediation_schema(),
            ),
        };
        ToolSpec::Function(ResponsesApiTool {
            name: self.name().to_string(),
            description: description.to_string(),
            strict: false,
            defer_loading: None,
            parameters: parse_tool_input_schema(&schema)
                .expect("Factory-owned tool schema must be valid"),
            output_schema: None,
        })
    }
}

impl FactoryTurnStage {
    fn tool_kinds(self) -> &'static [FactoryToolKind] {
        match self {
            Self::Plan => &[FactoryToolKind::Decompose],
            Self::Execute => &[FactoryToolKind::Progress],
            Self::Review => &[FactoryToolKind::Review],
            Self::Remediate => &[FactoryToolKind::Remediation],
        }
    }
}

fn parse_args<T: for<'de> Deserialize<'de>>(call: &ToolCall) -> Result<T, FunctionCallError> {
    serde_json::from_str(call.function_arguments()?).map_err(|error| respond(error.to_string()))
}

fn receipt(
    operation: &'static str,
    state: &FactoryState,
) -> Result<Box<dyn ToolOutput>, FunctionCallError> {
    let value = serde_json::to_value(MutationReceipt {
        operation,
        revision: state.revision,
        work_unit_count: state.work_units.len(),
        finding_count: state
            .review
            .as_ref()
            .map_or(0, |review| review.findings.len()),
        remediation_count: state.remediations.len(),
    })
    .map_err(|error| FunctionCallError::Fatal(error.to_string()))?;
    Ok(Box::new(JsonToolOutput::new(value)))
}

fn respond(message: impl Into<String>) -> FunctionCallError {
    FunctionCallError::RespondToModel(message.into())
}

fn validate_decomposition(units: &[WorkUnitArgs]) -> Result<(), String> {
    if units.is_empty() {
        return Err("Factory decomposition requires at least one work unit".to_string());
    }
    if units.len() > MAX_WORK_UNITS {
        return Err(format!(
            "Factory decomposition supports at most {MAX_WORK_UNITS} work units"
        ));
    }
    let mut ids = HashSet::new();
    for unit in units {
        require_bounded_text("work unit id", &unit.id, MAX_IDENTIFIER_CHARS)?;
        require_bounded_text("work unit title", &unit.title, MAX_TITLE_CHARS)?;
        require_bounded_text("work unit description", &unit.description, MAX_DETAIL_CHARS)?;
        if unit.depends_on.len() > MAX_DEPENDENCIES {
            return Err(format!(
                "work unit {} supports at most {MAX_DEPENDENCIES} dependencies",
                unit.id
            ));
        }
        if !ids.insert(unit.id.as_str()) {
            return Err(format!("duplicate Factory work unit id {}", unit.id));
        }
    }
    for unit in units {
        let mut dependencies = HashSet::new();
        for dependency in &unit.depends_on {
            require_bounded_text("work unit dependency ID", dependency, MAX_IDENTIFIER_CHARS)?;
            if dependency == &unit.id {
                return Err(format!("work unit {} cannot depend on itself", unit.id));
            }
            if !ids.contains(dependency.as_str()) {
                return Err(format!(
                    "work unit {} depends on unknown unit {dependency}",
                    unit.id
                ));
            }
            if !dependencies.insert(dependency) {
                return Err(format!(
                    "work unit {} repeats dependency {dependency}",
                    unit.id
                ));
            }
        }
    }
    validate_acyclic(units)
}

fn validate_acyclic(units: &[WorkUnitArgs]) -> Result<(), String> {
    let mut remaining = units
        .iter()
        .map(|unit| (unit.id.as_str(), unit.depends_on.len()))
        .collect::<HashMap<_, _>>();
    let mut completed = HashSet::new();
    loop {
        let ready = remaining
            .iter()
            .filter(|(id, dependency_count)| !completed.contains(**id) && **dependency_count == 0)
            .map(|(id, _)| *id)
            .collect::<Vec<_>>();
        if ready.is_empty() {
            break;
        }
        for id in ready {
            completed.insert(id);
            for unit in units
                .iter()
                .filter(|unit| unit.depends_on.iter().any(|dep| dep == id))
            {
                if let Some(count) = remaining.get_mut(unit.id.as_str()) {
                    *count = count.saturating_sub(1);
                }
            }
        }
    }
    if completed.len() == units.len() {
        Ok(())
    } else {
        Err("Factory work unit dependencies contain a cycle".to_string())
    }
}

fn validate_review(args: &ReviewArgs, state: &FactoryState) -> Result<(), String> {
    require_bounded_text("review summary", &args.summary, MAX_SUMMARY_CHARS)?;
    if args.findings.len() > MAX_FINDINGS {
        return Err(format!(
            "Factory review supports at most {MAX_FINDINGS} findings"
        ));
    }
    if args.verdict != FactoryReviewVerdict::Approve && args.findings.is_empty() {
        return Err("non-approved Factory reviews require at least one finding".to_string());
    }
    if args.verdict == FactoryReviewVerdict::Approve && !args.findings.is_empty() {
        return Err(
            "approved Factory reviews require an empty findings array; put passing evidence in the summary"
                .to_string(),
        );
    }
    let unit_ids = state
        .work_units
        .iter()
        .map(|unit| unit.id.as_str())
        .collect::<HashSet<_>>();
    let mut finding_ids = HashSet::new();
    for finding in &args.findings {
        require_bounded_text("finding id", &finding.id, MAX_IDENTIFIER_CHARS)?;
        require_bounded_text("finding unit id", &finding.unit_id, MAX_IDENTIFIER_CHARS)?;
        require_bounded_text("finding title", &finding.title, MAX_TITLE_CHARS)?;
        require_bounded_text("finding evidence", &finding.evidence, MAX_DETAIL_CHARS)?;
        require_bounded_text(
            "finding recommendation",
            &finding.recommendation,
            MAX_DETAIL_CHARS,
        )?;
        if !finding_ids.insert(finding.id.as_str()) {
            return Err(format!("duplicate Factory finding id {}", finding.id));
        }
        if !unit_ids.contains(finding.unit_id.as_str()) {
            return Err(format!(
                "finding {} references unknown unit {}",
                finding.id, finding.unit_id
            ));
        }
    }
    Ok(())
}

fn validate_remediations(
    dispositions: &[FactoryRemediationRecord],
    state: &FactoryState,
) -> Result<(), String> {
    if dispositions.is_empty() {
        return Err("Factory remediation requires at least one disposition".to_string());
    }
    if dispositions.len() > MAX_FINDINGS {
        return Err(format!(
            "Factory remediation supports at most {MAX_FINDINGS} dispositions"
        ));
    }
    let review = state
        .review
        .as_ref()
        .ok_or_else(|| "Factory remediation requires a current review".to_string())?;
    let findings = review
        .findings
        .iter()
        .map(|finding| (finding.id.as_str(), finding.unit_id.as_str()))
        .collect::<HashMap<_, _>>();
    let mut seen = HashSet::new();
    for disposition in dispositions {
        require_bounded_text(
            "remediation finding id",
            &disposition.finding_id,
            MAX_IDENTIFIER_CHARS,
        )?;
        require_bounded_text(
            "remediation unit id",
            &disposition.unit_id,
            MAX_IDENTIFIER_CHARS,
        )?;
        require_bounded_text(
            "remediation rationale",
            &disposition.rationale,
            MAX_DETAIL_CHARS,
        )?;
        if !seen.insert(disposition.finding_id.as_str()) {
            return Err(format!(
                "duplicate remediation disposition for {}",
                disposition.finding_id
            ));
        }
        let unit_id = findings
            .get(disposition.finding_id.as_str())
            .ok_or_else(|| {
                format!(
                    "remediation references unknown finding {}",
                    disposition.finding_id
                )
            })?;
        if *unit_id != disposition.unit_id {
            return Err(format!(
                "remediation {} must use finding unit {}",
                disposition.finding_id, unit_id
            ));
        }
    }
    Ok(())
}

fn object(properties: Value, required: &[&str]) -> Value {
    json!({
        "type": "object",
        "properties": properties,
        "required": required,
        "additionalProperties": false
    })
}

fn decomposition_schema() -> Value {
    object(
        json!({
            "units": {
                "type": "array",
                "description": "Complete replacement decomposition in dependency order.",
                "minItems": 1,
                "maxItems": MAX_WORK_UNITS,
                "items": object(json!({
                    "id": {"type": "string", "maxLength": MAX_IDENTIFIER_CHARS, "description": "Stable short work-unit ID."},
                    "title": {"type": "string", "maxLength": MAX_TITLE_CHARS},
                    "description": {"type": "string", "maxLength": MAX_DETAIL_CHARS},
                    "depends_on": {
                        "type": "array",
                        "maxItems": MAX_DEPENDENCIES,
                        "items": {"type": "string", "maxLength": MAX_IDENTIFIER_CHARS}
                    }
                }), &["id", "title", "description", "depends_on"])
            }
        }),
        &["units"],
    )
}

fn progress_schema() -> Value {
    object(
        json!({
            "unit_id": {"type": "string", "maxLength": MAX_IDENTIFIER_CHARS},
            "status": {"type": "string", "enum": ["completed"]},
            "summary": {"type": "string", "maxLength": MAX_SUMMARY_CHARS, "description": "Concise implementation and verification evidence."}
        }),
        &["unit_id", "status", "summary"],
    )
}

fn finding_schema() -> Value {
    object(
        json!({
            "id": {"type": "string", "maxLength": MAX_IDENTIFIER_CHARS},
            "severity": {"type": "string", "enum": ["critical", "major", "minor"]},
            "unit_id": {"type": "string", "maxLength": MAX_IDENTIFIER_CHARS},
            "title": {"type": "string", "maxLength": MAX_TITLE_CHARS},
            "evidence": {"type": "string", "maxLength": MAX_DETAIL_CHARS},
            "recommendation": {"type": "string", "maxLength": MAX_DETAIL_CHARS}
        }),
        &[
            "id",
            "severity",
            "unit_id",
            "title",
            "evidence",
            "recommendation",
        ],
    )
}

fn review_schema() -> Value {
    object(
        json!({
            "verdict": {
                "type": "string",
                "enum": ["approve", "request_changes", "blocked"],
                "description": "Use approve only when there are no findings."
            },
            "summary": {
                "type": "string",
                "maxLength": MAX_SUMMARY_CHARS,
                "description": "Overall verdict evidence, including all passing evidence for approve."
            },
            "findings": {
                "type": "array",
                "maxItems": MAX_FINDINGS,
                "description": "Must be empty for approve; must contain at least one actionable issue for request_changes or blocked.",
                "items": finding_schema()
            }
        }),
        &["verdict", "summary", "findings"],
    )
}

fn remediation_schema() -> Value {
    object(
        json!({
            "dispositions": {
                "type": "array",
                "minItems": 1,
                "maxItems": MAX_FINDINGS,
                "items": object(json!({
                    "finding_id": {"type": "string", "maxLength": MAX_IDENTIFIER_CHARS},
                    "disposition": {"type": "string", "enum": ["accepted", "rejected", "deferred", "resolved"]},
                    "rationale": {"type": "string", "maxLength": MAX_DETAIL_CHARS},
                    "unit_id": {"type": "string", "maxLength": MAX_IDENTIFIER_CHARS}
                }), &["finding_id", "disposition", "rationale", "unit_id"])
            }
        }),
        &["dispositions"],
    )
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use codex_extension_api::ConversationHistory;
    use codex_extension_api::NoopTurnItemEmitter;
    use codex_extension_api::ToolPayload;
    use codex_utils_output_truncation::TruncationPolicy;

    use super::*;
    use crate::FactoryBackendError;
    use crate::FactoryBackendFuture;
    use crate::FactoryStateBackend;
    use crate::FactoryStateDurability;
    use crate::state::FactoryStateRegistry;

    struct ProgressBackend {
        state: Mutex<Option<FactoryState>>,
    }

    impl ProgressBackend {
        fn new(state: FactoryState) -> Self {
            Self {
                state: Mutex::new(Some(state)),
            }
        }
    }

    impl FactoryStateBackend for ProgressBackend {
        fn load<'a>(
            &'a self,
            _thread_id: &'a str,
        ) -> FactoryBackendFuture<'a, Option<FactoryState>> {
            Box::pin(async move {
                self.state
                    .lock()
                    .map(|state| state.clone())
                    .map_err(|_| FactoryBackendError::new("progress backend lock failed"))
            })
        }

        fn save<'a>(
            &'a self,
            _thread_id: &'a str,
            state: FactoryState,
        ) -> FactoryBackendFuture<'a, ()> {
            Box::pin(async move {
                *self
                    .state
                    .lock()
                    .map_err(|_| FactoryBackendError::new("progress backend lock failed"))? =
                    Some(state);
                Ok(())
            })
        }

        fn durability(&self) -> FactoryStateDurability {
            FactoryStateDurability::ProcessMemory
        }
    }

    fn unit(index: usize) -> WorkUnitArgs {
        WorkUnitArgs {
            id: format!("unit-{index}"),
            title: "Title".to_string(),
            description: "Description".to_string(),
            depends_on: Vec::new(),
        }
    }

    fn progress_state(units: &[(&str, FactoryProgressStatus)]) -> FactoryState {
        FactoryState {
            work_units: units
                .iter()
                .map(|(id, status)| FactoryWorkUnit {
                    id: (*id).to_string(),
                    title: format!("Complete {id}"),
                    description: format!("Implement and verify {id}."),
                    depends_on: Vec::new(),
                    status: *status,
                    progress_summary: None,
                })
                .collect(),
            ..FactoryState::default()
        }
    }

    async fn progress_executor(
        initial: FactoryState,
    ) -> (FactoryToolExecutor, Arc<FactoryThreadState>) {
        let states = FactoryStateRegistry::default();
        let state = states
            .get_or_create("progress-thread", Arc::new(ProgressBackend::new(initial)))
            .await;
        (
            FactoryToolExecutor {
                kind: FactoryToolKind::Progress,
                state: Arc::clone(&state),
                active_thread_id: "progress-thread".to_string(),
                detached_review_context: None,
            },
            state,
        )
    }

    fn progress_call(unit_id: &str, status: FactoryProgressStatus, summary: &str) -> ToolCall {
        ToolCall {
            turn_id: "execute-turn".to_string(),
            call_id: format!("progress-{unit_id}"),
            tool_name: ToolName::plain(PROGRESS_TOOL),
            model: "mock-model".to_string(),
            codex_turn_metadata: Some(
                json!({
                    crate::FACTORY_STAGE_METADATA_KEY:
                        FactoryTurnStage::Execute.as_wire_name(),
                })
                .to_string(),
            ),
            truncation_policy: TruncationPolicy::Bytes(1024),
            conversation_history: ConversationHistory::default(),
            turn_item_emitter: Arc::new(NoopTurnItemEmitter),
            environments: Vec::new(),
            payload: ToolPayload::Function {
                arguments: json!({
                    "unit_id": unit_id,
                    "status": status,
                    "summary": summary,
                })
                .to_string(),
            },
        }
    }

    #[test]
    fn decomposition_rejects_oversized_new_state() {
        let units = (0..=MAX_WORK_UNITS).map(unit).collect::<Vec<_>>();
        assert!(validate_decomposition(&units).is_err());

        let mut units = vec![unit(0)];
        units[0].description = "x".repeat(MAX_DETAIL_CHARS + 1);
        assert!(validate_decomposition(&units).is_err());
    }

    #[test]
    fn each_stage_advertises_only_its_mutation_tool() {
        for (stage, expected) in [
            (FactoryTurnStage::Plan, DECOMPOSE_TOOL),
            (FactoryTurnStage::Execute, PROGRESS_TOOL),
            (FactoryTurnStage::Review, REVIEW_TOOL),
            (FactoryTurnStage::Remediate, REMEDIATION_TOOL),
        ] {
            assert_eq!(
                stage
                    .tool_kinds()
                    .iter()
                    .map(|kind| kind.name())
                    .collect::<Vec<_>>(),
                vec![expected]
            );
        }
    }

    #[test]
    fn plan_disables_codex_file_mutation_tools() {
        let extension = FactoryExtension {
            backend: Arc::new(ProgressBackend::new(FactoryState::default())),
            states: Arc::new(crate::state::FactoryStateRegistry::default()),
            stage: FactoryTurnStage::Plan,
        };
        let session = codex_extension_api::ExtensionData::new("session".to_string());
        let thread = codex_extension_api::ExtensionData::new("thread".to_string());
        let step = codex_extension_api::ExtensionData::new("step".to_string());

        let disabled = extension.disabled_tools_for_step(&session, &thread, &step);

        assert_eq!(
            disabled,
            vec![
                ToolName::plain("apply_patch"),
                ToolName::plain("request_permissions")
            ]
        );
    }

    #[tokio::test]
    async fn progress_completes_pending_once_and_rejects_rewrites_without_mutation() {
        let (executor, state) = progress_executor(progress_state(&[(
            "pending-unit",
            FactoryProgressStatus::Pending,
        )]))
        .await;

        executor
            .progress(progress_call(
                "pending-unit",
                FactoryProgressStatus::Completed,
                "Implementation finished and verification passed.",
            ))
            .await
            .expect("pending work unit should complete once");
        let completed = state.snapshot().await.expect("completed progress state");
        assert_eq!(completed.revision, 1);
        assert_eq!(
            completed.work_units[0].status,
            FactoryProgressStatus::Completed
        );
        assert_eq!(
            completed.work_units[0].progress_summary.as_deref(),
            Some("Implementation finished and verification passed.")
        );

        let repeated = executor
            .progress(progress_call(
                "pending-unit",
                FactoryProgressStatus::Completed,
                "Replace the original evidence.",
            ))
            .await;
        assert!(repeated.is_err());
        assert_eq!(
            state
                .snapshot()
                .await
                .expect("state after repeated completion"),
            completed
        );

        let rewrite = executor
            .progress(progress_call(
                "pending-unit",
                FactoryProgressStatus::Pending,
                "Reopen completed work.",
            ))
            .await;
        assert!(rewrite.is_err());
        assert_eq!(
            state.snapshot().await.expect("state after status rewrite"),
            completed
        );
    }

    #[tokio::test]
    async fn progress_completes_legacy_incomplete_states() {
        let (executor, state) = progress_executor(progress_state(&[
            ("running-unit", FactoryProgressStatus::InProgress),
            ("blocked-unit", FactoryProgressStatus::Blocked),
        ]))
        .await;

        for unit_id in ["running-unit", "blocked-unit"] {
            executor
                .progress(progress_call(
                    unit_id,
                    FactoryProgressStatus::Completed,
                    "Recovered work finished and verification passed.",
                ))
                .await
                .expect("legacy incomplete work unit should complete");
        }

        let completed = state.snapshot().await.expect("recovered progress state");
        assert_eq!(completed.revision, 2);
        assert!(
            completed
                .work_units
                .iter()
                .all(|unit| unit.status == FactoryProgressStatus::Completed)
        );
    }

    #[tokio::test]
    async fn progress_rejects_non_completed_requests_without_mutation() {
        let (executor, state) = progress_executor(progress_state(&[(
            "pending-unit",
            FactoryProgressStatus::Pending,
        )]))
        .await;
        let initial = state.snapshot().await.expect("initial progress state");

        for status in [
            FactoryProgressStatus::Pending,
            FactoryProgressStatus::InProgress,
            FactoryProgressStatus::Blocked,
        ] {
            assert!(
                executor
                    .progress(progress_call(
                        "pending-unit",
                        status,
                        "Do not persist this status.",
                    ))
                    .await
                    .is_err()
            );
            assert_eq!(
                state.snapshot().await.expect("state after rejected status"),
                initial
            );
        }
    }
}
