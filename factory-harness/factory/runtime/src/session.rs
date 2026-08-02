use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use anyhow::bail;
use codex_app_server_client::InProcessAppServerClient;
use codex_app_server_client::InProcessClientStartArgs;
use codex_app_server_client::InProcessServerEvent;
use codex_app_server_protocol::AskForApproval;
use codex_app_server_protocol::ClientRequest;
use codex_app_server_protocol::DetachedReviewContext;
use codex_app_server_protocol::JSONRPCErrorError;
use codex_app_server_protocol::McpServerElicitationAction;
use codex_app_server_protocol::McpServerElicitationRequestResponse;
use codex_app_server_protocol::RequestId;
use codex_app_server_protocol::ReviewDelivery;
use codex_app_server_protocol::ReviewStartParams;
use codex_app_server_protocol::ReviewStartResponse;
use codex_app_server_protocol::ReviewTarget;
use codex_app_server_protocol::SandboxMode;
use codex_app_server_protocol::SandboxPolicy;
use codex_app_server_protocol::ServerNotification;
use codex_app_server_protocol::ServerRequest;
use codex_app_server_protocol::ThreadReadParams;
use codex_app_server_protocol::ThreadReadResponse;
use codex_app_server_protocol::ThreadResumeParams;
use codex_app_server_protocol::ThreadResumeResponse;
use codex_app_server_protocol::ThreadStartParams;
use codex_app_server_protocol::ThreadStartResponse;
use codex_app_server_protocol::Turn;
use codex_app_server_protocol::TurnInterruptParams;
use codex_app_server_protocol::TurnInterruptResponse;
use codex_app_server_protocol::TurnStartParams;
use codex_app_server_protocol::TurnStartResponse;
use codex_app_server_protocol::TurnStatus;
use codex_app_server_protocol::UserInput;
use codex_config::AbsolutePathBuf;
use codex_protocol::config_types::CollaborationMode;
use codex_protocol::config_types::ModeKind;
use codex_protocol::config_types::Settings;
use factory_coordinator::CancellationHandle;
use factory_extension::FACTORY_STAGE_METADATA_KEY;
use factory_extension::FactoryStateBackend;
use factory_extension::FactoryTurnStage;

use crate::events::AttemptEventWriter;

const INTERRUPT_TIMEOUT: Duration = Duration::from_secs(5);
// Upstream bounds its own drain at 45 seconds. The outer bound also covers a
// shutdown command that cannot be enqueued.
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(50);

pub type SessionResult<T> = anyhow::Result<T>;

/// Create a new durable parent thread or rejoin an existing one.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ParentThread {
    New,
    Resume { thread_id: String },
}

/// Exact app-server request and thread IDs for durable correlation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ThreadCorrelation {
    request_id: RequestId,
    thread_id: String,
}

impl ThreadCorrelation {
    pub fn request_id(&self) -> &RequestId {
        &self.request_id
    }

    pub fn thread_id(&self) -> &str {
        &self.thread_id
    }
}

/// Exact app-server request, thread, and turn IDs for durable correlation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TurnCorrelation {
    request_id: RequestId,
    thread_id: String,
    turn_id: String,
}

impl TurnCorrelation {
    pub fn request_id(&self) -> &RequestId {
        &self.request_id
    }

    pub fn thread_id(&self) -> &str {
        &self.thread_id
    }

    pub fn turn_id(&self) -> &str {
        &self.turn_id
    }
}

/// Compact autonomous wrapper around Codex's native in-process app-server.
pub struct AutonomousSession {
    client: Option<InProcessAppServerClient>,
    request_ids: RequestIdSequence,
    parent: ThreadCorrelation,
    cwd: AbsolutePathBuf,
    runtime_workspace_roots: Vec<AbsolutePathBuf>,
    model: String,
    model_provider: String,
    active_turn: Option<TurnCorrelation>,
    latest_turn: Option<TurnCorrelation>,
}

