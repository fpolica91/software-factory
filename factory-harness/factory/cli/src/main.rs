mod api;
mod config;
mod live;
mod output;
mod profile_guard;
mod transcript;

use std::ffi::OsString;
use std::io::IsTerminal;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command as ProcessCommand;
use std::process::Stdio;
use std::time::Duration;

use anyhow::Context;
use anyhow::Result;
use anyhow::anyhow;
use clap::Args;
use clap::Parser;
use clap::Subcommand;
use factory_coordinator::EnsureWorkspaceRequest;
use factory_coordinator::ExecutionProfile;
use factory_coordinator::FactoryTaskInput;
use factory_coordinator::JobDefinition;
use factory_coordinator::JobId;
use factory_coordinator::OperationDefinition;
use factory_providers::provider_profile;
use serde_json::json;
use sha2::Digest;
use sha2::Sha256;

use crate::api::ExportedResult;
use crate::api::FactorydClient;
use crate::live::LiveAction;
use crate::live::LiveScreen;
use crate::transcript::Transcript;

const POLL_INTERVAL: Duration = Duration::from_secs(1);
const EVENT_PAGE_SIZE: u32 = 200;
const AUTONOMOUS_INSTRUCTIONS: &str = "Operate autonomously until the Factory stage is complete. Do not ask the user questions or wait for clarification. Make reasonable repository-grounded assumptions, use the required tools, verify the result, and continue with another approach when one is unavailable.";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum OutputMode {
    Compact,
    Interactive,
    Json,
    Verbose,
}

impl OutputMode {
    fn from_flags(json: bool, verbose: bool) -> Self {
        if json {
            Self::Json
        } else if verbose {
            Self::Verbose
        } else if std::io::stdin().is_terminal()
            && std::io::stdout().is_terminal()
            && std::env::var("TERM").as_deref() != Ok("dumb")
        {
            Self::Interactive
        } else {
            Self::Compact
        }
    }

    fn json(self) -> bool {
        self == Self::Json
    }
}

#[derive(Debug)]
struct RepositoryLocation {
    repository_id: String,
    repository: String,
    base_ref: String,
    local_root: Option<PathBuf>,
}

#[derive(Parser)]
#[command(
    name = "factory",
    version,
    about = "Run durable Software Factory jobs through the native Codex harness"
)]
struct Cli {
    /// Base URL of the factoryd coordinator API.
    #[arg(
        long,
        global = true,
        env = "FACTORYD_URL",
        default_value = "http://127.0.0.1:8787"
    )]
    factoryd_url: String,

    /// Environment file used by provider onboarding and model selection.
    #[arg(
        long,
        global = true,
        env = "FACTORY_CONFIG_FILE",
        default_value = ".env"
    )]
    config_file: PathBuf,

    #[command(subcommand)]
    command: Option<FactoryCommand>,
}

#[derive(Subcommand)]
enum FactoryCommand {
    /// Configure a provider, API key, endpoint, and model.
    Configure(config::ConfigureArgs),
    /// List, inspect, or switch the active provider.
    Provider(config::ProviderArgs),
    /// List or switch models for the active provider.
    Model(config::ModelArgs),
    /// Start a durable four-stage Factory job.
    Run(RunArgs),
    /// Stream durable events and wait for a job to finish.
    Attach(AttachArgs),
    /// Print the current durable job state and results.
    Status(JobArgs),
    /// Stop a running durable job.
    Stop(JobArgs),
    /// Export a succeeded job as a standard binary Git patch.
    Export(ExportArgs),
    /// Apply a succeeded job to the current clean host checkout.
    Apply(ApplyArgs),
}

#[derive(Debug, Args, Default)]
struct RunArgs {
    /// Return after the durable job and managed worktree are created.
    #[arg(long)]
    detach: bool,

    /// Leave a successful result in Factory for an explicit apply/export.
    #[arg(long)]
    no_apply: bool,

    /// Emit newline-delimited JSON instead of human-readable output.
    #[arg(long)]
    json: bool,

    /// Print every durable event and its full detail instead of the compact view.
    #[arg(long, conflicts_with = "json")]
    verbose: bool,

    /// Remote Git URL. For a local checkout, run Factory from that repository.
    #[arg(long = "repository", visible_alias = "repo", conflicts_with = "cwd")]
    repository: Option<String>,

