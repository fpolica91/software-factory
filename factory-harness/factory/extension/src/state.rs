use std::fmt;
use std::sync::Arc;

use codex_extension_api::ExtensionData;
use tokio::sync::Mutex;

use crate::FactoryBackendError;
use crate::FactoryState;
use crate::FactoryStateBackend;
use crate::FactoryStateDurability;

/// Factory-owned state shared by native contributors for one Codex thread.
pub struct FactoryThreadState {
    thread_id: String,
    backend: Arc<dyn FactoryStateBackend>,
    state: Mutex<FactoryState>,
    load_error: Option<FactoryBackendError>,
}

impl fmt::Debug for FactoryThreadState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FactoryThreadState")
            .field("thread_id", &self.thread_id)
            .field("durability", &self.backend.durability())
            .field("load_error", &self.load_error)
            .finish_non_exhaustive()
    }
}

impl FactoryThreadState {
    /// Returns the Codex thread identity used as the backend key.
    pub fn thread_id(&self) -> &str {
        &self.thread_id
    }

    pub fn durability(&self) -> FactoryStateDurability {
        self.backend.durability()
    }

    pub async fn snapshot(&self) -> Result<FactoryState, FactoryBackendError> {
        self.ensure_loaded()?;
        Ok(self.state.lock().await.clone())
    }

    pub(crate) async fn update(
        &self,
        mutation: impl FnOnce(&mut FactoryState) -> Result<(), String>,
    ) -> Result<FactoryState, FactoryBackendError> {
        self.ensure_loaded()?;
        let mut current = self.state.lock().await;
        let mut next = current.clone();
        mutation(&mut next).map_err(FactoryBackendError::new)?;
        next.revision = next.revision.saturating_add(1);
        self.backend.save(&self.thread_id, next.clone()).await?;
        *current = next.clone();
        Ok(next)
    }

    fn ensure_loaded(&self) -> Result<(), FactoryBackendError> {
        match &self.load_error {
            Some(error) => Err(error.clone()),
            None => Ok(()),
        }
    }
}

pub(crate) async fn initialize_thread_state(
    thread_store: &ExtensionData,
    backend: Arc<dyn FactoryStateBackend>,
) -> Arc<FactoryThreadState> {
    initialize_thread_state_with_key(thread_store, thread_store.level_id(), backend).await
}

pub(crate) async fn initialize_thread_state_with_key(
    thread_store: &ExtensionData,
    durable_thread_key: &str,
    backend: Arc<dyn FactoryStateBackend>,
) -> Arc<FactoryThreadState> {
    if let Some(existing) = thread_store.get::<FactoryThreadState>() {
        return existing;
    }

    let thread_id = durable_thread_key.to_string();
    let (state, load_error) = match backend.load(&thread_id).await {
        Ok(state) => (state.unwrap_or_default(), None),
        Err(error) => (FactoryState::default(), Some(error)),
    };
    thread_store.get_or_init(|| FactoryThreadState {
        thread_id,
        backend,
        state: Mutex::new(state),
        load_error,
    })
}
