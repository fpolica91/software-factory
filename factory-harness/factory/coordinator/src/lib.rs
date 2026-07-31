//! Durable state and recovery coordination for `factoryd`.
//!
//! The coordinator owns the lifecycle outside Codex: durable jobs,
//! operations, attempts, checkpoints, correlations, leases, and retries. It
//! communicates with the execution kernel only through Factory Protocol IDs.

mod domain;
mod error;
mod http;
mod rows;
mod schema;
mod store;
mod workspace;

pub use domain::AttemptFailure;
pub use domain::AttemptRecord;
pub use domain::AttemptState;
pub use domain::CheckpointId;
pub use domain::CheckpointRecord;
pub use domain::ClaimRequest;
pub use domain::CoordinatorInstanceId;
pub use domain::CorrelationRecordId;
pub use domain::DurableCorrelationRecord;
pub use domain::DurableJob;
pub use domain::EnsureWorkspaceRequest;
pub use domain::FactoryThreadStateDocument;
pub use domain::FactoryThreadStateRecord;
pub use domain::JobDefinition;
pub use domain::JobRecord;
pub use domain::JobState;
pub use domain::NewCheckpoint;
pub use domain::NewPendingRequest;
pub use domain::OperationDefinition;
pub use domain::OperationRecord;
pub use domain::OperationState;
pub use domain::PendingRequestId;
pub use domain::PendingRequestRecord;
pub use domain::PendingRequestResolution;
pub use domain::PendingRequestState;
pub use domain::RecoveryCause;
pub use domain::RecoveryLease;
pub use domain::RecoverySelection;
pub use domain::RenewLeaseRequest;
pub use domain::ResumeStrategy;
pub use domain::StageCheckpointRecord;
pub use domain::WorkspaceRecord;
pub use domain::WorkspaceState;
pub use error::CoordinatorError;
pub use error::Result;
pub use http::RecoveryClaimRequest;
pub use http::serve_http;
pub use http::serve_http_with_workspaces;
pub use store::CoordinatorStore;
pub use workspace::WorkspaceManager;
