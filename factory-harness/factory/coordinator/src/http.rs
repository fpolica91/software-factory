use crate::AttemptFailure;
use crate::ClaimRequest;
use crate::CoordinatorError;
use crate::CoordinatorInstanceId;
use crate::CoordinatorStore;
use crate::EnsureWorkspaceRequest;
use crate::FactoryThreadStateDocument;
use crate::NewCheckpoint;
use crate::NewPendingRequest;
use crate::PendingRequestId;
use crate::PendingRequestResolution;
use crate::RenewLeaseRequest;
use crate::WorkspaceManager;
use axum::Json;
use axum::Router;
use axum::extract::FromRef;
use axum::extract::Path;
use axum::extract::Query;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::response::Response;
use axum::routing::get;
use axum::routing::post;
use factory_protocol::FactoryCorrelation;
use factory_protocol::ids::AttemptId;
use factory_protocol::ids::JobId;
use factory_protocol::ids::OperationId;
use factory_protocol::ids::ThreadId;
use serde::Deserialize;
use serde::Serialize;
use tokio::net::TcpListener;

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecoveryClaimRequest {
    pub job_id: Option<JobId>,
    pub owner_instance_id: CoordinatorInstanceId,
    pub lease_seconds: u32,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PendingRequestListQuery {
    job_id: Option<JobId>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct HealthResponse {
    status: &'static str,
}

#[derive(Debug, Serialize)]
struct ErrorEnvelope {
    error: ErrorBody,
}

#[derive(Debug, Serialize)]
struct ErrorBody {
    code: &'static str,
    message: String,
}

enum ApiError {
    Coordinator(CoordinatorError),
    ThreadStateNotFound(ThreadId),
}

#[derive(Clone)]
struct ApiState {
    store: CoordinatorStore,
    workspaces: WorkspaceManager,
}

impl FromRef<ApiState> for CoordinatorStore {
    fn from_ref(state: &ApiState) -> Self {
        state.store.clone()
    }
}

impl FromRef<ApiState> for WorkspaceManager {
    fn from_ref(state: &ApiState) -> Self {
        state.workspaces.clone()
    }
}

impl From<CoordinatorError> for ApiError {
    fn from(error: CoordinatorError) -> Self {
        Self::Coordinator(error)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, code, message) = match self {
            Self::ThreadStateNotFound(thread_id) => (
                StatusCode::NOT_FOUND,
                "threadStateNotFound",
                format!("Factory state for thread {thread_id} was not found"),
            ),
            Self::Coordinator(error) => {
                let (status, code) = match &error {
                    CoordinatorError::InvalidJobDefinition(_) => {
                        (StatusCode::BAD_REQUEST, "invalidJobDefinition")
                    }
                    CoordinatorError::WorkflowRunConflict(_) => {
                        (StatusCode::CONFLICT, "workflowRunConflict")
                    }
                    CoordinatorError::InvalidInput(_) => (StatusCode::BAD_REQUEST, "invalidInput"),
                    CoordinatorError::JobNotFound(_) => (StatusCode::NOT_FOUND, "jobNotFound"),
                    CoordinatorError::JobNotCancellable { .. } => {
                        (StatusCode::CONFLICT, "jobNotCancellable")
                    }
                    CoordinatorError::WorkspaceNotFound(_) => {
                        (StatusCode::NOT_FOUND, "workspaceNotFound")
                    }
                    CoordinatorError::Workspace(_) => {
                        (StatusCode::UNPROCESSABLE_ENTITY, "workspaceOperationFailed")
                    }
                    CoordinatorError::AttemptNotRunning(_) => {
                        (StatusCode::CONFLICT, "attemptNotRunning")
                    }
                    CoordinatorError::AttemptLeaseUnavailable(_) => {
                        (StatusCode::CONFLICT, "attemptLeaseUnavailable")
                    }
                    CoordinatorError::CorrelationMismatch => {
                        (StatusCode::CONFLICT, "correlationMismatch")
                    }
                    CoordinatorError::CheckpointCorrelationMismatch => {
                        (StatusCode::CONFLICT, "checkpointCorrelationMismatch")
                    }
                    CoordinatorError::PendingRequestNotFound(_) => {
                        (StatusCode::NOT_FOUND, "pendingRequestNotFound")
                    }
                    CoordinatorError::PendingRequestInactive(_) => {
                        (StatusCode::CONFLICT, "pendingRequestInactive")
                    }
                    CoordinatorError::PendingRequestConflict(_) => {
                        (StatusCode::CONFLICT, "pendingRequestConflict")
                    }
                    CoordinatorError::PendingRequestPairing(_) => {
                        (StatusCode::CONFLICT, "pendingRequestPairingMismatch")
                    }
                    CoordinatorError::PendingRequestPayload(_) => {
                        (StatusCode::BAD_REQUEST, "invalidPendingRequestPayload")
                    }
                    CoordinatorError::Database(_)
                    | CoordinatorError::ThreadStateDecode(_)
                    | CoordinatorError::UnsupportedState { .. }
                    | CoordinatorError::NumericRange { .. } => {
                        (StatusCode::INTERNAL_SERVER_ERROR, "internalError")
                    }
                };
                (status, code, error.to_string())
            }
        };
        (
            status,
            Json(ErrorEnvelope {
                error: ErrorBody { code, message },
            }),
        )
            .into_response()
    }
}

type ApiResult = Result<Response, ApiError>;

pub async fn serve_http(store: CoordinatorStore, listener: TcpListener) -> std::io::Result<()> {
    let workspaces = WorkspaceManager::from_env().map_err(std::io::Error::other)?;
    serve_http_with_workspaces(store, workspaces, listener).await
}

pub async fn serve_http_with_workspaces(
    store: CoordinatorStore,
    workspaces: WorkspaceManager,
    listener: TcpListener,
) -> std::io::Result<()> {
    axum::serve(listener, router(ApiState { store, workspaces }))
        .with_graceful_shutdown(shutdown_signal())
        .await
}

fn router(state: ApiState) -> Router {
    Router::new()
        .route("/healthz", get(health))
        .route("/v1/jobs", post(create_job))
        .route("/v1/jobs/active", get(list_active_jobs))
        .route("/v1/jobs/{job_id}", get(load_job))
        .route("/v1/jobs/{job_id}/cancel", post(cancel_job))
        .route("/v1/jobs/{job_id}/attempts", get(list_job_attempts))
        .route(
            "/v1/jobs/{job_id}/stage-checkpoints",
            get(list_stage_checkpoints),
        )
        .route(
            "/v1/jobs/{job_id}/workspace",
            get(load_workspace)
                .put(ensure_workspace)
                .delete(remove_workspace),
        )
        .route(
            "/v1/jobs/{job_id}/workspace/revision",
            post(refresh_workspace_revision),
        )
        .route("/v1/recoveries/claim", post(claim_recovery))
        .route("/v1/operations/{operation_id}/claim", post(claim_operation))
        .route("/v1/correlations", post(append_correlation))
        .route(
            "/v1/pending-requests",
            get(list_pending_requests).post(register_pending_request),
        )
        .route(
            "/v1/pending-requests/{pending_request_id}",
            get(load_pending_request),
        )
        .route(
            "/v1/pending-requests/{pending_request_id}/resolve",
            post(resolve_pending_request),
        )
        .route("/v1/checkpoints", post(save_checkpoint))
        .route("/v1/attempts/{attempt_id}/complete", post(complete_attempt))
        .route("/v1/attempts/{attempt_id}/fail", post(fail_attempt))
        .route("/v1/attempts/{attempt_id}/renew", post(renew_attempt))
        .route(
            "/v1/threads/{thread_id}/state",
            get(load_thread_state).put(put_thread_state),
        )
        .with_state(state)
}

async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
}

