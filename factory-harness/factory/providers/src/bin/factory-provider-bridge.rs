use std::path::PathBuf;

use factory_providers::BridgeOptions;
use factory_providers::ProviderBridge;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let port = std::env::var("FACTORY_PROVIDER_PORT")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(10101);
    let state_dir = std::env::var_os("FACTORY_PROVIDER_STATE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("software-factory/provider-bridge"));

    let mut options = BridgeOptions::glm(port, state_dir);
    if let Ok(bind_host) = std::env::var("FACTORY_PROVIDER_BIND_HOST") {
        options.bind_host = bind_host;
    }
    if let Ok(advertised_base_url) = std::env::var("FACTORY_PROVIDER_ADVERTISED_URL") {
        options.advertised_base_url = Some(advertised_base_url);
    }
    if let Some(resource_dir) = std::env::var_os("FACTORY_PROVIDER_RESOURCE_DIR") {
        options.resource_dir = Some(PathBuf::from(resource_dir));
    }
    if let Ok(upstream_provider) = std::env::var("FACTORY_PROVIDER_UPSTREAM_ID") {
        options.upstream_provider = upstream_provider;
    }
    if let Ok(model) = std::env::var("FACTORY_PROVIDER_UPSTREAM_MODEL") {
        options.model = model;
    }
    if let Ok(base_url) = std::env::var("FACTORY_PROVIDER_UPSTREAM_BASE_URL")
        .or_else(|_| std::env::var("FACTORY_ZAI_BASE_URL"))
    {
        options.upstream_base_url = base_url;
    }
    if let Ok(api_key_env) = std::env::var("FACTORY_PROVIDER_UPSTREAM_API_KEY_ENV")
        .or_else(|_| std::env::var("FACTORY_ZAI_API_KEY_ENV"))
    {
        options.api_key_env = api_key_env;
    }

    let mut bridge = ProviderBridge::start(options).await?;
    println!(
        "{}",
        serde_json::json!({
            "endpoint": bridge.endpoint(),
            "model": bridge.selection().model,
            "modelProvider": bridge.selection().model_provider,
            "modelCatalogJson": bridge.selection().model_catalog_json,
            "threadStart": bridge.selection().thread_start_fields()
        })
    );

    tokio::signal::ctrl_c().await?;
    bridge.shutdown().await?;
    Ok(())
}
