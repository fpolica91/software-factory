use std::collections::BTreeMap;
use std::fs::OpenOptions;
use std::io::ErrorKind;
use std::io::IsTerminal;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

use anyhow::Context;
use anyhow::Result;
use anyhow::anyhow;
use clap::Args;
use factory_coordinator::ExecutionProfile;
use factory_providers::AdapterKind;
use factory_providers::GENERATED_MODEL_CATALOG_PATH;
use factory_providers::ProviderProfile;
use factory_providers::provider_profile;
use factory_providers::provider_profiles;

use crate::api::FactorydClient;
use crate::profile_guard::ExistingProfile;
use crate::profile_guard::ensure_profile_change_is_safe;

#[derive(Args)]
pub struct ConfigureArgs {
    /// Print the active configuration without exposing its API key.
    #[arg(long)]
    show: bool,

    /// Provider ID: openai, anthropic, deepseek, or zai.
    #[arg(long)]
    provider: Option<String>,

    /// Model ID. Interactive configuration presents the provider's known models.
    #[arg(long)]
    model: Option<String>,

    /// API key. Prefer the provider-specific environment variable in automation.
    #[arg(long)]
    api_key: Option<String>,

    /// Upstream endpoint choice or URL. Z.AI accepts `coding` or `standard`.
    #[arg(long)]
    base: Option<String>,

    /// Switch even when active jobs require another or unknown profile.
    #[arg(long)]
    force: bool,
}

#[derive(Args)]
pub struct ProviderArgs {
    /// Provider ID, `list`, or `show`. Omit it for an interactive selector.
    name: Option<String>,

    /// Model selected while switching providers.
    #[arg(long)]
    model: Option<String>,

    /// API key. Prefer the provider-specific environment variable in automation.
    #[arg(long)]
    api_key: Option<String>,

    /// Upstream endpoint choice or URL.
    #[arg(long)]
    base: Option<String>,

    /// Switch even when active jobs require another or unknown profile.
    #[arg(long)]
    force: bool,
}

#[derive(Args)]
pub struct ModelArgs {
    /// Model ID or `list`. Omit it for an interactive selector.
    model: Option<String>,

    /// Switch even when active jobs require another or unknown profile.
    #[arg(long)]
    force: bool,
}

pub async fn configure(path: &Path, client: &FactorydClient, args: ConfigureArgs) -> Result<i32> {
    let mut env = EnvFile::load(path)?;
    if args.show {
        if args.provider.is_some()
            || args.model.is_some()
            || args.api_key.is_some()
            || args.base.is_some()
            || args.force
        {
            return Err(anyhow!(
                "--show cannot be combined with configuration values"
            ));
        }
        show(&env)?;
        return Ok(0);
    }

    let interactive = interactive_terminal();
    let profile = select_provider(args.provider.as_deref(), &env, interactive)?;
    apply_provider(
        &mut env,
        profile,
        ProviderOverrides {
            model: args.model,
            api_key: args.api_key,
            base: args.base,
            force: args.force,
        },
        interactive,
        client,
    )
    .await?;
    Ok(0)
}

pub async fn provider(path: &Path, client: &FactorydClient, args: ProviderArgs) -> Result<i32> {
    let mut env = EnvFile::load(path)?;
    match args.name.as_deref() {
        Some("list") => {
            ensure_no_provider_options(&args)?;
            print_providers();
            return Ok(0);
        }
        Some("show") => {
            ensure_no_provider_options(&args)?;
            show(&env)?;
            return Ok(0);
        }
        _ => {}
    }

    let interactive = interactive_terminal();
    let profile = select_provider(args.name.as_deref(), &env, interactive)?;
    apply_provider(
        &mut env,
        profile,
        ProviderOverrides {
            model: args.model,
            api_key: args.api_key,
            base: args.base,
            force: args.force,
        },
        interactive,
        client,
    )
    .await?;
    Ok(0)
}

