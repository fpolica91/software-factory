use std::collections::HashMap;

use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;

use crate::FactoryBackendError;
use crate::FactoryProgressStatus;
use crate::FactoryRemediationRecord;
use crate::FactoryReviewCycle;
use crate::FactoryReviewRecoveryBaseline;
use crate::FactoryReviewReport;
use crate::FactoryState;
use crate::FactorySubagentActivity;
use crate::FactorySubagentHistory;
use crate::FactoryWorkUnit;

/// Typed codec for the single Factory-owned durable state document.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FactoryStateDocument {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    decomposition: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    progress: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    review: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    remediation: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    review_history: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    subagents: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    review_recovery: Option<Value>,
}

#[derive(Debug, Deserialize, Serialize)]
struct DecompositionDocument {
    revision: u64,
    work_units: Vec<WorkUnitDefinition>,
}

#[derive(Debug, Deserialize, Serialize)]
struct WorkUnitDefinition {
    id: String,
    title: String,
    description: String,
    depends_on: Vec<String>,
}

#[derive(Debug, Deserialize, Serialize)]
struct ProgressDocument {
    work_units: Vec<WorkUnitProgress>,
}

#[derive(Debug, Deserialize, Serialize)]
struct WorkUnitProgress {
    id: String,
    status: FactoryProgressStatus,
    progress_summary: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
struct RemediationDocument {
    records: Vec<FactoryRemediationRecord>,
}

#[derive(Debug, Deserialize, Serialize)]
struct ReviewHistoryDocument {
    cycles: Vec<FactoryReviewCycle>,
}

#[derive(Debug, Deserialize, Serialize)]
struct SubagentsDocument {
    activities: Vec<FactorySubagentActivity>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    history: Option<FactorySubagentHistory>,
}

impl FactoryStateDocument {
    pub fn from_state(state: &FactoryState) -> Result<Self, FactoryBackendError> {
        let decomposition = DecompositionDocument {
            revision: state.revision,
            work_units: state
                .work_units
                .iter()
                .map(|unit| WorkUnitDefinition {
                    id: unit.id.clone(),
                    title: unit.title.clone(),
                    description: unit.description.clone(),
                    depends_on: unit.depends_on.clone(),
                })
                .collect(),
        };
        let progress = ProgressDocument {
            work_units: state
                .work_units
                .iter()
                .map(|unit| WorkUnitProgress {
                    id: unit.id.clone(),
                    status: unit.status,
                    progress_summary: unit.progress_summary.clone(),
                })
                .collect(),
        };
        Ok(Self {
            decomposition: Some(to_value("decomposition", decomposition)?),
            progress: Some(to_value("progress", progress)?),
            review: state
                .review
                .as_ref()
                .map(|review| to_value("review", review))
                .transpose()?,
            remediation: Some(to_value(
                "remediation",
                RemediationDocument {
                    records: state.remediations.clone(),
                },
            )?),
            review_history: Some(to_value(
                "review history",
                ReviewHistoryDocument {
                    cycles: state.review_history.clone(),
                },
            )?),
            subagents: Some(to_value(
                "subagents",
                SubagentsDocument {
                    activities: state.subagents.clone(),
                    history: state.subagent_history.clone(),
                },
            )?),
            review_recovery: state
                .review_recovery
                .as_ref()
                .map(|baseline| to_value("review recovery", baseline))
                .transpose()?,
        })
    }

