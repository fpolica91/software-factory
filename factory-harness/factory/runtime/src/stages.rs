use codex_app_server_protocol::SandboxPolicy;
use codex_protocol::config_types::ModeKind;
use factory_extension::FactoryProgressStatus;
use factory_extension::FactoryReviewReport;
use factory_extension::FactoryReviewVerdict;
use factory_extension::FactoryState;
use factory_extension::FactoryTurnStage;
use factory_extension::FactoryWorkUnit;
use std::collections::HashMap;
use std::collections::HashSet;
use std::error::Error;
use std::fmt;
use std::str::FromStr;

pub const AUTONOMOUS_PROMPT: &str = "Work autonomously until this stage is complete. Persist the required Factory state with the named Factory tools. Do not stop at advice or ask for confirmation.";
pub const PLAN_PROMPT: &str = "Inspect the repository without modifying it. On every Plan attempt, create a fresh, dependency-ordered decomposition after rollback. The only permitted Factory state mutation during Plan is exactly one call to factory_decompose. Do not call factory_update_progress, factory_record_review, or factory_record_remediation. Every persisted work unit must remain pending with no progress or result summary. Do not implement any work during planning.";
pub const EXECUTE_PROMPT: &str = "Implement every incomplete work unit in dependency order. For each unit, implement and verify the result, then call factory_update_progress exactly once with completed and a concise evidence-based summary. Do not mark a unit completed before its work and verification are finished. Do not finish while any unit is incomplete.";
pub const REVIEW_PROMPT: &str = "Independently inspect the completed changes and verification results. Treat task instructions explicitly scoped to execution as evidence to verify, not actions to repeat during review. Call factory_record_review exactly once. An approve verdict must use an empty findings array and put passing evidence in the summary. A request_changes or blocked verdict must include at least one concrete finding tied to a work-unit ID. Do not modify files during review.";
pub const REMEDIATE_PROMPT: &str = "Address every current review finding, verify each fix, and call factory_record_remediation exactly once with one matching disposition per finding. Do not leave any finding undispositioned.";
pub const ITERATE_PROMPT: &str = "Address the newest follow-up feedback section appended to the task. Implement and verify every requested change. After verifying, call factory_update_progress exactly once for each affected work unit with completed and an updated evidence-based summary covering the follow-up work; update at least one unit. Do not create a new decomposition. Do not call factory_record_review or factory_record_remediation.";

pub type StageResult<T = ()> = std::result::Result<T, StageValidationError>;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum OperationKind {
    Plan,
    Execute,
    Review,
    Remediate,
    Iterate,
}

impl OperationKind {
    /// The four base stages every factory.task begins with. Continuation
    /// rounds append iterate, review, remediate triples after them.
    pub const ALL: [Self; 4] = [Self::Plan, Self::Execute, Self::Review, Self::Remediate];

    /// The stage kind required at a durable factory.task ordinal: the base
    /// stages, then repeating iterate, review, remediate rounds.
    pub fn at_ordinal(ordinal: u32) -> Self {
        let ordinal = ordinal as usize;
        if let Some(kind) = Self::ALL.get(ordinal) {
            return *kind;
        }
        match (ordinal - Self::ALL.len()) % 3 {
            0 => Self::Iterate,
            1 => Self::Review,
            _ => Self::Remediate,
        }
    }

    pub const fn as_wire_name(self) -> &'static str {
        match self {
            Self::Plan => "codex.plan",
            Self::Execute => "codex.execute",
            Self::Review => "codex.review",
            Self::Remediate => "codex.remediate",
            Self::Iterate => "codex.iterate",
        }
    }

    pub const fn prompt(self) -> &'static str {
        match self {
            Self::Plan => PLAN_PROMPT,
            Self::Execute => EXECUTE_PROMPT,
            Self::Review => REVIEW_PROMPT,
            Self::Remediate => REMEDIATE_PROMPT,
            Self::Iterate => ITERATE_PROMPT,
        }
    }

    pub(crate) const fn collaboration_mode(self) -> ModeKind {
        match self {
            Self::Plan | Self::Execute | Self::Review | Self::Remediate | Self::Iterate => {
                ModeKind::Default
            }
        }
    }

    pub(crate) const fn turn_stage(self) -> FactoryTurnStage {
        match self {
            Self::Plan => FactoryTurnStage::Plan,
            Self::Execute | Self::Iterate => FactoryTurnStage::Execute,
            Self::Review => FactoryTurnStage::Review,
            Self::Remediate => FactoryTurnStage::Remediate,
        }
    }

    pub(crate) const fn sandbox_policy(self) -> SandboxPolicy {
        match self {
            Self::Plan => SandboxPolicy::ReadOnly {
                network_access: true,
            },
            Self::Execute | Self::Review | Self::Remediate | Self::Iterate => {
                SandboxPolicy::DangerFullAccess
            }
        }
    }

    pub fn validate(
        self,
        state: &FactoryState,
        review_baseline: Option<&ReviewBaseline>,
    ) -> StageResult {
        validate_stage(self, state, review_baseline)
    }
}