pub async fn model(path: &Path, client: &FactorydClient, args: ModelArgs) -> Result<i32> {
    let mut env = EnvFile::load(path)?;
    let profile = active_profile(&env)?;
    if args.model.as_deref() == Some("list") {
        if args.force {
            return Err(anyhow!("model list cannot be combined with --force"));
        }
        print_models(profile);
        return Ok(0);
    }

    let selected = match args.model {
        Some(model) => validate_single_line("model", model)?,
        None if interactive_terminal() => prompt_model(profile, env.get("FACTORY_MODEL"))?,
        None => return Err(anyhow!("model ID is required without a terminal")),
    };
    ensure_profile_change_is_safe(
        client,
        &configured_execution_profile(&env),
        &ExecutionProfile {
            provider: profile.id.to_string(),
            model: selected.clone(),
        },
        args.force,
    )
    .await?;
    env.set("FACTORY_MODEL", &selected);
    env.write()?;
    println!("Now using {} with model {selected}.", profile.label);
    Ok(0)
}

/// Requested configuration overrides shared by `configure` and `provider`.
struct ProviderOverrides {
    model: Option<String>,
    api_key: Option<String>,
    base: Option<String>,
    force: bool,
}

async fn apply_provider(
    env: &mut EnvFile,
    profile: &'static ProviderProfile,
    overrides: ProviderOverrides,
    interactive: bool,
    client: &FactorydClient,
) -> Result<()> {
    let ProviderOverrides {
        model: requested_model,
        api_key: requested_key,
        base: requested_base,
        force,
    } = overrides;
    let current_provider = env.get("FACTORY_PROVIDER_ADAPTER");
    let current_model = (current_provider == Some(profile.id))
        .then(|| env.get("FACTORY_MODEL"))
        .flatten();
    let model = match requested_model {
        Some(model) => validate_single_line("model", model)?,
        None if interactive => prompt_model(profile, current_model)?,
        None => current_model
            .map(str::to_string)
            .unwrap_or_else(|| profile.default_model.to_string()),
    };
    ensure_profile_change_is_safe(
        client,
        &configured_execution_profile(env),
        &ExecutionProfile {
            provider: profile.id.to_string(),
            model: model.clone(),
        },
        force,
    )
    .await?;
    let api_key = select_api_key(env, profile, requested_key, interactive)?;
    let upstream_base = select_base(env, profile, requested_base, interactive)?;

    env.set("FACTORY_PROVIDER_ADAPTER", profile.id);
    env.set("FACTORY_MODEL", &model);
    env.set(profile.api_key_env, &api_key);
    env.set(
        "FACTORY_PROVIDER_BASE_URL",
        &runtime_base_url(profile, &upstream_base),
    );
    env.set(
        "FACTORY_MODEL_CATALOG_JSON",
        if profile.adapter_kind == AdapterKind::DirectResponses {
            ""
        } else {
            GENERATED_MODEL_CATALOG_PATH
        },
    );
    env.set(upstream_base_variable(profile), &upstream_base);
    env.remove("FACTORY_MODEL_PROVIDER");
    env.remove("FACTORY_PROVIDER_NAME");
    env.remove("FACTORY_PROVIDER_AUTH");
    env.remove("FACTORY_PROVIDER_API_KEY");
    env.remove("FACTORY_PROVIDER_BRIDGE_TOKEN");
    env.remove("FACTORY_PROVIDER_AUTH_TOKEN");
    env.write()?;

    println!("Configured {} with model {model}.", profile.label);
    Ok(())
}

fn select_provider(
    requested: Option<&str>,
    env: &EnvFile,
    interactive: bool,
) -> Result<&'static ProviderProfile> {
    if let Some(requested) = requested {
        return resolve_profile(requested);
    }
    if !interactive {
        return env
            .get("FACTORY_PROVIDER_ADAPTER")
            .ok_or_else(|| anyhow!("provider is required without a terminal"))
            .and_then(resolve_profile);
    }

    eprintln!("Choose a provider:");
    for (index, profile) in provider_profiles().iter().enumerate() {
        eprintln!("  {}) {}", index + 1, profile.label);
    }
    let default = env
        .get("FACTORY_PROVIDER_ADAPTER")
        .and_then(|id| resolve_profile(id).ok())
        .map(|profile| profile.id)
        .unwrap_or("openai");
    let answer = prompt_line(&format!("Provider [{default}]: "))?;
    let answer = if answer.is_empty() { default } else { &answer };
    if let Ok(index) = answer.parse::<usize>()
        && let Some(profile) = provider_profiles().get(index.saturating_sub(1))
    {
        return Ok(profile);
    }
    resolve_profile(answer)
}

