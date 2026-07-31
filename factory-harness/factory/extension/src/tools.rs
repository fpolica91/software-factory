use std::collections::HashMap;
use std::collections::HashSet;
use std::sync::Arc;

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
        [
            FactoryToolKind::Decompose,
            FactoryToolKind::Progress,
            FactoryToolKind::Review,
            FactoryToolKind::Remediation,
        ]
        .into_iter()
        .map(|kind| {
            Arc::new(FactoryToolExecutor {
                kind,
                state: Arc::clone(&state),
            }) as Arc<dyn ToolExecutor<ToolCall>>
        })
        .collect()
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
                state.work_units = units;
                state.review = None;
                state.remediations.clear();
                Ok(())
            })
            .await
            .map_err(|error| respond(error.to_string()))?;
        receipt("decompose", &state)
    }

    async fn progress(&self, call: ToolCall) -> Result<Box<dyn ToolOutput>, FunctionCallError> {
        let args: ProgressArgs = parse_args(&call)?;
        require_text("unit_id", &args.unit_id).map_err(respond)?;
        require_text("summary", &args.summary).map_err(respond)?;
        let state = self
            .state
            .update(move |state| {
                let unit_index = state
                    .work_units
                    .iter()
                    .position(|unit| unit.id == args.unit_id)
                    .ok_or_else(|| format!("unknown Factory work unit {}", args.unit_id))?;
                if matches!(
                    args.status,
                    FactoryProgressStatus::InProgress | FactoryProgressStatus::Completed
                ) {
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
                }
                let unit = &mut state.work_units[unit_index];
                unit.status = args.status;
                unit.progress_summary = Some(args.summary);
                Ok(())
            })
            .await
            .map_err(|error| respond(error.to_string()))?;
        receipt("progress", &state)
    }

    async fn review(&self, call: ToolCall) -> Result<Box<dyn ToolOutput>, FunctionCallError> {
        let args: ReviewArgs = parse_args(&call)?;
        validate_review(
            &args,
            &self
                .state
                .snapshot()
                .await
                .map_err(|error| respond(error.to_string()))?,
        )
        .map_err(respond)?;
        let report = FactoryReviewReport {
            verdict: args.verdict,
            summary: args.summary,
            findings: args.findings,
        };
        let state = self
            .state
            .update(move |state| {
                state.review = Some(report);
                state.remediations.clear();
                Ok(())
            })
            .await
            .map_err(|error| respond(error.to_string()))?;
        receipt("review", &state)
    }

    async fn remediation(&self, call: ToolCall) -> Result<Box<dyn ToolOutput>, FunctionCallError> {
        let args: RemediationArgs = parse_args(&call)?;
        let snapshot = self
            .state
            .snapshot()
            .await
            .map_err(|error| respond(error.to_string()))?;
        validate_remediations(&args.dispositions, &snapshot).map_err(respond)?;
        let dispositions = args.dispositions;
        let state = self
            .state
            .update(move |state| {
                state.remediations = dispositions;
                Ok(())
            })
            .await
            .map_err(|error| respond(error.to_string()))?;
        receipt("remediation", &state)
    }
}

impl FactoryToolKind {
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
                "Replace the current Factory decomposition with independently trackable work units and explicit dependency IDs. This resets downstream review and remediation state.",
                decomposition_schema(),
            ),
            Self::Progress => (
                "Update one Factory work unit's current status and concise progress summary. Dependencies must be complete before a unit starts or completes.",
                progress_schema(),
            ),
            Self::Review => (
                "Record the current structured Factory review verdict, summary, and findings tied to work-unit IDs. This replaces the prior review and resets remediation dispositions.",
                review_schema(),
            ),
            Self::Remediation => (
                "Record dispositions for findings in the current Factory review. Each finding ID and work-unit ID must match current Factory state.",
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

fn require_text(field: &str, value: &str) -> Result<(), String> {
    if value.trim().is_empty() {
        Err(format!("{field} must not be empty"))
    } else {
        Ok(())
    }
}

fn validate_decomposition(units: &[WorkUnitArgs]) -> Result<(), String> {
    if units.is_empty() {
        return Err("Factory decomposition requires at least one work unit".to_string());
    }
    let mut ids = HashSet::new();
    for unit in units {
        require_text("work unit id", &unit.id)?;
        require_text("work unit title", &unit.title)?;
        require_text("work unit description", &unit.description)?;
        if !ids.insert(unit.id.as_str()) {
            return Err(format!("duplicate Factory work unit id {}", unit.id));
        }
    }
    for unit in units {
        let mut dependencies = HashSet::new();
        for dependency in &unit.depends_on {
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
    require_text("review summary", &args.summary)?;
    if args.verdict != FactoryReviewVerdict::Approve && args.findings.is_empty() {
        return Err("non-approved Factory reviews require at least one finding".to_string());
    }
    let unit_ids = state
        .work_units
        .iter()
        .map(|unit| unit.id.as_str())
        .collect::<HashSet<_>>();
    let mut finding_ids = HashSet::new();
    for finding in &args.findings {
        require_text("finding id", &finding.id)?;
        require_text("finding title", &finding.title)?;
        require_text("finding evidence", &finding.evidence)?;
        require_text("finding recommendation", &finding.recommendation)?;
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
        require_text("remediation rationale", &disposition.rationale)?;
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
                "items": object(json!({
                    "id": {"type": "string", "description": "Stable short work-unit ID."},
                    "title": {"type": "string"},
                    "description": {"type": "string"},
                    "depends_on": {"type": "array", "items": {"type": "string"}}
                }), &["id", "title", "description", "depends_on"])
            }
        }),
        &["units"],
    )
}

fn progress_schema() -> Value {
    object(
        json!({
            "unit_id": {"type": "string"},
            "status": {"type": "string", "enum": ["pending", "in_progress", "completed", "blocked"]},
            "summary": {"type": "string", "description": "Concise current progress or blocker."}
        }),
        &["unit_id", "status", "summary"],
    )
}

fn finding_schema() -> Value {
    object(
        json!({
            "id": {"type": "string"},
            "severity": {"type": "string", "enum": ["critical", "major", "minor"]},
            "unit_id": {"type": "string"},
            "title": {"type": "string"},
            "evidence": {"type": "string"},
            "recommendation": {"type": "string"}
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
            "verdict": {"type": "string", "enum": ["approve", "request_changes", "blocked"]},
            "summary": {"type": "string"},
            "findings": {"type": "array", "items": finding_schema()}
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
                "items": object(json!({
                    "finding_id": {"type": "string"},
                    "disposition": {"type": "string", "enum": ["accepted", "rejected", "deferred", "resolved"]},
                    "rationale": {"type": "string"},
                    "unit_id": {"type": "string"}
                }), &["finding_id", "disposition", "rationale", "unit_id"])
            }
        }),
        &["dispositions"],
    )
}
