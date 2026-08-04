use std::error::Error;
use std::fmt;
use std::str::FromStr;

use factory_coordinator::CheckpointRecord;
use factory_coordinator::CorrelationRecordId;
use factory_coordinator::OperationCheckpoint;
use serde::Deserialize;
use serde::Deserializer;
use serde::Serialize;
use serde::Serializer;

use crate::stages::OperationKind;

pub const STAGE_CHECKPOINT_KIND: &str = "factory.stage";

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum StageCheckpointPhase {
    AwaitingTurn,
    TurnCompleted,
    Completed,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum StageTurnRole {
    Stage,
    Remediation,
    Review,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StageLineage {
    pub parent_execution_thread_id: String,
    pub active_thread_id: String,
    pub turn_id: String,
}

impl StageLineage {
    pub fn new(
        parent_execution_thread_id: impl Into<String>,
        active_thread_id: impl Into<String>,
        turn_id: impl Into<String>,
    ) -> Result<Self, StageCheckpointError> {
        let lineage = Self {
            parent_execution_thread_id: parent_execution_thread_id.into(),
            active_thread_id: active_thread_id.into(),
            turn_id: turn_id.into(),
        };
        lineage.validate()?;
        Ok(lineage)
    }

    fn validate(&self) -> Result<(), StageCheckpointError> {
        require_text("parentExecutionThreadId", &self.parent_execution_thread_id)?;
        require_text("activeThreadId", &self.active_thread_id)?;
        require_text("turnId", &self.turn_id)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FactoryStateBaselines {
    pub revision: u64,
    pub review_generation: Option<u64>,
    pub expected_review_parent_turn_id: Option<String>,
}

impl FactoryStateBaselines {
    pub fn new(
        revision: u64,
        review_generation: Option<u64>,
        expected_review_parent_turn_id: Option<String>,
    ) -> Self {
        Self {
            revision,
            review_generation,
            expected_review_parent_turn_id,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct StageCheckpoint {
    #[serde(
        deserialize_with = "deserialize_operation_kind",
        serialize_with = "serialize_operation_kind"
    )]
    pub operation: OperationKind,
    pub parent_execution_thread_id: String,
    pub active_thread_id: String,
    pub turn_id: String,
    pub phase: StageCheckpointPhase,
    pub turn_role: StageTurnRole,
    pub review_cycle: u32,
    pub state_revision_baseline: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review_generation_baseline: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_review_parent_turn_id: Option<String>,
}

impl StageCheckpoint {
    pub fn awaiting_turn(
        operation: OperationKind,
        lineage: StageLineage,
        turn_role: StageTurnRole,
        review_cycle: u32,
        baselines: FactoryStateBaselines,
    ) -> Result<Self, StageCheckpointError> {
        Self::new(
            operation,
            lineage,
            StageCheckpointPhase::AwaitingTurn,
            turn_role,
            review_cycle,
            baselines,
        )
    }

    pub fn turn_completed(
        operation: OperationKind,
        lineage: StageLineage,
        turn_role: StageTurnRole,
        review_cycle: u32,
        baselines: FactoryStateBaselines,
    ) -> Result<Self, StageCheckpointError> {
        Self::new(
            operation,
            lineage,
            StageCheckpointPhase::TurnCompleted,
            turn_role,
            review_cycle,
            baselines,
        )
    }

    pub fn completed(
        operation: OperationKind,
        lineage: StageLineage,
        turn_role: StageTurnRole,
        review_cycle: u32,
        baselines: FactoryStateBaselines,
    ) -> Result<Self, StageCheckpointError> {
        Self::new(
            operation,
            lineage,
            StageCheckpointPhase::Completed,
            turn_role,
            review_cycle,
            baselines,
        )
    }

    pub fn encode(
        &self,
        workspace_root: Option<String>,
        workspace_revision: Option<String>,
        correlation_id: Option<CorrelationRecordId>,
    ) -> Result<OperationCheckpoint, StageCheckpointError> {
        self.validate()?;
        let payload = serde_json::to_value(self)
            .map_err(|error| StageCheckpointError::Malformed(error.to_string()))?;
        Ok(OperationCheckpoint {
            kind: STAGE_CHECKPOINT_KIND.to_owned(),
            payload,
            workspace_root,
            workspace_revision,
            correlation_id,
        })
    }

    pub fn decode(record: &CheckpointRecord) -> Result<Self, StageCheckpointError> {
        if record.kind != STAGE_CHECKPOINT_KIND {
            return Err(StageCheckpointError::WrongKind {
                actual: record.kind.clone(),
            });
        }
        let checkpoint: Self = serde_json::from_value(record.payload.clone())
            .map_err(|error| StageCheckpointError::Malformed(error.to_string()))?;
        checkpoint.validate()?;
        Ok(checkpoint)
    }

    pub fn lineage(&self) -> StageLineage {
        StageLineage {
            parent_execution_thread_id: self.parent_execution_thread_id.clone(),
            active_thread_id: self.active_thread_id.clone(),
            turn_id: self.turn_id.clone(),
        }
    }

    pub fn lineage_from_completed(
        record: &CheckpointRecord,
    ) -> Result<StageLineage, StageCheckpointError> {
        let checkpoint = Self::decode(record)?;
        if checkpoint.phase != StageCheckpointPhase::Completed {
            return Err(StageCheckpointError::NotCompleted(checkpoint.phase));
        }
        Ok(checkpoint.lineage())
    }

    fn new(
        operation: OperationKind,
        lineage: StageLineage,
        phase: StageCheckpointPhase,
        turn_role: StageTurnRole,
        review_cycle: u32,
        baselines: FactoryStateBaselines,
    ) -> Result<Self, StageCheckpointError> {
        let checkpoint = Self {
            operation,
            parent_execution_thread_id: lineage.parent_execution_thread_id,
            active_thread_id: lineage.active_thread_id,
            turn_id: lineage.turn_id,
            phase,
            turn_role,
            review_cycle,
            state_revision_baseline: baselines.revision,
            review_generation_baseline: baselines.review_generation,
            expected_review_parent_turn_id: baselines.expected_review_parent_turn_id,
        };
        checkpoint.validate()?;
        Ok(checkpoint)
    }

    fn validate(&self) -> Result<(), StageCheckpointError> {
        self.lineage().validate()?;

        match (self.operation, self.turn_role, self.review_cycle) {
            (
                OperationKind::Plan | OperationKind::Execute | OperationKind::Iterate,
                StageTurnRole::Stage,
                0,
            ) => {}
            (OperationKind::Review, StageTurnRole::Review, 0) => {}
            (
                OperationKind::Remediate,
                StageTurnRole::Remediation | StageTurnRole::Review,
                cycle,
            ) if cycle > 0 => {}
            _ => {
                return Err(StageCheckpointError::Malformed(
                    "turnRole and reviewCycle do not match the operation".to_owned(),
                ));
            }
        }

        let review_turn = self.turn_role == StageTurnRole::Review;
        if review_turn != self.review_generation_baseline.is_some() {
            return Err(StageCheckpointError::Malformed(
                "reviewGenerationBaseline must be present only for review turns".to_owned(),
            ));
        }
        if review_turn != self.expected_review_parent_turn_id.is_some() {
            return Err(StageCheckpointError::Malformed(
                "expectedReviewParentTurnId must be present only for review turns".to_owned(),
            ));
        }
        if let Some(parent_turn_id) = &self.expected_review_parent_turn_id {
            require_text("expectedReviewParentTurnId", parent_turn_id)?;
        }

        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum StageCheckpointError {
    WrongKind { actual: String },
    Malformed(String),
    NotCompleted(StageCheckpointPhase),
}

impl fmt::Display for StageCheckpointError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::WrongKind { actual } => write!(
                formatter,
                "checkpoint kind must be {STAGE_CHECKPOINT_KIND}, got {actual}"
            ),
            Self::Malformed(detail) => write!(formatter, "malformed stage checkpoint: {detail}"),
            Self::NotCompleted(phase) => {
                write!(formatter, "stage checkpoint is not completed: {phase:?}")
            }
        }
    }
}

impl Error for StageCheckpointError {}

fn require_text(field: &'static str, value: &str) -> Result<(), StageCheckpointError> {
    if value.trim().is_empty() {
        return Err(StageCheckpointError::Malformed(format!(
            "{field} must not be empty"
        )));
    }
    Ok(())
}

fn serialize_operation_kind<S>(operation: &OperationKind, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    serializer.serialize_str(operation.as_wire_name())
}

fn deserialize_operation_kind<'de, D>(deserializer: D) -> Result<OperationKind, D::Error>
where
    D: Deserializer<'de>,
{
    let value = String::deserialize(deserializer)?;
    OperationKind::from_str(&value).map_err(serde::de::Error::custom)
}