impl AutonomousSession {
    /// Starts Codex with the supplied durable Factory state backend, then
    /// starts or resumes the parent thread. The correlation contains the exact
    /// request ID used for that app-server call.
    pub async fn start(
        args: InProcessClientStartArgs,
        backend: Arc<dyn FactoryStateBackend>,
        repository_id: factory_extension::FactoryRepositoryId,
        factory_stage: FactoryTurnStage,
        parent: ParentThread,
    ) -> SessionResult<(Self, ThreadCorrelation)> {
        let requested = RequestedThread {
            cwd: args.config.cwd.clone(),
            runtime_workspace_roots: args.config.workspace_roots.clone(),
            model: args.config.model.clone(),
            model_provider: args.config.model_provider_id.clone(),
            developer_instructions: args.config.developer_instructions.clone(),
            ephemeral: args.config.ephemeral,
        };
        let client =
            crate::in_process::start_with_backend(args, backend, repository_id, factory_stage)
                .await
                .context("start native Codex runtime")?;
        let mut request_ids = RequestIdSequence::default();
        let request_id = request_ids.next();

        let effective = match request_parent(&client, &requested, parent, request_id.clone()).await
        {
            Ok(effective) => effective,
            Err(error) => {
                let _ = shutdown_client(client).await;
                return Err(error);
            }
        };
        if let Err(error) = effective.require_exact(&requested) {
            let _ = shutdown_client(client).await;
            return Err(error);
        }

        let correlation = ThreadCorrelation {
            request_id,
            thread_id: effective.thread_id,
        };
        let session = Self {
            client: Some(client),
            request_ids,
            parent: correlation.clone(),
            cwd: effective.cwd,
            runtime_workspace_roots: effective.runtime_workspace_roots,
            model: effective.model,
            model_provider: effective.model_provider,
            active_turn: None,
            latest_turn: None,
        };
        Ok((session, correlation))
    }

    pub fn parent_correlation(&self) -> &ThreadCorrelation {
        &self.parent
    }

    pub fn parent_thread_id(&self) -> &str {
        self.parent.thread_id()
    }

    pub fn turn_correlation(&self) -> Option<&TurnCorrelation> {
        self.latest_turn.as_ref()
    }

    pub fn turn_thread_id(&self) -> Option<&str> {
        self.latest_turn.as_ref().map(TurnCorrelation::thread_id)
    }

    pub fn turn_id(&self) -> Option<&str> {
        self.latest_turn.as_ref().map(TurnCorrelation::turn_id)
    }

    pub fn model(&self) -> &str {
        &self.model
    }

    pub fn model_provider(&self) -> &str {
        &self.model_provider
    }

    /// Reads the exact persisted status of a Codex turn from rollout history.
    /// A missing turn is distinct from a completed turn and must not be used
    /// to promote a durable Factory checkpoint.
    pub async fn persisted_turn_status(
        &mut self,
        thread_id: &str,
        turn_id: &str,
    ) -> SessionResult<Option<TurnStatus>> {
        self.require_idle()?;
        if thread_id.trim().is_empty() {
            bail!("persisted turn thread id must not be empty");
        }
        if turn_id.trim().is_empty() {
            bail!("persisted turn id must not be empty");
        }

        let request_id = self.request_ids.next();
        let response: ThreadReadResponse = self
            .client()?
            .request_typed(ClientRequest::ThreadRead {
                request_id,
                params: ThreadReadParams {
                    thread_id: thread_id.to_string(),
                    include_turns: true,
                },
            })
            .await
            .context("thread/read persisted turn status")?;
        if response.thread.id != thread_id {
            bail!(
                "thread/read returned thread {} instead of {}",
                response.thread.id,
                thread_id
            );
        }
        Ok(response
            .thread
            .turns
            .into_iter()
            .find(|turn| turn.id == turn_id)
            .map(|turn| turn.status))
    }

    /// Starts a normal text turn on the durable parent thread.
    pub async fn start_text_turn(
        &mut self,
        prompt: impl Into<String>,
        sandbox_policy: SandboxPolicy,
        mode: ModeKind,
        factory_stage: FactoryTurnStage,
    ) -> SessionResult<TurnCorrelation> {
        self.require_idle()?;
        let prompt = prompt.into();
        if prompt.trim().is_empty() {
            bail!("turn prompt must not be empty");
        }
        let request_id = self.request_ids.next();
        let response: TurnStartResponse = self
            .client()?
            .request_typed(ClientRequest::TurnStart {
                request_id: request_id.clone(),
                params: TurnStartParams {
                    thread_id: self.parent.thread_id.clone(),
                    input: vec![UserInput::Text {
                        text: prompt,
                        text_elements: Vec::new(),
                    }],
                    cwd: Some(self.cwd.to_path_buf()),
                    runtime_workspace_roots: Some(self.runtime_workspace_roots.clone()),
                    approval_policy: Some(AskForApproval::Never),
                    sandbox_policy: Some(sandbox_policy),
                    model: Some(self.model.clone()),
                    collaboration_mode: Some(native_collaboration_mode(&self.model, mode)),
                    responsesapi_client_metadata: Some(HashMap::from([(
                        FACTORY_STAGE_METADATA_KEY.to_string(),
                        factory_stage.as_wire_name().to_string(),
                    )])),
                    ..TurnStartParams::default()
                },
            })
            .await
            .context("turn/start")?;
        Ok(self.record_turn(TurnCorrelation {
            request_id,
            thread_id: self.parent.thread_id.clone(),
            turn_id: response.turn.id,
        }))
    }