    /// Revision used to create the managed worktree.
    #[arg(
        long,
        requires = "repository",
        default_value_if("repository", clap::builder::ArgPredicate::IsPresent, "HEAD")
    )]
    base_ref: Option<String>,

    /// Internal checkout override for container and acceptance use.
    #[arg(long, conflicts_with = "repository", hide = true)]
    cwd: Option<PathBuf>,

    /// Task to complete. When omitted on a terminal, Factory prompts for it.
    #[arg(value_name = "TASK", num_args = 0.., trailing_var_arg = true)]
    task: Vec<String>,
}

#[derive(Debug, Args)]
struct AttachArgs {
    /// Emit JSON instead of human-readable output.
    #[arg(long)]
    json: bool,

    /// Print every durable event and its full detail instead of the compact view.
    #[arg(long, conflicts_with = "json")]
    verbose: bool,

    /// Durable Factory job ID.
    job_id: String,
}

#[derive(Debug, Args)]
struct JobArgs {
    /// Emit JSON instead of human-readable output.
    #[arg(long)]
    json: bool,

    /// Durable Factory job ID.
    job_id: String,
}

#[derive(Debug, Args)]
struct ExportArgs {
    /// Durable Factory job ID.
    job_id: String,

    /// Patch destination. Use `-` for standard output.
    #[arg(short, long)]
    output: Option<PathBuf>,
}

#[derive(Debug, Args)]
struct ApplyArgs {
    /// Durable Factory job ID.
    job_id: String,

    /// Internal checkout override for container and acceptance use.
    #[arg(long, hide = true)]
    cwd: Option<PathBuf>,
}

#[tokio::main]
async fn main() {
    match run().await {
        Ok(code) => std::process::exit(code),
        Err(error) => {
            eprintln!("{error:#}");
            std::process::exit(1);
        }
    }
}

async fn run() -> Result<i32> {
    let cli = Cli::parse_from(normalize_shorthand(std::env::args_os()));
    match cli.command {
        Some(FactoryCommand::Configure(args)) => {
            config::configure(
                &cli.config_file,
                &FactorydClient::new(&cli.factoryd_url)?,
                args,
            )
            .await
        }
        Some(FactoryCommand::Provider(args)) => {
            config::provider(
                &cli.config_file,
                &FactorydClient::new(&cli.factoryd_url)?,
                args,
            )
            .await
        }
        Some(FactoryCommand::Model(args)) => {
            config::model(
                &cli.config_file,
                &FactorydClient::new(&cli.factoryd_url)?,
                args,
            )
            .await
        }
        Some(FactoryCommand::Run(args)) => {
            run_job(&FactorydClient::new(&cli.factoryd_url)?, args).await
        }
        Some(FactoryCommand::Attach(args)) => {
            attach(
                &FactorydClient::new(&cli.factoryd_url)?,
                validate_job_id(&args.job_id)?,
                OutputMode::from_flags(args.json, args.verbose),
            )
            .await
        }
        Some(FactoryCommand::Status(args)) => {
            status(
                &FactorydClient::new(&cli.factoryd_url)?,
                validate_job_id(&args.job_id)?,
                args.json,
            )
            .await
        }
        Some(FactoryCommand::Stop(args)) => {
            stop(
                &FactorydClient::new(&cli.factoryd_url)?,
                validate_job_id(&args.job_id)?,
                args.json,
            )
            .await
        }
        Some(FactoryCommand::Export(args)) => {
            export_job_result(
                &FactorydClient::new(&cli.factoryd_url)?,
                validate_job_id(&args.job_id)?,
                args.output,
            )
            .await
        }
        Some(FactoryCommand::Apply(args)) => {
            let root =
                local_repository_root(args.cwd.as_deref().unwrap_or_else(|| Path::new(".")))?;
            let repository_id = required_env("FACTORY_HOST_REPOSITORY_ID")?;
            validate_repository_id("local", &repository_id)?;
            let job_id = validate_job_id(&args.job_id)?;
            apply_result(
                &FactorydClient::new(&cli.factoryd_url)?,
                &job_id,
                &root,
                &repository_id,
                false,
            )
            .await?;
            Ok(0)
        }
        None => run_job(&FactorydClient::new(&cli.factoryd_url)?, RunArgs::default()).await,
    }
}