impl fmt::Display for OperationKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_wire_name())
    }
}

impl FromStr for OperationKind {
    type Err = StageValidationError;

    fn from_str(value: &str) -> StageResult<Self> {
        match value {
            "codex.plan" => Ok(Self::Plan),
            "codex.execute" => Ok(Self::Execute),
            "codex.review" => Ok(Self::Review),
            "codex.remediate" => Ok(Self::Remediate),
            "codex.iterate" => Ok(Self::Iterate),
            _ => Err(StageValidationError::UnknownOperationKind(
                value.to_string(),
            )),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReviewBaseline {
    generation: u64,
    parent_thread_id: String,
    parent_turn_id: String,
}

impl ReviewBaseline {
    pub fn capture(
        state: &FactoryState,
        parent_thread_id: impl Into<String>,
        parent_turn_id: impl Into<String>,
    ) -> StageResult<Self> {
        Self::from_parts(
            state.review.as_ref().map_or(0, |review| review.generation),
            parent_thread_id,
            parent_turn_id,
        )
    }

    pub fn from_parts(
        generation: u64,
        parent_thread_id: impl Into<String>,
        parent_turn_id: impl Into<String>,
    ) -> StageResult<Self> {
        let baseline = Self {
            generation,
            parent_thread_id: parent_thread_id.into(),
            parent_turn_id: parent_turn_id.into(),
        };
        require_text("review parent thread id", &baseline.parent_thread_id)?;
        require_text("review parent turn id", &baseline.parent_turn_id)?;
        Ok(baseline)
    }

    pub const fn generation(&self) -> u64 {
        self.generation
    }

    pub fn parent_thread_id(&self) -> &str {
        &self.parent_thread_id
    }

    pub fn parent_turn_id(&self) -> &str {
        &self.parent_turn_id
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum StageValidationError {
    UnknownOperationKind(String),
    Missing(&'static str),
    Invalid { field: &'static str, detail: String },
}

impl StageValidationError {
    fn invalid(field: &'static str, detail: impl Into<String>) -> Self {
        Self::Invalid {
            field,
            detail: detail.into(),
        }
    }
}

impl fmt::Display for StageValidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownOperationKind(kind) => {
                write!(formatter, "unknown Factory operation kind {kind:?}")
            }
            Self::Missing(field) => write!(formatter, "Factory state is missing {field}"),
            Self::Invalid { field, detail } => {
                write!(formatter, "invalid Factory {field}: {detail}")
            }
        }
    }
}

impl Error for StageValidationError {}

pub fn validate_stage(
    kind: OperationKind,
    state: &FactoryState,
    review_baseline: Option<&ReviewBaseline>,
) -> StageResult {
    match kind {
        OperationKind::Plan => validate_plan(state),
        // A completed iterate turn must leave the same durable shape as
        // execute: every unit completed with an evidence-based summary.
        OperationKind::Execute | OperationKind::Iterate => validate_execute(state),
        OperationKind::Review => validate_review(
            state,
            review_baseline.ok_or(StageValidationError::Missing("review baseline"))?,
        ),
        OperationKind::Remediate => validate_remediation(state),
    }
}

pub fn validate_plan(state: &FactoryState) -> StageResult {
    validate_decomposition(state)?;
    for unit in &state.work_units {
        if unit.status != FactoryProgressStatus::Pending {
            return Err(StageValidationError::invalid(
                "planning state",
                format!("unit {} has execution status {:?}", unit.id, unit.status),
            ));
        }
        if unit.progress_summary.is_some() {
            return Err(StageValidationError::invalid(
                "planning state",
                format!("unit {} has an execution progress summary", unit.id),
            ));
        }
    }
    if state.review.is_some() || !state.remediations.is_empty() || !state.review_history.is_empty()
    {
        return Err(StageValidationError::invalid(
            "planning state",
            "planning must not record review or remediation results",
        ));
    }
    Ok(())
}

fn validate_decomposition(state: &FactoryState) -> StageResult {
    let units = &state.work_units;
    if units.is_empty() {
        return Err(StageValidationError::Missing("work-unit decomposition"));
    }

    let mut ids = HashSet::with_capacity(units.len());
    for unit in units {
        require_text("work unit id", &unit.id)?;
        require_text("work unit title", &unit.title)?;
        require_text("work unit description", &unit.description)?;
        if !ids.insert(unit.id.as_str()) {
            return Err(StageValidationError::invalid(
                "work-unit decomposition",
                format!("duplicate unit id {}", unit.id),
            ));
        }
    }

    for unit in units {
        let mut dependencies = HashSet::with_capacity(unit.depends_on.len());
        for dependency in &unit.depends_on {
            require_text("work unit dependency", dependency)?;
            if dependency == &unit.id {
                return Err(StageValidationError::invalid(
                    "work-unit decomposition",
                    format!("unit {} depends on itself", unit.id),
                ));
            }
            if !ids.contains(dependency.as_str()) {
                return Err(StageValidationError::invalid(
                    "work-unit decomposition",
                    format!("unit {} depends on unknown unit {dependency}", unit.id),
                ));
            }
            if !dependencies.insert(dependency.as_str()) {
                return Err(StageValidationError::invalid(
                    "work-unit decomposition",
                    format!("unit {} repeats dependency {dependency}", unit.id),
                ));
            }
        }
    }

    validate_acyclic(units)
}

pub fn validate_execute(state: &FactoryState) -> StageResult {
    validate_decomposition(state)?;
    for unit in &state.work_units {
        if unit.status != FactoryProgressStatus::Completed {
            return Err(StageValidationError::invalid(
                "execution progress",
                format!("unit {} is not completed", unit.id),
            ));
        }
        match unit.progress_summary.as_deref() {
            Some(summary) => require_text("work unit progress summary", summary)?,
            None => {
                return Err(StageValidationError::invalid(
                    "execution progress",
                    format!("unit {} has no progress summary", unit.id),
                ));
            }
        }
    }
    Ok(())
}

pub fn validate_review(state: &FactoryState, baseline: &ReviewBaseline) -> StageResult {
    validate_execute(state)?;
    let review = state
        .review
        .as_ref()
        .ok_or(StageValidationError::Missing("review"))?;
    validate_review_report(review, state, Some(baseline))
}

pub fn validate_remediation(state: &FactoryState) -> StageResult {
    validate_execute(state)?;
    let review = state
        .review
        .as_ref()
        .ok_or(StageValidationError::Missing("review"))?;
    validate_review_report(review, state, None)?;
    if review.verdict == FactoryReviewVerdict::Approve {
        return if state.remediations.is_empty() {
            Ok(())
        } else {
            Err(StageValidationError::invalid(
                "remediation",
                "an approved review must not have remediation records",
            ))
        };
    }

    if state.remediations.len() != review.findings.len() {
        return Err(StageValidationError::invalid(
            "remediation",
            format!(
                "expected {} records, found {}",
                review.findings.len(),
                state.remediations.len()
            ),
        ));
    }

    let findings = review
        .findings
        .iter()
        .map(|finding| (finding.id.as_str(), finding.unit_id.as_str()))
        .collect::<HashMap<_, _>>();
    let mut seen = HashSet::with_capacity(state.remediations.len());
    for record in &state.remediations {
        require_text("remediation finding id", &record.finding_id)?;
        require_text("remediation unit id", &record.unit_id)?;
        require_text("remediation rationale", &record.rationale)?;
        if !seen.insert(record.finding_id.as_str()) {
            return Err(StageValidationError::invalid(
                "remediation",
                format!("duplicate record for finding {}", record.finding_id),
            ));
        }
        let expected_unit = findings.get(record.finding_id.as_str()).ok_or_else(|| {
            StageValidationError::invalid(
                "remediation",
                format!("unknown finding {}", record.finding_id),
            )
        })?;
        if *expected_unit != record.unit_id {
            return Err(StageValidationError::invalid(
                "remediation",
                format!(
                    "finding {} belongs to unit {}, not {}",
                    record.finding_id, expected_unit, record.unit_id
                ),
            ));
        }
    }
    Ok(())
}

fn validate_review_report(
    review: &FactoryReviewReport,
    state: &FactoryState,
    baseline: Option<&ReviewBaseline>,
) -> StageResult {
    if review.generation == 0 {
        return Err(StageValidationError::invalid(
            "review generation",
            "generation must be positive",
        ));
    }
    if let Some(baseline) = baseline
        && review.generation <= baseline.generation
    {
        return Err(StageValidationError::invalid(
            "review generation",
            format!(
                "generation {} did not advance beyond {}",
                review.generation, baseline.generation
            ),
        ));
    }

    require_text("review summary", &review.summary)?;
    require_optional_text(
        "review recorded turn id",
        review.recorded_turn_id.as_deref(),
    )?;
    let recorded_thread_id = require_optional_text(
        "review recorded thread id",
        review.recorded_thread_id.as_deref(),
    )?;
    let recorded_parent_thread_id = require_optional_text(
        "review recorded parent thread id",
        review.recorded_parent_thread_id.as_deref(),
    )?;
    require_optional_text(
        "review recorded parent turn id",
        review.recorded_parent_turn_id.as_deref(),
    )?;
    let subagent_kind = require_optional_text(
        "review recorded subagent kind",
        review.recorded_subagent_kind.as_deref(),
    )?;
    if subagent_kind != "review" {
        return Err(StageValidationError::invalid(
            "review provenance",
            format!("subagent kind must be review, found {subagent_kind:?}"),
        ));
    }
    if recorded_thread_id == recorded_parent_thread_id {
        return Err(StageValidationError::invalid(
            "review provenance",
            "review thread must differ from its parent thread",
        ));
    }
    if let Some(baseline) = baseline {
        if recorded_parent_thread_id != baseline.parent_thread_id {
            return Err(StageValidationError::invalid(
                "review provenance",
                format!(
                    "parent thread is {recorded_parent_thread_id:?}, expected {:?}",
                    baseline.parent_thread_id
                ),
            ));
        }
        let recorded_parent_turn_id = review
            .recorded_parent_turn_id
            .as_deref()
            .expect("review parent turn was validated above");
        if recorded_parent_turn_id != baseline.parent_turn_id {
            return Err(StageValidationError::invalid(
                "review provenance",
                format!(
                    "parent turn is {recorded_parent_turn_id:?}, expected {:?}",
                    baseline.parent_turn_id
                ),
            ));
        }
    }
    if review.verdict != FactoryReviewVerdict::Approve && review.findings.is_empty() {
        return Err(StageValidationError::invalid(
            "review findings",
            "a non-approved review requires at least one finding",
        ));
    }
    if review.verdict == FactoryReviewVerdict::Approve && !review.findings.is_empty() {
        return Err(StageValidationError::invalid(
            "review findings",
            "an approved review must not contain findings",
        ));
    }

    let unit_ids = state
        .work_units
        .iter()
        .map(|unit| unit.id.as_str())
        .collect::<HashSet<_>>();
    let mut finding_ids = HashSet::with_capacity(review.findings.len());
    for finding in &review.findings {
        require_text("review finding id", &finding.id)?;
        require_text("review finding unit id", &finding.unit_id)?;
        require_text("review finding title", &finding.title)?;
        require_text("review finding evidence", &finding.evidence)?;
        require_text("review finding recommendation", &finding.recommendation)?;
        if !finding_ids.insert(finding.id.as_str()) {
            return Err(StageValidationError::invalid(
                "review findings",
                format!("duplicate finding id {}", finding.id),
            ));
        }
        if !unit_ids.contains(finding.unit_id.as_str()) {
            return Err(StageValidationError::invalid(
                "review findings",
                format!(
                    "finding {} references unknown unit {}",
                    finding.id, finding.unit_id
                ),
            ));
        }
    }
    Ok(())
}

fn validate_acyclic(units: &[FactoryWorkUnit]) -> StageResult {
    let mut remaining = units
        .iter()
        .map(|unit| (unit.id.as_str(), unit.depends_on.len()))
        .collect::<HashMap<_, _>>();
    let mut completed = HashSet::with_capacity(units.len());
    loop {
        let ready = remaining
            .iter()
            .filter(|(id, count)| !completed.contains(**id) && **count == 0)
            .map(|(id, _)| *id)
            .collect::<Vec<_>>();
        if ready.is_empty() {
            break;
        }
        for id in ready {
            completed.insert(id);
            for unit in units
                .iter()
                .filter(|unit| unit.depends_on.iter().any(|dependency| dependency == id))
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
        Err(StageValidationError::invalid(
            "work-unit decomposition",
            "dependency graph contains a cycle",
        ))
    }
}

fn require_optional_text<'a>(field: &'static str, value: Option<&'a str>) -> StageResult<&'a str> {
    let value = value.ok_or(StageValidationError::Missing(field))?;
    require_text(field, value)?;
    Ok(value)
}

fn require_text(field: &'static str, value: &str) -> StageResult {
    if value.trim().is_empty() {
        Err(StageValidationError::invalid(field, "must not be empty"))
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state_with_unit(status: FactoryProgressStatus, summary: Option<&str>) -> FactoryState {
        FactoryState {
            revision: 1,
            work_units: vec![FactoryWorkUnit {
                id: "unit-1".to_string(),
                title: "Implement the change".to_string(),
                description: "Make and verify the requested change.".to_string(),
                depends_on: Vec::new(),
                status,
                progress_summary: summary.map(str::to_string),
            }],
            ..FactoryState::default()
        }
    }

    #[test]
    fn plan_accepts_only_decomposition_state() {
        validate_plan(&state_with_unit(FactoryProgressStatus::Pending, None)).unwrap();

        let error = validate_plan(&state_with_unit(
            FactoryProgressStatus::Completed,
            Some("implemented during planning"),
        ))
        .unwrap_err();
        assert!(error.to_string().contains("planning state"));
    }

    #[test]
    fn plan_prompt_names_the_exact_pure_state_contract() {
        assert!(PLAN_PROMPT.contains("On every Plan attempt"));
        assert!(PLAN_PROMPT.contains("exactly one call to factory_decompose"));
        for forbidden in [
            "factory_update_progress",
            "factory_record_review",
            "factory_record_remediation",
        ] {
            assert!(PLAN_PROMPT.contains(forbidden));
        }
        assert!(PLAN_PROMPT.contains("must remain pending"));
        assert!(PLAN_PROMPT.contains("no progress or result summary"));
        assert_eq!(OperationKind::Plan.collaboration_mode(), ModeKind::Default);
        assert_eq!(OperationKind::Plan.turn_stage(), FactoryTurnStage::Plan);
        assert_eq!(
            OperationKind::Plan.sandbox_policy(),
            SandboxPolicy::ReadOnly {
                network_access: true,
            }
        );
        assert_eq!(
            OperationKind::Execute.sandbox_policy(),
            SandboxPolicy::DangerFullAccess
        );
    }

    #[test]
    fn execute_accepts_completed_units_without_reusing_plan_purity_gate() {
        validate_execute(&state_with_unit(
            FactoryProgressStatus::Completed,
            Some("verified with the repository check"),
        ))
        .unwrap();
    }

    #[test]
    fn execute_prompt_requires_one_completed_update_per_incomplete_unit() {
        assert!(EXECUTE_PROMPT.contains("exactly once with completed"));
        assert!(EXECUTE_PROMPT.contains("evidence-based summary"));
        assert!(!EXECUTE_PROMPT.contains("in_progress"));
    }

    #[test]
    fn ordinals_map_to_base_stages_then_continuation_rounds() {
        let expected = [
            OperationKind::Plan,
            OperationKind::Execute,
            OperationKind::Review,
            OperationKind::Remediate,
            OperationKind::Iterate,
            OperationKind::Review,
            OperationKind::Remediate,
            OperationKind::Iterate,
            OperationKind::Review,
            OperationKind::Remediate,
        ];
        for (ordinal, kind) in expected.into_iter().enumerate() {
            assert_eq!(OperationKind::at_ordinal(ordinal as u32), kind);
        }
        assert_eq!(
            "codex.iterate".parse::<OperationKind>().unwrap(),
            OperationKind::Iterate
        );
    }

    #[test]
    fn iterate_reuses_the_execute_state_contract_with_full_access() {
        validate_stage(
            OperationKind::Iterate,
            &state_with_unit(
                FactoryProgressStatus::Completed,
                Some("follow-up verified against the repository"),
            ),
            None,
        )
        .unwrap();
        assert_eq!(
            OperationKind::Iterate.sandbox_policy(),
            SandboxPolicy::DangerFullAccess
        );
        assert_eq!(
            OperationKind::Iterate.turn_stage(),
            FactoryTurnStage::Execute
        );
        assert!(ITERATE_PROMPT.contains("follow-up feedback"));
        assert!(ITERATE_PROMPT.contains("factory_update_progress"));
        assert!(ITERATE_PROMPT.contains("Do not create a new decomposition"));
    }
}