fn resolve_profile(id: &str) -> Result<&'static ProviderProfile> {
    let id = match id.trim() {
        "claude" => "anthropic",
        canonical => canonical,
    };
    provider_profile(id).ok_or_else(|| {
        anyhow!("unknown provider {id}; expected openai, anthropic, deepseek, or zai")
    })
}

fn active_profile(env: &EnvFile) -> Result<&'static ProviderProfile> {
    env.get("FACTORY_PROVIDER_ADAPTER")
        .ok_or_else(|| anyhow!("no provider is configured; run `factory configure`"))
        .and_then(resolve_profile)
}

fn configured_execution_profile(env: &EnvFile) -> ExistingProfile {
    let provider = env
        .get("FACTORY_PROVIDER_ADAPTER")
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let model = env
        .get("FACTORY_MODEL")
        .map(str::trim)
        .filter(|value| !value.is_empty());
    match (provider, model) {
        (None, None) => ExistingProfile::Unconfigured,
        (Some(provider), Some(model)) => ExistingProfile::Complete(ExecutionProfile {
            provider: resolve_profile(provider)
                .ok()
                .map_or(provider, |profile| profile.id)
                .to_string(),
            model: model.to_string(),
        }),
        _ => ExistingProfile::Partial,
    }
}

fn select_api_key(
    env: &EnvFile,
    profile: &ProviderProfile,
    requested: Option<String>,
    interactive: bool,
) -> Result<String> {
    if let Some(key) = requested {
        return validate_secret(key);
    }
    if let Ok(key) = std::env::var(profile.api_key_env)
        && !key.trim().is_empty()
    {
        return validate_secret(key);
    }
    let existing = env.get(profile.api_key_env).unwrap_or_default();
    if !interactive {
        if existing.is_empty() {
            return Err(anyhow!(
                "{} is required; set it in the environment or pass --api-key",
                profile.api_key_env
            ));
        }
        return Ok(existing.to_string());
    }

    if existing.is_empty() {
        eprint!("{} API key (input hidden): ", profile.label);
    } else {
        eprint!(
            "{} API key (input hidden; Enter keeps configured key): ",
            profile.label
        );
    }
    std::io::stderr().flush().context("show API key prompt")?;
    let entered = rpassword::read_password().context("read API key")?;
    let key = if entered.is_empty() {
        existing.to_string()
    } else {
        entered
    };
    let key = validate_secret(key)?;
    eprintln!("API key received.");
    Ok(key)
}

fn validate_secret(value: String) -> Result<String> {
    if value.trim().is_empty() {
        return Err(anyhow!("API key cannot be empty"));
    }
    if value.contains(['\n', '\r']) {
        return Err(anyhow!("API key must be a single line"));
    }
    Ok(value)
}

fn prompt_model(profile: &ProviderProfile, current: Option<&str>) -> Result<String> {
    eprintln!("Choose a model:");
    for (index, model) in profile.models.iter().enumerate() {
        eprintln!("  {}) {model}", index + 1);
    }
    eprintln!("  {}) Custom model ID", profile.models.len() + 1);
    let default = current.unwrap_or(profile.default_model);
    let answer = prompt_line(&format!("Model [{default}]: "))?;
    if answer.is_empty() {
        return Ok(default.to_string());
    }
    if let Ok(index) = answer.parse::<usize>() {
        if let Some(model) = profile.models.get(index.saturating_sub(1)) {
            return Ok((*model).to_string());
        }
        if index == profile.models.len() + 1 {
            return validate_single_line("model", prompt_line("Model ID: ")?);
        }
        return Err(anyhow!("choose one of the listed models"));
    }
    validate_single_line("model", answer)
}

fn select_base(
    env: &EnvFile,
    profile: &ProviderProfile,
    requested: Option<String>,
    interactive: bool,
) -> Result<String> {
    let current = env
        .get(upstream_base_variable(profile))
        .unwrap_or(profile.base_urls[0].url);
    if let Some(requested) = requested {
        return resolve_base(profile, &requested);
    }
    if profile.base_urls.len() == 1 || !interactive {
        return Ok(current.to_string());
    }

    eprintln!("Choose the {} endpoint:", profile.label);
    for (index, base) in profile.base_urls.iter().enumerate() {
        eprintln!("  {}) {}", index + 1, base.label);
    }
    let default = profile
        .base_urls
        .iter()
        .position(|base| base.url == current)
        .map(|index| index + 1)
        .unwrap_or(1);
    let answer = prompt_line(&format!("Endpoint [{default}]: "))?;
    if answer.is_empty() {
        return Ok(current.to_string());
    }
    resolve_base(profile, &answer)
}