    /// Starts a detached native `review/start` using custom instructions.
    pub async fn start_review(
        &mut self,
        instructions: impl Into<String>,
        parent_turn_id: impl Into<String>,
        durable_state_key: impl Into<String>,
    ) -> SessionResult<TurnCorrelation> {
        self.require_idle()?;
        let instructions = instructions.into();
        if instructions.trim().is_empty() {
            bail!("review instructions must not be empty");
        }
        let parent_turn_id = parent_turn_id.into();
        if parent_turn_id.trim().is_empty() {
            bail!("review parent turn id must not be empty");
        }
        let durable_state_key = durable_state_key.into();
        if durable_state_key.trim().is_empty() {
            bail!("review durable state key must not be empty");
        }
        let request_id = self.request_ids.next();
        let response: ReviewStartResponse = self
            .client()?
            .request_typed(ClientRequest::ReviewStart {
                request_id: request_id.clone(),
                params: ReviewStartParams {
                    thread_id: self.parent.thread_id.clone(),
                    target: ReviewTarget::Custom { instructions },
                    delivery: Some(ReviewDelivery::Detached),
                    detached_context: Some(DetachedReviewContext {
                        parent_thread_id: self.parent.thread_id.clone(),
                        parent_turn_id,
                        durable_state_key,
                    }),
                },
            })
            .await
            .context("review/start")?;
        Ok(self.record_turn(TurnCorrelation {
            request_id,
            thread_id: response.review_thread_id,
            turn_id: response.turn.id,
        }))
    }

    /// Drains all events until the active thread and turn complete. Unrelated
    /// notifications are drained; every server request is answered.
    pub async fn wait_for_completion(
        &mut self,
        cancellation: &CancellationHandle,
        events: &mut AttemptEventWriter,
    ) -> SessionResult<Turn> {
        let active = self
            .active_turn
            .clone()
            .context("session has no active turn")?;
        loop {
            let next = {
                let client = self.client.as_mut().context("session is closed")?;
                tokio::select! {
                    _ = cancellation.cancelled() => NextEvent::Cancelled,
                    event = client.next_event() => NextEvent::Server(event),
                }
            };
            let event = match next {
                NextEvent::Cancelled => {
                    let _ = events
                        .emit(
                            "turn.cancelled",
                            serde_json::json!({
                                "message": "turn cancelled after the durable attempt stopped",
                            }),
                        )
                        .await;
                    return self.cancel(active).await;
                }
                NextEvent::Server(Some(event)) => event,
                NextEvent::Server(None) => {
                    self.active_turn = None;
                    events
                        .emit(
                            "turn.error",
                            serde_json::json!({
                                "message": "Codex event stream closed before turn completion",
                            }),
                        )
                        .await?;
                    bail!(
                        "app-server event stream closed while waiting for {}/{}",
                        active.thread_id,
                        active.turn_id
                    );
                }
            };

            match event {
                InProcessServerEvent::ServerRequest(request) => {
                    handle_server_request(self.client()?, *request).await?;
                }
                InProcessServerEvent::ServerNotification(notification) => {
                    let notification = *notification;
                    let terminal = matches!(
                        &notification,
                        ServerNotification::Error(error)
                            if error.thread_id == active.thread_id
                                && error.turn_id == active.turn_id
                                && !error.will_retry
                    ) || matches!(
                        &notification,
                        ServerNotification::TurnCompleted(completed)
                            if completed.thread_id == active.thread_id
                                && completed.turn.id == active.turn_id
                    );
                    let event_result = events.observe(&notification).await;
                    if terminal {
                        self.active_turn = None;
                    }
                    event_result?;
                    match notification {
                        ServerNotification::Error(error)
                            if error.thread_id == active.thread_id
                                && error.turn_id == active.turn_id
                                && !error.will_retry =>
                        {
                            bail!(
                                "turn {}/{} failed: {}",
                                active.thread_id,
                                active.turn_id,
                                error.error
                            );
                        }
                        ServerNotification::TurnCompleted(completed)
                            if completed.thread_id == active.thread_id
                                && completed.turn.id == active.turn_id =>
                        {
                            let turn = completed.turn;
                            if turn.status == TurnStatus::Completed {
                                return Ok(turn);
                            }
                            let detail = turn
                                .error
                                .as_ref()
                                .map_or_else(String::new, |error| format!(": {error}"));
                            bail!(
                                "turn {}/{} ended with status {:?}{}",
                                active.thread_id,
                                active.turn_id,
                                turn.status,
                                detail
                            );
                        }
                        _ => {}
                    }
                }
                InProcessServerEvent::Lagged { skipped } => {
                    self.active_turn = None;
                    events
                        .emit(
                            "turn.error",
                            serde_json::json!({
                                "message": format!(
                                    "Codex event stream lost {skipped} events"
                                ),
                                "skipped": skipped,
                            }),
                        )
                        .await?;
                    bail!(
                        "app-server event stream lost {skipped} events; durable execution cannot continue"
                    );
                }
            }
        }
    }