    pub fn into_state(self) -> Result<FactoryState, FactoryBackendError> {
        let Some(decomposition) = self.decomposition else {
            if self.progress.is_none()
                && self.review.is_none()
                && self.remediation.is_none()
                && self.review_history.is_none()
                && self.subagents.is_none()
                && self.review_recovery.is_none()
            {
                return Ok(FactoryState::default());
            }
            return Err(FactoryBackendError::new(
                "Factory state has contributor data without a decomposition",
            ));
        };
        let decomposition: DecompositionDocument = from_value("decomposition", decomposition)?;
        let progress = self
            .progress
            .ok_or_else(|| FactoryBackendError::new("Factory state is missing progress"))?;
        let progress: ProgressDocument = from_value("progress", progress)?;
        let mut progress_by_id = HashMap::new();
        for entry in progress.work_units {
            if progress_by_id.insert(entry.id.clone(), entry).is_some() {
                return Err(FactoryBackendError::new(
                    "Factory progress contains a duplicate work-unit ID",
                ));
            }
        }
        let mut work_units = Vec::with_capacity(decomposition.work_units.len());
        for definition in decomposition.work_units {
            let progress = progress_by_id.remove(&definition.id).ok_or_else(|| {
                FactoryBackendError::new(format!(
                    "Factory progress is missing work unit {}",
                    definition.id
                ))
            })?;
            work_units.push(FactoryWorkUnit {
                id: definition.id,
                title: definition.title,
                description: definition.description,
                depends_on: definition.depends_on,
                status: progress.status,
                progress_summary: progress.progress_summary,
            });
        }
        if !progress_by_id.is_empty() {
            return Err(FactoryBackendError::new(
                "Factory progress references an unknown work-unit ID",
            ));
        }
        let review = self
            .review
            .map(|value| from_value::<FactoryReviewReport>("review", value))
            .transpose()?;
        let remediations = self
            .remediation
            .map(|value| from_value::<RemediationDocument>("remediation", value))
            .transpose()?
            .map_or_else(Vec::new, |document| document.records);
        let review_history = self
            .review_history
            .map(|value| from_value::<ReviewHistoryDocument>("review history", value))
            .transpose()?
            .map_or_else(Vec::new, |document| document.cycles);
        let subagents = self
            .subagents
            .map(|value| from_value::<SubagentsDocument>("subagents", value))
            .transpose()?;
        let subagent_history = subagents
            .as_ref()
            .and_then(|document| document.history.clone());
        let subagents = subagents.map_or_else(Vec::new, |document| document.activities);
        let review_recovery = self
            .review_recovery
            .map(|value| from_value::<FactoryReviewRecoveryBaseline>("review recovery", value))
            .transpose()?;
        Ok(FactoryState {
            revision: decomposition.revision,
            work_units,
            review,
            remediations,
            review_history,
            subagents,
            subagent_history,
            review_recovery,
        })
    }
}

fn to_value(contributor: &str, value: impl Serialize) -> Result<Value, FactoryBackendError> {
    serde_json::to_value(value).map_err(|error| {
        FactoryBackendError::new(format!("encode Factory {contributor} state: {error}"))
    })
}

fn from_value<T: for<'de> Deserialize<'de>>(
    contributor: &str,
    value: Value,
) -> Result<T, FactoryBackendError> {
    serde_json::from_value(value).map_err(|error| {
        FactoryBackendError::new(format!("decode Factory {contributor} state: {error}"))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typed_document_round_trips_factory_state() {
        let mut state = FactoryState {
            revision: 7,
            work_units: vec![FactoryWorkUnit {
                id: "unit-1".to_string(),
                title: "Implement".to_string(),
                description: "Implement the durable state boundary".to_string(),
                depends_on: Vec::new(),
                status: FactoryProgressStatus::InProgress,
                progress_summary: Some("writing state".to_string()),
            }],
            review: None,
            remediations: Vec::new(),
            review_history: Vec::new(),
            subagents: Vec::new(),
            subagent_history: None,
            review_recovery: None,
        };
        state.record_review(FactoryReviewReport {
            generation: 0,
            recorded_turn_id: Some("review-turn-1".to_string()),
            recorded_thread_id: Some("review-thread-1".to_string()),
            recorded_parent_thread_id: Some("parent-thread".to_string()),
            recorded_parent_turn_id: Some("execute-turn".to_string()),
            recorded_subagent_kind: Some("review".to_string()),
            verdict: crate::FactoryReviewVerdict::RequestChanges,
            summary: "Fix the finding".to_string(),
            findings: vec![crate::FactoryReviewFinding {
                id: "finding-1".to_string(),
                severity: crate::FactoryFindingSeverity::Major,
                unit_id: "unit-1".to_string(),
                title: "Incorrect result".to_string(),
                evidence: "The observed bytes differ".to_string(),
                recommendation: "Write the expected bytes".to_string(),
            }],
        });
        let retried_review = FactoryReviewReport {
            generation: 0,
            recorded_turn_id: Some("review-turn-2".to_string()),
            recorded_thread_id: Some("review-thread-2".to_string()),
            recorded_parent_thread_id: Some("parent-thread".to_string()),
            recorded_parent_turn_id: Some("execute-turn".to_string()),
            recorded_subagent_kind: Some("review".to_string()),
            verdict: crate::FactoryReviewVerdict::RequestChanges,
            summary: "Retry confirmed the finding".to_string(),
            findings: vec![crate::FactoryReviewFinding {
                id: "finding-1".to_string(),
                severity: crate::FactoryFindingSeverity::Major,
                unit_id: "unit-1".to_string(),
                title: "Incorrect result".to_string(),
                evidence: "The observed bytes still differ".to_string(),
                recommendation: "Write the expected bytes".to_string(),
            }],
        };
        state.record_review(retried_review.clone());
        assert_eq!(state.review.as_ref().unwrap().generation, 2);
        assert!(state.review_history.is_empty());
        state.record_review(retried_review);
        assert_eq!(state.review.as_ref().unwrap().generation, 2);
        assert!(state.review_history.is_empty());
        state.remediations.push(FactoryRemediationRecord {
            finding_id: "finding-1".to_string(),
            disposition: crate::FactoryRemediationDisposition::Resolved,
            rationale: "Corrected and verified the bytes".to_string(),
            unit_id: "unit-1".to_string(),
        });
        state.record_review(FactoryReviewReport {
            generation: 0,
            recorded_turn_id: Some("review-turn-3".to_string()),
            recorded_thread_id: Some("review-thread-3".to_string()),
            recorded_parent_thread_id: Some("parent-thread".to_string()),
            recorded_parent_turn_id: Some("remediation-turn".to_string()),
            recorded_subagent_kind: Some("review".to_string()),
            verdict: crate::FactoryReviewVerdict::Approve,
            summary: "Verified".to_string(),
            findings: Vec::new(),
        });

        assert_eq!(state.review_history.len(), 1);
        assert_eq!(state.review_history[0].remediations.len(), 1);
        assert_eq!(state.review.as_ref().unwrap().generation, 3);
        assert!(state.remediations.is_empty());
        state.subagent_history = Some(FactorySubagentHistory {
            source: crate::FactorySubagentHistorySource::CoordinatorJobEvents,
            event_kind: "factory.subagent.activity".to_string(),
            latest_sequence: 42,
        });

        let document = FactoryStateDocument::from_state(&state).unwrap();
        assert_eq!(document.into_state().unwrap(), state);
    }

    #[test]
    fn contributor_data_without_decomposition_is_rejected() {
        let document = FactoryStateDocument {
            progress: Some(serde_json::json!({ "work_units": [] })),
            ..FactoryStateDocument::default()
        };

        assert!(document.into_state().is_err());
    }

    #[test]
    fn review_recovery_baseline_survives_durable_document_round_trip() {
        let mut state = FactoryState {
            revision: 4,
            work_units: vec![FactoryWorkUnit {
                id: "unit-1".to_string(),
                title: "Implement".to_string(),
                description: "Preserve the implementation".to_string(),
                depends_on: Vec::new(),
                status: FactoryProgressStatus::Completed,
                progress_summary: Some("implemented".to_string()),
            }],
            ..FactoryState::default()
        };
        state.prepare_review_recovery();
        state.revision = 5;
        state.remediations.push(FactoryRemediationRecord {
            finding_id: "partial-review-write".to_string(),
            disposition: crate::FactoryRemediationDisposition::Resolved,
            rationale: "must be rolled back".to_string(),
            unit_id: "unit-1".to_string(),
        });

        let document = FactoryStateDocument::from_state(&state).unwrap();
        let mut recovered = document.into_state().unwrap();

        assert!(recovered.review_recovery.is_some());
        assert!(recovered.rollback_review_recovery());
        assert_eq!(recovered.revision, 4);
        assert!(recovered.remediations.is_empty());
        assert!(recovered.review_recovery.is_none());
    }
}
