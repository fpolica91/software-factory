use std::error::Error;
use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;

use codex_app_server_client::DEFAULT_IN_PROCESS_CHANNEL_CAPACITY;
use codex_app_server_client::EnvironmentManager;
use codex_app_server_client::InProcessClientStartArgs;
use codex_arg0::Arg0DispatchPaths;
use codex_config::AbsolutePathBuf;
use codex_config::CloudConfigBundleLoader;
use codex_config::LoaderOverrides;
use codex_core::config::ConfigBuilder;
use codex_core::config::ConfigOverrides;
use codex_feedback::CodexFeedback;
use codex_protocol::config_types::SandboxMode;
use codex_protocol::protocol::AskForApproval;
use codex_protocol::protocol::SessionSource;
use factory_providers::CodexProviderSelection;

/// Inputs needed to embed the upstream Codex runtime in a durable worker.
pub struct WorkerBootstrap {
    pub arg0_paths: Arg0DispatchPaths,
    pub workspace_cwd: PathBuf,
    pub codex_home: Option<PathBuf>,
    pub provider: CodexProviderSelection,
}

const CONTAINER_SANDBOX_OVERRIDE: &str = "features.use_legacy_landlock";

#[derive(Debug)]
pub enum BootstrapError {
    WorkspacePath(std::io::Error),
    Config(std::io::Error),
    Environment(Box<dyn Error + Send + Sync>),
    ProviderConfig(String),
}

impl fmt::Display for BootstrapError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::WorkspacePath(source) => write!(formatter, "invalid workspace path: {source}"),
            Self::Config(source) => write!(formatter, "failed to build Codex config: {source}"),
            Self::Environment(source) => {
                write!(
                    formatter,
                    "failed to initialize execution environment: {source}"
                )
            }
            Self::ProviderConfig(source) => {
                write!(formatter, "invalid provider configuration: {source}")
            }
        }
    }
}

impl Error for BootstrapError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::WorkspacePath(source) | Self::Config(source) => Some(source),
            Self::Environment(source) => Some(source.as_ref()),
            Self::ProviderConfig(_) => None,
        }
    }
}

/// Builds the complete upstream in-process startup arguments without starting
/// telemetry or creating a second Factory-specific runtime configuration path.
pub async fn build_worker_start_args(
    input: WorkerBootstrap,
) -> Result<InProcessClientStartArgs, BootstrapError> {
    let workspace_root = AbsolutePathBuf::relative_to_current_dir(&input.workspace_cwd)
        .map_err(BootstrapError::WorkspacePath)?;
    let loader_overrides = LoaderOverrides::default();
    let cloud_config_bundle = CloudConfigBundleLoader::default();
    let model = input.provider.model.clone();
    let model_provider = input.provider.model_provider.clone();
    let mut cli_overrides = input
        .provider
        .config
        .into_iter()
        .map(|(key, value)| {
            toml::Value::try_from(value)
                .map(|value| (key, value))
                .map_err(|source| BootstrapError::ProviderConfig(source.to_string()))
        })
        .collect::<Result<Vec<_>, _>>()?;
    cli_overrides.push((
        CONTAINER_SANDBOX_OVERRIDE.to_string(),
        toml::Value::Boolean(true),
    ));

    let config_overrides = ConfigOverrides {
        model: Some(model),
        model_provider: Some(model_provider),
        cwd: Some(workspace_root.to_path_buf()),
        approval_policy: Some(AskForApproval::Never),
        sandbox_mode: Some(SandboxMode::DangerFullAccess),
        codex_self_exe: input.arg0_paths.codex_self_exe.clone(),
        codex_linux_sandbox_exe: input.arg0_paths.codex_linux_sandbox_exe.clone(),
        main_execve_wrapper_exe: input.arg0_paths.main_execve_wrapper_exe.clone(),
        workspace_roots: Some(vec![workspace_root]),
        ..ConfigOverrides::default()
    };

    let mut config_builder = ConfigBuilder::default()
        .cli_overrides(cli_overrides.clone())
        .harness_overrides(config_overrides)
        .loader_overrides(loader_overrides.clone())
        .strict_config(false)
        .cloud_config_bundle(cloud_config_bundle.clone());
    if let Some(codex_home) = input.codex_home {
        config_builder = config_builder.codex_home(codex_home);
    }

    let mut config = config_builder
        .build()
        .await
        .map_err(BootstrapError::Config)?;
    config.analytics_enabled = Some(false);

    // The production executor replaces this inert remote-only manager with the
    // freshly ensured per-job URL before starting every Codex session. Keeping
    // the bootstrap manager remote-only makes an accidental local/static
    // fallback impossible.
    let environment_manager = EnvironmentManager::remote_only(
        "factory-unprovisioned".to_string(),
        "ws://127.0.0.1:9".to_string(),
        None,
        config.http_client_factory(),
    )
    .map_err(|source| BootstrapError::Environment(Box::new(source)))?;
    let state_db = codex_core::init_state_db(&config).await;

    Ok(InProcessClientStartArgs {
        arg0_paths: input.arg0_paths,
        config: Arc::new(config),
        cli_overrides,
        loader_overrides,
        strict_config: false,
        cloud_config_bundle,
        feedback: CodexFeedback::new(),
        log_db: None,
        state_db,
        environment_manager: Arc::new(environment_manager),
        config_warnings: Vec::new(),
        session_source: SessionSource::Exec,
        enable_codex_api_key_env: true,
        client_name: "factory_worker".to_string(),
        client_version: env!("CARGO_PKG_VERSION").to_string(),
        experimental_api: true,
        mcp_server_openai_form_elicitation: false,
        opt_out_notification_methods: Vec::new(),
        channel_capacity: DEFAULT_IN_PROCESS_CHANNEL_CAPACITY,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn configured_remote_manager_has_no_local_fallback() {
        let manager = EnvironmentManager::create_for_tests(
            Some("ws://127.0.0.1:9".to_string()),
            /*local_runtime_paths*/ None,
        )
        .await;

        assert_eq!(manager.default_environment_id(), Some("remote"));
        assert!(manager.try_local_environment().is_none());
        assert!(manager.default_environment().unwrap().is_remote());
    }
}
