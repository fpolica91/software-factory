//! Full Codex runtime composition for Software Factory.
//!
//! The durable worker uses Codex app-server through the upstream in-process
//! client. Factory extensions attach through a public app-server extension
//! seam; this crate does not replace the Codex agent loop with a downstream
//! loop.

use std::io::Result as IoResult;
use std::sync::Arc;

use codex_app_server::AppServerExtensionInstaller;

pub mod bootstrap;
pub mod checkpoint;
pub mod events;
pub mod execution_environment;
pub mod executor;
pub mod kubernetes_execution_environment;
mod kubernetes_pod;
pub mod session;
pub mod stages;

pub use kubernetes_pod::KubernetesExecutionEnvironmentConfig;
pub use kubernetes_pod::KubernetesResourceConfig;

/// Upstream typed client surface for Rust hosts embedding the full app-server
/// lifecycle. These types retain upstream semantics without a parallel
/// Factory transport.
pub mod in_process {
    use std::io::Result as IoResult;
    use std::sync::Arc;

    pub use codex_app_server_client::DEFAULT_IN_PROCESS_CHANNEL_CAPACITY;
    pub use codex_app_server_client::InProcessAppServerClient;
    pub use codex_app_server_client::InProcessClientStartArgs;
    pub use codex_app_server_client::InProcessServerEvent;

    /// Starts the full in-process Codex lifecycle with a host-provided Factory
    /// state backend. Durable hosts must construct and fence that backend.
    pub async fn start_with_backend(
        args: InProcessClientStartArgs,
        backend: Arc<dyn factory_extension::FactoryStateBackend>,
        repository_id: factory_extension::FactoryRepositoryId,
        stage: factory_extension::FactoryTurnStage,
    ) -> IoResult<InProcessAppServerClient> {
        let mut args = args;
        let mut config = args.config.as_ref().clone();
        config.analytics_enabled = Some(false);
        args.config = Arc::new(config);
        InProcessAppServerClient::start_with_extension_installers_and_options(
            args,
            super::factory_extension_installers(backend, repository_id, stage)?,
            codex_app_server::PluginStartupTasks::Skip,
        )
        .await
    }
}

fn factory_extension_installers(
    state_backend: Arc<dyn factory_extension::FactoryStateBackend>,
    repository_id: factory_extension::FactoryRepositoryId,
    stage: factory_extension::FactoryTurnStage,
) -> IoResult<Vec<AppServerExtensionInstaller>> {
    let memory = match optional_env("FACTORY_QDRANT_URL")? {
        Some(url) => Some(
            factory_extension::FactoryMemory::qdrant(factory_extension::QdrantMemoryConfig {
                url,
                api_key: optional_env("FACTORY_QDRANT_API_KEY")?.filter(|value| !value.is_empty()),
                collection: optional_env("FACTORY_QDRANT_COLLECTION")?
                    .unwrap_or_else(|| "factory_memories".to_string()),
                namespace: optional_env("FACTORY_MEMORY_NAMESPACE")?
                    .unwrap_or_else(|| "default".to_string()),
            })
            .map_err(invalid_input)?,
        ),
        None => {
            eprintln!("factory-worker: long-term memory disabled (FACTORY_QDRANT_URL is not set)");
            None
        }
    };
    Ok(vec![Arc::new(move |registry| {
        factory_extension::install_with_backend(registry, Arc::clone(&state_backend), stage);
        if let Some(memory) = &memory {
            factory_extension::install_memory(
                registry,
                memory.clone(),
                repository_id.clone(),
                stage,
            );
        }
    })])
}

fn optional_env(name: &str) -> IoResult<Option<String>> {
    match std::env::var(name) {
        Ok(value) => Ok(Some(value)),
        Err(std::env::VarError::NotPresent) => Ok(None),
        Err(error) => Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, error)),
    }
}

fn invalid_input(error: impl std::error::Error + Send + Sync + 'static) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidInput, error)
}