async fn health() -> Json<HealthResponse> {
    Json(HealthResponse { status: "ok" })
}

async fn create_job(
    State(store): State<CoordinatorStore>,
    Json(definition): Json<crate::JobDefinition>,
) -> ApiResult {
    let job = store.create_job(definition).await?;
    Ok((StatusCode::CREATED, Json(job)).into_response())
}

async fn load_job(State(store): State<CoordinatorStore>, Path(job_id): Path<String>) -> ApiResult {
    let job = store.load_job(&JobId::new(job_id)).await?;
    Ok(Json(job).into_response())
}

async fn list_active_jobs(State(store): State<CoordinatorStore>) -> ApiResult {
    let jobs = store.list_active_jobs().await?;
    Ok(Json(jobs).into_response())
}

async fn cancel_job(
    State(store): State<CoordinatorStore>,
    Path(job_id): Path<String>,
) -> ApiResult {
    let job = store.cancel_job(&JobId::new(job_id)).await?;
    Ok(Json(job).into_response())
}

async fn list_stage_checkpoints(
    State(store): State<CoordinatorStore>,
    Path(job_id): Path<String>,
) -> ApiResult {
    let checkpoints = store.list_stage_checkpoints(&JobId::new(job_id)).await?;
    Ok(Json(checkpoints).into_response())
}

async fn list_job_attempts(
    State(store): State<CoordinatorStore>,
    Path(job_id): Path<String>,
) -> ApiResult {
    let attempts = store.list_job_attempts(&JobId::new(job_id)).await?;
    Ok(Json(attempts).into_response())
}

async fn ensure_workspace(
    State(state): State<ApiState>,
    Path(job_id): Path<String>,
    Json(request): Json<EnsureWorkspaceRequest>,
) -> ApiResult {
    let workspace = state
        .workspaces
        .ensure(&state.store, &JobId::new(job_id), request)
        .await?;
    Ok((StatusCode::OK, Json(workspace)).into_response())
}

async fn load_workspace(State(state): State<ApiState>, Path(job_id): Path<String>) -> ApiResult {
    let workspace = state
        .workspaces
        .load(&state.store, &JobId::new(job_id))
        .await?;
    Ok((StatusCode::OK, Json(workspace)).into_response())
}

