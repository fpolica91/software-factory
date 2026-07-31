//! Native Codex extension composition for Software Factory.
//!
//! This crate owns Factory contributors while Codex remains the execution
//! kernel. It does not implement an agent loop.

mod backend;
mod context;
mod factoryd;
mod memory;
mod model;
mod state;
mod subagents;
mod tools;

use std::sync::Arc;

use codex_extension_api::ExtensionData;
use codex_extension_api::ExtensionFuture;
use codex_extension_api::ExtensionRegistryBuilder;
use codex_extension_api::ThreadLifecycleContributor;
use codex_extension_api::ThreadResumeInput;
use codex_extension_api::ThreadStartInput;
use codex_protocol::protocol::SessionSource;
use codex_protocol::protocol::SubAgentSource;

pub use backend::FactoryBackendError;
pub use backend::FactoryBackendFuture;
pub use backend::FactoryStateBackend;
pub use backend::FactoryStateDurability;
pub use backend::InMemoryFactoryStateBackend;
pub use factoryd::FactorydStateBackend;
pub use memory::DenseDistance;
pub use memory::FactoryMemory;
pub use memory::FactoryMemoryError;
pub use memory::FactoryMemoryFuture;
pub use memory::FactoryMemoryHit;
pub use memory::FactoryMemoryRecord;
pub use memory::FactoryMemoryStore;
pub use memory::LexicalSparseVectorizer;
pub use memory::MemoryVector;
pub use memory::MemoryVectorKind;
pub use memory::MemoryVectorizer;
pub use memory::QdrantMemoryConfig;
pub use model::FactoryFindingSeverity;
pub use model::FactoryProgressStatus;
pub use model::FactoryRemediationDisposition;
pub use model::FactoryRemediationRecord;
pub use model::FactoryReviewFinding;
pub use model::FactoryReviewReport;
pub use model::FactoryReviewVerdict;
pub use model::FactoryState;
pub use model::FactorySubagentActivity;
pub use model::FactorySubagentState;
pub use model::FactorySubagentStatus;
pub use model::FactorySubagentTool;
pub use model::FactorySubagentToolCallStatus;
pub use model::FactoryWorkUnit;
pub use state::FactoryThreadState;

#[derive(Clone)]
struct FactoryExtension {
    backend: Arc<dyn FactoryStateBackend>,
}

impl<C: Sync> ThreadLifecycleContributor<C> for FactoryExtension {
    fn on_thread_start<'a>(&'a self, input: ThreadStartInput<'a, C>) -> ExtensionFuture<'a, ()> {
        Box::pin(async move {
            let durable_thread_key = durable_thread_key_for_start(&input);
            state::initialize_thread_state_with_key(
                input.thread_store,
                durable_thread_key,
                Arc::clone(&self.backend),
            )
            .await;
        })
    }

    fn on_thread_resume<'a>(&'a self, input: ThreadResumeInput<'a>) -> ExtensionFuture<'a, ()> {
        Box::pin(async move {
            state::initialize_thread_state(input.thread_store, Arc::clone(&self.backend)).await;
        })
    }
}

fn durable_thread_key_for_start<'a, C>(input: &ThreadStartInput<'a, C>) -> &'a str {
    if matches!(
        input.session_source,
        SessionSource::SubAgent(SubAgentSource::Review)
    ) {
        input.session_store.level_id()
    } else {
        input.thread_store.level_id()
    }
}

/// Installs Factory contributors with the process-memory backend.
///
/// State survives thread recreation inside one runtime process, but not a
/// process crash. `factoryd` supplies a durable backend through
/// [`install_with_backend`] in a later slice.
pub fn install<C: Sync + 'static>(registry: &mut ExtensionRegistryBuilder<C>) {
    install_with_backend(registry, Arc::new(InMemoryFactoryStateBackend::default()));
}

/// Installs Factory contributors using a host-provided thread-keyed backend.
pub fn install_with_backend<C: Sync + 'static>(
    registry: &mut ExtensionRegistryBuilder<C>,
    backend: Arc<dyn FactoryStateBackend>,
) {
    let extension = Arc::new(FactoryExtension { backend });
    registry.thread_lifecycle_contributor(extension.clone());
    registry.prompt_contributor(extension.clone());
    registry.turn_item_contributor(extension.clone());
    registry.tool_contributor(extension);
}

/// Installs optional Qdrant-backed long-term memory contributors.
pub fn install_memory<C: Sync + 'static>(
    registry: &mut ExtensionRegistryBuilder<C>,
    memory: FactoryMemory,
) {
    memory::install_memory(registry, memory);
}

/// Returns Factory-owned state after native thread initialization.
pub fn thread_state(thread_store: &ExtensionData) -> Option<Arc<FactoryThreadState>> {
    thread_store.get::<FactoryThreadState>()
}