fn resolve_base(profile: &ProviderProfile, value: &str) -> Result<String> {
    if let Ok(index) = value.parse::<usize>()
        && let Some(base) = profile.base_urls.get(index.saturating_sub(1))
    {
        return Ok(base.url.to_string());
    }
    if let Some(base) = profile
        .base_urls
        .iter()
        .find(|base| base.id.eq_ignore_ascii_case(value))
    {
        return Ok(base.url.to_string());
    }
    if value.starts_with("http://") || value.starts_with("https://") {
        let value = validate_single_line("base URL", value.to_string())?;
        let parsed = reqwest::Url::parse(&value).context("parse base URL")?;
        if parsed.host_str().is_none() || parsed.query().is_some() || parsed.fragment().is_some() {
            return Err(anyhow!(
                "base URL must include a host and cannot contain a query or fragment"
            ));
        }
        return Ok(value);
    }
    Err(anyhow!("unknown endpoint choice {value}"))
}

fn runtime_base_url(profile: &ProviderProfile, upstream_base: &str) -> String {
    match profile.adapter_kind {
        AdapterKind::DirectResponses => upstream_base.to_string(),
        AdapterKind::AnthropicMessages => "http://claude-provider:10101/v1".to_string(),
        AdapterKind::ChatCompletions if profile.id == "deepseek" => {
            "http://deepseek-provider:10101/v1".to_string()
        }
        AdapterKind::ChatCompletions if profile.id == "zai" => {
            "http://zai-provider:10101/v1".to_string()
        }
        _ => unreachable!("profiles are closed over the supported provider set"),
    }
}

pub(crate) fn upstream_base_variable(profile: &ProviderProfile) -> &'static str {
    match profile.id {
        "openai" => "FACTORY_OPENAI_BASE_URL",
        "anthropic" => "FACTORY_CLAUDE_BASE_URL",
        "deepseek" => "FACTORY_DEEPSEEK_BASE_URL",
        "zai" => "FACTORY_ZAI_BASE_URL",
        _ => unreachable!("profiles are closed over the supported provider set"),
    }
}

fn show(env: &EnvFile) -> Result<()> {
    let profile = active_profile(env)?;
    let model = env.get("FACTORY_MODEL").unwrap_or(profile.default_model);
    let endpoint = env
        .get(upstream_base_variable(profile))
        .unwrap_or(profile.base_urls[0].url);
    println!("Active provider:");
    println!("  provider: {} ({})", profile.label, profile.id);
    println!("  model: {model}");
    println!("  endpoint: {endpoint}");
    println!(
        "  API key: {}",
        if env
            .get(profile.api_key_env)
            .is_some_and(|key| !key.is_empty())
        {
            "configured"
        } else {
            "missing"
        }
    );
    Ok(())
}

fn print_providers() {
    println!("Supported providers:");
    for profile in provider_profiles() {
        println!("  {:<10} {}", profile.id, profile.label);
    }
}

fn print_models(profile: &ProviderProfile) {
    println!("{} models:", profile.label);
    for model in profile.models {
        println!("  {model}");
    }
    println!("  custom model ID");
}

fn ensure_no_provider_options(args: &ProviderArgs) -> Result<()> {
    if args.model.is_some() || args.api_key.is_some() || args.base.is_some() || args.force {
        return Err(anyhow!(
            "list/show cannot be combined with provider options"
        ));
    }
    Ok(())
}

fn validate_single_line(label: &str, value: String) -> Result<String> {
    let value = value.trim().to_string();
    if value.is_empty() {
        return Err(anyhow!("{label} cannot be empty"));
    }
    if value.contains(['\n', '\r']) {
        return Err(anyhow!("{label} must be a single line"));
    }
    Ok(value)
}

fn interactive_terminal() -> bool {
    std::io::stdin().is_terminal() && std::io::stderr().is_terminal()
}

pub(crate) fn prompt_line(prompt: &str) -> Result<String> {
    eprint!("{prompt}");
    std::io::stderr().flush().context("show prompt")?;
    let mut answer = String::new();
    std::io::stdin()
        .read_line(&mut answer)
        .context("read terminal input")?;
    Ok(answer.trim().to_string())
}

