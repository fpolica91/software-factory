use crate::CompletedStageOutput;
use crate::CoordinatorError;
use crate::DurableJob;
use crate::JobId;
use crate::Result;
use crate::WorkspaceRecord;
use std::fs::OpenOptions;
use std::path::Path;
use std::path::PathBuf;
use uuid::Uuid;

pub const FACTORY_ARTIFACT_ROOT_ENV: &str = "FACTORY_ARTIFACT_ROOT";

/// Factory-owned projection files. Agents never choose these names or write
/// these files directly.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum JobArtifactFile {
    Job,
    Task,
    Plan,
    Execute,
    Iterate,
    Review,
    Remediate,
    Result,
    Findings,
}

impl JobArtifactFile {
    pub const ALL: [Self; 9] = [
        Self::Job,
        Self::Task,
        Self::Plan,
        Self::Execute,
        Self::Iterate,
        Self::Review,
        Self::Remediate,
        Self::Result,
        Self::Findings,
    ];

    pub const fn file_name(self) -> &'static str {
        match self {
            Self::Job => "job.json",
            Self::Task => "task.md",
            Self::Plan => "plan.md",
            Self::Execute => "execute.md",
            Self::Iterate => "iterate.md",
            Self::Review => "review.md",
            Self::Remediate => "remediate.md",
            Self::Result => "result.md",
            Self::Findings => "findings.json",
        }
    }

    /// Repeated stages (continuation reviews, remediations, and iterates)
    /// project onto one file per stage kind holding the latest settled output.
    pub fn for_stage(stage: &str) -> Option<Self> {
        match stage {
            "plan" => Some(Self::Plan),
            "execute" => Some(Self::Execute),
            "iterate" => Some(Self::Iterate),
            "review" => Some(Self::Review),
            "remediate" => Some(Self::Remediate),
            _ => None,
        }
    }
}

/// Both directories contain disposable projections of durable coordinator
/// events. The coordinator copy lets detached jobs retain rendered files; a
/// matching local checkout contributes an optional workspace projection.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ArtifactPaths {
    coordinator_dir: PathBuf,
    local_projection_dir: Option<PathBuf>,
}

impl ArtifactPaths {
    pub fn coordinator_dir(&self) -> &Path {
        &self.coordinator_dir
    }

