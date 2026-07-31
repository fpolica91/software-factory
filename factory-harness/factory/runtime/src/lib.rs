//! Full Codex runtime composition for Software Factory.
//!
//! The process boundary uses Codex app-server's stdio lifecycle. Rust hosts
//! use the same lifecycle through the upstream in-process client. Factory
//! extensions attach through a public app-server extension seam; this
//! crate does not replace the Codex agent loop with a downstream loop.

use std::io::Result as IoResult;
use std::sync::Arc;

use codex_app_server::AppServerExtensionInstaller;
use codex_arg0::Arg0DispatchPaths;
use codex_config::LoaderOverrides;
use codex_utils_cli::CliConfigOverrides;
use serde::Serialize;

pub use factory_protocol::ProtocolManifest;

const CODEX_APP_SERVER_V2_MAJOR: u16 = 2;
const CODEX_APP_SERVER_V2_SCHEMA_SHA256: &str = env!("FACTORY_CODEX_APP_SERVER_V2_SCHEMA_SHA256");

/// Factory Protocol identity compiled into a Factory runtime distribution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FactoryProtocolIdentity {
    pub version: factory_protocol::ProtocolVersion,
    pub schema_sha256: String,
}

/// Codex app-server V2 wire version compiled into a Factory runtime distribution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodexAppServerV2Version {
    pub major: u16,
}

/// Codex app-server V2 identity compiled into a Factory runtime distribution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodexAppServerV2Identity {
    pub version: CodexAppServerV2Version,
    pub schema_sha256: String,
}

/// Complete process-boundary identity served by this Factory runtime build.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeDistributionManifest {
    pub factory_protocol: FactoryProtocolIdentity,
    pub source_codex_revision: String,
    pub codex_app_server_v2: CodexAppServerV2Identity,
}

/// Returns the complete distribution manifest used for active negotiation.
pub fn protocol_manifest() -> RuntimeDistributionManifest {
    let factory_manifest = legacy_protocol_manifest();
    RuntimeDistributionManifest {
        factory_protocol: FactoryProtocolIdentity {
            version: factory_manifest.version,
            schema_sha256: factory_manifest.schema_sha256,
        },
        source_codex_revision: factory_manifest.source_codex_revision,
        codex_app_server_v2: CodexAppServerV2Identity {
            version: CodexAppServerV2Version {
                major: CODEX_APP_SERVER_V2_MAJOR,
            },
            schema_sha256: CODEX_APP_SERVER_V2_SCHEMA_SHA256.to_string(),
        },
    }
}

/// Returns the Factory Protocol V1-only manifest for compatibility consumers.
pub fn legacy_protocol_manifest() -> ProtocolManifest {
    ProtocolManifest::current()
}

/// Upstream typed client surface for Rust hosts embedding the full app-server
/// lifecycle. These types retain upstream semantics and are not a separate
/// Factory protocol.
pub mod in_process {
    use std::io::Result as IoResult;
    use std::sync::Arc;

    pub use codex_app_server_client::DEFAULT_IN_PROCESS_CHANNEL_CAPACITY;
    pub use codex_app_server_client::InProcessAppServerClient;
    pub use codex_app_server_client::InProcessClientStartArgs;
    pub use codex_app_server_client::InProcessServerEvent;

    /// Starts the full in-process Codex lifecycle with Factory's native
    /// extension contributors installed.
    pub async fn start(mut args: InProcessClientStartArgs) -> IoResult<InProcessAppServerClient> {
        let mut config = args.config.as_ref().clone();
        config.analytics_enabled = Some(false);
        args.config = Arc::new(config);
        InProcessAppServerClient::start_with_extension_installers_and_options(
            args,
            super::factory_extension_installers()?,
            codex_app_server::PluginStartupTasks::Skip,
        )
        .await
    }
}

/// Runs the complete Codex app-server lifecycle over its standard stdio
/// transport with unintended external analytics disabled by default.
pub async fn run_stdio(
    arg0_paths: Arg0DispatchPaths,
    cli_config_overrides: CliConfigOverrides,
    loader_overrides: LoaderOverrides,
    strict_config: bool,
) -> IoResult<()> {
    codex_app_server::run_main_with_extension_installers_and_runtime_options(
        arg0_paths,
        cli_config_overrides,
        loader_overrides,
        strict_config,
        /*default_analytics_enabled*/ false,
        codex_app_server::AppServerRuntimeOptions {
            code_mode_host_transport: codex_app_server::CodeModeHostTransport::Local,
            plugin_startup_tasks: codex_app_server::PluginStartupTasks::Skip,
            remote_control_startup_mode:
                codex_app_server::RemoteControlStartupMode::DisabledEphemeral,
            install_shutdown_signal_handler: true,
        },
        factory_extension_installers()?,
    )
    .await
}

fn factory_extension_installers() -> IoResult<Vec<AppServerExtensionInstaller>> {
    let state_backend = optional_env("FACTORYD_URL")?
        .map(factory_extension::FactorydStateBackend::new)
        .transpose()
        .map_err(invalid_input)?
        .map(|backend| Arc::new(backend) as Arc<dyn factory_extension::FactoryStateBackend>);
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
            eprintln!("factory-runtime: long-term memory disabled (FACTORY_QDRANT_URL is not set)");
            None
        }
    };
    Ok(vec![Arc::new(move |registry| {
        match &state_backend {
            Some(backend) => {
                factory_extension::install_with_backend(registry, Arc::clone(backend));
            }
            None => factory_extension::install(registry),
        }
        if let Some(memory) = &memory {
            factory_extension::install_memory(registry, memory.clone());
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