    pub async fn shutdown(mut self) -> SessionResult<()> {
        match self.client.take() {
            Some(client) => shutdown_client(client).await,
            None => Ok(()),
        }
    }

    fn client(&self) -> SessionResult<&InProcessAppServerClient> {
        self.client.as_ref().context("session is closed")
    }

    fn require_idle(&self) -> SessionResult<()> {
        if let Some(turn) = &self.active_turn {
            bail!(
                "thread {} already has active turn {}",
                turn.thread_id,
                turn.turn_id
            );
        }
        Ok(())
    }

    fn record_turn(&mut self, turn: TurnCorrelation) -> TurnCorrelation {
        self.active_turn = Some(turn.clone());
        self.latest_turn = Some(turn.clone());
        turn
    }

    async fn cancel<T>(&mut self, active: TurnCorrelation) -> SessionResult<T> {
        let request_id = self.request_ids.next();
        let interrupt_error = match self.client.as_ref() {
            Some(client) => {
                let request =
                    client.request_typed::<TurnInterruptResponse>(ClientRequest::TurnInterrupt {
                        request_id: request_id.clone(),
                        params: TurnInterruptParams {
                            thread_id: active.thread_id.clone(),
                            turn_id: active.turn_id.clone(),
                        },
                    });
                match tokio::time::timeout(INTERRUPT_TIMEOUT, request).await {
                    Ok(Ok(_)) => None,
                    Ok(Err(error)) => Some(error.to_string()),
                    Err(_) => Some(format!(
                        "turn/interrupt exceeded {} seconds",
                        INTERRUPT_TIMEOUT.as_secs()
                    )),
                }
            }
            None => Some("session was already closed".to_string()),
        };
        self.active_turn = None;
        let shutdown_error = match self.client.take() {
            Some(client) => shutdown_client(client).await.err(),
            None => None,
        };
        let mut message = format!(
            "turn {}/{} cancelled with request {}",
            active.thread_id, active.turn_id, request_id
        );
        if let Some(error) = interrupt_error {
            message.push_str("; interrupt error: ");
            message.push_str(&error.to_string());
        }
        if let Some(error) = shutdown_error {
            message.push_str("; shutdown error: ");
            message.push_str(&error.to_string());
        }
        bail!(message)
    }
}

fn native_collaboration_mode(model: &str, mode: ModeKind) -> CollaborationMode {
    CollaborationMode {
        mode,
        settings: Settings {
            model: model.to_string(),
            reasoning_effort: None,
            developer_instructions: None,
        },
    }
}

struct RequestedThread {
    cwd: AbsolutePathBuf,
    runtime_workspace_roots: Vec<AbsolutePathBuf>,
    model: Option<String>,
    model_provider: String,
    developer_instructions: Option<String>,
    ephemeral: bool,
}