    pub fn local_projection_dir(&self) -> Option<&Path> {
        self.local_projection_dir.as_deref()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ArtifactManager {
    root: PathBuf,
    host_repository_id: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ArtifactProjectionWarning {
    file: JobArtifactFile,
    message: String,
}

/// Holds the per-job projection lock while its caller reloads durable events
/// and replaces rendered files.
pub struct ArtifactPublicationGuard {
    _file: std::fs::File,
}

impl ArtifactProjectionWarning {
    pub const fn file(&self) -> JobArtifactFile {
        self.file
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

impl ArtifactManager {
    pub fn from_env() -> Result<Self> {
        let root = std::env::var_os(FACTORY_ARTIFACT_ROOT_ENV)
            .map(PathBuf::from)
            .unwrap_or_else(|| std::env::temp_dir().join("software-factory/artifacts"));
        let host_repository_id = std::env::var("FACTORY_HOST_REPOSITORY_ID")
            .ok()
            .filter(|value| !value.trim().is_empty());
        Self::new(root, host_repository_id)
    }

    pub fn new(root: impl Into<PathBuf>, host_repository_id: Option<String>) -> Result<Self> {
        let root = root.into();
        if root.as_os_str().is_empty() {
            return Err(CoordinatorError::InvalidInput(format!(
                "{FACTORY_ARTIFACT_ROOT_ENV} must not be empty"
            )));
        }
        Ok(Self {
            root,
            host_repository_id,
        })
    }

    pub fn job_paths(&self, job_id: &JobId, repository_id: &str) -> ArtifactPaths {
        let coordinator_dir = self.root.join("coordinator/jobs").join(job_id.as_str());
        let local_projection_dir = (repository_id.starts_with("local:")
            && self.host_repository_id.as_deref() == Some(repository_id))
        .then(|| self.root.join("local/jobs").join(job_id.as_str()));
        ArtifactPaths {
            coordinator_dir,
            local_projection_dir,
        }
    }

    /// Creates the coordinator-side projection directory.
    pub async fn ensure_coordinator_job_dir(
        &self,
        job_id: &JobId,
        repository_id: &str,
    ) -> Result<ArtifactPaths> {
        let paths = self.job_paths(job_id, repository_id);
        tokio::fs::create_dir_all(paths.coordinator_dir())
            .await
            .map_err(|error| artifact_io(paths.coordinator_dir(), error))?;
        Ok(paths)
    }

    /// Atomically replaces the coordinator copy, then refreshes the optional
    /// matching-checkout projection. Coordinator-copy errors fail the write;
    /// workspace projection errors are returned as warnings only.
    pub async fn write_job_file(
        &self,
        paths: &ArtifactPaths,
        file: JobArtifactFile,
        contents: &[u8],
    ) -> Result<Option<ArtifactProjectionWarning>> {
        tokio::fs::create_dir_all(paths.coordinator_dir())
            .await
            .map_err(|error| artifact_io(paths.coordinator_dir(), error))?;
        let coordinator_copy = paths.coordinator_dir().join(file.file_name());
        atomic_replace(&coordinator_copy, contents)
            .await
            .map_err(|error| artifact_io(&coordinator_copy, error))?;

        let Some(projection_dir) = paths.local_projection_dir() else {
            return Ok(None);
        };
        Ok(project_file(projection_dir, file, contents)
            .await
            .err()
            .map(|error| ArtifactProjectionWarning {
                file,
                message: error.to_string(),
            }))
    }

    /// Creates the immutable job/task inventory as soon as a job is known.
    /// Runtime publication calls this again for externally-created jobs.
    pub async fn initialize_job_files(
        &self,
        job: &DurableJob,
        workspace: Option<&WorkspaceRecord>,
    ) -> Result<Vec<ArtifactProjectionWarning>> {
        let repository_id = workspace
            .map(|workspace| workspace.repository_id.as_str())
            .or_else(|| {
                job.job
                    .input
                    .get("repositoryId")
                    .and_then(|value| value.as_str())
            })
            .unwrap_or_default();
        let paths = self
            .ensure_coordinator_job_dir(&job.job.job_id, repository_id)
            .await?;
        let (job_json, task_markdown) = inventory_contents(job, workspace, repository_id)?;
        let mut warnings = Vec::new();
        for (file, contents) in [
            (JobArtifactFile::Job, job_json.as_slice()),
            (JobArtifactFile::Task, task_markdown.as_bytes()),
        ] {
            if let Some(warning) = self.write_job_file(&paths, file, contents).await? {
                warnings.push(warning);
            }
        }
        Ok(warnings)
    }

    /// Creates missing inventory files without rewriting current metadata.
    /// The guard keeps observer repair ordered with settled-output projection.
    pub async fn initialize_missing_job_files(
        &self,
        paths: &ArtifactPaths,
        job: &DurableJob,
        _guard: &ArtifactPublicationGuard,
    ) -> Result<Vec<ArtifactProjectionWarning>> {
        let repository_id = job
            .job
            .input
            .get("repositoryId")
            .and_then(|value| value.as_str())
            .unwrap_or_default();
        let (job_json, task_markdown) = inventory_contents(job, None, repository_id)?;
        let mut warnings = Vec::new();
        for (file, generated) in [
            (JobArtifactFile::Job, job_json.as_slice()),
            (JobArtifactFile::Task, task_markdown.as_bytes()),
        ] {
            match self.read_job_file(paths, file).await? {
                Some(current) => {
                    let Some(projection_dir) = paths.local_projection_dir() else {
                        continue;
                    };
                    if let Err(error) = project_file(projection_dir, file, &current).await {
                        warnings.push(ArtifactProjectionWarning {
                            file,
                            message: artifact_io(&projection_dir.join(file.file_name()), error)
                                .to_string(),
                        });
                    }
                }
                None => {
                    if let Some(warning) = self.write_job_file(paths, file, generated).await? {
                        warnings.push(warning);
                    }
                }
            }
        }
        Ok(warnings)
    }

    /// Repairs a matching local projection from the coordinator copy and reports
    /// per-file projection failures without exposing container paths.
    pub async fn reconcile_projection(
        &self,
        paths: &ArtifactPaths,
    ) -> Result<Vec<ArtifactProjectionWarning>> {
        let Some(projection_dir) = paths.local_projection_dir() else {
            return Ok(Vec::new());
        };
        let mut warnings = Vec::new();
        for file in JobArtifactFile::ALL {
            let Some(contents) = self.read_job_file(paths, file).await? else {
                continue;
            };
            if let Err(error) = project_file(projection_dir, file, &contents).await {
                warnings.push(ArtifactProjectionWarning {
                    file,
                    message: error.to_string(),
                });
            }
        }
        Ok(warnings)
    }

    /// Rebuilds disposable artifact files from currently settled event output.
    /// The lock only prevents interleaved file replacement; durable events are
    /// always the source of truth. The cumulative result is replaced last.
    pub async fn publish_settled_outputs(
        &self,
        paths: &ArtifactPaths,
        outputs: &[CompletedStageOutput],
        _guard: &ArtifactPublicationGuard,
    ) -> Result<Vec<ArtifactProjectionWarning>> {
        let mut warnings = Vec::new();
        let mut files = Vec::<(JobArtifactFile, Vec<u8>)>::new();
        for output in outputs {
            let file = JobArtifactFile::for_stage(&output.stage).ok_or_else(|| {
                CoordinatorError::InvalidInput(format!(
                    "settled artifact stage {:?} is not recognized",
                    output.stage
                ))
            })?;
            files.push((file, output.markdown.as_bytes().to_vec()));
        }
        match crate::stage_output::render_job_findings(outputs)
            .map_err(|error| CoordinatorError::InvalidInput(error.to_string()))?
        {
            Some(findings) => files.push((JobArtifactFile::Findings, findings)),
            None => {
                if let Some(warning) = self
                    .remove_job_file(paths, JobArtifactFile::Findings)
                    .await?
                {
                    warnings.push(warning);
                }
            }
        }
        files.push((
            JobArtifactFile::Result,
            crate::render_job_result(outputs).into_bytes(),
        ));

        for (file, contents) in files {
            if let Some(warning) = self.write_job_file(paths, file, &contents).await? {
                warnings.push(warning);
            }
        }
        Ok(warnings)
    }

    async fn remove_job_file(
        &self,
        paths: &ArtifactPaths,
        file: JobArtifactFile,
    ) -> Result<Option<ArtifactProjectionWarning>> {
        let coordinator_copy = paths.coordinator_dir().join(file.file_name());
        remove_generated_file(&coordinator_copy)
            .await
            .map_err(|error| artifact_io(&coordinator_copy, error))?;

        let Some(projection_dir) = paths.local_projection_dir() else {
            return Ok(None);
        };
        let projected = projection_dir.join(file.file_name());
        Ok(remove_generated_file(&projected)
            .await
            .err()
            .map(|error| ArtifactProjectionWarning {
                file,
                message: artifact_io(&projected, error).to_string(),
            }))
    }

    pub async fn acquire_publication_guard(
        &self,
        paths: &ArtifactPaths,
    ) -> Result<ArtifactPublicationGuard> {
        tokio::fs::create_dir_all(paths.coordinator_dir())
            .await
            .map_err(|error| artifact_io(paths.coordinator_dir(), error))?;
        Ok(ArtifactPublicationGuard {
            _file: acquire_publication_lock(paths.coordinator_dir()).await?,
        })
    }

    pub async fn projected_file_matches(
        &self,
        paths: &ArtifactPaths,
        file: JobArtifactFile,
    ) -> bool {
        let Some(directory) = paths.local_projection_dir() else {
            return false;
        };
        let coordinator_copy = paths.coordinator_dir().join(file.file_name());
        let projected = directory.join(file.file_name());
        if !regular_file(&coordinator_copy).await || !regular_file(&projected).await {
            return false;
        }
        match tokio::try_join!(
            tokio::fs::read(coordinator_copy),
            tokio::fs::read(projected)
        ) {
            Ok((coordinator_copy, projected)) => coordinator_copy == projected,
            Err(_) => false,
        }
    }

    /// Reads the disposable coordinator copy. A missing file is expected
    /// for pre-artifact jobs and partially completed jobs.
    pub async fn read_job_file(
        &self,
        paths: &ArtifactPaths,
        file: JobArtifactFile,
    ) -> Result<Option<Vec<u8>>> {
        let path = paths.coordinator_dir().join(file.file_name());
        match tokio::fs::read(&path).await {
            Ok(contents) => Ok(Some(contents)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(artifact_io(&path, error)),
        }
    }
}

fn inventory_contents(
    job: &DurableJob,
    workspace: Option<&WorkspaceRecord>,
    repository_id: &str,
) -> Result<(Vec<u8>, String)> {
    let execution_profile = job.job.input.get("executionProfile");
    let mut metadata = serde_json::json!({
        "jobId": job.job.job_id,
        "task": job.job.input.get("task").and_then(|value| value.as_str()),
        "provider": execution_profile.and_then(|value| value.get("provider")).and_then(|value| value.as_str()),
        "model": execution_profile.and_then(|value| value.get("model")).and_then(|value| value.as_str()),
        "repositoryId": repository_id,
    });
    if let Some(workspace) = workspace {
        metadata["baseRef"] = serde_json::Value::String(workspace.base_ref.clone());
        metadata["baseRevision"] = serde_json::Value::String(workspace.base_revision.clone());
    }
    let job_json = serde_json::to_vec_pretty(&metadata)?;
    let task = job
        .job
        .input
        .get("task")
        .and_then(|value| value.as_str())
        .unwrap_or_default();
    let mut task_markdown = format!("# Task\n\n{task}");
    if !task_markdown.ends_with('\n') {
        task_markdown.push('\n');
    }
    Ok((job_json, task_markdown))
}

async fn atomic_replace(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    let parent = path.parent().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "artifact path has no parent directory",
        )
    })?;
    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "artifact path has no UTF-8 file name",
            )
        })?;
    let temporary = parent.join(format!(".{file_name}.{}.tmp", Uuid::new_v4()));
    tokio::fs::write(&temporary, contents).await?;
    if let Err(error) = tokio::fs::rename(&temporary, path).await {
        let _ = tokio::fs::remove_file(&temporary).await;
        return Err(error);
    }
    Ok(())
}

