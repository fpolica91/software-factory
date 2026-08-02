use std::error::Error;
use std::fmt;
use std::future::Future;
use std::pin::Pin;

use serde_json::Value;

use crate::FactoryState;

/// Whether a backend survives only this process or an external process crash.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FactoryStateDurability {
    ProcessMemory,
    Durable,
}

/// Cursor into the host's append-only event history.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FactoryEventReference {
    pub sequence: u64,
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

/// Load/save boundary supplied by the durable Factory host.
pub trait FactoryStateBackend: Send + Sync {
    fn load<'a>(&'a self, thread_id: &'a str) -> FactoryBackendFuture<'a, Option<FactoryState>>;

    fn save<'a>(&'a self, thread_id: &'a str, state: FactoryState) -> FactoryBackendFuture<'a, ()>;

    /// Archives detailed history before its current-state projection is
    /// bounded. `None` means no separate event substrate is available, so the
    /// caller must retain the full detail in state.
    fn append_event<'a>(
        &'a self,
        _kind: &'a str,
        _payload: Value,
        _deduplication_key: &'a str,
    ) -> FactoryBackendFuture<'a, Option<FactoryEventReference>> {
        Box::pin(async { Ok(None) })
    }

    fn durability(&self) -> FactoryStateDurability;
}
