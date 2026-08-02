use factory_providers::AdapterConfig;
use factory_providers::ProviderAdapter;

const HELP: &str = "factory-provider-bridge

Translate one configured provider's streaming API into the Responses API used by Codex.

Usage: factory-provider-bridge

Configuration is read from the environment:
  FACTORY_PROVIDER_UPSTREAM_ID             anthropic, deepseek, or zai (default: zai)
  FACTORY_PROVIDER_UPSTREAM_MODEL          model identifier (provider default when unset)
  FACTORY_PROVIDER_UPSTREAM_BASE_URL       provider API base URL
  FACTORY_PROVIDER_UPSTREAM_API_KEY_ENV    name of the variable containing the API key
  FACTORY_PROVIDER_BIND_HOST               listen host (default: 127.0.0.1)
  FACTORY_PROVIDER_PORT                    listen port (default: 10101)
  FACTORY_PROVIDER_ADVERTISED_URL          URL advertised to Codex
  FACTORY_PROVIDER_STATE_DIR               generated model-catalog directory
  FACTORY_PROVIDER_MAX_TOKENS              maximum provider output tokens

The API key itself must be set in the selected provider's key variable, such as
ANTHROPIC_API_KEY, DEEPSEEK_API_KEY, or ZAI_API_KEY.
";

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut arguments = std::env::args_os().skip(1);
    if let Some(argument) = arguments.next() {
        if (argument == "--help" || argument == "-h") && arguments.next().is_none() {
            print!("{HELP}");
            return Ok(());
        }
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("unexpected argument {:?}; use --help", argument),
        )
        .into());
    }
    let config = AdapterConfig::from_env()?;
    let adapter = ProviderAdapter::bind(config).await?;
    println!(
        "{}",
        serde_json::json!({
            "endpoint": adapter.endpoint(),
            "model": adapter.selection().model,
            "modelProvider": adapter.selection().model_provider,
            "modelCatalogJson": adapter.selection().model_catalog_json,
            "threadStart": adapter.selection().thread_start_fields()
        })
    );
    adapter.run().await?;
    Ok(())
}
