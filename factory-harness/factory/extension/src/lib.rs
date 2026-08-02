//! Native Codex extension composition for Software Factory.
//!
//! This crate owns Factory contributors while Codex remains the execution
//! kernel. It does not implement an agent loop.

mod backend;
mod context;
mod factoryd;
mod limits;
mod memory;
mod model;
mod stage;
mod state;
mod state_document;
mod subagents;
mod tools;

use std::sync::Arc;

use codex_extension_api::DetachedReviewThreadContext;
use codex_extension_api::ExtensionData;
use codex_extension_api::ExtensionFuture;
use codex_extension_api::ExtensionRegistryBuilder;
use codex_extension_api::ThreadLifecycleContributor;
use codex_extension_api::ThreadResumeInput;
use codex_extension_api::ThreadStartInput;
use codex_protocol::protocol::SessionSource;
use codex_protocol::protocol::SubAgentSource;
use stage::FactoryThreadScope;

pub use backend::FactoryBackendError;
pub use backend::FactoryBackendFuture;
pub use backend::FactoryEventReference;
pub use backend::FactoryStateBackend;
pub use backend::FactoryStateDurability;
pub use factoryd::FactoryStateFence;
pub use factoryd::FactorydStateBackend;
pub use memory::DenseDistance;
pub use memory::FactoryMemory;
pub use memory::FactoryMemoryError;
pub use memory::FactoryMemoryFuture;
pub use memory::FactoryMemoryHit;
pub use memory::FactoryMemoryRecord;
pub use memory::FactoryMemoryScope;
pub use memory::FactoryMemoryStore;
pub use memory::FactoryRepositoryId;
pub use memory::LexicalSparseVectorizer;
pub use memory::MemoryVector;
pub use memory::MemoryVectorKind;
pub use memory::MemoryVectorizer;
pub use memory::QdrantMemoryConfig;
pub use model::FactoryFindingSeverity;
pub use model::FactoryProgressStatus;
pub use model::FactoryRemediationDisposition;
pub use model::FactoryRemediationRecord;
pub use model::FactoryReviewCycle;
pub use model::FactoryReviewFinding;
pub use model::FactoryReviewRecoveryBaseline;
pub use model::FactoryReviewReport;
pub use model::FactoryReviewVerdict;
pub use model::FactoryState;
pub use model::FactorySubagentActivity;
pub use model::FactorySubagentHistory;
pub use model::FactorySubagentHistorySource;
pub use model::FactorySubagentState;
pub use model::FactorySubagentStatus;
pub use model::FactorySubagentTool;
pub use model::FactorySubagentToolCallStatus;
pub use model::FactoryWorkUnit;
pub use stage::FACTORY_STAGE_METADATA_KEY;
pub use stage::FactoryTurnStage;
pub use state::FactoryThreadState;
pub use state_document::FactoryStateDocument;

#[derive(Clone)]
struct FactoryExtension {
    backend: Arc<dyn FactoryStateBackend>,
    states: Arc<state::FactoryStateRegistry>,
    stage: FactoryTurnStage,
}

impl<C: Sync> ThreadLifecycleContributor<C> for FactoryExtension {
    fn on_thread_start<'a>(&'a self, input: ThreadStartInput<'a, C>) -> ExtensionFuture<'a, ()> {
        Box::pin(async move {
            let scope = if input
                .thread_store
                .get::<DetachedReviewThreadContext>()
                .is_some()
                || matches!(
                    input.session_source,
                    SessionSource::SubAgent(SubAgentSource::Review)
                ) {
                FactoryThreadScope::DetachedReview
            } else if matches!(input.session_source, SessionSource::SubAgent(_)) {
                FactoryThreadScope::Subagent
            } else {
                FactoryThreadScope::Parent
            };
            input.thread_store.insert(scope);
            let durable_thread_key = durable_thread_key_for_start(&input);
            let shared = self
                .states
                .get_or_create(&durable_thread_key, Arc::clone(&self.backend))
                .await;
            state::attach_thread_state(input.thread_store, shared);
        })
    }

    fn on_thread_resume<'a>(&'a self, input: ThreadResumeInput<'a>) -> ExtensionFuture<'a, ()> {
        Box::pin(async move {
            if input.thread_store.get::<FactoryThreadScope>().is_none() {
                let scope = if input
                    .thread_store
                    .get::<DetachedReviewThreadContext>()
                    .is_some()
                {
                    FactoryThreadScope::DetachedReview
                } else {
                    FactoryThreadScope::Parent
                };
                input.thread_store.insert(scope);
            }
            let shared = self
                .states
                .get_or_create(input.thread_store.level_id(), Arc::clone(&self.backend))
                .await;
            state::attach_thread_state(input.thread_store, shared);
        })
    }
}

fn durable_thread_key_for_start<C>(input: &ThreadStartInput<'_, C>) -> String {
    if let Some(context) = input.thread_store.get::<DetachedReviewThreadContext>() {
        context.durable_state_key.clone()
    } else if matches!(
        input.session_source,
        SessionSource::SubAgent(SubAgentSource::Review)
    ) {
        input.session_store.level_id().to_string()
    } else {
        input.thread_store.level_id().to_string()
    }
}

/// Installs Factory contributors using a host-provided thread-keyed backend.
pub fn install_with_backend<C: Sync + 'static>(
    registry: &mut ExtensionRegistryBuilder<C>,
    backend: Arc<dyn FactoryStateBackend>,
    stage: FactoryTurnStage,
) {
    let extension = Arc::new(FactoryExtension {
        backend,
        states: Arc::new(state::FactoryStateRegistry::default()),
        stage,
    });
    registry.thread_lifecycle_contributor(extension.clone());
    registry.prompt_contributor(extension.clone());
    registry.turn_item_contributor(extension.clone());
    registry.tool_contributor(extension);
}

/// Installs optional Qdrant-backed long-term memory contributors.
pub fn install_memory<C: Sync + 'static>(
    registry: &mut ExtensionRegistryBuilder<C>,
    memory: FactoryMemory,
    repository_id: FactoryRepositoryId,
    stage: FactoryTurnStage,
) {
    memory::install_memory(registry, memory, repository_id, stage);
}

/// Returns Factory-owned state after native thread initialization.
pub fn thread_state(thread_store: &ExtensionData) -> Option<Arc<FactoryThreadState>> {
    state::attached_thread_state(thread_store)
}
