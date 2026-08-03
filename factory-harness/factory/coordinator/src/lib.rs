//! Durable state and recovery coordination for `factoryd`.
//!
//! The coordinator owns the lifecycle outside Codex: durable jobs,
//! operations, attempts, checkpoints, correlations, events, leases, and
//! retries. It stores only the durable Factory context needed around kernel
//! execution.

mod artifacts;
mod correlation;
mod domain;
mod error;
mod http;
mod ids;
mod rows;
mod runner;
mod schema;
mod stage_output;
mod store;
mod workspace;

pub use artifacts::ArtifactManager;
pub use artifacts::ArtifactPaths;
pub use artifacts::ArtifactProjectionWarning;
pub use artifacts::JobArtifactFile;
pub use correlation::Correlation;
pub use domain::AttemptFailure;
pub use domain::AttemptFence;
pub use domain::AttemptRecord;
pub use domain::AttemptSettlement;
pub use domain::AttemptState;
pub use domain::CheckpointId;
pub use domain::CheckpointRecord;
pub use domain::ClaimRequest;
pub use domain::CoordinatorInstanceId;
pub use domain::CorrelationRecordId;
pub use domain::DurableCorrelationRecord;
pub use domain::DurableJob;
pub use domain::EnsureWorkspaceRequest;
pub use domain::ExecutionProfile;
pub use domain::FactoryTaskInput;
pub use domain::FactoryThreadStateRecord;
pub use domain::JobDefinition;
pub use domain::JobEventPage;
pub use domain::JobEventRecord;
pub use domain::JobRecord;
pub use domain::JobState;
pub use domain::NewAttemptEvent;
pub use domain::NewCheckpoint;
pub use domain::NewJobEvent;
pub use domain::OperationDefinition;
pub use domain::OperationRecord;
pub use domain::OperationState;
pub use domain::RecoveryCause;
pub use domain::RecoveryLease;
pub use domain::RecoverySelection;
pub use domain::RenewLeaseRequest;
pub use domain::ResumeStrategy;
pub use domain::StageCheckpointRecord;
pub use domain::WorkspaceBinding;
pub use domain::WorkspaceRecord;
pub use domain::WorkspaceResult;
pub use domain::WorkspaceState;
pub use error::CoordinatorError;
pub use error::Result;
pub use http::serve_http;
pub use http::serve_http_with_workspaces;
pub use ids::AttemptId;
pub use ids::ItemId;
pub use ids::JobId;
pub use ids::OperationId;
pub use ids::RequestId;
pub use ids::ThreadId;
pub use ids::TurnId;
pub use runner::CancellationHandle;
pub use runner::CheckpointWriter;
pub use runner::DurableRunner;
pub use runner::OperationCheckpoint;
pub use runner::OperationExecutionContext;
pub use runner::OperationExecutionResult;
pub use runner::OperationExecutor;
pub use runner::OperationOutcome;
pub use runner::RunnerConfig;
pub use stage_output::CompletedStageOutput;
pub use stage_output::reduce_settled_job_outputs;
pub use stage_output::render_job_result;
pub use store::CoordinatorStore;
pub use store::WorkspaceExecutionGuard;
pub use workspace::WorkspaceManager;
pub use workspace::WorkspaceSnapshot;
