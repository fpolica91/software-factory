use std::fmt;
use std::path::PathBuf;

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
use thiserror::Error;

use crate::profiles::AdapterKind;
use crate::profiles::ProviderProfile;
use crate::profiles::provider_profile;

pub const MODEL_CATALOG_FILE: &str = "codex-models.json";

#[derive(Clone)]
pub struct AdapterConfig {
    pub bind_host: String,
    pub port: u16,
    pub advertised_base_url: String,
    pub profile: &'static ProviderProfile,
    pub model: String,
    pub upstream_base_url: String,
    pub api_key_env: String,
    pub api_key: String,
    pub state_dir: PathBuf,
    pub max_tokens: u32,
}

impl fmt::Debug for AdapterConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AdapterConfig")
            .field("bind_host", &self.bind_host)
            .field("port", &self.port)
            .field("advertised_base_url", &self.advertised_base_url)
            .field("profile", &self.profile.id)
            .field("model", &self.model)
            .field("upstream_base_url", &self.upstream_base_url)
            .field("api_key_env", &self.api_key_env)
            .field("state_dir", &self.state_dir)
            .field("max_tokens", &self.max_tokens)
            .finish_non_exhaustive()
    }
}

impl AdapterConfig {
    pub fn from_env() -> Result<Self, ConfigError> {
        let configured_id =
            std::env::var("FACTORY_PROVIDER_UPSTREAM_ID").unwrap_or_else(|_| "zai".to_string());
        let profile = provider_profile(&configured_id)
            .ok_or_else(|| ConfigError::UnknownProvider(configured_id.clone()))?;
        if profile.adapter_kind == AdapterKind::DirectResponses {
            return Err(ConfigError::DirectProvider(profile.id.to_string()));
        }

        let bind_host =
            std::env::var("FACTORY_PROVIDER_BIND_HOST").unwrap_or_else(|_| "127.0.0.1".to_string());
        let port = env_parse("FACTORY_PROVIDER_PORT", 10101_u16)?;
        let advertised_host = if bind_host == "0.0.0.0" || bind_host == "::" {
            "127.0.0.1"
        } else {
            bind_host.as_str()
        };
        let advertised_base_url = std::env::var("FACTORY_PROVIDER_ADVERTISED_URL")
            .unwrap_or_else(|_| format!("http://{advertised_host}:{port}/v1"));
        let model = std::env::var("FACTORY_PROVIDER_UPSTREAM_MODEL")
            .unwrap_or_else(|_| profile.default_model.to_string());
        if model.trim().is_empty() {
            return Err(ConfigError::Invalid("model must not be empty"));
        }
        let upstream_base_url = std::env::var("FACTORY_PROVIDER_UPSTREAM_BASE_URL")
            .unwrap_or_else(|_| profile.base_urls[0].url.to_string())
            .trim_end_matches('/')
            .to_string();
        let api_key_env = std::env::var("FACTORY_PROVIDER_UPSTREAM_API_KEY_ENV")
            .unwrap_or_else(|_| profile.api_key_env.to_string());
        let api_key = std::env::var(&api_key_env)
            .map_err(|_| ConfigError::MissingApiKey(api_key_env.clone()))?;
        let state_dir = std::env::var_os("FACTORY_PROVIDER_STATE_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| std::env::temp_dir().join("software-factory/provider"));
        let max_tokens = env_parse("FACTORY_PROVIDER_MAX_TOKENS", 65_536_u32)?;

        Ok(Self {
            bind_host,
            port,
            advertised_base_url,
            profile,
            model,
            upstream_base_url,
            api_key_env,
            api_key,
            state_dir,
            max_tokens,
        })
    }
}

fn env_parse<T>(name: &'static str, default: T) -> Result<T, ConfigError>
where
    T: std::str::FromStr,
{
    match std::env::var(name) {
        Ok(value) => value.parse().map_err(|_| ConfigError::InvalidEnv(name)),
        Err(_) => Ok(default),
    }
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("unknown provider profile {0}; expected openai, anthropic, deepseek, or zai")]
    UnknownProvider(String),
    #[error("{0} uses the Responses API directly and does not need the Factory adapter")]
    DirectProvider(String),
    #[error("{0} is required to start the provider adapter")]
    MissingApiKey(String),
    #[error("invalid value for {0}")]
    InvalidEnv(&'static str),
    #[error("invalid provider configuration: {0}")]
    Invalid(&'static str),
}

pub async fn write_model_catalog(config: &AdapterConfig) -> Result<PathBuf, std::io::Error> {
    tokio::fs::create_dir_all(&config.state_dir).await?;
    let path = config.state_dir.join(MODEL_CATALOG_FILE);
    let catalog = model_catalog(config.profile, &config.model);
    let bytes = serde_json::to_vec_pretty(&catalog).expect("model catalog is serializable");
    tokio::fs::write(&path, bytes).await?;
    Ok(path)
}

fn model_catalog(profile: &ProviderProfile, model: &str) -> ModelsResponse {
    ModelsResponse {
        models: vec![ModelInfo {
            slug: model.to_string(),
            display_name: model.to_string(),
            description: Some(format!(
                "{} through the Factory transport adapter.",
                profile.label
            )),
            default_reasoning_level: Some(ReasoningEffort::Medium),
            supported_reasoning_levels: [
                ReasoningEffort::Low,
                ReasoningEffort::Medium,
                ReasoningEffort::High,
                ReasoningEffort::XHigh,
                ReasoningEffort::Max,
            ]
            .into_iter()
            .map(|effort| {
                let description = effort.to_string();
                ReasoningEffortPreset {
                    effort,
                    description,
                }
            })
            .collect(),
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
            context_window: Some(profile.context_window),
            max_context_window: Some(profile.context_window),
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
