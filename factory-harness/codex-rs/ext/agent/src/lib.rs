use codex_core::CodexThread;
use codex_core::NewThread;
use codex_core::StartThreadOptions;
use codex_core::ThreadManager;
use codex_core::config::Config;
use codex_extension_api::ExtensionDataInit;
use codex_protocol::ThreadId;
use codex_protocol::error::CodexErr;
use codex_protocol::error::Result as CodexResult;
use codex_protocol::protocol::SessionSource;
use codex_protocol::protocol::ThreadSource;
use codex_protocol::protocol::W3cTraceContext;
use codex_protocol::user_input::UserInput;
use std::sync::Arc;
use std::sync::Weak;

/// A fully resolved agent invocation.
///
/// Agent discovery owns rendering `prompt`, including any selected skill
/// references. The runtime starts that prompt with the requested history policy.
pub struct AgentInvocation {
    pub config: Config,
    pub prompt: String,
    pub parent_trace: Option<W3cTraceContext>,
}

/// A spawned agent whose initial turn has been submitted.
pub struct AgentRun {
    pub thread_id: ThreadId,
    pub turn_id: String,
    pub thread: Arc<CodexThread>,
}

/// Selects whether a spawned agent inherits the parent conversation.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum AgentHistoryPolicy {
    /// Preserve the existing subagent behavior by forking the parent history.
    #[default]
    ForkParent,
    /// Start with an empty model conversation while retaining host metadata.
    Fresh,
}

/// Optional host metadata for a spawned agent.
///
/// [`AgentRunner::start`] preserves the upstream defaults. Hosts that need an
/// explicit source or typed extension attachments can use
/// [`AgentRunner::start_with_options`].
#[derive(Default)]
pub struct AgentStartOptions {
    pub history_policy: AgentHistoryPolicy,
    pub session_source: Option<SessionSource>,
    pub thread_source: Option<ThreadSource>,
    pub thread_extension_init: ExtensionDataInit,
}

/// Runs resolved agents in threads owned by the provided [`ThreadManager`].
#[derive(Clone)]
pub struct AgentRunner {
    thread_manager: Weak<ThreadManager>,
}

impl AgentRunner {
    pub fn new(thread_manager: Weak<ThreadManager>) -> Self {
        Self { thread_manager }
    }

    /// Starts a resolved agent in a fork of `parent_thread_id`.
    pub async fn start(
        &self,
        parent_thread_id: ThreadId,
        invocation: AgentInvocation,
    ) -> CodexResult<AgentRun> {
        self.start_with_options(parent_thread_id, invocation, AgentStartOptions::default())
            .await
    }

    /// Starts a resolved agent with explicit host-owned thread metadata.
    pub async fn start_with_options(
        &self,
        parent_thread_id: ThreadId,
        invocation: AgentInvocation,
        options: AgentStartOptions,
    ) -> CodexResult<AgentRun> {
        let AgentInvocation {
            config,
            prompt,
            parent_trace,
        } = invocation;
        if prompt.trim().is_empty() {
            return Err(CodexErr::InvalidRequest(
                "agent prompt must not be empty".to_string(),
            ));
        }

        let thread_manager = self
            .thread_manager
            .upgrade()
            .ok_or_else(|| CodexErr::UnsupportedOperation("thread manager dropped".to_string()))?;
        let start_options = StartThreadOptions {
            parent_trace: parent_trace.clone(),
            session_source: options.session_source,
            thread_source: options.thread_source,
            thread_extension_init: options.thread_extension_init,
            ..StartThreadOptions::new(config)
        };
        let NewThread {
            thread_id, thread, ..
        } = match options.history_policy {
            AgentHistoryPolicy::ForkParent => {
                thread_manager
                    .spawn_subagent(parent_thread_id, start_options)
                    .await?
            }
            AgentHistoryPolicy::Fresh => {
                thread_manager.get_thread(parent_thread_id).await?;
                thread_manager.start_thread(start_options).await?
            }
        };
        let turn_id = thread
            .submit_with_trace(
                vec![UserInput::Text {
                    text: prompt,
                    text_elements: Vec::new(),
                }]
                .into(),
                parent_trace,
            )
            .await?;

        Ok(AgentRun {
            thread_id,
            turn_id,
            thread,
        })
    }
}