impl RequestedThread {
    fn start_params(&self) -> ThreadStartParams {
        ThreadStartParams {
            model: self.model.clone(),
            model_provider: Some(self.model_provider.clone()),
            cwd: Some(self.cwd.to_string_lossy().into_owned()),
            runtime_workspace_roots: Some(self.runtime_workspace_roots.clone()),
            approval_policy: Some(AskForApproval::Never),
            sandbox: Some(SandboxMode::DangerFullAccess),
            permissions: None,
            developer_instructions: self.developer_instructions.clone(),
            ephemeral: Some(self.ephemeral),
            ..ThreadStartParams::default()
        }
    }

    fn resume_params(&self, thread_id: String) -> ThreadResumeParams {
        ThreadResumeParams {
            thread_id,
            model: self.model.clone(),
            model_provider: Some(self.model_provider.clone()),
            cwd: Some(self.cwd.to_string_lossy().into_owned()),
            runtime_workspace_roots: Some(self.runtime_workspace_roots.clone()),
            approval_policy: Some(AskForApproval::Never),
            sandbox: Some(SandboxMode::DangerFullAccess),
            permissions: None,
            developer_instructions: self.developer_instructions.clone(),
            exclude_turns: true,
            ..ThreadResumeParams::default()
        }
    }
}

struct EffectiveThread {
    thread_id: String,
    cwd: AbsolutePathBuf,
    runtime_workspace_roots: Vec<AbsolutePathBuf>,
    model: String,
    model_provider: String,
    approval_policy: AskForApproval,
    sandbox: SandboxPolicy,
}

impl EffectiveThread {
    fn from_start(response: ThreadStartResponse) -> Self {
        Self {
            thread_id: response.thread.id,
            cwd: response.cwd,
            runtime_workspace_roots: response.runtime_workspace_roots,
            model: response.model,
            model_provider: response.model_provider,
            approval_policy: response.approval_policy,
            sandbox: response.sandbox,
        }
    }

    fn from_resume(response: ThreadResumeResponse) -> Self {
        Self {
            thread_id: response.thread.id,
            cwd: response.cwd,
            runtime_workspace_roots: response.runtime_workspace_roots,
            model: response.model,
            model_provider: response.model_provider,
            approval_policy: response.approval_policy,
            sandbox: response.sandbox,
        }
    }

    fn require_exact(&self, requested: &RequestedThread) -> SessionResult<()> {
        if self.thread_id.trim().is_empty() {
            bail!("app-server returned an empty thread id");
        }
        if self.cwd != requested.cwd {
            bail!(
                "app-server selected cwd {} instead of {}",
                self.cwd.to_string_lossy(),
                requested.cwd.to_string_lossy()
            );
        }
        if self.runtime_workspace_roots != requested.runtime_workspace_roots {
            bail!("app-server changed the requested runtime workspace roots");
        }
        if let Some(model) = &requested.model
            && &self.model != model
        {
            bail!(
                "app-server selected model {:?} instead of {:?}",
                self.model,
                model
            );
        }
        if self.model_provider != requested.model_provider {
            bail!(
                "app-server selected provider {:?} instead of {:?}",
                self.model_provider,
                requested.model_provider
            );
        }
        if self.approval_policy != AskForApproval::Never {
            bail!("app-server did not apply the never-approve policy");
        }
        if !matches!(self.sandbox, SandboxPolicy::DangerFullAccess) {
            bail!("app-server did not apply danger-full-access");
        }
        Ok(())
    }
}

async fn request_parent(
    client: &InProcessAppServerClient,
    requested: &RequestedThread,
    parent: ParentThread,
    request_id: RequestId,
) -> SessionResult<EffectiveThread> {
    match parent {
        ParentThread::New => {
            let response: ThreadStartResponse = client
                .request_typed(ClientRequest::ThreadStart {
                    request_id,
                    params: requested.start_params(),
                })
                .await
                .context("thread/start")?;
            Ok(EffectiveThread::from_start(response))
        }
        ParentThread::Resume { thread_id } => {
            if thread_id.trim().is_empty() {
                bail!("resume thread id must not be empty");
            }
            let response: ThreadResumeResponse = client
                .request_typed(ClientRequest::ThreadResume {
                    request_id,
                    params: requested.resume_params(thread_id),
                })
                .await
                .context("thread/resume")?;
            Ok(EffectiveThread::from_resume(response))
        }
    }
}

