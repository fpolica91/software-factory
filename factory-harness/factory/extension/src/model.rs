use serde::Deserialize;
use serde::Serialize;

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct FactoryState {
    pub revision: u64,
    pub work_units: Vec<FactoryWorkUnit>,
    pub review: Option<FactoryReviewReport>,
    pub remediations: Vec<FactoryRemediationRecord>,
    #[serde(default)]
    pub subagents: Vec<FactorySubagentActivity>,
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
