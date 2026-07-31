use anyhow::Result;
use clap::Parser;
use clap::Subcommand;
use codex_arg0::Arg0DispatchPaths;
use codex_arg0::arg0_dispatch_or_else;
use codex_config::LoaderOverrides;
use codex_utils_cli::CliConfigOverrides;
use factory_runtime::legacy_protocol_manifest;
use factory_runtime::protocol_manifest;
use factory_runtime::run_stdio;

#[derive(Debug, Parser)]
#[command(version)]
struct FactoryRuntimeArgs {
    #[command(subcommand)]
    command: Option<FactoryRuntimeCommand>,

    #[command(flatten)]
    config_overrides: CliConfigOverrides,

    /// Fail if config.toml contains unknown configuration fields.
    #[arg(long, default_value_t = false)]
    strict_config: bool,
}

#[derive(Debug, Subcommand)]
enum FactoryRuntimeCommand {
    /// Print the active Factory runtime distribution manifest as JSON and exit.
    ProtocolManifest,

    /// Print the legacy Factory Protocol V1-only manifest as JSON and exit.
    LegacyProtocolManifest,
}

fn main() -> Result<()> {
    arg0_dispatch_or_else(|arg0_paths: Arg0DispatchPaths| async move {
        let FactoryRuntimeArgs {
            command,
            config_overrides,
            strict_config,
        } = FactoryRuntimeArgs::parse();

        match command {
            Some(FactoryRuntimeCommand::ProtocolManifest) => {
                let manifest_json = serde_json::to_string(&protocol_manifest())?;
                println!("{manifest_json}");
            }
            Some(FactoryRuntimeCommand::LegacyProtocolManifest) => {
                let manifest_json = serde_json::to_string(&legacy_protocol_manifest())?;
                println!("{manifest_json}");
            }
            None => {
                run_stdio(
                    arg0_paths,
                    config_overrides,
                    LoaderOverrides::default(),
                    strict_config,
                )
                .await?;
            }
        }
        Ok(())
    })
}
