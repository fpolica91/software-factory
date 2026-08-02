use anyhow::Context;
use anyhow::Result;
use clap::Parser;
use clap::Subcommand;
use factory_coordinator::CoordinatorStore;
use serde::Serialize;
use std::io::Write;
use std::net::SocketAddr;
use tokio::net::TcpListener;

#[derive(Debug, Parser)]
#[command(name = "factoryd", about = "Software Factory durable coordinator")]
struct Cli {
    #[arg(long)]
    database_url: String,
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Apply coordinator schema migrations.
    Migrate,
    /// Serve the coordinator JSON API.
    Serve {
        #[arg(long, default_value = "0.0.0.0:8787")]
        bind: SocketAddr,
    },
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct MigrationReceipt {
    schema: &'static str,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ServerReceipt {
    listening: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Migrate => {
            let store = connected_store(&cli.database_url).await?;
            store.close().await;
            print_json(&MigrationReceipt { schema: "current" })?;
        }
        Command::Serve { bind } => serve(&cli.database_url, bind).await?,
    }
    Ok(())
}

async fn serve(database_url: &str, bind: SocketAddr) -> Result<()> {
    let store = connected_store(database_url).await?;
    let listener = TcpListener::bind(bind)
        .await
        .with_context(|| format!("bind factoryd HTTP server to {bind}"))?;
    let listening = listener
        .local_addr()
        .context("read factoryd HTTP listener address")?;
    print_json(&ServerReceipt {
        listening: listening.to_string(),
    })?;
    std::io::stdout()
        .flush()
        .context("flush factoryd server receipt")?;
    factory_coordinator::serve_http(store, listener)
        .await
        .context("serve factoryd HTTP API")
}

async fn connected_store(database_url: &str) -> Result<CoordinatorStore> {
    let store = CoordinatorStore::connect(database_url)
        .await
        .context("connect factoryd to PostgreSQL")?;
    store
        .migrate()
        .await
        .context("apply factoryd coordinator schema")?;
    Ok(store)
}

fn print_json(value: &impl Serialize) -> Result<()> {
    println!("{}", serde_json::to_string(value)?);
    Ok(())
}