async fn run_job(client: &FactorydClient, args: RunArgs) -> Result<i32> {
    let output_mode = OutputMode::from_flags(args.json, args.verbose);
    let repository = repository_location(&args)?;
    let task = task_text(args.task)?;
    let definition = job_definition(
        task,
        execution_profile_from_env()?,
        repository.repository_id.clone(),
    );
    let created = client.create_job(&definition).await?;
    let job_id = created.job.job_id.clone();
    output::print_created(&created, output_mode.json())?;
    let workspace = match client
        .ensure_workspace(
            &job_id,
            &EnsureWorkspaceRequest {
                repository_id: repository.repository_id.clone(),
                repository: repository.repository.clone(),
                base_ref: repository.base_ref.clone(),
            },
        )
        .await
    {
        Ok(workspace) => workspace,
        Err(error) => {
            return Err(error.context(format!(
                "create managed worktree for job {job_id}; the durable job was not cancelled because clone or fetch may still be running; inspect it with `factory status {job_id}` or stop it explicitly with `factory stop {job_id}`"
            )));
        }
    };

    output::print_workspace_ready(&created, &workspace.root, args.detach, output_mode.json())?;
    if args.detach {
        return Ok(0);
    }
    let exit_code = attach(client, job_id.clone(), output_mode).await?;
    if exit_code == 0
        && !args.no_apply
        && let Some(root) = repository.local_root
    {
        apply_result(
            client,
            &job_id,
            &root,
            &repository.repository_id,
            output_mode.json(),
        )
        .await?;
    }
    Ok(exit_code)
}

async fn attach(client: &FactorydClient, job_id: JobId, output_mode: OutputMode) -> Result<i32> {
    if output_mode == OutputMode::Interactive {
        return monitor_interactive(client, job_id).await;
    }

    let monitor = monitor_job(client, job_id.clone(), output_mode);
    tokio::pin!(monitor);
    tokio::select! {
        result = &mut monitor => result,
        signal = tokio::signal::ctrl_c() => {
            signal.context("listen for Ctrl-C")?;
            output::print_detached(&job_id, output_mode.json())?;
            Ok(130)
        }
    }
}

