use crate::ids::AttemptId;
use crate::ids::ItemId;
use crate::ids::JobId;
use crate::ids::OperationId;
use crate::ids::RequestId;
use crate::ids::ThreadId;
use crate::ids::TurnId;
use serde::Deserialize;
use serde::Serialize;

/// Durable Factory context associated with one runtime request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Correlation {
    pub job_id: JobId,
    pub operation_id: OperationId,
    pub attempt_id: AttemptId,
    pub request_id: RequestId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<ThreadId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<TurnId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item_id: Option<ItemId>,
}
