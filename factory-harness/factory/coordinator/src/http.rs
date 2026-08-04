use crate::ArtifactManager;
use crate::AttemptFence;
use crate::CoordinatorError;
use crate::CoordinatorInstanceId;
use crate::CoordinatorStore;
use crate::EnsureWorkspaceRequest;
use crate::NewAttemptEvent;
use crate::NewJobEvent;
use crate::WorkspaceManager;
use crate::ids::AttemptId;
use crate::ids::JobId;
use crate::ids::ThreadId;
use axum::Json;
use axum::Router;
use axum::body::Body;
use axum::extract::FromRef;
use axum::extract::Path;
use axum::extract::Query;
use axum::extract::State;
use axum::http::HeaderValue;
use axum::http::StatusCode;
use axum::http::header::CONTENT_TYPE;
use axum::response::IntoResponse;
use axum::response::Response;
use axum::routing::get;
use axum::routing::post;
use serde::Deserialize;
use serde::Serialize;
use tokio::net::TcpListener;

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
struct JobEventListQuery {
    #[serde(default)]
    after: u64,
    #[serde(default = "default_job_event_limit")]
    limit: u32,
}

fn default_job_event_limit() -> u32 {
    200
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AttemptEventWriteRequest {
    owner_instance_id: CoordinatorInstanceId,
    lease_epoch: u64,
    #[serde(flatten)]
    event: NewAttemptEvent,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ThreadStateWriteRequest {
    attempt_id: AttemptId,
    owner_instance_id: CoordinatorInstanceId,
    lease_epoch: u64,
    state: serde_json::Value,
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
    artifacts: ArtifactManager,
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
                    CoordinatorError::InvalidInput(_) => (StatusCode::BAD_REQUEST, "invalidInput"),
                    CoordinatorError::JobNotFound(_) => (StatusCode::NOT_FOUND, "jobNotFound"),
                    CoordinatorError::JobNotCancellable { .. } => {
                        (StatusCode::CONFLICT, "jobNotCancellable")
                    }
                    CoordinatorError::JobNotContinuable { .. } => {
                        (StatusCode::CONFLICT, "jobNotContinuable")
                    }
                    CoordinatorError::JobCancellationRequested(_) => {
                        (StatusCode::CONFLICT, "jobCancellationRequested")
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
                    CoordinatorError::ThreadStateOwnershipMismatch { .. } => {
                        (StatusCode::CONFLICT, "threadStateOwnershipMismatch")
                    }
                    CoordinatorError::Database(_)
                    | CoordinatorError::Serialization(_)
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
    let artifacts = ArtifactManager::from_env().map_err(std::io::Error::other)?;
    axum::serve(
        listener,
        router(ApiState {
            store,
            workspaces,
            artifacts,
        }),
    )
    .with_graceful_shutdown(shutdown_signal())
    .await
}

fn router(state: ApiState) -> Router {
    Router::new()
        .route("/healthz", get(health))
        .route("/jobs", post(create_job))
        .route("/jobs/active", get(list_active_jobs))
        .route("/jobs/{job_id}", get(load_job))
        .route("/jobs/{job_id}/cancel", post(cancel_job))
        .route("/jobs/{job_id}/continue", post(continue_job))
        .route("/jobs/{job_id}/attempts", get(list_job_attempts))
        .route("/jobs/{job_id}/events", get(list_job_events))
        .route("/jobs/{job_id}/result", get(export_workspace_result))
        .route(
            "/jobs/{job_id}/stage-checkpoints",
            get(list_stage_checkpoints),
        )
        .route(
            "/jobs/{job_id}/workspace",
            get(load_workspace)
                .put(ensure_workspace)
                .delete(remove_workspace),
        )
        .route(
            "/jobs/{job_id}/workspace/revision",
            post(refresh_workspace_revision),
        )
        .route("/attempts/{attempt_id}/events", post(append_attempt_event))
        .route(
            "/threads/{thread_id}/state",
            get(load_thread_state).put(put_thread_state),
        )
        .with_state(state)
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::SignalKind;

        let terminate = tokio::signal::unix::signal(SignalKind::terminate());
        match terminate {
            Ok(mut terminate) => {
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => {}
                    _ = terminate.recv() => {}
                }
            }
            Err(_) => {
                let _ = tokio::signal::ctrl_c().await;
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

async fn health() -> Json<HealthResponse> {
    Json(HealthResponse { status: "ok" })
}

async fn create_job(
    State(state): State<ApiState>,
    Json(definition): Json<crate::JobDefinition>,
) -> ApiResult {
    let job = state.store.create_job(definition).await?;
    initialize_job_artifacts(&state, &job, None).await;
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

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ContinueJobRequest {
    feedback: String,
}

async fn continue_job(
    State(state): State<ApiState>,
    Path(job_id): Path<String>,
    Json(request): Json<ContinueJobRequest>,
) -> ApiResult {
    let job = state
        .store
        .continue_job(&JobId::new(job_id), &request.feedback)
        .await?;
    let workspace = state.store.load_workspace(&job.job.job_id).await?;
    initialize_job_artifacts(&state, &job, workspace.as_ref()).await;
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

async fn list_job_events(
    State(store): State<CoordinatorStore>,
    Path(job_id): Path<String>,
    Query(query): Query<JobEventListQuery>,
) -> ApiResult {
    let page = store
        .list_job_events(&JobId::new(job_id), query.after, query.limit)
        .await?;
    Ok(Json(page).into_response())
}

async fn export_workspace_result(
    State(state): State<ApiState>,
    Path(job_id): Path<String>,
) -> ApiResult {
    let result = state
        .workspaces
        .export_result(&state.store, &JobId::new(job_id))
        .await?;
    let mut response = Response::new(Body::from(result.patch));
    response.headers_mut().insert(
        CONTENT_TYPE,
        HeaderValue::from_static("application/vnd.git.patch"),
    );
    insert_result_header(
        &mut response,
        "x-factory-repository-id",
        &result.repository_id,
    )?;
    insert_result_header(
        &mut response,
        "x-factory-base-revision",
        &result.base_revision,
    )?;
    insert_result_header(
        &mut response,
        "x-factory-patch-sha256",
        &result.patch_sha256,
    )?;
    Ok(response)
}

fn insert_result_header(
    response: &mut Response,
    name: &'static str,
    value: &str,
) -> Result<(), ApiError> {
    let value = HeaderValue::from_str(value).map_err(|error| {
        CoordinatorError::Workspace(format!("invalid {name} result metadata: {error}"))
    })?;
    response.headers_mut().insert(name, value);
    Ok(())
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
    if let Ok(job) = state.store.load_job(&workspace.job_id).await {
        initialize_job_artifacts(&state, &job, Some(&workspace)).await;
    }
    Ok((StatusCode::OK, Json(workspace)).into_response())
}

async fn initialize_job_artifacts(
    state: &ApiState,
    job: &crate::DurableJob,
    workspace: Option<&crate::WorkspaceRecord>,
) {
    let result = state.artifacts.initialize_job_files(job, workspace).await;
    let warning = match result {
        Ok(warnings) if warnings.is_empty() => return,
        Ok(warnings) => format!(
            "{} local artifact projection file(s) could not be refreshed",
            warnings.len()
        ),
        Err(error) => format!("job artifact initialization failed: {error}"),
    };
    eprintln!("factoryd: {warning}");
    let _ = state
        .store
        .append_job_event(NewJobEvent {
            job_id: job.job.job_id.clone(),
            kind: "artifact.warning".to_string(),
            payload: serde_json::json!({"message": warning}),
        })
        .await;
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

async fn append_attempt_event(
    State(store): State<CoordinatorStore>,
    Path(attempt_id): Path<String>,
    Json(request): Json<AttemptEventWriteRequest>,
) -> ApiResult {
    let fence = AttemptFence {
        attempt_id: AttemptId::new(attempt_id),
        owner_instance_id: request.owner_instance_id,
        lease_epoch: request.lease_epoch,
    };
    let record = store.append_attempt_event(&fence, request.event).await?;
    Ok((StatusCode::CREATED, Json(record)).into_response())
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
    Json(request): Json<ThreadStateWriteRequest>,
) -> ApiResult {
    let fence = AttemptFence {
        attempt_id: request.attempt_id,
        owner_instance_id: request.owner_instance_id,
        lease_epoch: request.lease_epoch,
    };
    let state = store
        .put_thread_state(&fence, &ThreadId::new(thread_id), request.state)
        .await?;
    Ok(Json(state).into_response())
}