struct EnvFile {
    path: PathBuf,
    lines: Vec<String>,
    values: BTreeMap<String, String>,
    updates: BTreeMap<String, Option<String>>,
}

impl EnvFile {
    fn load(path: &Path) -> Result<Self> {
        let contents = match std::fs::read_to_string(path) {
            Ok(contents) => contents,
            Err(error) if error.kind() == ErrorKind::NotFound => String::new(),
            Err(error) => {
                return Err(error).with_context(|| format!("read {}", path.display()));
            }
        };
        let lines = contents.lines().map(str::to_string).collect::<Vec<_>>();
        let values = lines
            .iter()
            .filter_map(|line| parse_assignment(line))
            .collect();
        Ok(Self {
            path: path.to_path_buf(),
            lines,
            values,
            updates: BTreeMap::new(),
        })
    }

    fn get(&self, key: &str) -> Option<&str> {
        if let Some(update) = self.updates.get(key) {
            return update.as_deref();
        }
        self.values.get(key).map(String::as_str)
    }

    fn set(&mut self, key: &str, value: &str) {
        self.updates
            .insert(key.to_string(), Some(value.to_string()));
    }

    fn remove(&mut self, key: &str) {
        self.updates.insert(key.to_string(), None);
    }

    fn write(&mut self) -> Result<()> {
        if let Some(parent) = self.path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create {}", parent.display()))?;
        }
        let mut emitted = BTreeMap::<String, ()>::new();
        let mut output = String::new();
        for line in &self.lines {
            let key = assignment_key(line);
            match key.and_then(|key| self.updates.get(key).map(|value| (key, value))) {
                Some((key, Some(value))) if !emitted.contains_key(key) => {
                    output.push_str(&format_assignment(key, value));
                    output.push('\n');
                    emitted.insert(key.to_string(), ());
                }
                Some((key, _)) => {
                    emitted.insert(key.to_string(), ());
                }
                None => {
                    output.push_str(line);
                    output.push('\n');
                }
            }
        }
        for (key, value) in &self.updates {
            if emitted.contains_key(key) {
                continue;
            }
            if let Some(value) = value {
                output.push_str(&format_assignment(key, value));
                output.push('\n');
            }
        }

        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let temporary = self
            .path
            .with_extension(format!("factory-tmp-{}-{stamp}", std::process::id()));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options
            .open(&temporary)
            .with_context(|| format!("create {}", temporary.display()))?;
        file.write_all(output.as_bytes())
            .with_context(|| format!("write {}", temporary.display()))?;
        file.sync_all()
            .with_context(|| format!("flush {}", temporary.display()))?;
        std::fs::rename(&temporary, &self.path).with_context(|| {
            format!(
                "replace {} with {}",
                self.path.display(),
                temporary.display()
            )
        })?;
        self.lines = output.lines().map(str::to_string).collect();
        self.values = self
            .lines
            .iter()
            .filter_map(|line| parse_assignment(line))
            .collect();
        self.updates.clear();
        Ok(())
    }
}

fn assignment_key(line: &str) -> Option<&str> {
    let (key, _) = line.split_once('=')?;
    let key = key.trim();
    (!key.is_empty()
        && key
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '_'))
    .then_some(key)
}

fn parse_assignment(line: &str) -> Option<(String, String)> {
    let (key, value) = line.split_once('=')?;
    let key = key.trim();
    if key.is_empty()
        || !key
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '_')
    {
        return None;
    }
    let value = value.trim();
    let value = if value.starts_with('\'') {
        decode_quoted(value, '\'')?
    } else if value.starts_with('"') {
        decode_quoted(value, '"')?
    } else {
        value
            .split_once(" #")
            .map_or(value, |(value, _)| value)
            .trim_end()
            .to_string()
    };
    Some((key.to_string(), value))
}

fn decode_quoted(value: &str, quote: char) -> Option<String> {
    let mut decoded = String::new();
    let mut escaped = false;
    for character in value.chars().skip(1) {
        if escaped {
            escaped = false;
            if quote == '\'' {
                if character != '\'' {
                    decoded.push('\\');
                }
                decoded.push(character);
            } else {
                match character {
                    'n' => decoded.push('\n'),
                    'r' => decoded.push('\r'),
                    't' => decoded.push('\t'),
                    '"' => decoded.push('"'),
                    '\\' => decoded.push('\\'),
                    '$' => decoded.push('$'),
                    other => {
                        decoded.push('\\');
                        decoded.push(other);
                    }
                }
            }
        } else if character == '\\' {
            escaped = true;
        } else if character == quote {
            return Some(decoded);
        } else {
            decoded.push(character);
        }
    }
    None
}

