use std::collections::BTreeMap;
use std::env::current_exe;
use std::path::Path;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use codex_models_manager::model_info::BASE_INSTRUCTIONS;
use codex_protocol::config_types::ReasoningSummary;
use codex_protocol::openai_models::ApplyPatchToolType;
use codex_protocol::openai_models::ConfigShellToolType;
use codex_protocol::openai_models::InputModality;
use codex_protocol::openai_models::ModelInfo;
use codex_protocol::openai_models::ModelVisibility;
use codex_protocol::openai_models::ModelsResponse;
use codex_protocol::openai_models::ReasoningEffort;
use codex_protocol::openai_models::ReasoningEffortPreset;
use codex_protocol::openai_models::TruncationPolicyConfig;
use codex_protocol::openai_models::WebSearchToolType;
use serde_json::Value;
use serde_json::json;
use thiserror::Error;
use tokio::io::AsyncReadExt;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpStream;
use tokio::process::Child;
use tokio::process::Command;
use tokio::time::sleep;
use tokio::time::timeout;

pub const BRIDGE_PACKAGE: &str = "@bitkyc08/opencodex";
pub const BRIDGE_VERSION: &str = "2.8.0";
pub const CODEX_PROVIDER_ID: &str = "factory-provider";
pub const DEFAULT_MODEL: &str = "glm-5.2";
pub const STANDARD_UPSTREAM_BASE_URL: &str = "https://api.z.ai/api/paas/v4/";
pub const CODING_PLAN_UPSTREAM_BASE_URL: &str = "https://api.z.ai/api/coding/paas/v4";
pub const DEFAULT_UPSTREAM_BASE_URL: &str = CODING_PLAN_UPSTREAM_BASE_URL;
pub const DEFAULT_API_KEY_ENV: &str = "ZAI_API_KEY";
pub const DEFAULT_ADMISSION_TOKEN_ENV: &str = "FACTORY_PROVIDER_AUTH_TOKEN";
pub const DEFAULT_CONTEXT_WINDOW: i64 = 1_000_000;
pub const MODEL_CATALOG_FILE: &str = "codex-models.json";
pub const INSTALLED_BRIDGE_RELATIVE_DIR: &str = "../lib/software-factory/provider-bridge";

#[derive(Debug, Clone)]
pub struct BridgeOptions {
    pub port: u16,
    pub bind_host: String,
    pub advertised_base_url: Option<String>,
    pub resource_dir: Option<PathBuf>,
    pub state_dir: PathBuf,
    pub model: String,
    pub upstream_base_url: String,
    pub api_key_env: String,
    pub startup_timeout: Duration,
}

impl BridgeOptions {
    pub fn glm(port: u16, state_dir: impl Into<PathBuf>) -> Self {
        Self {
            port,
            bind_host: "127.0.0.1".to_string(),
            advertised_base_url: None,
            resource_dir: None,
            state_dir: state_dir.into(),
            model: DEFAULT_MODEL.to_string(),
            upstream_base_url: DEFAULT_UPSTREAM_BASE_URL.to_string(),
            api_key_env: DEFAULT_API_KEY_ENV.to_string(),
            startup_timeout: Duration::from_secs(30),
        }
    }