async fn project_file(
    projection_dir: &Path,
    file: JobArtifactFile,
    contents: &[u8],
) -> std::io::Result<()> {
    tokio::fs::create_dir_all(projection_dir).await?;
    atomic_replace(&projection_dir.join(file.file_name()), contents).await
}

async fn remove_generated_file(path: &Path) -> std::io::Result<()> {
    match tokio::fs::remove_file(path).await {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

async fn acquire_publication_lock(coordinator_dir: &Path) -> Result<std::fs::File> {
    let path = coordinator_dir.join(".publication.lock");
    tokio::task::spawn_blocking(move || {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .map_err(|error| artifact_io(&path, error))?;
        file.lock().map_err(|error| artifact_io(&path, error))?;
        Ok(file)
    })
    .await
    .map_err(|error| CoordinatorError::Workspace(format!("artifact lock task failed: {error}")))?
}

async fn regular_file(path: &Path) -> bool {
    tokio::fs::symlink_metadata(path)
        .await
        .is_ok_and(|metadata| metadata.file_type().is_file())
}

fn artifact_io(path: &Path, error: std::io::Error) -> CoordinatorError {
    let _ = path;
    CoordinatorError::Workspace(format!("artifact storage operation failed: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temporary_root(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("factory-artifacts-{name}-{}", Uuid::new_v4()))
    }

    fn inventory_job(job_id: &str) -> DurableJob {
        serde_json::from_value(serde_json::json!({
            "job": {
                "jobId": job_id,
                "kind": "factory.task",
                "input": {
                    "task": "Audit the codebase",
                    "repositoryId": "local:checkout",
                    "executionProfile": {"provider": "deepseek", "model": "deepseek-chat"}
                },
                "state": "succeeded",
                "createdAt": "2026-08-03T00:00:00Z",
                "updatedAt": "2026-08-03T00:00:00Z"
            },
            "operations": []
        }))
        .unwrap()
    }

    #[test]
    fn matching_local_repository_has_coordinator_and_projection_paths() {
        let manager =
            ArtifactManager::new("/factory-artifacts", Some("local:checkout".to_string())).unwrap();
        let job_id = JobId::new("job-one");

        let paths = manager.job_paths(&job_id, "local:checkout");
        assert_eq!(
            paths.coordinator_dir(),
            Path::new("/factory-artifacts/coordinator/jobs/job-one")
        );
        assert_eq!(
            paths.local_projection_dir(),
            Some(Path::new("/factory-artifacts/local/jobs/job-one"))
        );
    }

    #[test]
    fn remote_and_other_local_repositories_have_only_coordinator_copies() {
        let manager =
            ArtifactManager::new("/factory-artifacts", Some("local:checkout".to_string())).unwrap();
        let job_id = JobId::new("job-two");

        for repository_id in ["remote:origin", "local:another", "legacy:job"] {
            let paths = manager.job_paths(&job_id, repository_id);
            assert_eq!(
                paths.coordinator_dir(),
                Path::new("/factory-artifacts/coordinator/jobs/job-two")
            );
            assert_eq!(paths.local_projection_dir(), None);
        }
    }

    #[tokio::test]
    async fn atomic_overwrite_refreshes_both_projections_without_temp_files() {
        let root = temporary_root("overwrite");
        let manager = ArtifactManager::new(&root, Some("local:checkout".to_string())).unwrap();
        let paths = manager.job_paths(&JobId::new("job-overwrite"), "local:checkout");

        assert_eq!(
            manager
                .write_job_file(&paths, JobArtifactFile::Review, b"old\ncontent")
                .await
                .unwrap(),
            None
        );
        assert_eq!(
            manager
                .write_job_file(&paths, JobArtifactFile::Review, b"new\nfull\ncontent")
                .await
                .unwrap(),
            None
        );

        for directory in [
            paths.coordinator_dir(),
            paths.local_projection_dir().unwrap(),
        ] {
            assert_eq!(
                tokio::fs::read(directory.join("review.md")).await.unwrap(),
                b"new\nfull\ncontent"
            );
            let mut entries = tokio::fs::read_dir(directory).await.unwrap();
            while let Some(entry) = entries.next_entry().await.unwrap() {
                assert!(!entry.file_name().to_string_lossy().ends_with(".tmp"));
            }
        }
        tokio::fs::remove_dir_all(root).await.unwrap();
    }

    #[tokio::test]
    async fn coordinator_copy_survives_workspace_projection_failure() {
        let root = temporary_root("projection-failure");
        let manager = ArtifactManager::new(&root, Some("local:checkout".to_string())).unwrap();
        let job_id = JobId::new("job-projection-failure");
        let paths = manager.job_paths(&job_id, "local:checkout");
        let projection_dir = paths.local_projection_dir().unwrap();
        tokio::fs::create_dir_all(projection_dir.parent().unwrap())
            .await
            .unwrap();
        tokio::fs::write(projection_dir, b"not a directory")
            .await
            .unwrap();

        let warning = manager
            .write_job_file(&paths, JobArtifactFile::Result, b"rendered result")
            .await
            .unwrap()
            .expect("projection warning");
        assert_eq!(warning.file(), JobArtifactFile::Result);
        assert!(!warning.message().is_empty());
        assert!(!warning.message().contains(root.to_string_lossy().as_ref()));
        assert_eq!(
            tokio::fs::read(paths.coordinator_dir().join("result.md"))
                .await
                .unwrap(),
            b"rendered result"
        );
        tokio::fs::remove_dir_all(root).await.unwrap();
    }

    #[tokio::test]
    async fn initializes_job_inventory_before_any_stage_output() {
        let root = temporary_root("initialize");
        let manager = ArtifactManager::new(&root, Some("local:checkout".to_string())).unwrap();
        let job: DurableJob = serde_json::from_value(serde_json::json!({
            "job": {
                "jobId": "job-initialize",
                "kind": "factory.task",
                "input": {"task": "Audit the codebase", "repositoryId": "local:checkout"},
                "state": "queued",
                "createdAt": "2026-08-03T00:00:00Z",
                "updatedAt": "2026-08-03T00:00:00Z"
            },
            "operations": []
        }))
        .unwrap();

        assert!(
            manager
                .initialize_job_files(&job, None)
                .await
                .unwrap()
                .is_empty()
        );
        let paths = manager.job_paths(&job.job.job_id, "local:checkout");
        for directory in [
            paths.coordinator_dir(),
            paths.local_projection_dir().unwrap(),
        ] {
            assert!(directory.join("job.json").is_file());
            assert_eq!(
                tokio::fs::read_to_string(directory.join("task.md"))
                    .await
                    .unwrap(),
                "# Task\n\nAudit the codebase\n"
            );
            assert!(!directory.join("result.md").exists());
        }
        let metadata: serde_json::Value = serde_json::from_slice(
            &tokio::fs::read(paths.coordinator_dir().join("job.json"))
                .await
                .unwrap(),
        )
        .unwrap();
        assert!(metadata.get("input").is_none());
        assert!(metadata.get("developerInstructions").is_none());
        assert_eq!(metadata["task"], "Audit the codebase");
        assert_eq!(metadata["repositoryId"], "local:checkout");
        assert!(metadata.get("repository").is_none());
        tokio::fs::remove_dir_all(root).await.unwrap();
    }

    #[tokio::test]
    async fn observer_repair_generates_both_missing_inventory_files_and_host_projection() {
        let root = temporary_root("missing-inventory");
        let manager = ArtifactManager::new(&root, Some("local:checkout".to_string())).unwrap();
        let job = inventory_job("job-missing-inventory");
        let paths = manager.job_paths(&job.job.job_id, "local:checkout");
        let guard = manager.acquire_publication_guard(&paths).await.unwrap();

        assert!(
            manager
                .initialize_missing_job_files(&paths, &job, &guard)
                .await
                .unwrap()
                .is_empty()
        );
        for directory in [
            paths.coordinator_dir(),
            paths.local_projection_dir().unwrap(),
        ] {
            let metadata: serde_json::Value =
                serde_json::from_slice(&tokio::fs::read(directory.join("job.json")).await.unwrap())
                    .unwrap();
            assert_eq!(metadata["provider"], "deepseek");
            assert_eq!(metadata["model"], "deepseek-chat");
            assert!(metadata.get("baseRef").is_none());
            assert!(metadata.get("baseRevision").is_none());
            assert_eq!(
                tokio::fs::read_to_string(directory.join("task.md"))
                    .await
                    .unwrap(),
                "# Task\n\nAudit the codebase\n"
            );
        }
        tokio::fs::remove_dir_all(root).await.unwrap();
    }

    #[tokio::test]
    async fn observer_repair_preserves_current_metadata_and_generates_only_missing_task() {
        let root = temporary_root("partial-inventory");
        let manager = ArtifactManager::new(&root, Some("local:checkout".to_string())).unwrap();
        let job = inventory_job("job-partial-inventory");
        let paths = manager.job_paths(&job.job.job_id, "local:checkout");
        tokio::fs::create_dir_all(paths.coordinator_dir())
            .await
            .unwrap();
        let current = br#"{"baseRef":"main","baseRevision":"abc123","sentinel":true}"#;
        tokio::fs::write(paths.coordinator_dir().join("job.json"), current)
            .await
            .unwrap();
        let guard = manager.acquire_publication_guard(&paths).await.unwrap();

        assert!(
            manager
                .initialize_missing_job_files(&paths, &job, &guard)
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            tokio::fs::read(paths.coordinator_dir().join("job.json"))
                .await
                .unwrap(),
            current
        );
        assert_eq!(
            tokio::fs::read(paths.local_projection_dir().unwrap().join("job.json"))
                .await
                .unwrap(),
            current
        );
        let metadata: serde_json::Value = serde_json::from_slice(current).unwrap();
        assert_eq!(metadata["baseRef"], "main");
        assert_eq!(metadata["baseRevision"], "abc123");
        for directory in [
            paths.coordinator_dir(),
            paths.local_projection_dir().unwrap(),
        ] {
            assert_eq!(
                tokio::fs::read_to_string(directory.join("task.md"))
                    .await
                    .unwrap(),
                "# Task\n\nAudit the codebase\n"
            );
        }
        tokio::fs::remove_dir_all(root).await.unwrap();
    }

    #[tokio::test]
    async fn unknown_legacy_findings_remove_stale_projection_but_explicit_empty_is_written() {
        let root = temporary_root("legacy-findings");
        let manager = ArtifactManager::new(&root, Some("local:checkout".to_string())).unwrap();
        let paths = manager.job_paths(&JobId::new("job-legacy-findings"), "local:checkout");
        manager
            .write_job_file(&paths, JobArtifactFile::Findings, br#"[{"stale":true}]"#)
            .await
            .unwrap();
        let guard = manager.acquire_publication_guard(&paths).await.unwrap();
        let mut outputs = vec![CompletedStageOutput {
            operation_id: crate::OperationId::new("review-op"),
            stage: "review".to_string(),
            markdown: "Approved".to_string(),
            findings: None,
        }];

        manager
            .publish_settled_outputs(&paths, &outputs, &guard)
            .await
            .unwrap();
        for directory in [
            paths.coordinator_dir(),
            paths.local_projection_dir().unwrap(),
        ] {
            assert!(!directory.join("findings.json").exists());
        }

        outputs[0].findings = Some(serde_json::json!([]));
        manager
            .publish_settled_outputs(&paths, &outputs, &guard)
            .await
            .unwrap();
        for directory in [
            paths.coordinator_dir(),
            paths.local_projection_dir().unwrap(),
        ] {
            assert_eq!(
                tokio::fs::read(directory.join("findings.json"))
                    .await
                    .unwrap(),
                b"[]"
            );
        }
        tokio::fs::remove_dir_all(root).await.unwrap();
    }

    #[tokio::test]
    async fn reconciliation_repairs_missing_workspace_projection() {
        let root = temporary_root("reconcile");
        let manager = ArtifactManager::new(&root, Some("local:checkout".to_string())).unwrap();
        let paths = manager.job_paths(&JobId::new("job-reconcile"), "local:checkout");
        manager
            .write_job_file(&paths, JobArtifactFile::Result, b"# Result\n")
            .await
            .unwrap();
        tokio::fs::remove_file(
            paths
                .local_projection_dir()
                .unwrap()
                .join(JobArtifactFile::Result.file_name()),
        )
        .await
        .unwrap();

        assert!(
            manager
                .reconcile_projection(&paths)
                .await
                .unwrap()
                .is_empty()
        );
        assert!(
            manager
                .projected_file_matches(&paths, JobArtifactFile::Result)
                .await
        );
        tokio::fs::remove_dir_all(root).await.unwrap();
    }

    #[tokio::test]
    async fn projected_availability_rejects_stale_bytes_and_non_regular_files() {
        let root = temporary_root("projection-types");
        let manager = ArtifactManager::new(&root, Some("local:checkout".to_string())).unwrap();
        let paths = manager.job_paths(&JobId::new("job-types"), "local:checkout");
        manager
            .write_job_file(&paths, JobArtifactFile::Result, b"rendered")
            .await
            .unwrap();
        assert!(
            manager
                .projected_file_matches(&paths, JobArtifactFile::Result)
                .await
        );
        let projected = paths.local_projection_dir().unwrap().join("result.md");
        tokio::fs::write(&projected, b"stale").await.unwrap();
        assert!(
            !manager
                .projected_file_matches(&paths, JobArtifactFile::Result)
                .await
        );
        tokio::fs::remove_file(&projected).await.unwrap();
        tokio::fs::create_dir(&projected).await.unwrap();
        assert!(
            !manager
                .projected_file_matches(&paths, JobArtifactFile::Result)
                .await
        );
        tokio::fs::remove_dir(&projected).await.unwrap();
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(paths.coordinator_dir().join("result.md"), &projected)
                .unwrap();
            assert!(
                !manager
                    .projected_file_matches(&paths, JobArtifactFile::Result)
                    .await
            );
        }
        tokio::fs::remove_dir_all(root).await.unwrap();
    }
}
