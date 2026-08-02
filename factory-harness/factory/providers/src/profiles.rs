use std::collections::BTreeMap;
use std::path::PathBuf;

use serde_json::Value;
use serde_json::json;

pub const CODEX_PROVIDER_ID: &str = "factory-provider";
pub const GENERATED_MODEL_CATALOG_PATH: &str =
    "/var/lib/software-factory/provider/codex-models.json";

const OPENAI_MODELS: &[&str] = &["gpt-5.6-sol", "gpt-5.6-terra", "gpt-5.6-luna"];
const ANTHROPIC_MODELS: &[&str] = &["claude-sonnet-5", "claude-opus-5", "claude-fable-5"];
const DEEPSEEK_MODELS: &[&str] = &["deepseek-v4-pro", "deepseek-v4-flash"];
const ZAI_MODELS: &[&str] = &["glm-5.2", "glm-5.1", "glm-5"];

const OPENAI_BASES: &[BaseUrlChoice] = &[BaseUrlChoice {
    id: "standard",
    label: "OpenAI API",
    url: "https://api.openai.com/v1",
}];
const ANTHROPIC_BASES: &[BaseUrlChoice] = &[BaseUrlChoice {
    id: "standard",
    label: "Claude API",
    url: "https://api.anthropic.com",
}];
const DEEPSEEK_BASES: &[BaseUrlChoice] = &[BaseUrlChoice {
    id: "standard",
    label: "DeepSeek API",
    url: "https://api.deepseek.com",
}];
const ZAI_BASES: &[BaseUrlChoice] = &[
    BaseUrlChoice {
        id: "coding",
        label: "Coding Developer Plan",
        url: "https://api.z.ai/api/coding/paas/v4",
    },
    BaseUrlChoice {
        id: "standard",
        label: "Standard API",
        url: "https://api.z.ai/api/paas/v4",
    },
];

const PROFILES: &[ProviderProfile] = &[
    ProviderProfile {
        id: "openai",
        label: "OpenAI",
        adapter_kind: AdapterKind::DirectResponses,
        api_key_env: "OPENAI_API_KEY",
        default_model: "gpt-5.6-sol",
        models: OPENAI_MODELS,
        base_urls: OPENAI_BASES,
        context_window: 1_050_000,
    },
    ProviderProfile {
        id: "anthropic",
        label: "Anthropic Claude",
        adapter_kind: AdapterKind::AnthropicMessages,
        api_key_env: "ANTHROPIC_API_KEY",
        default_model: "claude-sonnet-5",
        models: ANTHROPIC_MODELS,
        base_urls: ANTHROPIC_BASES,
        context_window: 1_000_000,
    },
    ProviderProfile {
        id: "deepseek",
        label: "DeepSeek",
        adapter_kind: AdapterKind::ChatCompletions,
        api_key_env: "DEEPSEEK_API_KEY",
        default_model: "deepseek-v4-pro",
        models: DEEPSEEK_MODELS,
        base_urls: DEEPSEEK_BASES,
        context_window: 128_000,
    },
    ProviderProfile {
        id: "zai",
        label: "Z.AI",
        adapter_kind: AdapterKind::ChatCompletions,
        api_key_env: "ZAI_API_KEY",
        default_model: "glm-5.2",
        models: ZAI_MODELS,
        base_urls: ZAI_BASES,
        context_window: 1_000_000,
    },
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdapterKind {
    DirectResponses,
    ChatCompletions,
    AnthropicMessages,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BaseUrlChoice {
    pub id: &'static str,
    pub label: &'static str,
    pub url: &'static str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProviderProfile {
    pub id: &'static str,
    pub label: &'static str,
    pub adapter_kind: AdapterKind,
    pub api_key_env: &'static str,
    pub default_model: &'static str,
    pub models: &'static [&'static str],
    pub base_urls: &'static [BaseUrlChoice],
    pub context_window: i64,
}

pub fn provider_profiles() -> &'static [ProviderProfile] {
    PROFILES
}

pub fn provider_profile(id: &str) -> Option<&'static ProviderProfile> {
    PROFILES.iter().find(|profile| profile.id == id)
}

#[derive(Debug, Clone, PartialEq)]
pub struct CodexProviderSelection {
    pub model: String,
    pub model_provider: String,
    pub model_catalog_json: Option<PathBuf>,
    pub config: BTreeMap<String, Value>,
}

impl CodexProviderSelection {
    pub fn for_profile(
        profile: &ProviderProfile,
        base_url: impl Into<String>,
        model: impl Into<String>,
        model_catalog_json: Option<PathBuf>,
    ) -> Self {
        let base_url = base_url.into().trim_end_matches('/').to_string();
        let model = model.into();
        let model_catalog_json = model_catalog_json
            .filter(|path| !path.as_os_str().is_empty())
            .or_else(|| {
                (profile.adapter_kind != AdapterKind::DirectResponses)
                    .then(|| PathBuf::from(GENERATED_MODEL_CATALOG_PATH))
            });
        let mut provider = json!({
            "name": profile.label,
            "base_url": base_url,
            "wire_api": "responses",
            "requires_openai_auth": false,
            "supports_websockets": false
        });
        if profile.adapter_kind == AdapterKind::DirectResponses {
            provider["env_key"] = json!(profile.api_key_env);
        }
        let mut config =
            BTreeMap::from([(format!("model_providers.{CODEX_PROVIDER_ID}"), provider)]);
        if let Some(path) = &model_catalog_json {
            config.insert("model_catalog_json".to_string(), json!(path));
        }
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