async fn monitor_job(
    client: &FactorydClient,
    job_id: JobId,
    output_mode: OutputMode,
) -> Result<i32> {
    let mut cursor = 0;
    let mut previous_snapshot = None;
    let mut transcript = Transcript::default();
    loop {
        cursor = drain_events(client, &job_id, cursor, output_mode, &mut transcript).await?;

        let job = client.load_job(&job_id).await?;
        transcript.correlate_job(&job);
        let snapshot = output::job_snapshot(&job)?;
        if previous_snapshot.as_deref() != Some(snapshot.as_str()) {
            match output_mode {
                OutputMode::Compact => output::print_progress_compact(&job),
                OutputMode::Verbose => output::print_progress(&job),
                OutputMode::Interactive | OutputMode::Json => {}
            }
        }
        previous_snapshot = Some(snapshot);

        if output::terminal(job.job.state) {
            // Stage completion and terminal job state commit atomically. The
            // state request can therefore observe terminal immediately after
            // the preceding event page. Drain once more from the exact cursor
            // before printing the terminal result so that final event is not
            // omitted from attach or its JSON replay.
            let _ = drain_events(client, &job_id, cursor, output_mode, &mut transcript).await?;
            transcript.correlate_job(&job);
            let (stages, attempts) = tokio::try_join!(
                client.list_stage_checkpoints(&job_id),
                client.list_attempts(&job_id),
            )?;
            let results = if output_mode == OutputMode::Compact {
                transcript.stage_results()
            } else {
                Vec::new()
            };
            output::print_final(&job, &stages, &attempts, &results, output_mode.json())?;
            return Ok(output::job_state_exit_code(job.job.state));
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

async fn monitor_interactive(client: &FactorydClient, job_id: JobId) -> Result<i32> {
    let mut cursor = 0;
    let mut transcript = Transcript::default();
    let mut screen = LiveScreen::new()?;
    loop {
        cursor = drain_events(
            client,
            &job_id,
            cursor,
            OutputMode::Interactive,
            &mut transcript,
        )
        .await?;
        let job = client.load_job(&job_id).await?;
        transcript.correlate_job(&job);
        screen.draw(&job, &transcript, output::terminal(job.job.state))?;

        if output::terminal(job.job.state) {
            let _ = drain_events(
                client,
                &job_id,
                cursor,
                OutputMode::Interactive,
                &mut transcript,
            )
            .await?;
            transcript.correlate_job(&job);
            let (stages, attempts) = tokio::try_join!(
                client.list_stage_checkpoints(&job_id),
                client.list_attempts(&job_id),
            )?;
            screen.inspect_completed(&job, &transcript, Duration::from_secs(3))?;
            screen.restore()?;
            let results = transcript.stage_results();
            output::print_final(&job, &stages, &attempts, &results, false)?;
            return Ok(output::job_state_exit_code(job.job.state));
        }

        if matches!(
            screen.wait_for_action(POLL_INTERVAL, transcript.rows().len())?,
            LiveAction::Detach
        ) {
            screen.restore()?;
            output::print_detached(&job_id, false)?;
            return Ok(130);
        }
    }
}

async fn drain_events(
    client: &FactorydClient,
    job_id: &JobId,
    mut cursor: u64,
    output_mode: OutputMode,
    transcript: &mut Transcript,
) -> Result<u64> {
    loop {
        let page = client.list_events(job_id, cursor, EVENT_PAGE_SIZE).await?;
        let count = page.events.len();
        for event in &page.events {
            match output_mode {
                OutputMode::Compact => {
                    transcript.ingest(event);
                    output::print_compact_event(event);
                }
                OutputMode::Json => output::print_event(event, true)?,
                OutputMode::Verbose => output::print_event(event, false)?,
                OutputMode::Interactive => transcript.ingest(event),
            }
        }
        cursor = page.next_cursor;
        if count < EVENT_PAGE_SIZE as usize {
            return Ok(cursor);
        }
    }
}

async fn status(client: &FactorydClient, job_id: JobId, json_output: bool) -> Result<i32> {
    let (job, stages, attempts) = tokio::try_join!(
        client.load_job(&job_id),
        client.list_stage_checkpoints(&job_id),
        client.list_attempts(&job_id),
    )?;
    if json_output {
        output::print_status_json(&job, &stages, &attempts)?;
    } else {
        output::print_progress(&job);
        if output::terminal(job.job.state) {
            output::print_final(&job, &stages, &attempts, &[], false)?;
        }
    }
    Ok(output::job_state_exit_code(job.job.state))
}

async fn stop(client: &FactorydClient, job_id: JobId, json_output: bool) -> Result<i32> {
    let job = client.cancel_job(&job_id).await?;
    output::print_stopped(&job, json_output)?;
    Ok(0)
}

async fn export_job_result(
    client: &FactorydClient,
    job_id: JobId,
    output: Option<PathBuf>,
) -> Result<i32> {
    let result = client.export_result(&job_id).await?;
    verify_patch_digest(&result.patch, &result.patch_sha256)?;
    let output = output.unwrap_or_else(|| PathBuf::from(format!("factory-{job_id}.patch")));
    if output == Path::new("-") {
        std::io::stdout()
            .write_all(&result.patch)
            .context("write Factory result patch to stdout")?;
        std::io::stdout().flush().context("flush result patch")?;
        return Ok(0);
    }
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&output)
        .with_context(|| format!("create result patch {}", output.display()))?;
    file.write_all(&result.patch)
        .with_context(|| format!("write result patch {}", output.display()))?;
    file.flush()
        .with_context(|| format!("flush result patch {}", output.display()))?;
    println!("Exported job {job_id} to {}", output.display());
    Ok(0)
}

async fn apply_result(
    client: &FactorydClient,
    job_id: &JobId,
    root: &Path,
    repository_id: &str,
    json_output: bool,
) -> Result<()> {
    let result = client.export_result(job_id).await?;
    apply_exported_result_for_checkout(job_id, root, repository_id, &result, json_output)
}

fn apply_exported_result_for_checkout(
    job_id: &JobId,
    root: &Path,
    local_repository_id: &str,
    result: &ExportedResult,
    json_output: bool,
) -> Result<()> {
    verify_patch_digest(&result.patch, &result.patch_sha256)?;
    let repository_id = checkout_repository_id(root, local_repository_id, &result.repository_id)?;
    apply_exported_result(job_id, root, &repository_id, result, json_output)
}

fn checkout_repository_id(
    root: &Path,
    local_repository_id: &str,
    expected_repository_id: &str,
) -> Result<String> {
    if expected_repository_id.starts_with("local:") {
        validate_repository_id("local", local_repository_id)?;
        return Ok(local_repository_id.to_string());
    }

    validate_repository_id("remote", expected_repository_id)?;
    let origin = git_output(root, &["remote", "get-url", "origin"]).with_context(|| {
        format!(
            "read origin URL from {}; no files were changed",
            root.display()
        )
    })?;
    let origin =
        String::from_utf8(origin).context("Git origin URL is not UTF-8; no files were changed")?;
    let origin = normalize_remote_repository(&origin)
        .context("normalize Git origin URL; no files were changed")?;
    Ok(hashed_repository_id("remote", &origin))
}

fn apply_exported_result(
    job_id: &JobId,
    root: &Path,
    repository_id: &str,
    result: &ExportedResult,
    json_output: bool,
) -> Result<()> {
    verify_patch_digest(&result.patch, &result.patch_sha256)?;
    if result.repository_id != repository_id {
        return Err(anyhow!(
            "job {job_id} belongs to repository {}, not this checkout {}; no files were changed",
            result.repository_id,
            repository_id
        ));
    }
    let head = git_output(root, &["rev-parse", "HEAD"])?;
    let head = String::from_utf8(head).context("Git HEAD is not UTF-8")?;
    if head.trim() != result.base_revision {
        return Err(anyhow!(
            "job {job_id} was based on {}, but this checkout is at {}; no files were changed; use `factory export {job_id}` to preserve the result",
            result.base_revision,
            head.trim()
        ));
    }
    let status = git_output(
        root,
        &["status", "--porcelain=v1", "-z", "--untracked-files=all"],
    )?;
    if !status.is_empty() {
        return Err(anyhow!(
            "this checkout has uncommitted or untracked work; no files were changed; commit or stash it, then run `factory apply {job_id}`, or use `factory export {job_id}`"
        ));
    }
    if !result.patch.is_empty() {
        git_apply(root, &result.patch, true).with_context(|| {
            format!("job {job_id} does not apply cleanly; no files were changed")
        })?;
        git_apply(root, &result.patch, false).with_context(|| {
            format!("apply job {job_id}; Git rejected the patch after its clean preflight")
        })?;
    }
    if json_output {
        println!(
            "{}",
            json!({
                "kind": "resultApplied",
                "jobId": job_id,
                "repositoryId": repository_id,
                "baseRevision": result.base_revision,
                "patchSha256": result.patch_sha256,
                "changed": !result.patch.is_empty(),
            })
        );
    } else if result.patch.is_empty() {
        println!("Job {job_id} made no repository changes.");
    } else {
        println!("Applied job {job_id} to {}", root.display());
    }
    Ok(())
}

fn verify_patch_digest(patch: &[u8], expected: &str) -> Result<()> {
    let actual = format!("{:x}", Sha256::digest(patch));
    if actual != expected {
        return Err(anyhow!(
            "Factory result digest mismatch: expected {expected}, received {actual}"
        ));
    }
    Ok(())
}

fn git_output(root: &Path, args: &[&str]) -> Result<Vec<u8>> {
    let output = ProcessCommand::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .with_context(|| format!("run git {} in {}", args.join(" "), root.display()))?;
    if !output.status.success() {
        return Err(anyhow!(
            "git {} failed with {}: {}",
            args.join(" "),
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(output.stdout)
}

fn git_apply(root: &Path, patch: &[u8], check: bool) -> Result<()> {
    let mut command = ProcessCommand::new("git");
    command.arg("-C").arg(root).arg("apply");
    if check {
        command.arg("--check");
    }
    let mut child = command
        .args(["--binary", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("start git apply in {}", root.display()))?;
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| anyhow!("git apply stdin was unavailable"))?;
    stdin
        .write_all(patch)
        .context("send result patch to git apply")?;
    drop(stdin);
    let output = child.wait_with_output().context("wait for git apply")?;
    if !output.status.success() {
        return Err(anyhow!(
            "git apply failed with {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(())
}

fn task_text(parts: Vec<String>) -> Result<String> {
    let task = parts.join(" ").trim().to_string();
    if !task.is_empty() {
        return Ok(task);
    }
    if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
        return Err(anyhow!(
            "a task is required when Factory is not attached to a terminal"
        ));
    }
    print!("Task: ");
    std::io::stdout().flush().context("show task prompt")?;
    let mut task = String::new();
    std::io::stdin()
        .read_line(&mut task)
        .context("read task from terminal")?;
    let task = task.trim().to_string();
    if task.is_empty() {
        return Err(anyhow!("task must not be empty"));
    }
    Ok(task)
}

fn repository_location(args: &RunArgs) -> Result<RepositoryLocation> {
    if let Some(repository) = &args.repository {
        let repository = normalize_remote_repository(repository)?;
        validate_remote_repository_url(&repository)?;
        let base_ref = args.base_ref.as_deref().unwrap_or("HEAD").trim();
        if base_ref.is_empty() {
            return Err(anyhow!("base ref must not be empty"));
        }
        return Ok(RepositoryLocation {
            repository_id: hashed_repository_id("remote", &repository),
            repository,
            base_ref: base_ref.to_string(),
            local_root: None,
        });
    }

    let cwd = args.cwd.as_deref().unwrap_or_else(|| Path::new("."));
    let root = local_repository_root(cwd)?;
    let repository_id = std::env::var("FACTORY_HOST_REPOSITORY_ID")
        .context("FACTORY_HOST_REPOSITORY_ID is missing; invoke the Rust CLI through the host `factory` launcher")?;
    validate_repository_id("local", &repository_id)?;
    Ok(RepositoryLocation {
        repository_id,
        repository: root
            .to_str()
            .ok_or_else(|| anyhow!("Git repository path is not UTF-8: {}", root.display()))?
            .to_string(),
        base_ref: "HEAD".to_string(),
        local_root: Some(root),
    })
}

fn local_repository_root(cwd: &Path) -> Result<PathBuf> {
    let cwd = std::fs::canonicalize(cwd)
        .with_context(|| format!("resolve local repository path {}", cwd.display()))?;
    let result = ProcessCommand::new("git")
        .arg("-C")
        .arg(&cwd)
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .with_context(|| format!("inspect Git repository at {}", cwd.display()))?;
    if !result.status.success() {
        let detail = String::from_utf8_lossy(&result.stderr).trim().to_string();
        return Err(anyhow!(
            "{} is not inside a Git repository: {detail}",
            cwd.display()
        ));
    }
    let root = String::from_utf8(result.stdout).context("Git repository path is not UTF-8")?;
    let root = std::fs::canonicalize(root.trim()).context("resolve Git repository root")?;
    Ok(root)
}

fn execution_profile_from_env() -> Result<ExecutionProfile> {
    let configured_provider = required_env("FACTORY_PROVIDER_ADAPTER")?;
    let provider = provider_profile(&configured_provider)
        .ok_or_else(|| anyhow!("unknown provider profile {configured_provider:?}"))?
        .id
        .to_string();
    let model = required_env("FACTORY_MODEL")?;
    Ok(ExecutionProfile { provider, model })
}

fn required_env(name: &str) -> Result<String> {
    let value = std::env::var(name).with_context(|| format!("{name} is not configured"))?;
    let value = value.trim();
    if value.is_empty() {
        return Err(anyhow!("{name} is not configured"));
    }
    Ok(value.to_string())
}

fn normalize_remote_repository(value: &str) -> Result<String> {
    let value = value.trim().trim_end_matches('/');
    if value.is_empty() {
        return Err(anyhow!("repository must not be empty"));
    }
    Ok(value.to_string())
}

fn validate_remote_repository_url(value: &str) -> Result<()> {
    let scheme_url = value.split_once("://").is_some_and(|(scheme, remainder)| {
        matches!(scheme, "http" | "https" | "ssh" | "git") && !remainder.is_empty()
    });
    let scp_url = value.split_once(':').is_some_and(|(authority, path)| {
        authority.contains('@')
            && !authority.chars().any(char::is_whitespace)
            && !path.is_empty()
            && !path.chars().any(char::is_whitespace)
    });
    if scheme_url || scp_url {
        return Ok(());
    }
    Err(anyhow!(
        "`--repository` accepts a remote Git URL, not a host path; for a local checkout, run `cd <repo> && factory run ...`"
    ))
}

fn hashed_repository_id(kind: &str, value: &str) -> String {
    let digest = Sha256::digest(value.as_bytes());
    format!("{kind}:{digest:x}")
}

fn validate_repository_id(kind: &str, value: &str) -> Result<()> {
    let Some(digest) = value.strip_prefix(&format!("{kind}:")) else {
        return Err(anyhow!("invalid {kind} repository identity"));
    };
    if digest.len() != 64 || !digest.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(anyhow!("invalid {kind} repository identity"));
    }
    Ok(())
}

fn job_definition(
    task: String,
    execution_profile: ExecutionProfile,
    repository_id: String,
) -> JobDefinition {
    const OPERATIONS: [&str; 4] = [
        "codex.plan",
        "codex.execute",
        "codex.review",
        "codex.remediate",
    ];
    JobDefinition {
        kind: "factory.task".to_string(),
        input: serde_json::to_value(FactoryTaskInput {
            task,
            execution_profile: Some(execution_profile),
            repository_id: Some(repository_id),
            developer_instructions: Some(AUTONOMOUS_INSTRUCTIONS.to_string()),
        })
        .expect("TaskInput always serializes"),
        operations: OPERATIONS
            .into_iter()
            .map(|kind| OperationDefinition {
                kind: kind.to_string(),
                input: json!({}),
                max_attempts: 3,
            })
            .collect(),
    }
}

fn validate_job_id(job_id: &str) -> Result<JobId> {
    if job_id.trim().is_empty() {
        return Err(anyhow!("job ID must not be empty"));
    }
    Ok(JobId::new(job_id.trim()))
}

fn normalize_shorthand(args: impl IntoIterator<Item = OsString>) -> Vec<OsString> {
    let mut args = args.into_iter().collect::<Vec<_>>();
    let mut index = 1;
    while index < args.len() {
        let value = args[index].to_string_lossy();
        if matches!(value.as_ref(), "--factoryd-url" | "--config-file") {
            index += 2;
            continue;
        }
        if value.starts_with("--factoryd-url=") || value.starts_with("--config-file=") {
            index += 1;
            continue;
        }
        if matches!(
            value.as_ref(),
            "configure"
                | "provider"
                | "model"
                | "run"
                | "attach"
                | "status"
                | "stop"
                | "export"
                | "apply"
                | "help"
        ) {
            return args;
        }
        if matches!(value.as_ref(), "--help" | "-h" | "--version" | "-V") {
            return args;
        }
        args.insert(index, OsString::from("run"));
        return args;
    }
    args
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestRoot(PathBuf);

    impl TestRoot {
        fn new() -> Self {
            Self(std::env::temp_dir().join(format!(
                "factory-cli-result-test-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            )))
        }
    }

    impl Drop for TestRoot {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn git(root: &Path, args: &[&str]) -> String {
        String::from_utf8(git_output(root, args).unwrap())
            .unwrap()
            .trim()
            .to_string()
    }

    #[test]
    fn result_apply_refuses_every_conflict_before_mutating_host() {
        let fixture = TestRoot::new();
        std::fs::create_dir_all(&fixture.0).unwrap();
        let host = fixture.0.join("host");
        let result_checkout = fixture.0.join("result");
        ProcessCommand::new("git")
            .args(["init", "-b", "main"])
            .arg(&host)
            .status()
            .unwrap();
        git(&host, &["config", "user.name", "Factory CLI Test"]);
        git(
            &host,
            &["config", "user.email", "factory-cli@example.invalid"],
        );
        std::fs::write(host.join("README.md"), b"base\n").unwrap();
        git(&host, &["add", "README.md"]);
        git(&host, &["commit", "-m", "base"]);
        let base_revision = git(&host, &["rev-parse", "HEAD"]);
        ProcessCommand::new("git")
            .args(["clone", "--"])
            .arg(&host)
            .arg(&result_checkout)
            .status()
            .unwrap();
        std::fs::write(result_checkout.join("README.md"), b"Factory result\n").unwrap();
        let patch = git_output(
            &result_checkout,
            &[
                "diff",
                "--binary",
                "--full-index",
                "--no-renames",
                "HEAD",
                "--",
            ],
        )
        .unwrap();
        let repository_id =
            "local:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let job_id = JobId::new("result-gate");
        let mut result = ExportedResult {
            repository_id: repository_id.to_string(),
            base_revision: base_revision.clone(),
            patch_sha256: format!("{:x}", Sha256::digest(&patch)),
            patch,
        };

        let error = apply_exported_result(
            &job_id,
            &host,
            "local:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            &result,
            false,
        )
        .unwrap_err();
        assert!(error.to_string().contains("no files were changed"));
        assert_eq!(std::fs::read(host.join("README.md")).unwrap(), b"base\n");
        assert!(git(&host, &["status", "--porcelain"]).is_empty());

        result.base_revision = "0000000000000000000000000000000000000000".to_string();
        let error =
            apply_exported_result(&job_id, &host, repository_id, &result, false).unwrap_err();
        assert!(error.to_string().contains("no files were changed"));
        assert_eq!(std::fs::read(host.join("README.md")).unwrap(), b"base\n");
        result.base_revision = base_revision;

        std::fs::write(host.join("local.txt"), b"user work\n").unwrap();
        let error =
            apply_exported_result(&job_id, &host, repository_id, &result, false).unwrap_err();
        assert!(error.to_string().contains("no files were changed"));
        assert_eq!(std::fs::read(host.join("README.md")).unwrap(), b"base\n");
        assert_eq!(
            std::fs::read(host.join("local.txt")).unwrap(),
            b"user work\n"
        );

        std::fs::remove_file(host.join("local.txt")).unwrap();
        apply_exported_result(&job_id, &host, repository_id, &result, false).unwrap();
        assert_eq!(
            std::fs::read(host.join("README.md")).unwrap(),
            b"Factory result\n"
        );
    }

    #[test]
    fn remote_result_applies_to_clone_with_matching_normalized_origin() {
        let fixture = TestRoot::new();
        std::fs::create_dir_all(&fixture.0).unwrap();
        let origin = fixture.0.join("origin");
        let host = fixture.0.join("host");
        let result_checkout = fixture.0.join("result");
        ProcessCommand::new("git")
            .args(["init", "-b", "main"])
            .arg(&origin)
            .status()
            .unwrap();
        git(&origin, &["config", "user.name", "Factory CLI Test"]);
        git(
            &origin,
            &["config", "user.email", "factory-cli@example.invalid"],
        );
        std::fs::write(origin.join("README.md"), b"base\n").unwrap();
        git(&origin, &["add", "README.md"]);
        git(&origin, &["commit", "-m", "base"]);
        let base_revision = git(&origin, &["rev-parse", "HEAD"]);
        for checkout in [&host, &result_checkout] {
            ProcessCommand::new("git")
                .args(["clone", "--"])
                .arg(&origin)
                .arg(checkout)
                .status()
                .unwrap();
        }

        let origin_with_slash = format!("{}/", origin.display());
        git(&host, &["remote", "set-url", "origin", &origin_with_slash]);
        let normalized_origin = normalize_remote_repository(&origin_with_slash).unwrap();
        let repository_id = hashed_repository_id("remote", &normalized_origin);
        std::fs::write(result_checkout.join("README.md"), b"remote result\n").unwrap();
        let patch = git_output(
            &result_checkout,
            &[
                "diff",
                "--binary",
                "--full-index",
                "--no-renames",
                "HEAD",
                "--",
            ],
        )
        .unwrap();
        let result = ExportedResult {
            repository_id,
            base_revision,
            patch_sha256: format!("{:x}", Sha256::digest(&patch)),
            patch,
        };

        apply_exported_result_for_checkout(
            &JobId::new("remote-result-gate"),
            &host,
            "local:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            &result,
            false,
        )
        .unwrap();
        assert_eq!(
            std::fs::read(host.join("README.md")).unwrap(),
            b"remote result\n"
        );
    }

    #[test]
    fn repository_argument_accepts_only_remote_git_urls() {
        for repository in [
            "https://github.com/example/project.git",
            "ssh://git@github.com/example/project.git",
            "git://github.com/example/project.git",
            "git@github.com:example/project.git",
        ] {
            validate_remote_repository_url(repository).unwrap();
        }

        for repository in [
            "/tmp/project",
            "../project",
            ".",
            "project",
            "file:///tmp/project",
        ] {
            let error = validate_remote_repository_url(repository).unwrap_err();
            assert!(error.to_string().contains("cd <repo> && factory run"));
        }
    }

    #[test]
    fn run_help_advertises_remote_urls_and_hides_checkout_overrides() {
        let mut command = <Cli as clap::CommandFactory>::command();
        let run = command.find_subcommand_mut("run").unwrap();
        let help = run.render_long_help().to_string();
        assert!(help.contains("Remote Git URL"));
        assert!(help.contains("run Factory from that repository"));
        assert!(!help.contains("--cwd"));
    }
}
