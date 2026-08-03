use anyhow::Result;
use anyhow::anyhow;
use factory_coordinator::ArtifactManager;
use factory_coordinator::CompletedStageOutput;
use factory_coordinator::DurableJob;
use factory_coordinator::JobArtifactFile;
use factory_coordinator::JobEventRecord;
use factory_coordinator::reduce_settled_job_outputs;
use factory_coordinator::render_job_result;
use serde::Serialize;

use crate::api::FactorydClient;

const EVENT_PAGE_SIZE: u32 = 1_000;

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ArtifactFileMetadata {
    pub name: &'static str,
    pub available: bool,
    pub local_available: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ArtifactMetadata {
    pub ownership: &'static str,
    pub files: Vec<ArtifactFileMetadata>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub local_directory: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result_path: Option<String>,
    pub message: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct FullResult {
    pub markdown: String,
    pub artifacts: ArtifactMetadata,
}

pub(crate) async fn load_full_result(
    client: &FactorydClient,
    job: &DurableJob,
) -> Result<FullResult> {
    let manager = ArtifactManager::from_env().map_err(|error| anyhow!(error.to_string()))?;
    let paths = manager.job_paths(&job.job.job_id, repository_id(job));
    let guard = manager
        .acquire_publication_guard(&paths)
        .await
        .map_err(|error| anyhow!("lock artifact projection: {error}"))?;
    let job = client.load_job(&job.job.job_id).await?;
    let inventory_warning_count = manager
        .initialize_missing_job_files(&paths, &job, &guard)
        .await
        .map_err(|error| anyhow!("repair job artifact inventory: {error}"))?
        .len();
    let events = load_all_events(client, &job.job.job_id).await?;
    let outputs = reduce_settled_job_outputs(&job, &events)
        .map_err(|error| anyhow!("reconstruct result from durable events: {error}"))?;
    if outputs.is_empty() {
        return Err(anyhow!(
            "job {} has no completed stage result yet",
            job.job.job_id
        ));
    }
    let warnings = manager
        .publish_settled_outputs(&paths, &outputs, &guard)
        .await
        .map_err(|error| anyhow!("republish settled artifacts: {error}"))?;
    let artifacts = inspect_artifacts(
        &manager,
        &paths,
        &job,
        &outputs,
        inventory_warning_count + warnings.len(),
    )
    .await?;
    Ok(FullResult {
        markdown: render_job_result(&outputs),
        artifacts,
    })
}

/// Inventories whatever is materialized now, including queued and partially
/// completed jobs that do not have a final result yet.
pub(crate) async fn load_artifacts(
    client: &FactorydClient,
    job: &DurableJob,
) -> Result<ArtifactMetadata> {
    let manager = ArtifactManager::from_env().map_err(|error| anyhow!(error.to_string()))?;
    let paths = manager.job_paths(&job.job.job_id, repository_id(job));
    let guard = manager
        .acquire_publication_guard(&paths)
        .await
        .map_err(|error| anyhow!("lock artifact projection: {error}"))?;
    let job = client.load_job(&job.job.job_id).await?;
    let inventory_warning_count = manager
        .initialize_missing_job_files(&paths, &job, &guard)
        .await
        .map_err(|error| anyhow!("repair job artifact inventory: {error}"))?
        .len();
    let events = load_all_events(client, &job.job.job_id).await?;
    let outputs = reduce_settled_job_outputs(&job, &events)
        .map_err(|error| anyhow!("reconstruct settled artifact inventory: {error}"))?;
    if outputs.is_empty() {
        return inspect_artifacts(&manager, &paths, &job, &[], inventory_warning_count).await;
    }
    let warnings = manager
        .publish_settled_outputs(&paths, &outputs, &guard)
        .await
        .map_err(|error| anyhow!("republish settled artifacts: {error}"))?;
    inspect_artifacts(
        &manager,
        &paths,
        &job,
        &outputs,
        inventory_warning_count + warnings.len(),
    )
    .await
}

async fn inspect_artifacts(
    manager: &ArtifactManager,
    paths: &factory_coordinator::ArtifactPaths,
    job: &DurableJob,
    outputs: &[CompletedStageOutput],
    projection_warning_count: usize,
) -> Result<ArtifactMetadata> {
    let mut files = Vec::with_capacity(JobArtifactFile::ALL.len());
    for file in JobArtifactFile::ALL {
        let contents = manager
            .read_job_file(paths, file)
            .await
            .map_err(|error| anyhow!("read {}: {error}", file.file_name()))?;
        let current = artifact_is_current(file, outputs);
        files.push(ArtifactFileMetadata {
            name: file.file_name(),
            available: contents.is_some() && current,
            local_available: current && manager.projected_file_matches(paths, file).await,
        });
    }

    Ok(artifact_metadata(
        &job.job.job_id,
        files,
        projection_warning_count,
    ))
}

fn repository_id(job: &DurableJob) -> &str {
    job.job
        .input
        .get("repositoryId")
        .and_then(|value| value.as_str())
        .unwrap_or_default()
}

fn artifact_is_current(file: JobArtifactFile, outputs: &[CompletedStageOutput]) -> bool {
    match file {
        JobArtifactFile::Job | JobArtifactFile::Task => true,
        JobArtifactFile::Findings => outputs
            .last()
            .is_some_and(|output| output.findings.is_some()),
        JobArtifactFile::Result => !outputs.is_empty(),
        _ => outputs
            .iter()
            .any(|output| JobArtifactFile::for_stage(&output.stage) == Some(file)),
    }
}

fn artifact_metadata(
    job_id: &factory_coordinator::JobId,
    files: Vec<ArtifactFileMetadata>,
    projection_warning_count: usize,
) -> ArtifactMetadata {
    let any_local = files.iter().any(|file| file.local_available);
    let local_result = files
        .iter()
        .any(|file| file.name == JobArtifactFile::Result.file_name() && file.local_available);
    let local_directory = any_local.then(|| format!(".factory/jobs/{job_id}/"));
    let result_path = local_result.then(|| format!(".factory/jobs/{job_id}/result.md"));
    ArtifactMetadata {
        ownership: if any_local { "local" } else { "coordinator" },
        files,
        local_directory,
        result_path,
        message: if projection_warning_count > 0 {
            format!(
                "{projection_warning_count} workspace artifact file(s) could not be refreshed; `factory result {job_id}` reconstructs the durable result from events."
            )
        } else if any_local {
            "Materialized artifacts are available in the current workspace.".to_string()
        } else {
            format!(
                "Artifacts are coordinator-owned for this job; `factory result {job_id}` remains readable."
            )
        },
    }
}

async fn load_all_events(
    client: &FactorydClient,
    job_id: &factory_coordinator::JobId,
) -> Result<Vec<JobEventRecord>> {
    let mut cursor = 0;
    let mut events = Vec::new();
    loop {
        let page = client.list_events(job_id, cursor, EVENT_PAGE_SIZE).await?;
        let count = page.events.len();
        cursor = page.next_cursor;
        events.extend(page.events);
        if count < EVENT_PAGE_SIZE as usize {
            return Ok(events);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn files() -> Vec<ArtifactFileMetadata> {
        JobArtifactFile::ALL
            .into_iter()
            .map(|file| ArtifactFileMetadata {
                name: file.file_name(),
                available: file == JobArtifactFile::Result,
                local_available: file == JobArtifactFile::Result,
            })
            .collect()
    }

    #[test]
    fn findings_availability_distinguishes_legacy_unknown_from_explicit_empty() {
        let mut outputs = vec![CompletedStageOutput {
            operation_id: factory_coordinator::OperationId::new("review-op"),
            stage: "review".to_string(),
            markdown: "Approved".to_string(),
            findings: None,
        }];
        assert!(!artifact_is_current(JobArtifactFile::Findings, &outputs));

        outputs[0].findings = Some(serde_json::json!([]));
        assert!(artifact_is_current(JobArtifactFile::Findings, &outputs));
    }

    #[test]
    fn matching_workspace_reports_host_readable_paths() {
        let metadata = artifact_metadata(&factory_coordinator::JobId::new("local-job"), files(), 0);
        assert_eq!(metadata.ownership, "local");
        assert_eq!(
            metadata.local_directory.as_deref(),
            Some(".factory/jobs/local-job/")
        );
        assert_eq!(
            metadata.result_path.as_deref(),
            Some(".factory/jobs/local-job/result.md")
        );
    }

    #[test]
    fn remote_or_mismatched_workspace_never_prints_container_paths() {
        let files = files()
            .into_iter()
            .map(|file| ArtifactFileMetadata {
                local_available: false,
                ..file
            })
            .collect();
        let metadata = artifact_metadata(&factory_coordinator::JobId::new("remote-job"), files, 0);
        assert_eq!(metadata.ownership, "coordinator");
        assert_eq!(metadata.local_directory, None);
        assert_eq!(metadata.result_path, None);
        assert!(metadata.message.contains("coordinator-owned"));
        assert!(metadata.message.contains("factory result remote-job"));
        assert!(!metadata.message.contains("/factory-artifacts"));
    }

    #[test]
    fn matching_identity_without_projected_files_prints_no_local_path() {
        let files = JobArtifactFile::ALL
            .into_iter()
            .map(|file| ArtifactFileMetadata {
                name: file.file_name(),
                available: file == JobArtifactFile::Job,
                local_available: false,
            })
            .collect();
        let metadata = artifact_metadata(
            &factory_coordinator::JobId::new("unprojected-job"),
            files,
            1,
        );
        assert_eq!(metadata.local_directory, None);
        assert_eq!(metadata.result_path, None);
        assert!(!metadata.message.contains(".factory/"));
    }
}