fn format_assignment(key: &str, value: &str) -> String {
    let escaped = value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('$', "\\$");
    format!("{key}=\"{escaped}\"")
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestConfig {
        path: PathBuf,
    }

    impl TestConfig {
        fn new() -> Self {
            let stamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            Self {
                path: std::env::temp_dir().join(format!(
                    "factory-config-test-{}-{stamp}.env",
                    std::process::id()
                )),
            }
        }
    }

    impl Drop for TestConfig {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.path);
        }
    }

    #[tokio::test]
    async fn openai_custom_responses_base_is_persisted_as_runtime_endpoint() {
        let config = TestConfig::new();
        let mut env = EnvFile::load(&config.path).unwrap();
        let profile = provider_profile("openai").unwrap();
        let client = FactorydClient::new("http://127.0.0.1:9").unwrap();

        apply_provider(
            &mut env,
            profile,
            ProviderOverrides {
                model: Some("custom-responses-model".to_string()),
                api_key: Some("test-api-key".to_string()),
                base: Some("https://responses.example.test/v1".to_string()),
                force: false,
            },
            false,
            &client,
        )
        .await
        .unwrap();

        let persisted = EnvFile::load(&config.path).unwrap();
        assert_eq!(
            persisted.get("FACTORY_PROVIDER_BASE_URL"),
            Some("https://responses.example.test/v1")
        );
        assert_eq!(
            persisted.get(upstream_base_variable(profile)),
            Some("https://responses.example.test/v1")
        );
    }

    #[test]
    fn unsupported_custom_base_is_rejected_before_it_is_persisted() {
        let profile = provider_profile("openai").unwrap();

        assert!(resolve_base(profile, "https://").is_err());
        assert!(resolve_base(profile, "https://responses.example.test/v1?mode=test").is_err());
        assert!(resolve_base(profile, "file:///tmp/provider").is_err());
    }

    #[test]
    fn managed_values_round_trip_compose_sensitive_characters() {
        let config = TestConfig::new();
        let values = [
            ("DOLLAR", "$TOKEN-${OTHER}"),
            ("COMMENT", "value # literal"),
            ("QUOTES", "double \" and single '"),
            ("SLASH", "path\\ending\\"),
            ("SPACE", " leading and trailing "),
        ];
        let mut env = EnvFile::load(&config.path).unwrap();
        for (key, value) in values {
            env.set(key, value);
        }
        env.write().unwrap();

        let persisted = EnvFile::load(&config.path).unwrap();
        for (key, value) in values {
            assert_eq!(persisted.get(key), Some(value));
        }
        let raw = std::fs::read_to_string(&config.path).unwrap();
        assert!(raw.contains("DOLLAR=\"\\$TOKEN-\\${OTHER}\""));
        assert!(raw.contains("SLASH=\"path\\\\ending\\\\\""));
    }

    #[test]
    fn decoder_preserves_legacy_single_quoted_literals() {
        assert_eq!(
            parse_assignment("TOKEN='literal $value # text'").unwrap().1,
            "literal $value # text"
        );
        assert_eq!(
            parse_assignment("QUOTE='Let\\'s go!'").unwrap().1,
            "Let's go!"
        );
    }

    #[test]
    fn only_two_blank_profile_fields_are_first_time_configuration() {
        let config = TestConfig::new();
        let mut env = EnvFile::load(&config.path).unwrap();
        assert!(matches!(
            configured_execution_profile(&env),
            ExistingProfile::Unconfigured
        ));

        env.set("FACTORY_PROVIDER_ADAPTER", "openai");
        assert!(matches!(
            configured_execution_profile(&env),
            ExistingProfile::Partial
        ));
        env.set("FACTORY_MODEL", "gpt-5.6-sol");
        assert!(matches!(
            configured_execution_profile(&env),
            ExistingProfile::Complete(_)
        ));
    }

    #[test]
    fn claude_is_only_a_configuration_input_alias() {
        assert_eq!(resolve_profile("claude").unwrap().id, "anthropic");
        assert!(provider_profile("claude").is_none());
    }
}
