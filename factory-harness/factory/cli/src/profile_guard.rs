use anyhow::Result;
use anyhow::anyhow;
use factory_coordinator::DurableJob;
use factory_coordinator::ExecutionProfile;
use factory_coordinator::FactoryTaskInput;
use factory_coordinator::JobState;

use crate::api::FactorydClient;

pub enum ExistingProfile {
    Unconfigured,
    Complete(ExecutionProfile),
    Partial,
}

pub async fn ensure_profile_change_is_safe(
    client: &FactorydClient,
    current: &ExistingProfile,
    requested: &ExecutionProfile,
    force: bool,
) -> Result<()> {
    if matches!(current, ExistingProfile::Complete(profile) if profile == requested) {
        return Ok(());
    }

    let jobs = match client.list_active_jobs().await {
        Ok(jobs) => jobs,
        Err(error) if force => {
            eprintln!(
                "Warning: forcing provider/model configuration to {} / {} without checking active jobs: {error:#}\nThis changes configuration only; it does not stop or migrate any job.",
                requested.provider, requested.model
            );
            return Ok(());
        }
        // First-time onboarding must not depend on a running coordinator. If
        // one is available, however, its retained jobs are still respected.
        Err(_) if matches!(current, ExistingProfile::Unconfigured) => return Ok(()),
        Err(error) => {
            return Err(anyhow!(
                "cannot switch provider/model because Factory could not check active jobs: {error:#}\nStart PostgreSQL and factoryd, then retry. To accept the risk of stranding active jobs, retry with `--force`."
            ));
        }
    };
    let blockers = jobs
        .iter()
        .filter_map(|job| blocking_requirement(job, requested))
        .collect::<Vec<_>>();
    if blockers.is_empty() {
        return Ok(());
    }

    let details = blockers
        .iter()
        .map(format_blocker)
        .collect::<Vec<_>>()
        .join("\n");
    if force {
        eprintln!(
            "Warning: forcing provider/model switch to {} / {} while these active jobs require another or unknown profile:\n{details}\nThis changes configuration only; it does not stop or migrate those jobs.",
            requested.provider, requested.model
        );
        return Ok(());
    }

    Err(anyhow!(
        "cannot switch provider/model to {} / {} while these active jobs require another or unknown profile:\n{details}\nWait for them to finish, stop them, or serve their pinned profile. To accept stranding them, retry this command with `--force`.",
        requested.provider,
        requested.model
    ))
}

struct BlockingJob<'a> {
    job: &'a DurableJob,
    profile: Option<ExecutionProfile>,
}

fn blocking_requirement<'a>(
    job: &'a DurableJob,
    requested: &ExecutionProfile,
) -> Option<BlockingJob<'a>> {
    if job.job.kind != "factory.task"
        || !matches!(
            job.job.state,
            JobState::Queued | JobState::Running | JobState::Cancelling
        )
    {
        return None;
    }
    let profile = serde_json::from_value::<FactoryTaskInput>(job.job.input.clone())
        .ok()
        .and_then(|input| input.execution_profile)
        .and_then(normalize_job_profile);
    if profile.as_ref() == Some(requested) {
        return None;
    }
    Some(BlockingJob { job, profile })
}

fn format_blocker(blocker: &BlockingJob<'_>) -> String {
    let job_id = shell_quote(blocker.job.job.job_id.as_str());
    let mut output = format!(
        "  - {} [{}]",
        blocker.job.job.job_id,
        job_state_label(blocker.job.job.state)
    );
    if let Some(profile) = &blocker.profile {
        output.push_str(&format!(
            ": {} / {}\n    serve: factory configure --provider {} --model {}",
            profile.provider,
            profile.model,
            shell_quote(&profile.provider),
            shell_quote(&profile.model)
        ));
    } else {
        output.push_str(": legacy profile is unpinned or invalid (unknown; not guessed)");
    }
    output.push_str(&format!(
        "\n    inspect: factory status {job_id}\n    stop: factory stop {job_id}"
    ));
    output
}

fn normalize_job_profile(profile: ExecutionProfile) -> Option<ExecutionProfile> {
    let provider = profile.provider.trim();
    let model = profile.model.trim();
    if model.is_empty() || !matches!(provider, "openai" | "anthropic" | "deepseek" | "zai") {
        return None;
    }
    Some(ExecutionProfile {
        provider: provider.to_string(),
        model: model.to_string(),
    })
}

fn job_state_label(state: JobState) -> &'static str {
    match state {
        JobState::Queued => "queued",
        JobState::Running => "running",
        JobState::Cancelling => "cancelling",
        JobState::Succeeded => "succeeded",
        JobState::Failed => "failed",
        JobState::Cancelled => "cancelled",
    }
}

fn shell_quote(value: &str) -> String {
    if !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.' | b'/'))
    {
        return value.to_string();
    }
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands_quote_profile_and_job_values_for_a_shell() {
        assert_eq!(shell_quote("deepseek-v4-pro"), "deepseek-v4-pro");
        assert_eq!(shell_quote("model $VALUE's"), "'model $VALUE'\"'\"'s'");
    }

    #[test]
    fn only_canonical_job_profiles_are_valid() {
        assert!(
            normalize_job_profile(ExecutionProfile {
                provider: "claude".to_string(),
                model: "claude-sonnet-5".to_string(),
            })
            .is_none()
        );
        assert_eq!(
            normalize_job_profile(ExecutionProfile {
                provider: "anthropic".to_string(),
                model: "claude-sonnet-5".to_string(),
            }),
            Some(ExecutionProfile {
                provider: "anthropic".to_string(),
                model: "claude-sonnet-5".to_string(),
            })
        );
        assert!(
            normalize_job_profile(ExecutionProfile {
                provider: "removed-provider".to_string(),
                model: "model".to_string(),
            })
            .is_none()
        );
        assert!(
            normalize_job_profile(ExecutionProfile {
                provider: "openai".to_string(),
                model: "  ".to_string(),
            })
            .is_none()
        );
    }
}
