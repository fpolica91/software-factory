use std::collections::BTreeMap;
use std::path::PathBuf;

use serde_json::json;

use super::CODEX_PROVIDER_ID;
use super::CodexProviderSelection;
use super::DEFAULT_ADMISSION_TOKEN_ENV;

#[test]
fn bridge_selection_omits_catalog_for_codex_fallback_metadata() {
    let selection = CodexProviderSelection::for_bridge(
        "http://deepseek-provider:10101/v1/",
        "deepseek-v4-pro",
        None,
    );
    let config = BTreeMap::from([(
        format!("model_providers.{CODEX_PROVIDER_ID}"),
        json!({
            "name": "Software Factory provider bridge",
            "base_url": "http://deepseek-provider:10101/v1",
            "wire_api": "responses",
            "requires_openai_auth": false,
            "supports_websockets": false,
            "env_http_headers": {
                "X-OpenCodex-API-Key": DEFAULT_ADMISSION_TOKEN_ENV
            }
        }),
    )]);

    assert_eq!(
        selection,
        CodexProviderSelection {
            model: "deepseek-v4-pro".to_string(),
            model_provider: CODEX_PROVIDER_ID.to_string(),
            model_catalog_json: None,
            config,
        }
    );
}

#[test]
fn bridge_selection_preserves_explicit_model_catalog() {
    let catalog = PathBuf::from("/state/codex-models.json");
    let selection = CodexProviderSelection::for_bridge(
        "http://zai-provider:10101/v1",
        "glm-5.2",
        Some(catalog.clone()),
    );
    let config = BTreeMap::from([
        (
            "model_catalog_json".to_string(),
            json!("/state/codex-models.json"),
        ),
        (
            format!("model_providers.{CODEX_PROVIDER_ID}"),
            json!({
                "name": "Software Factory provider bridge",
                "base_url": "http://zai-provider:10101/v1",
                "wire_api": "responses",
                "requires_openai_auth": false,
                "supports_websockets": false,
                "env_http_headers": {
                    "X-OpenCodex-API-Key": DEFAULT_ADMISSION_TOKEN_ENV
                }
            }),
        ),
    ]);

    assert_eq!(
        selection,
        CodexProviderSelection {
            model: "glm-5.2".to_string(),
            model_provider: CODEX_PROVIDER_ID.to_string(),
            model_catalog_json: Some(catalog),
            config,
        }
    );
}