#[derive(Default)]
struct RequestIdSequence(i64);

impl RequestIdSequence {
    fn next(&mut self) -> RequestId {
        self.0 += 1;
        RequestId::Integer(self.0)
    }
}

enum NextEvent {
    Cancelled,
    Server(Option<InProcessServerEvent>),
}

async fn handle_server_request(
    client: &InProcessAppServerClient,
    request: ServerRequest,
) -> SessionResult<()> {
    if let ServerRequest::McpServerElicitationRequest { request_id, .. } = request {
        let response = serde_json::to_value(McpServerElicitationRequestResponse {
            action: McpServerElicitationAction::Cancel,
            content: None,
            meta: None,
        })
        .context("encode MCP elicitation cancellation")?;
        return client
            .resolve_server_request(request_id, response)
            .await
            .context("resolve mcpServer/elicitation/request");
    }

    let (request_id, method, reason) = rejection(request);
    client
        .reject_server_request(
            request_id,
            JSONRPCErrorError {
                code: -32000,
                message: reason,
                data: None,
            },
        )
        .await
        .with_context(|| format!("reject {method}"))
}

fn rejection(request: ServerRequest) -> (RequestId, &'static str, String) {
    match request {
        ServerRequest::CommandExecutionRequestApproval { request_id, params } => (
            request_id,
            "item/commandExecution/requestApproval",
            format!(
                "command execution approval is not supported in exec mode for thread `{}`",
                params.thread_id
            ),
        ),
        ServerRequest::FileChangeRequestApproval { request_id, params } => (
            request_id,
            "item/fileChange/requestApproval",
            format!(
                "file change approval is not supported in exec mode for thread `{}`",
                params.thread_id
            ),
        ),
        ServerRequest::ToolRequestUserInput { request_id, params } => (
            request_id,
            "item/tool/requestUserInput",
            format!(
                "request_user_input is not supported in exec mode for thread `{}`",
                params.thread_id
            ),
        ),
        ServerRequest::DynamicToolCall { request_id, params } => (
            request_id,
            "item/tool/call",
            format!(
                "dynamic tool calls are not supported in exec mode for thread `{}`",
                params.thread_id
            ),
        ),
        ServerRequest::ChatgptAuthTokensRefresh { request_id, .. } => (
            request_id,
            "account/chatgptAuthTokens/refresh",
            "chatgpt auth token refresh is not supported in exec mode".to_string(),
        ),
        ServerRequest::AttestationGenerate { request_id, .. } => (
            request_id,
            "attestation/generate",
            "attestation generation is not supported in exec mode".to_string(),
        ),
        ServerRequest::CurrentTimeRead { request_id, .. } => (
            request_id,
            "currentTime/read",
            "external current time is not supported in exec mode".to_string(),
        ),
        ServerRequest::ApplyPatchApproval { request_id, params } => (
            request_id,
            "applyPatchApproval",
            format!(
                "apply_patch approval is not supported in exec mode for thread `{}`",
                params.conversation_id
            ),
        ),
        ServerRequest::ExecCommandApproval { request_id, params } => (
            request_id,
            "execCommandApproval",
            format!(
                "exec command approval is not supported in exec mode for thread `{}`",
                params.conversation_id
            ),
        ),
        ServerRequest::PermissionsRequestApproval { request_id, params } => (
            request_id,
            "item/permissions/requestApproval",
            format!(
                "permissions approval is not supported in exec mode for thread `{}`",
                params.thread_id
            ),
        ),
        ServerRequest::McpServerElicitationRequest { .. } => {
            unreachable!("MCP elicitation is resolved before rejection")
        }
    }
}

async fn shutdown_client(client: InProcessAppServerClient) -> SessionResult<()> {
    match tokio::time::timeout(SHUTDOWN_TIMEOUT, client.shutdown()).await {
        Ok(result) => result.context("shut down in-process app-server"),
        Err(_) => bail!(
            "in-process app-server shutdown exceeded {} seconds",
            SHUTDOWN_TIMEOUT.as_secs()
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_default_mode_uses_the_selected_model_and_builtin_instructions() {
        let mode = native_collaboration_mode("glm-5.2", ModeKind::Default);
        assert_eq!(mode.mode, ModeKind::Default);
        assert_eq!(mode.settings.model, "glm-5.2");
        assert_eq!(mode.settings.developer_instructions, None);
    }
}
