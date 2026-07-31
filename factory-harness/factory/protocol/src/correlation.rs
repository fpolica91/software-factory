use crate::ids::AttemptId;
use crate::ids::FactoryRequestId;
use crate::ids::ItemId;
use crate::ids::JobId;
use crate::ids::OperationId;
use crate::ids::TaskRunExternalId;
use crate::ids::ThreadId;
use crate::ids::TurnId;
use crate::ids::WorkflowRunId;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use ts_rs::TS;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct FactoryCorrelation {
    pub job_id: JobId,
    pub operation_id: OperationId,
    pub attempt_id: AttemptId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "WorkflowRunId")]
    #[ts(optional)]
    pub workflow_run_id: Option<WorkflowRunId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "TaskRunExternalId")]
    #[ts(optional)]
    pub task_run_external_id: Option<TaskRunExternalId>,
    pub request_id: FactoryRequestId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "ThreadId")]
    #[ts(optional)]
    pub thread_id: Option<ThreadId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "TurnId")]
    #[ts(optional)]
    pub turn_id: Option<TurnId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "ItemId")]
    #[ts(optional)]
    pub item_id: Option<ItemId>,
}