    pub fn standard(port: u16, state_dir: impl Into<PathBuf>) -> Self {
        let mut options = Self::glm(port, state_dir);
        options.upstream_base_url = STANDARD_UPSTREAM_BASE_URL.to_string();
        options
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct CodexProviderSelection {
    pub model: String,
    pub model_provider: String,
    pub model_catalog_json: PathBuf,
    pub config: BTreeMap<String, Value>,
}

impl CodexProviderSelection {
    pub fn for_bridge(
        base_url: impl Into<String>,
        model: impl Into<String>,
        model_catalog_json: impl Into<PathBuf>,
    ) -> Self {
        let base_url = base_url.into().trim_end_matches('/').to_string();
        let model = model.into();
        let model_catalog_json = model_catalog_json.into();
        let mut config = BTreeMap::new();
        config.insert(
            format!("model_providers.{CODEX_PROVIDER_ID}"),
            json!({
                "name": "Software Factory provider bridge",
                "base_url": base_url,
                "wire_api": "responses",
                "requires_openai_auth": false,
                "supports_websockets": false,
                "env_http_headers": {
                    "X-OpenCodex-API-Key": DEFAULT_ADMISSION_TOKEN_ENV
                }
            }),
        );
        config.insert("model_catalog_json".to_string(), json!(model_catalog_json));

        Self {
            model,
            model_provider: CODEX_PROVIDER_ID.to_string(),
            model_catalog_json,
            config,
        }
    }

    pub fn thread_start_fields(&self) -> Value {
        json!({
            "model": self.model,
            "modelProvider": self.model_provider,
            "config": self.config
        })
    }
}

pub fn glm_model_catalog() -> ModelsResponse {
    ModelsResponse {
        models: vec![ModelInfo {
            slug: DEFAULT_MODEL.to_string(),
            display_name: "GLM-5.2".to_string(),
            description: Some(
                "Z.AI GLM-5.2 with the Software Factory Codex tool profile.".to_string(),
            ),
            default_reasoning_level: Some(ReasoningEffort::Medium),
            supported_reasoning_levels: vec![
                reasoning_effort(
                    ReasoningEffort::Low,
                    "Fast responses with lighter reasoning",
                ),
                reasoning_effort(
                    ReasoningEffort::Medium,
                    "Balances speed and reasoning depth for everyday tasks",
                ),
                reasoning_effort(
                    ReasoningEffort::High,
                    "Greater reasoning depth for complex problems",
                ),
                reasoning_effort(
                    ReasoningEffort::XHigh,
                    "Extra high reasoning depth for complex problems",
                ),
                reasoning_effort(
                    ReasoningEffort::Max,
                    "Maximum reasoning depth for the hardest tasks",
                ),
            ],
            shell_type: ConfigShellToolType::ShellCommand,
            visibility: ModelVisibility::List,
            supported_in_api: true,
            priority: 0,
            additional_speed_tiers: Vec::new(),
            service_tiers: Vec::new(),
            default_service_tier: None,
            availability_nux: None,
            upgrade: None,
            base_instructions: BASE_INSTRUCTIONS.to_string(),
            model_messages: None,
            include_skills_usage_instructions: true,
            supports_reasoning_summary_parameter: true,
            default_reasoning_summary: ReasoningSummary::Auto,
            support_verbosity: false,
            default_verbosity: None,
            apply_patch_tool_type: Some(ApplyPatchToolType::Freeform),
            web_search_tool_type: WebSearchToolType::Text,
            truncation_policy: TruncationPolicyConfig::tokens(10_000),
            supports_parallel_tool_calls: true,
            supports_image_detail_original: false,
            context_window: Some(DEFAULT_CONTEXT_WINDOW),
            max_context_window: Some(DEFAULT_CONTEXT_WINDOW),
            auto_compact_token_limit: None,
            comp_hash: None,
            effective_context_window_percent: 95,
            experimental_supported_tools: Vec::new(),
            input_modalities: vec![InputModality::Text],
            used_fallback_model_metadata: false,
            supports_search_tool: false,
            use_responses_lite: false,
            auto_review_model_override: None,
            tool_mode: None,
            multi_agent_version: None,
        }],
    }
}

fn reasoning_effort(effort: ReasoningEffort, description: &str) -> ReasoningEffortPreset {
    ReasoningEffortPreset {
        effort,
        description: description.to_string(),
    }
}

async fn write_model_catalog(state_dir: &Path) -> Result<PathBuf, BridgeError> {
    let catalog_path = state_dir.join(MODEL_CATALOG_FILE);
    let catalog = serde_json::to_vec_pretty(&glm_model_catalog())
        .map_err(BridgeError::SerializeModelCatalog)?;
    tokio::fs::write(&catalog_path, catalog)
        .await
        .map_err(|source| BridgeError::WriteModelCatalog {
            path: catalog_path.clone(),
            source,
        })?;
    Ok(catalog_path)
}

fn resolve_bridge_dir(configured: Option<&Path>) -> Result<PathBuf, BridgeError> {
    if let Some(configured) = configured {
        if bridge_resources_are_installed(configured) {
            return configured
                .canonicalize()
                .map_err(|source| BridgeError::ResolveResourceDir {
                    path: configured.to_path_buf(),
                    source,
                });
        }
        return Err(BridgeError::ResourcesMissing(vec![
            configured.to_path_buf(),
        ]));
    }

    let executable = current_exe().map_err(BridgeError::ResolveExecutable)?;
    let mut candidates = Vec::new();
    if let Some(parent) = executable.parent() {
        candidates.push(parent.join(INSTALLED_BRIDGE_RELATIVE_DIR));
    }
    candidates.push(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("bridge"));
    if let Some(candidate) = candidates
        .iter()
        .find(|candidate| bridge_resources_are_installed(candidate))
    {
        return candidate
            .canonicalize()
            .map_err(|source| BridgeError::ResolveResourceDir {
                path: candidate.clone(),
                source,
            });
    }
    Err(BridgeError::ResourcesMissing(candidates))
}

fn bridge_resources_are_installed(path: &Path) -> bool {
    path.join("node_modules/.bin/bun").is_file() && path.join("src/serve.ts").is_file()
}

#[derive(Debug, Error)]
pub enum BridgeError {
    #[error("failed to resolve the provider bridge installation from the current executable: {0}")]
    ResolveExecutable(#[source] std::io::Error),
    #[error("provider bridge resources are not installed; searched {0:?}")]
    ResourcesMissing(Vec<PathBuf>),
    #[error("failed to resolve provider bridge resource directory {path}: {source}")]
    ResolveResourceDir {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("failed to create provider state directory {path}: {source}")]
    CreateStateDir {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("failed to resolve provider state directory {path}: {source}")]
    ResolveStateDir {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("only model `{DEFAULT_MODEL}` is supported by the Z.AI profile, got `{0}`")]
    UnsupportedModel(String),
    #[error("failed to serialize the Codex model catalog: {0}")]
    SerializeModelCatalog(#[source] serde_json::Error),
    #[error("failed to write Codex model catalog {path}: {source}")]
    WriteModelCatalog {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("failed to start provider bridge: {0}")]
    Spawn(#[source] std::io::Error),
    #[error("provider bridge exited during startup with status {0}")]
    Exited(std::process::ExitStatus),
    #[error("provider bridge did not become ready at {endpoint} within {timeout:?}")]
    StartupTimeout { endpoint: String, timeout: Duration },
    #[error("failed to inspect provider bridge process: {0}")]
    Inspect(#[source] std::io::Error),
    #[error("failed to stop provider bridge: {0}")]
    Shutdown(#[source] std::io::Error),
}

pub struct ProviderBridge {
    child: Child,
    selection: CodexProviderSelection,
    endpoint: String,
    port: u16,
}

impl ProviderBridge {
    pub async fn start(options: BridgeOptions) -> Result<Self, BridgeError> {
        let bridge_dir = resolve_bridge_dir(options.resource_dir.as_deref())?;
        let bun = bridge_dir.join("node_modules/.bin/bun");
        if options.model != DEFAULT_MODEL {
            return Err(BridgeError::UnsupportedModel(options.model));
        }

        tokio::fs::create_dir_all(&options.state_dir)
            .await
            .map_err(|source| BridgeError::CreateStateDir {
                path: options.state_dir.clone(),
                source,
            })?;
        let state_dir = tokio::fs::canonicalize(&options.state_dir)
            .await
            .map_err(|source| BridgeError::ResolveStateDir {
                path: options.state_dir.clone(),
                source,
            })?;
        let model_catalog_json = write_model_catalog(&state_dir).await?;

        let mut command = Command::new(bun);
        command
            .arg("run")
            .arg("src/serve.ts")
            .arg("--port")
            .arg(options.port.to_string())
            .arg("--host")
            .arg(&options.bind_host)
            .arg("--state-dir")
            .arg(&state_dir)
            .current_dir(&bridge_dir)
            .env("FACTORY_ZAI_BASE_URL", &options.upstream_base_url)
            .env("FACTORY_ZAI_API_KEY_ENV", &options.api_key_env)
            .env("OPENCODEX_DEBUG", "0")
            .kill_on_drop(true)
            .stdin(Stdio::null())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit());

        let child = command.spawn().map_err(BridgeError::Spawn)?;
        let endpoint = options
            .advertised_base_url
            .unwrap_or_else(|| format!("http://127.0.0.1:{}/v1", options.port));
        let selection =
            CodexProviderSelection::for_bridge(&endpoint, DEFAULT_MODEL, model_catalog_json);
        let mut bridge = Self {
            child,
            selection,
            endpoint,
            port: options.port,
        };

        let wait_result = timeout(options.startup_timeout, async {
            loop {
                if let Some(status) = bridge.child.try_wait().map_err(BridgeError::Inspect)? {
                    return Err(BridgeError::Exited(status));
                }
                if bridge.health_check().await {
                    return Ok(());
                }
                sleep(Duration::from_millis(100)).await;
            }
        })
        .await;

        match wait_result {
            Ok(result) => result?,
            Err(_) => {
                let _ = bridge.child.start_kill();
                return Err(BridgeError::StartupTimeout {
                    endpoint: bridge.endpoint.clone(),
                    timeout: options.startup_timeout,
                });
            }
        }

        Ok(bridge)
    }

    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    pub fn selection(&self) -> &CodexProviderSelection {
        &self.selection
    }

    pub async fn shutdown(&mut self) -> Result<(), BridgeError> {
        if self
            .child
            .try_wait()
            .map_err(BridgeError::Inspect)?
            .is_some()
        {
            return Ok(());
        }
        self.child.start_kill().map_err(BridgeError::Shutdown)?;
        self.child.wait().await.map_err(BridgeError::Shutdown)?;
        Ok(())
    }

    async fn health_check(&self) -> bool {
        let Ok(mut stream) = TcpStream::connect(("127.0.0.1", self.port)).await else {
            return false;
        };
        let request = b"GET /healthz HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n";
        if stream.write_all(request).await.is_err() {
            return false;
        }
        let mut response = Vec::with_capacity(1024);
        if stream
            .take(16 * 1024)
            .read_to_end(&mut response)
            .await
            .is_err()
        {
            return false;
        }
        response
            .windows(b"\"service\":\"opencodex\"".len())
            .any(|window| window == b"\"service\":\"opencodex\"")
    }
}

impl Drop for ProviderBridge {
    fn drop(&mut self) {
        let _ = self.child.start_kill();
    }
}