async fn refresh_workspace_revision(
    State(state): State<ApiState>,
    Path(job_id): Path<String>,
) -> ApiResult {
    let workspace = state
        .workspaces
        .refresh_revision(&state.store, &JobId::new(job_id))
        .await?;
    Ok((StatusCode::OK, Json(workspace)).into_response())
}

async fn remove_workspace(State(state): State<ApiState>, Path(job_id): Path<String>) -> ApiResult {
    let workspace = state
        .workspaces
        .remove(&state.store, &JobId::new(job_id))
        .await?;
    Ok((StatusCode::OK, Json(workspace)).into_response())
}

async fn claim_recovery(
    State(store): State<CoordinatorStore>,
    Json(request): Json<RecoveryClaimRequest>,
) -> ApiResult {
    let claim = ClaimRequest {
        owner_instance_id: request.owner_instance_id,
        lease_seconds: request.lease_seconds,
    };
    let lease = match request.job_id {
        Some(job_id) => store.claim_recovery_for_job(&job_id, &claim).await?,
        None => store.claim_next_recovery(&claim).await?,
    };
    Ok(optional_lease_response(lease))
}

async fn claim_operation(
    State(store): State<CoordinatorStore>,
    Path(operation_id): Path<String>,
    Json(request): Json<ClaimRequest>,
) -> ApiResult {
    let lease = store
        .claim_recovery_for_operation(&OperationId::new(operation_id), &request)
        .await?;
    Ok(optional_lease_response(lease))
}

fn optional_lease_response(lease: Option<crate::RecoveryLease>) -> Response {
    match lease {
        Some(lease) => Json(lease).into_response(),
        None => StatusCode::NO_CONTENT.into_response(),
    }
}

async fn append_correlation(
    State(store): State<CoordinatorStore>,
    Json(correlation): Json<FactoryCorrelation>,
) -> ApiResult {
    let record = store.append_correlation(&correlation).await?;
    Ok((StatusCode::CREATED, Json(record)).into_response())
}

async fn register_pending_request(
    State(store): State<CoordinatorStore>,
    Json(pending): Json<NewPendingRequest>,
) -> ApiResult {
    let (record, created) = store.register_pending_request(pending).await?;
    let status = if created {
        StatusCode::CREATED
    } else {
        StatusCode::OK
    };
    Ok((status, Json(record)).into_response())
}

async fn list_pending_requests(
    State(store): State<CoordinatorStore>,
    Query(query): Query<PendingRequestListQuery>,
) -> ApiResult {
    let records = store.list_pending_requests(query.job_id.as_ref()).await?;
    Ok(Json(records).into_response())
}

async fn load_pending_request(
    State(store): State<CoordinatorStore>,
    Path(pending_request_id): Path<String>,
) -> ApiResult {
    let record = store
        .load_pending_request(&PendingRequestId::new(pending_request_id))
        .await?;
    Ok(Json(record).into_response())
}

async fn resolve_pending_request(
    State(store): State<CoordinatorStore>,
    Path(pending_request_id): Path<String>,
    Json(resolution): Json<PendingRequestResolution>,
) -> ApiResult {
    let record = store
        .resolve_pending_request(&PendingRequestId::new(pending_request_id), resolution)
        .await?;
    Ok(Json(record).into_response())
}

async fn save_checkpoint(
    State(store): State<CoordinatorStore>,
    Json(checkpoint): Json<NewCheckpoint>,
) -> ApiResult {
    let record = store.save_checkpoint(checkpoint).await?;
    Ok((StatusCode::CREATED, Json(record)).into_response())
}

async fn complete_attempt(
    State(store): State<CoordinatorStore>,
    Path(attempt_id): Path<String>,
) -> ApiResult {
    store.complete_attempt(&AttemptId::new(attempt_id)).await?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

async fn fail_attempt(
    State(store): State<CoordinatorStore>,
    Path(attempt_id): Path<String>,
    Json(failure): Json<AttemptFailure>,
) -> ApiResult {
    store
        .fail_attempt(&AttemptId::new(attempt_id), failure)
        .await?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

async fn renew_attempt(
    State(store): State<CoordinatorStore>,
    Path(attempt_id): Path<String>,
    Json(request): Json<RenewLeaseRequest>,
) -> ApiResult {
    let attempt = store
        .renew_attempt(&AttemptId::new(attempt_id), &request)
        .await?;
    Ok(Json(attempt).into_response())
}

async fn load_thread_state(
    State(store): State<CoordinatorStore>,
    Path(thread_id): Path<String>,
) -> ApiResult {
    let thread_id = ThreadId::new(thread_id);
    let state = store
        .load_thread_state(&thread_id)
        .await?
        .ok_or_else(|| ApiError::ThreadStateNotFound(thread_id))?;
    Ok(Json(state).into_response())
}

async fn put_thread_state(
    State(store): State<CoordinatorStore>,
    Path(thread_id): Path<String>,
    Json(state): Json<FactoryThreadStateDocument>,
) -> ApiResult {
    let state = store
        .put_thread_state(&ThreadId::new(thread_id), state)
        .await?;
    Ok(Json(state).into_response())
}
