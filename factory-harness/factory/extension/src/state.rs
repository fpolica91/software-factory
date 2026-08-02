use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;

use codex_extension_api::ExtensionData;
use tokio::sync::Mutex;

use crate::FactoryBackendError;
use crate::FactoryEventReference;
use crate::FactoryState;
use crate::FactoryStateBackend;
use crate::FactoryStateDurability;

/// Factory-owned state shared by native contributors for one Codex thread.
pub struct FactoryThreadState {
    thread_id: String,
    backend: Arc<dyn FactoryStateBackend>,
    state: Mutex<Option<FactoryState>>,
}

struct FactoryThreadStateAttachment(Arc<FactoryThreadState>);

#[derive(Default)]
pub(crate) struct FactoryStateRegistry {
    states: Mutex<HashMap<String, Arc<FactoryThreadState>>>,
}

impl FactoryStateRegistry {
    pub(crate) async fn get_or_create(
        &self,
        durable_thread_key: &str,
        backend: Arc<dyn FactoryStateBackend>,
    ) -> Arc<FactoryThreadState> {
        let thread_id = durable_thread_key.to_string();
        let candidate = Arc::new(FactoryThreadState {
            thread_id: thread_id.clone(),
            backend,
            state: Mutex::new(None),
        });
        Arc::clone(
            self.states
                .lock()
                .await
                .entry(thread_id)
                .or_insert(candidate),
        )
    }
}

impl fmt::Debug for FactoryThreadState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FactoryThreadState")
            .field("thread_id", &self.thread_id)
            .field("durability", &self.backend.durability())
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
        let mut current = self.state.lock().await;
        self.load_if_needed(&mut current).await?;
        Ok(current.as_ref().expect("Factory state is loaded").clone())
    }

    pub(crate) async fn update(
        &self,
        mutation: impl FnOnce(&mut FactoryState) -> Result<(), String>,
    ) -> Result<FactoryState, FactoryBackendError> {
        let mut current = self.state.lock().await;
        self.load_if_needed(&mut current).await?;
        let mut next = current.as_ref().expect("Factory state is loaded").clone();
        mutation(&mut next).map_err(FactoryBackendError::new)?;
        next.revision = next.revision.saturating_add(1);
        self.backend.save(&self.thread_id, next.clone()).await?;
        *current = Some(next.clone());
        Ok(next)
    }

    pub(crate) async fn append_event(
        &self,
        kind: &str,
        payload: serde_json::Value,
        deduplication_key: &str,
    ) -> Result<Option<FactoryEventReference>, FactoryBackendError> {
        self.backend
            .append_event(kind, payload, deduplication_key)
            .await
    }

    async fn load_if_needed(
        &self,
        current: &mut Option<FactoryState>,
    ) -> Result<(), FactoryBackendError> {
        if current.is_none() {
            *current = Some(
                self.backend
                    .load(&self.thread_id)
                    .await?
                    .unwrap_or_default(),
            );
        }
        Ok(())
    }
}

pub(crate) fn attach_thread_state(
    thread_store: &ExtensionData,
    state: Arc<FactoryThreadState>,
) -> Arc<FactoryThreadState> {
    let attachment = thread_store.get_or_init(|| FactoryThreadStateAttachment(Arc::clone(&state)));
    Arc::clone(&attachment.0)
}

pub(crate) fn attached_thread_state(
    thread_store: &ExtensionData,
) -> Option<Arc<FactoryThreadState>> {
    thread_store
        .get::<FactoryThreadStateAttachment>()
        .map(|attachment| Arc::clone(&attachment.0))
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex as StdMutex;
    use std::sync::atomic::AtomicUsize;
    use std::sync::atomic::Ordering;

    use super::*;
    use crate::FactoryBackendFuture;

    #[derive(Default)]
    struct RecordingBackend {
        state: StdMutex<Option<FactoryState>>,
        appended_events: AtomicUsize,
    }

    impl FactoryStateBackend for RecordingBackend {
        fn load<'a>(
            &'a self,
            _thread_id: &'a str,
        ) -> FactoryBackendFuture<'a, Option<FactoryState>> {
            Box::pin(async move {
                self.state
                    .lock()
                    .map(|state| state.clone())
                    .map_err(|_| FactoryBackendError::new("test state lock failed"))
            })
        }

        fn save<'a>(
            &'a self,
            _thread_id: &'a str,
            state: FactoryState,
        ) -> FactoryBackendFuture<'a, ()> {
            Box::pin(async move {
                *self
                    .state
                    .lock()
                    .map_err(|_| FactoryBackendError::new("test state lock failed"))? = Some(state);
                Ok(())
            })
        }

        fn append_event<'a>(
            &'a self,
            _kind: &'a str,
            _payload: serde_json::Value,
            _deduplication_key: &'a str,
        ) -> FactoryBackendFuture<'a, Option<FactoryEventReference>> {
            Box::pin(async move {
                let sequence = self.appended_events.fetch_add(1, Ordering::SeqCst) + 1;
                Ok(Some(FactoryEventReference {
                    sequence: sequence as u64,
                }))
            })
        }

        fn durability(&self) -> FactoryStateDurability {
            FactoryStateDurability::Durable
        }
    }

    #[tokio::test]
    async fn unrelated_state_updates_append_no_subagent_events() {
        let backend = Arc::new(RecordingBackend::default());
        let thread_state = FactoryThreadState {
            thread_id: "thread-1".to_string(),
            backend: backend.clone(),
            state: Mutex::new(None),
        };

        thread_state.update(|_| Ok(())).await.unwrap();

        assert_eq!(backend.appended_events.load(Ordering::SeqCst), 0);
        assert_eq!(backend.state.lock().unwrap().as_ref().unwrap().revision, 1);
    }
}
