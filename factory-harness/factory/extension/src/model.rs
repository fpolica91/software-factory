use serde::Deserialize;
use serde::Serialize;

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct FactoryState {
    pub revision: u64,
    pub work_units: Vec<FactoryWorkUnit>,
    pub review: Option<FactoryReviewReport>,
    pub remediations: Vec<FactoryRemediationRecord>,
    #[serde(default)]
    pub review_history: Vec<FactoryReviewCycle>,
    #[serde(default)]
    pub subagents: Vec<FactorySubagentActivity>,
    #[serde(default)]
    pub subagent_history: Option<FactorySubagentHistory>,
    /// Runtime-only rollback point for a detached review. It is persisted by
    /// factoryd but intentionally omitted from model context.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review_recovery: Option<FactoryReviewRecoveryBaseline>,
}

/// Minimal non-recursive state changed by `factory_record_review`.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct FactoryReviewRecoveryBaseline {
    pub revision: u64,
    pub review: Option<FactoryReviewReport>,
    pub remediations: Vec<FactoryRemediationRecord>,
    pub review_history: Vec<FactoryReviewCycle>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct FactoryReviewCycle {
    pub review: FactoryReviewReport,
    pub remediations: Vec<FactoryRemediationRecord>,
}

impl FactoryState {
    /// Persists the exact review-owned fields before a detached review starts.
    /// An unfinished earlier review is rolled back before a replacement
    /// baseline is installed.
    pub fn prepare_review_recovery(&mut self) {
        self.rollback_review_recovery();
        self.review_recovery = Some(FactoryReviewRecoveryBaseline {
            revision: self.revision,
            review: self.review.clone(),
            remediations: self.remediations.clone(),
            review_history: self.review_history.clone(),
        });
    }

    /// Removes partial detached-review writes while retaining state owned by
    /// other Factory concerns.
    pub fn rollback_review_recovery(&mut self) -> bool {
        let Some(baseline) = self.review_recovery.take() else {
            return false;
        };
        self.revision = baseline.revision;
        self.review = baseline.review;
        self.remediations = baseline.remediations;
        self.review_history = baseline.review_history;
        true
    }

    /// Accepts a validated detached review and drops only its rollback point.
    pub fn commit_review_recovery(&mut self) -> bool {
        self.review_recovery.take().is_some()
    }

    pub(crate) fn record_review(&mut self, mut review: FactoryReviewReport) {
        let current_generation = self
            .review
            .as_ref()
            .map(|review| review.generation)
            .or_else(|| {
                self.review_history
                    .last()
                    .map(|cycle| cycle.review.generation)
            })
            .unwrap_or_default();
        let same_turn = self.review.as_ref().is_some_and(|current| {
            current.recorded_turn_id.is_some()
                && current.recorded_turn_id == review.recorded_turn_id
        });
        if same_turn {
            review.generation = current_generation.max(1);
            self.review = Some(review);
            return;
        }

        if self.remediations.is_empty() {
            self.review = None;
        } else if let Some(review) = self.review.take() {
            self.review_history.push(FactoryReviewCycle {
                review,
                remediations: std::mem::take(&mut self.remediations),
            });
        } else {
            self.remediations.clear();
        }
        review.generation = current_generation.saturating_add(1);
        self.review = Some(review);
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct FactorySubagentActivity {
    pub call_id: String,
    pub turn_id: String,
    pub sender_thread_id: String,
    pub receiver_thread_ids: Vec<String>,
    pub tool: FactorySubagentTool,
    pub prompt: Option<String>,
    pub status: FactorySubagentToolCallStatus,
    pub agents: Vec<FactorySubagentState>,
    pub created_at: String,
    pub updated_at: String,
}

/// Durable location of detailed activities omitted from the current-state
/// projection.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct FactorySubagentHistory {
    pub source: FactorySubagentHistorySource,
    pub event_kind: String,
    pub latest_sequence: u64,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FactorySubagentHistorySource {
    CoordinatorJobEvents,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FactorySubagentTool {
    SpawnAgent,
    SendInput,
    ResumeAgent,
    Wait,
    CloseAgent,
    Interact,
    InterruptAgent,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FactorySubagentToolCallStatus {
    InProgress,
    Completed,
    Failed,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct FactorySubagentState {
    pub thread_id: String,
    pub status: FactorySubagentStatus,
    pub terminal: bool,
    pub message: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FactorySubagentStatus {
    PendingInit,
    Running,
    Interrupted,
    Completed,
    Errored,
    Shutdown,
    NotFound,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct FactoryWorkUnit {
    pub id: String,
    pub title: String,
    pub description: String,
    pub depends_on: Vec<String>,
    pub status: FactoryProgressStatus,
    pub progress_summary: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FactoryProgressStatus {
    #[default]
    Pending,
    InProgress,
    Completed,
    Blocked,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct FactoryReviewReport {
    #[serde(default)]
    pub generation: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recorded_turn_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recorded_thread_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recorded_parent_thread_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recorded_parent_turn_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recorded_subagent_kind: Option<String>,
    pub verdict: FactoryReviewVerdict,
    pub summary: String,
    pub findings: Vec<FactoryReviewFinding>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FactoryReviewVerdict {
    Approve,
    RequestChanges,
    Blocked,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct FactoryReviewFinding {
    pub id: String,
    pub severity: FactoryFindingSeverity,
    pub unit_id: String,
    pub title: String,
    pub evidence: String,
    pub recommendation: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FactoryFindingSeverity {
    Critical,
    Major,
    Minor,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct FactoryRemediationRecord {
    pub finding_id: String,
    pub disposition: FactoryRemediationDisposition,
    pub rationale: String,
    pub unit_id: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FactoryRemediationDisposition {
    Accepted,
    Rejected,
    Deferred,
    Resolved,
}
