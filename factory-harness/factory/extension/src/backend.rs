use std::collections::HashMap;
use std::error::Error;
use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::RwLock;

use crate::FactoryState;

/// Whether a backend survives only this process or an external process crash.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FactoryStateDurability {
    ProcessMemory,
    Durable,
}

/// Backend failure surfaced to Factory tools and model context.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FactoryBackendError {
    message: String,
}

impl FactoryBackendError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for FactoryBackendError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl Error for FactoryBackendError {}

pub type FactoryBackendFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, FactoryBackendError>> + Send + 'a>>;

/// Load/save boundary implemented by process-memory and factoryd backends.
pub trait FactoryStateBackend: Send + Sync {
    fn load<'a>(&'a self, thread_id: &'a str) -> FactoryBackendFuture<'a, Option<FactoryState>>;

    fn save<'a>(&'a self, thread_id: &'a str, state: FactoryState) -> FactoryBackendFuture<'a, ()>;

    fn durability(&self) -> FactoryStateDurability;
}

/// Process-local backend used until `factoryd` supplies crash durability.
#[derive(Debug, Default)]
pub struct InMemoryFactoryStateBackend {
    states: RwLock<HashMap<String, FactoryState>>,
}

impl FactoryStateBackend for InMemoryFactoryStateBackend {
    fn load<'a>(&'a self, thread_id: &'a str) -> FactoryBackendFuture<'a, Option<FactoryState>> {
        Box::pin(async move {
            self.states
                .read()
                .map(|states| states.get(thread_id).cloned())
                .map_err(|_| FactoryBackendError::new("Factory state backend lock failed"))
        })
    }

    fn save<'a>(&'a self, thread_id: &'a str, state: FactoryState) -> FactoryBackendFuture<'a, ()> {
        Box::pin(async move {
            self.states
                .write()
                .map_err(|_| FactoryBackendError::new("Factory state backend lock failed"))?
                .insert(thread_id.to_string(), state);
            Ok(())
        })
    }

    fn durability(&self) -> FactoryStateDurability {
        FactoryStateDurability::ProcessMemory
    }
}
