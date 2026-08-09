use crate::CoordinatorError;
use crate::CoordinatorStore;
use crate::EnsureWorkspaceRequest;
use crate::ExecutionEnvironmentStatus;
use crate::JobState;
use crate::Result;
use crate::WorkspaceBinding;
use crate::WorkspaceRecord;
use crate::WorkspaceResult;
use crate::WorkspaceState;
use crate::ids::JobId;
use serde::Deserialize;
use serde::Serialize;
use sha2::Digest;
use sha2::Sha256;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::process::Command;
use tokio::sync::Mutex;
use uuid::Uuid;

const REVIEW_SNAPSHOT_METADATA: &str = "snapshot.json";
const REVIEW_SNAPSHOT_INDEX: &str = "baseline.index";
const REVIEW_SNAPSHOT_TREE_INDEX: &str = "tree.index";
const REMOTE_BRANCH_REFSPEC: &str = "+refs/heads/*:refs/remotes/origin/*";
const REMOTE_TAG_REFSPEC: &str = "+refs/tags/*:refs/tags/*";

/// Opaque identity for one durable pre-review workspace snapshot.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkspaceSnapshot {
    job_id: JobId,
    snapshot_id: String,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct WorkspaceSnapshotMetadata {
    snapshot_id: String,
    job_id: String,
    root: String,
    branch_name: String,
    head: String,
    tree: String,
    #[serde(default)]
    index_tree: String,
    had_index: bool,
    #[serde(default)]
    mutation_detected: bool,
}

/// Materializes and reuses one repository-neutral Git worktree per durable job.
#[derive(Clone)]
pub struct WorkspaceManager {
    root: PathBuf,
    mutation_gate: Arc<Mutex<()>>,
}

impl WorkspaceManager {
    pub fn from_env() -> Result<Self> {
        let root = std::env::var_os("FACTORY_WORKSPACE_ROOT")
            .map(PathBuf::from)
            .unwrap_or_else(|| std::env::temp_dir().join("software-factory/workspaces"));
        Self::new(root)
    }

    pub fn new(root: impl Into<PathBuf>) -> Result<Self> {
        let root = root.into();
        if root.as_os_str().is_empty() {
            return Err(CoordinatorError::InvalidInput(
                "FACTORY_WORKSPACE_ROOT must not be empty".to_string(),
            ));
        }
        Ok(Self {
            root,
            mutation_gate: Arc::new(Mutex::new(())),
        })
    }

    pub async fn ensure(
        &self,
        store: &CoordinatorStore,
        job_id: &JobId,
        request: EnsureWorkspaceRequest,
    ) -> Result<WorkspaceRecord> {
        validate_request(&request)?;
        let _workspace_guard = store.acquire_workspace_execution(job_id).await?;
        let _guard = self.mutation_gate.lock().await;
        store.load_job(job_id).await?;

        let existing = store.load_workspace(job_id).await?;
        if let Some(existing) = existing.as_ref() {
            if existing.repository_id != request.repository_id
                || existing.repository != request.repository
                || existing.base_ref != request.base_ref
            {
                return Err(CoordinatorError::InvalidInput(format!(
                    "job {job_id} is already bound to repository identity {} at {}",
                    existing.repository_id, existing.base_ref
                )));
            }
            if existing.state == WorkspaceState::Active && Path::new(&existing.root).is_dir() {
                return Ok(existing.clone());
            }
        }

        let worktree = self.root.join("jobs").join(job_id.as_str());
        tokio::fs::create_dir_all(self.root.join("mirrors"))
            .await
            .map_err(workspace_io)?;
        tokio::fs::create_dir_all(self.root.join("jobs"))
            .await
            .map_err(workspace_io)?;

        let _repository_guard = store
            .acquire_workspace_repository(&request.repository_id)
            .await?;
        let (mirror, resolved_revision) = self
            .ensure_mirror(
                &request.repository_id,
                &request.repository,
                &request.base_ref,
            )
            .await?;
        let revision = match existing.as_ref() {
            Some(existing) => {
                ensure_mirror_has_revision(&mirror, &existing.base_revision).await?;
                existing.base_revision.clone()
            }
            None => resolved_revision,
        };
        run_git(["--git-dir", path_text(&mirror)?, "worktree", "prune"]).await?;
        if worktree.exists() {
            run_git([
                "--git-dir",
                path_text(&mirror)?,
                "worktree",
                "remove",
                "--force",
                path_text(&worktree)?,
            ])
            .await?;
        }

        let branch_name = format!("factory/{job_id}");
        run_git([
            "--git-dir",
            path_text(&mirror)?,
            "worktree",
            "add",
            "--force",
            "-B",
            branch_name.as_str(),
            path_text(&worktree)?,
            revision.as_str(),
        ])
        .await?;

        let canonical_root = tokio::fs::canonicalize(&worktree)
            .await
            .map_err(workspace_io)?;
        let revision = run_git(["-C", path_text(&canonical_root)?, "rev-parse", "HEAD"]).await?;
        store
            .put_workspace(&WorkspaceBinding {
                job_id: job_id.clone(),
                repository_id: request.repository_id.clone(),
                repository: request.repository.clone(),
                base_ref: request.base_ref.clone(),
                base_revision: revision.clone(),
                branch_name,
                root: path_text(&canonical_root)?.to_string(),
                revision,
            })
            .await
    }

    async fn ensure_mirror(
        &self,
        repository_id: &str,
        repository: &str,
        base_ref: &str,
    ) -> Result<(PathBuf, String)> {
        let mirror = self.mirror_path(repository_id);
        if mirror.exists() {
            if legacy_mirror_matches_repository(&mirror, repository).await? {
                migrate_legacy_mirror(&mirror).await?;
            }
            if mirror_matches_repository(&mirror, repository).await? {
                refresh_remote(&mirror).await?;
                let revision = resolve_mirror_revision(&mirror, base_ref)
                    .await?
                    .ok_or_else(|| unresolved_base_ref(repository, base_ref))?;
                return Ok((mirror, revision));
            }
        }

        if mirror.exists() {
            let quarantine = sibling_path(&mirror, "invalid")?;
            tokio::fs::rename(&mirror, &quarantine)
                .await
                .map_err(workspace_io)?;
        }

        let candidate = sibling_path(&mirror, "partial")?;
        initialize_remote_cache(&candidate, repository).await?;
        if !mirror_matches_repository(&candidate, repository).await? {
            return Err(CoordinatorError::Workspace(format!(
                "git initialization did not create a bare remote-tracking cache with the expected origin at {}",
                candidate.display()
            )));
        }
        let revision = resolve_mirror_revision(&candidate, base_ref)
            .await?
            .ok_or_else(|| unresolved_base_ref(repository, base_ref))?;
        tokio::fs::rename(&candidate, &mirror)
            .await
            .map_err(workspace_io)?;
        Ok((mirror, revision))
    }

    pub async fn load(&self, store: &CoordinatorStore, job_id: &JobId) -> Result<WorkspaceRecord> {
        store
            .load_workspace(job_id)
            .await?
            .ok_or_else(|| CoordinatorError::WorkspaceNotFound(job_id.clone()))
    }

    /// Returns the exact shared Git common directory used by a managed
    /// worktree for one repository identity.
    pub fn repository_metadata_root(&self, repository_id: &str) -> Result<String> {
        if repository_id.trim().is_empty() {
            return Err(CoordinatorError::InvalidInput(
                "workspace repositoryId must not be empty".to_string(),
            ));
        }
        path_text(&self.mirror_path(repository_id)).map(str::to_string)
    }

    pub async fn refresh_revision(
        &self,
        store: &CoordinatorStore,
        job_id: &JobId,
    ) -> Result<WorkspaceRecord> {
        let _workspace_guard = store.acquire_workspace_execution(job_id).await?;
        let _guard = self.mutation_gate.lock().await;
        let current = self.load(store, job_id).await?;
        if current.state != WorkspaceState::Active {
            return Err(CoordinatorError::Workspace(format!(
                "workspace for job {job_id} is not active"
            )));
        }
        let revision = run_git(["-C", current.root.as_str(), "rev-parse", "HEAD"]).await?;
        store
            .put_workspace(&WorkspaceBinding {
                job_id: job_id.clone(),
                repository_id: current.repository_id,
                repository: current.repository,
                base_ref: current.base_ref,
                base_revision: current.base_revision,
                branch_name: current.branch_name,
                root: current.root,
                revision,
            })
            .await
    }

    /// Exports the complete succeeded worktree relative to its immutable base
    /// as a standard Git binary patch without changing the managed index.
    pub async fn export_result(
        &self,
        store: &CoordinatorStore,
        job_id: &JobId,
    ) -> Result<WorkspaceResult> {
        let _workspace_guard = store.acquire_workspace_execution(job_id).await?;
        let _guard = self.mutation_gate.lock().await;
        let job = store.load_job(job_id).await?;
        if job.job.state != JobState::Succeeded {
            return Err(CoordinatorError::InvalidInput(format!(
                "job {job_id} is {:?}, not succeeded",
                job.job.state
            )));
        }
        let workspace = self.load(store, job_id).await?;
        let worktree = self.managed_worktree_path(&workspace).await?;
        let indexes = self.root.join("result-indexes");
        tokio::fs::create_dir_all(&indexes)
            .await
            .map_err(workspace_io)?;
        let index = indexes.join(format!("{job_id}-{}.index", Uuid::new_v4()));
        let index_lock = index.with_extension("index.lock");
        let generated = generate_result_patch(&worktree, &workspace.base_revision, &index).await;
        let _ = tokio::fs::remove_file(&index).await;
        let _ = tokio::fs::remove_file(&index_lock).await;
        let patch = generated?;
        let patch_sha256 = format!("{:x}", Sha256::digest(&patch));
        Ok(WorkspaceResult {
            job_id: job_id.clone(),
            repository_id: workspace.repository_id,
            base_revision: workspace.base_revision,
            patch_sha256,
            patch,
        })
    }

    /// Verifies that a Factory-owned disposable worktree is still exactly at
    /// its recorded revision and has no tracked or untracked changes.
    pub async fn validate_pristine(&self, workspace: &WorkspaceRecord) -> Result<()> {
        let worktree = self.managed_worktree_path(workspace).await?;
        self.validate_pristine_unlocked(workspace, &worktree).await
    }

    /// Restores the disposable worktree in place so an already-mounted
    /// execution environment keeps observing the same directory inode.
    /// Missing or structurally invalid worktrees require an explicit backend
    /// rebind before [`Self::recreate`] may replace the directory.
    pub async fn restore(&self, workspace: &WorkspaceRecord) -> Result<()> {
        let _guard = self.mutation_gate.lock().await;
        let worktree = self.managed_worktree_path(workspace).await?;
        let mirror = self.mirror_path(&workspace.repository_id);
        if !mirror_matches_repository(&mirror, &workspace.repository).await? {
            return Err(CoordinatorError::Workspace(format!(
                "managed mirror for job {} is unavailable or does not match {}",
                workspace.job_id, workspace.repository
            )));
        }
        ensure_mirror_has_revision(&mirror, &workspace.revision).await?;
        if !linked_worktree_matches(&worktree, &mirror).await? {
            return Err(CoordinatorError::WorkspaceRebindRequired {
                job_id: workspace.job_id.clone(),
                reason: "managed linked worktree is missing or does not use its recorded Git common directory"
                    .to_string(),
            });
        }

        run_git([
            "-C",
            path_text(&worktree)?,
            "checkout",
            "--force",
            "-B",
            workspace.branch_name.as_str(),
            workspace.revision.as_str(),
        ])
        .await?;
        run_git([
            "-C",
            path_text(&worktree)?,
            "reset",
            "--hard",
            workspace.revision.as_str(),
        ])
        .await?;
        run_git(["-C", path_text(&worktree)?, "clean", "-ffdx"]).await?;

        self.validate_pristine_unlocked(workspace, &worktree).await
    }

    /// Recreates a missing or structurally invalid disposable worktree.
    /// Callers must first stop and remove every execution backend that mounts
    /// this workspace root.
    pub async fn recreate(&self, workspace: &WorkspaceRecord) -> Result<()> {
        let _guard = self.mutation_gate.lock().await;
        let worktree = self.managed_worktree_path(workspace).await?;
        let mirror = self.mirror_path(&workspace.repository_id);
        if !mirror_matches_repository(&mirror, &workspace.repository).await? {
            return Err(CoordinatorError::Workspace(format!(
                "managed mirror for job {} is unavailable or does not match {}",
                workspace.job_id, workspace.repository
            )));
        }
        ensure_mirror_has_revision(&mirror, &workspace.revision).await?;

        if worktree.exists() && linked_worktree_matches(&worktree, &mirror).await? {
            run_git([
                "--git-dir",
                path_text(&mirror)?,
                "worktree",
                "remove",
                "--force",
                path_text(&worktree)?,
            ])
            .await?;
        } else if worktree.exists() {
            tokio::fs::remove_dir_all(&worktree)
                .await
                .map_err(workspace_io)?;
        }
        run_git(["--git-dir", path_text(&mirror)?, "worktree", "prune"]).await?;
        run_git([
            "--git-dir",
            path_text(&mirror)?,
            "worktree",
            "add",
            "--force",
            "-B",
            workspace.branch_name.as_str(),
            path_text(&worktree)?,
            workspace.revision.as_str(),
        ])
        .await?;

        self.validate_pristine_unlocked(workspace, &worktree).await
    }

    /// Captures the implementation content that a detached review must not
    /// change. Tracked files and nonignored untracked files are preserved;
    /// ignored build and test artifacts are deliberately excluded.
    ///
    /// The metadata lives on the shared workspace volume so a replacement
    /// process can restore it after process loss.
    pub async fn capture_review_snapshot(
        &self,
        workspace: &WorkspaceRecord,
    ) -> Result<WorkspaceSnapshot> {
        if self.recover_review_snapshot(workspace).await? {
            return Err(CoordinatorError::Workspace(format!(
                "review mutation recovery for job {} has not been acknowledged",
                workspace.job_id
            )));
        }
        let worktree = self.managed_worktree_path(workspace).await?;
        let snapshot_id = Uuid::new_v4().to_string();
        let snapshot_dir = self.review_snapshot_dir(&workspace.job_id);
        if snapshot_dir.exists() {
            tokio::fs::remove_dir_all(&snapshot_dir)
                .await
                .map_err(workspace_io)?;
        }
        tokio::fs::create_dir_all(&snapshot_dir)
            .await
            .map_err(workspace_io)?;

        let actual_index = self.git_index_path(&worktree).await?;
        let baseline_index = snapshot_dir.join(REVIEW_SNAPSHOT_INDEX);
        let tree_index = snapshot_dir.join(REVIEW_SNAPSHOT_TREE_INDEX);
        let had_index = actual_index.is_file();
        if had_index {
            tokio::fs::copy(&actual_index, &baseline_index)
                .await
                .map_err(workspace_io)?;
            tokio::fs::copy(&actual_index, &tree_index)
                .await
                .map_err(workspace_io)?;
        } else {
            run_git_with_index(&worktree, &tree_index, ["read-tree", "HEAD"]).await?;
        }
        let index_tree = run_git_with_index(&worktree, &tree_index, ["write-tree"]).await?;
        run_git_with_index(&worktree, &tree_index, ["add", "-A", "--", "."]).await?;

        let metadata = WorkspaceSnapshotMetadata {
            snapshot_id: snapshot_id.clone(),
            job_id: workspace.job_id.as_str().to_string(),
            root: workspace.root.clone(),
            branch_name: run_git([
                "-C",
                path_text(&worktree)?,
                "symbolic-ref",
                "--short",
                "HEAD",
            ])
            .await?,
            head: run_git(["-C", path_text(&worktree)?, "rev-parse", "HEAD"]).await?,
            tree: run_git_with_index(&worktree, &tree_index, ["write-tree"]).await?,
            index_tree,
            had_index,
            mutation_detected: false,
        };
        write_review_snapshot_metadata(&snapshot_dir, &metadata).await?;

        Ok(WorkspaceSnapshot {
            job_id: workspace.job_id.clone(),
            snapshot_id,
        })
    }

    /// Restores a completed or failed detached review to its exact source
    /// snapshot. Returns `true` when review activity changed guarded content.
    pub async fn restore_review_snapshot(
        &self,
        workspace: &WorkspaceRecord,
        snapshot: WorkspaceSnapshot,
    ) -> Result<bool> {
        if snapshot.job_id != workspace.job_id {
            return Err(CoordinatorError::Workspace(format!(
                "review snapshot belongs to job {}, not {}",
                snapshot.job_id, workspace.job_id
            )));
        }
        let metadata = self.load_review_snapshot(workspace).await?.ok_or_else(|| {
            CoordinatorError::Workspace(format!(
                "review snapshot for job {} was not found",
                workspace.job_id
            ))
        })?;
        if metadata.snapshot_id != snapshot.snapshot_id {
            return Err(CoordinatorError::Workspace(format!(
                "review snapshot identity changed for job {}",
                workspace.job_id
            )));
        }
        self.restore_review_snapshot_metadata(workspace, metadata)
            .await
    }

    /// Recovers a pre-review snapshot left by process death. This is called
    /// before a replacement runtime can inspect or mutate the worktree.
    pub async fn recover_review_snapshot(&self, workspace: &WorkspaceRecord) -> Result<bool> {
        let Some(metadata) = self.load_review_snapshot(workspace).await? else {
            return Ok(false);
        };
        self.restore_review_snapshot_metadata(workspace, metadata)
            .await
    }

    /// Clears a retained mutation marker only after the corresponding Factory
    /// review state rollback has been durably saved.
    pub async fn acknowledge_review_mutation(&self, workspace: &WorkspaceRecord) -> Result<()> {
        let metadata = self.load_review_snapshot(workspace).await?.ok_or_else(|| {
            CoordinatorError::Workspace(format!(
                "review mutation marker for job {} was not found",
                workspace.job_id
            ))
        })?;
        if metadata.job_id != workspace.job_id.as_str()
            || metadata.root != workspace.root
            || !metadata.mutation_detected
        {
            return Err(CoordinatorError::Workspace(format!(
                "review mutation marker does not match workspace {}",
                workspace.job_id
            )));
        }
        tokio::fs::remove_dir_all(self.review_snapshot_dir(&workspace.job_id))
            .await
            .map_err(workspace_io)
    }

    async fn restore_review_snapshot_metadata(
        &self,
        workspace: &WorkspaceRecord,
        mut metadata: WorkspaceSnapshotMetadata,
    ) -> Result<bool> {
        if metadata.job_id != workspace.job_id.as_str()
            || metadata.root != workspace.root
            || metadata.branch_name != workspace.branch_name
        {
            return Err(CoordinatorError::Workspace(format!(
                "review snapshot metadata does not match workspace {}",
                workspace.job_id
            )));
        }
        let worktree = self.managed_worktree_path(workspace).await?;
        let snapshot_dir = self.review_snapshot_dir(&workspace.job_id);
        let baseline_index = snapshot_dir.join(REVIEW_SNAPSHOT_INDEX);
        if metadata.had_index && !baseline_index.is_file() {
            return Err(CoordinatorError::Workspace(format!(
                "review snapshot index is missing for job {}",
                workspace.job_id
            )));
        }

        let current_index = self.git_index_path(&worktree).await?;
        let comparison_index = snapshot_dir.join("comparison.index");
        let current_had_index = current_index.is_file();
        if current_had_index {
            tokio::fs::copy(&current_index, &comparison_index)
                .await
                .map_err(workspace_io)?;
        } else {
            run_git_with_index(&worktree, &comparison_index, ["read-tree", "HEAD"]).await?;
        }
        let current_index_tree =
            run_git_with_index(&worktree, &comparison_index, ["write-tree"]).await?;
        run_git_with_index(&worktree, &comparison_index, ["add", "-A", "--", "."]).await?;
        let current_tree = run_git_with_index(&worktree, &comparison_index, ["write-tree"]).await?;
        let current_head =
            optional_git_output(["-C", path_text(&worktree)?, "rev-parse", "HEAD"]).await?;
        let current_branch = optional_git_output([
            "-C",
            path_text(&worktree)?,
            "symbolic-ref",
            "--short",
            "HEAD",
        ])
        .await?;
        let baseline_index_bytes = if metadata.had_index {
            Some(
                tokio::fs::read(&baseline_index)
                    .await
                    .map_err(workspace_io)?,
            )
        } else {
            None
        };
        let baseline_index_tree = if metadata.index_tree.is_empty() {
            if metadata.had_index {
                run_git_with_index(&worktree, &baseline_index, ["write-tree"]).await?
            } else {
                let head_tree = format!("{}^{{tree}}", metadata.head);
                run_git(["-C", path_text(&worktree)?, "rev-parse", head_tree.as_str()]).await?
            }
        } else {
            metadata.index_tree.clone()
        };
        let changed = metadata.mutation_detected
            || current_tree != metadata.tree
            || current_head.as_deref() != Some(metadata.head.as_str())
            || current_branch.as_deref() != Some(metadata.branch_name.as_str())
            || metadata.had_index != current_had_index
            || current_index_tree != baseline_index_tree;

        if changed {
            if !metadata.mutation_detected {
                metadata.mutation_detected = true;
                write_review_snapshot_metadata(&snapshot_dir, &metadata).await?;
            }
            run_git([
                "-C",
                path_text(&worktree)?,
                "checkout",
                "--force",
                "-B",
                metadata.branch_name.as_str(),
                metadata.head.as_str(),
            ])
            .await?;
            run_git([
                "-C",
                path_text(&worktree)?,
                "reset",
                "--hard",
                metadata.head.as_str(),
            ])
            .await?;
            run_git(["-C", path_text(&worktree)?, "clean", "-ffd"]).await?;
            run_git([
                "-C",
                path_text(&worktree)?,
                "read-tree",
                "--reset",
                "-u",
                metadata.tree.as_str(),
            ])
            .await?;
            if metadata.had_index {
                let index_candidate = current_index.with_extension("factory-review-restore");
                tokio::fs::write(
                    &index_candidate,
                    baseline_index_bytes.expect("validated baseline index"),
                )
                .await
                .map_err(workspace_io)?;
                tokio::fs::rename(index_candidate, &current_index)
                    .await
                    .map_err(workspace_io)?;
            } else if current_index.exists() {
                tokio::fs::remove_file(&current_index)
                    .await
                    .map_err(workspace_io)?;
            }
        } else {
            tokio::fs::remove_dir_all(snapshot_dir)
                .await
                .map_err(workspace_io)?;
        }
        Ok(changed)
    }

    async fn load_review_snapshot(
        &self,
        workspace: &WorkspaceRecord,
    ) -> Result<Option<WorkspaceSnapshotMetadata>> {
        let path = self
            .review_snapshot_dir(&workspace.job_id)
            .join(REVIEW_SNAPSHOT_METADATA);
        let encoded = match tokio::fs::read(path).await {
            Ok(encoded) => encoded,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(workspace_io(error)),
        };
        serde_json::from_slice(&encoded)
            .map(Some)
            .map_err(Into::into)
    }

    async fn git_index_path(&self, worktree: &Path) -> Result<PathBuf> {
        run_git([
            "-C",
            path_text(worktree)?,
            "rev-parse",
            "--path-format=absolute",
            "--git-path",
            "index",
        ])
        .await
        .map(PathBuf::from)
    }

    fn review_snapshot_dir(&self, job_id: &JobId) -> PathBuf {
        self.root.join("review-snapshots").join(job_id.as_str())
    }

    pub async fn remove(
        &self,
        store: &CoordinatorStore,
        job_id: &JobId,
    ) -> Result<WorkspaceRecord> {
        let _workspace_guard = store.acquire_workspace_execution(job_id).await?;
        let _guard = self.mutation_gate.lock().await;
        let current = self.load(store, job_id).await?;
        if current.state == WorkspaceState::Removed {
            return Ok(current);
        }
        if let Some(environment) = store.load_execution_environment(job_id).await?
            && environment.status != ExecutionEnvironmentStatus::Released
        {
            return Err(CoordinatorError::InvalidInput(format!(
                "workspace for job {job_id} cannot be removed while execution environment {} generation {} is {:?}",
                environment.environment_id, environment.generation, environment.status
            )));
        }
        let worktree = PathBuf::from(&current.root);
        if worktree.exists() {
            let mirror = self.mirror_path(&current.repository_id);
            run_git([
                "--git-dir",
                path_text(&mirror)?,
                "worktree",
                "remove",
                "--force",
                path_text(&worktree)?,
            ])
            .await?;
        }
        store.mark_workspace_removed(job_id).await
    }

    fn mirror_path(&self, repository_id: &str) -> PathBuf {
        let digest = Sha256::digest(repository_id.as_bytes());
        self.root.join("mirrors").join(format!("{digest:x}.git"))
    }

    async fn managed_worktree_path(&self, workspace: &WorkspaceRecord) -> Result<PathBuf> {
        if workspace.state != WorkspaceState::Active {
            return Err(CoordinatorError::Workspace(format!(
                "workspace for job {} is not active",
                workspace.job_id
            )));
        }
        let jobs_root = tokio::fs::canonicalize(self.root.join("jobs"))
            .await
            .map_err(workspace_io)?;
        let expected = jobs_root.join(workspace.job_id.as_str());
        if Path::new(&workspace.root) != expected {
            return Err(CoordinatorError::Workspace(format!(
                "workspace for job {} is outside the managed jobs root",
                workspace.job_id
            )));
        }
        Ok(expected)
    }

    async fn validate_pristine_unlocked(
        &self,
        workspace: &WorkspaceRecord,
        worktree: &Path,
    ) -> Result<()> {
        let head = run_git(["-C", path_text(worktree)?, "rev-parse", "HEAD"]).await?;
        let branch = optional_git_output([
            "-C",
            path_text(worktree)?,
            "symbolic-ref",
            "--short",
            "HEAD",
        ])
        .await?;
        let status = run_git([
            "-C",
            path_text(worktree)?,
            "status",
            "--porcelain=v1",
            "-z",
            "--untracked-files=all",
            "--ignored=matching",
        ])
        .await?;
        if head == workspace.revision
            && branch.as_deref() == Some(workspace.branch_name.as_str())
            && status.is_empty()
        {
            return Ok(());
        }

        let changes = status
            .split('\0')
            .filter(|entry| !entry.is_empty())
            .take(20)
            .collect::<Vec<_>>()
            .join(", ");
        Err(CoordinatorError::Workspace(format!(
            "managed worktree for job {} drifted from revision {} (HEAD {head}; branch {}; changes: {})",
            workspace.job_id,
            workspace.revision,
            branch.as_deref().unwrap_or("detached"),
            if changes.is_empty() { "none" } else { &changes }
        )))
    }
}

async fn linked_worktree_matches(worktree: &Path, mirror: &Path) -> Result<bool> {
    if !worktree.is_dir() {
        return Ok(false);
    }
    let canonical_worktree = match tokio::fs::canonicalize(worktree).await {
        Ok(path) => path,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(workspace_io(error)),
    };
    if canonical_worktree != worktree {
        return Ok(false);
    }
    let Some(top_level) = optional_git_output([
        "-C",
        path_text(worktree)?,
        "rev-parse",
        "--path-format=absolute",
        "--show-toplevel",
    ])
    .await?
    else {
        return Ok(false);
    };
    let Some(common_dir) = optional_git_output([
        "-C",
        path_text(worktree)?,
        "rev-parse",
        "--path-format=absolute",
        "--git-common-dir",
    ])
    .await?
    else {
        return Ok(false);
    };
    let canonical_mirror = tokio::fs::canonicalize(mirror)
        .await
        .map_err(workspace_io)?;
    Ok(Path::new(&top_level) == canonical_worktree && Path::new(&common_dir) == canonical_mirror)
}

fn validate_request(request: &EnsureWorkspaceRequest) -> Result<()> {
    if request.repository_id.trim().is_empty() {
        return Err(CoordinatorError::InvalidInput(
            "workspace repositoryId must not be empty".to_string(),
        ));
    }
    if request.repository.trim().is_empty() {
        return Err(CoordinatorError::InvalidInput(
            "workspace repository must not be empty".to_string(),
        ));
    }
    if request.base_ref.trim().is_empty() {
        return Err(CoordinatorError::InvalidInput(
            "workspace baseRef must not be empty".to_string(),
        ));
    }
    Ok(())
}

fn path_text(path: &Path) -> Result<&str> {
    path.to_str().ok_or_else(|| {
        CoordinatorError::Workspace(format!("workspace path is not UTF-8: {}", path.display()))
    })
}

fn workspace_io(error: std::io::Error) -> CoordinatorError {
    CoordinatorError::Workspace(error.to_string())
}

async fn write_review_snapshot_metadata(
    snapshot_dir: &Path,
    metadata: &WorkspaceSnapshotMetadata,
) -> Result<()> {
    let metadata_path = snapshot_dir.join(REVIEW_SNAPSHOT_METADATA);
    let candidate = snapshot_dir.join(format!("{REVIEW_SNAPSHOT_METADATA}.partial"));
    let encoded = serde_json::to_vec(metadata)?;
    tokio::fs::write(&candidate, encoded)
        .await
        .map_err(workspace_io)?;
    tokio::fs::rename(candidate, metadata_path)
        .await
        .map_err(workspace_io)
}

fn sibling_path(path: &Path, state: &str) -> Result<PathBuf> {
    let file_name = path.file_name().ok_or_else(|| {
        CoordinatorError::Workspace(format!("mirror path has no file name: {}", path.display()))
    })?;
    let mut sibling_name = file_name.to_os_string();
    sibling_name.push(format!(".{state}-{}", Uuid::new_v4()));
    Ok(path.with_file_name(sibling_name))
}

async fn mirror_matches_repository(path: &Path, repository: &str) -> Result<bool> {
    if !bare_origin_matches(path, repository).await? {
        return Ok(false);
    }
    let mirror_mode = optional_git_output([
        "--git-dir",
        path_text(path)?,
        "config",
        "--bool",
        "--get",
        "remote.origin.mirror",
    ])
    .await?;
    let fetch = optional_git_output([
        "--git-dir",
        path_text(path)?,
        "config",
        "--get-all",
        "remote.origin.fetch",
    ])
    .await?;
    let fetch_lines = fetch
        .as_deref()
        .map(str::lines)
        .into_iter()
        .flatten()
        .collect::<std::collections::BTreeSet<_>>();
    let first_ref = optional_git_output([
        "--git-dir",
        path_text(path)?,
        "for-each-ref",
        "--count=1",
        "--format=%(objectname)",
    ])
    .await?;
    Ok(mirror_mode.as_deref() != Some("true")
        && fetch_lines.len() == 2
        && fetch_lines.contains(REMOTE_BRANCH_REFSPEC)
        && fetch_lines.contains(REMOTE_TAG_REFSPEC)
        && first_ref.as_deref().is_some_and(|value| !value.is_empty()))
}

async fn legacy_mirror_matches_repository(path: &Path, repository: &str) -> Result<bool> {
    if !bare_origin_matches(path, repository).await? {
        return Ok(false);
    }
    let mirror_mode = optional_git_output([
        "--git-dir",
        path_text(path)?,
        "config",
        "--bool",
        "--get",
        "remote.origin.mirror",
    ])
    .await?;
    let fetch = optional_git_output([
        "--git-dir",
        path_text(path)?,
        "config",
        "--get-all",
        "remote.origin.fetch",
    ])
    .await?;
    Ok(mirror_mode.as_deref() == Some("true")
        || fetch
            .as_deref()
            .is_some_and(|value| value.lines().any(|line| line == "+refs/*:refs/*")))
}

async fn bare_origin_matches(path: &Path, repository: &str) -> Result<bool> {
    if !path.is_dir() {
        return Ok(false);
    }
    let bare = optional_git_output([
        "--git-dir",
        path_text(path)?,
        "rev-parse",
        "--is-bare-repository",
    ])
    .await?;
    if bare.as_deref() != Some("true") {
        return Ok(false);
    }
    let origin = optional_git_output([
        "--git-dir",
        path_text(path)?,
        "config",
        "--get",
        "remote.origin.url",
    ])
    .await?;
    Ok(origin.as_deref() == Some(repository))
}

async fn initialize_remote_cache(path: &Path, repository: &str) -> Result<()> {
    run_git(["init", "--bare", "--", path_text(path)?]).await?;
    run_git([
        "--git-dir",
        path_text(path)?,
        "remote",
        "add",
        "origin",
        repository,
    ])
    .await?;
    configure_remote_tracking(path).await?;
    refresh_remote(path).await
}

async fn migrate_legacy_mirror(path: &Path) -> Result<()> {
    run_git([
        "--git-dir",
        path_text(path)?,
        "config",
        "--unset-all",
        "remote.origin.fetch",
    ])
    .await?;
    let _ = optional_git_output([
        "--git-dir",
        path_text(path)?,
        "config",
        "--unset-all",
        "remote.origin.mirror",
    ])
    .await?;
    configure_remote_tracking(path).await
}

async fn configure_remote_tracking(path: &Path) -> Result<()> {
    run_git([
        "--git-dir",
        path_text(path)?,
        "config",
        "--add",
        "remote.origin.fetch",
        REMOTE_BRANCH_REFSPEC,
    ])
    .await?;
    run_git([
        "--git-dir",
        path_text(path)?,
        "config",
        "--add",
        "remote.origin.fetch",
        REMOTE_TAG_REFSPEC,
    ])
    .await?;
    Ok(())
}

async fn refresh_remote(path: &Path) -> Result<()> {
    run_git(["--git-dir", path_text(path)?, "remote", "update", "--prune"]).await?;
    let _ = optional_git_output([
        "--git-dir",
        path_text(path)?,
        "remote",
        "set-head",
        "origin",
        "--auto",
    ])
    .await?;
    Ok(())
}

async fn resolve_mirror_revision(path: &Path, base_ref: &str) -> Result<Option<String>> {
    let candidates = mirror_revision_candidates(base_ref);
    for candidate in &candidates {
        if let Some(revision) = optional_git_output([
            "--git-dir",
            path_text(path)?,
            "rev-parse",
            "--verify",
            &format!("{candidate}^{{commit}}"),
        ])
        .await?
        {
            return Ok(Some(revision));
        }
    }
    Ok(None)
}

fn mirror_revision_candidates(base_ref: &str) -> Vec<String> {
    if base_ref == "HEAD" {
        return vec!["refs/remotes/origin/HEAD".to_string()];
    }
    if let Some(branch) = base_ref.strip_prefix("refs/heads/") {
        return vec![format!("refs/remotes/origin/{branch}")];
    }
    if let Some(branch) = base_ref.strip_prefix("origin/") {
        return vec![format!("refs/remotes/origin/{branch}")];
    }
    if base_ref.starts_with("refs/") {
        return vec![base_ref.to_string()];
    }
    vec![
        format!("refs/remotes/origin/{base_ref}"),
        format!("refs/tags/{base_ref}"),
        base_ref.to_string(),
    ]
}

async fn ensure_mirror_has_revision(path: &Path, revision: &str) -> Result<()> {
    let resolved = optional_git_output([
        "--git-dir",
        path_text(path)?,
        "rev-parse",
        "--verify",
        &format!("{revision}^{{commit}}"),
    ])
    .await?;
    if resolved.as_deref() == Some(revision) {
        return Ok(());
    }
    Err(CoordinatorError::Workspace(format!(
        "managed cache no longer contains recorded base revision {revision}"
    )))
}

fn unresolved_base_ref(repository: &str, base_ref: &str) -> CoordinatorError {
    CoordinatorError::Workspace(format!(
        "repository {repository} does not resolve requested base ref {base_ref} after update"
    ))
}

async fn optional_git_output<'a>(
    args: impl IntoIterator<Item = &'a str>,
) -> Result<Option<String>> {
    let args = args.into_iter().collect::<Vec<_>>();
    let output = Command::new("git")
        .args(&args)
        .output()
        .await
        .map_err(workspace_io)?;
    if !output.status.success() {
        return Ok(None);
    }
    Ok(Some(
        String::from_utf8_lossy(&output.stdout).trim().to_string(),
    ))
}

async fn run_git<'a>(args: impl IntoIterator<Item = &'a str>) -> Result<String> {
    let args = args.into_iter().collect::<Vec<_>>();
    let output = Command::new("git")
        .args(&args)
        .output()
        .await
        .map_err(workspace_io)?;
    if !output.status.success() {
        return Err(CoordinatorError::Workspace(format!(
            "git {} failed with {}: {}",
            args.join(" "),
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

async fn run_git_with_index<'a>(
    worktree: &Path,
    index: &Path,
    args: impl IntoIterator<Item = &'a str>,
) -> Result<String> {
    let args = args.into_iter().collect::<Vec<_>>();
    let output = Command::new("git")
        .arg("-C")
        .arg(worktree)
        .args(&args)
        .env("GIT_INDEX_FILE", index)
        .output()
        .await
        .map_err(workspace_io)?;
    if !output.status.success() {
        return Err(CoordinatorError::Workspace(format!(
            "git -C {} {} with temporary index failed with {}: {}",
            worktree.display(),
            args.join(" "),
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

async fn run_git_bytes_with_index<'a>(
    worktree: &Path,
    index: &Path,
    args: impl IntoIterator<Item = &'a str>,
) -> Result<Vec<u8>> {
    let args = args.into_iter().collect::<Vec<_>>();
    let output = Command::new("git")
        .arg("-C")
        .arg(worktree)
        .args(&args)
        .env("GIT_INDEX_FILE", index)
        .output()
        .await
        .map_err(workspace_io)?;
    if !output.status.success() {
        return Err(CoordinatorError::Workspace(format!(
            "git {} failed with {}: {}",
            args.join(" "),
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(output.stdout)
}

async fn generate_result_patch(
    worktree: &Path,
    base_revision: &str,
    index: &Path,
) -> Result<Vec<u8>> {
    run_git_with_index(worktree, index, ["read-tree", base_revision]).await?;
    run_git_with_index(worktree, index, ["add", "-A", "--", "."]).await?;
    run_git_bytes_with_index(
        worktree,
        index,
        [
            "diff",
            "--cached",
            "--binary",
            "--full-index",
            "--no-renames",
            "--no-ext-diff",
            "--no-textconv",
            base_revision,
            "--",
        ],
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestRoot(PathBuf);

    impl TestRoot {
        fn new() -> Self {
            Self(std::env::temp_dir().join(format!(
                "factory-mirror-test-{}-{}",
                std::process::id(),
                Uuid::new_v4()
            )))
        }
    }

    impl Drop for TestRoot {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn repository_metadata_root_is_stable_and_repository_scoped() {
        let manager = WorkspaceManager::new("/workspaces").unwrap();
        let first = manager.repository_metadata_root("repository-a").unwrap();
        let repeated = manager.repository_metadata_root("repository-a").unwrap();
        let second = manager.repository_metadata_root("repository-b").unwrap();

        assert_eq!(first, repeated);
        assert!(first.starts_with("/workspaces/mirrors/"));
        assert!(first.ends_with(".git"));
        assert_ne!(first, second);
        assert!(manager.repository_metadata_root(" ").is_err());
    }

    async fn create_source(root: &Path, name: &str, contents: &str) -> (PathBuf, String) {
        let source = root.join(name);
        run_git(["init", "-b", "main", path_text(&source).unwrap()])
            .await
            .unwrap();
        run_git([
            "-C",
            path_text(&source).unwrap(),
            "config",
            "user.name",
            "Factory Mirror Test",
        ])
        .await
        .unwrap();
        run_git([
            "-C",
            path_text(&source).unwrap(),
            "config",
            "user.email",
            "factory-mirror@example.invalid",
        ])
        .await
        .unwrap();
        tokio::fs::write(source.join("README.md"), contents)
            .await
            .unwrap();
        run_git(["-C", path_text(&source).unwrap(), "add", "README.md"])
            .await
            .unwrap();
        run_git([
            "-C",
            path_text(&source).unwrap(),
            "commit",
            "-m",
            "source fixture",
        ])
        .await
        .unwrap();
        let revision = run_git(["-C", path_text(&source).unwrap(), "rev-parse", "HEAD"])
            .await
            .unwrap();
        (source, revision)
    }

    async fn create_managed_record(
        root: &Path,
        source: &Path,
        job_name: &str,
    ) -> (WorkspaceManager, WorkspaceRecord, PathBuf) {
        let workspaces = root.join(format!("workspaces-{job_name}"));
        tokio::fs::create_dir_all(workspaces.join("mirrors"))
            .await
            .unwrap();
        tokio::fs::create_dir_all(workspaces.join("jobs"))
            .await
            .unwrap();
        let manager = WorkspaceManager::new(&workspaces).unwrap();
        let repository = path_text(source).unwrap();
        let repository_id = format!("fixture:{job_name}:{repository}");
        let (mirror, revision) = manager
            .ensure_mirror(&repository_id, repository, "main")
            .await
            .unwrap();
        let job_id = JobId::new(job_name);
        let worktree = workspaces.join("jobs").join(job_id.as_str());
        let branch_name = format!("factory/{job_id}");
        run_git([
            "--git-dir",
            path_text(&mirror).unwrap(),
            "worktree",
            "add",
            "--force",
            "-B",
            branch_name.as_str(),
            path_text(&worktree).unwrap(),
            revision.as_str(),
        ])
        .await
        .unwrap();
        let canonical_root = tokio::fs::canonicalize(&worktree).await.unwrap();
        let now = chrono::Utc::now();
        let record = WorkspaceRecord {
            job_id,
            repository_id,
            repository: repository.to_string(),
            base_ref: "main".to_string(),
            base_revision: revision.clone(),
            branch_name,
            root: path_text(&canonical_root).unwrap().to_string(),
            revision,
            state: WorkspaceState::Active,
            created_at: now,
            updated_at: now,
        };
        (manager, record, canonical_root)
    }

    async fn quarantined_marker_exists(mirrors: &Path, marker: &str) -> bool {
        let mut entries = tokio::fs::read_dir(mirrors).await.unwrap();
        while let Some(entry) = entries.next_entry().await.unwrap() {
            if entry.file_name().to_string_lossy().contains(".invalid-")
                && entry.path().join(marker).is_file()
            {
                return true;
            }
        }
        false
    }

    #[tokio::test]
    async fn repository_identity_keeps_fixed_locator_mirrors_separate() {
        let root = TestRoot::new();
        tokio::fs::create_dir_all(&root.0).await.unwrap();
        let (first_source, first_revision) =
            create_source(&root.0, "project", "first repository\n").await;
        let workspaces = root.0.join("workspaces-repository-identity");
        let manager = WorkspaceManager::new(&workspaces).unwrap();
        let locator = path_text(&first_source).unwrap().to_string();
        let (first_mirror, resolved_first) = manager
            .ensure_mirror("local:first", &locator, "main")
            .await
            .unwrap();
        let (_, resolved_first_head) = manager
            .ensure_mirror("local:first", &locator, "HEAD")
            .await
            .unwrap();
        assert_eq!(resolved_first_head, first_revision);

        tokio::fs::rename(&first_source, root.0.join("first-repository"))
            .await
            .unwrap();
        let (second_source, second_revision) =
            create_source(&root.0, "project", "second repository\n").await;
        assert_eq!(path_text(&second_source).unwrap(), locator);
        let (second_mirror, resolved_second) = manager
            .ensure_mirror("local:second", &locator, "main")
            .await
            .unwrap();

        assert_ne!(first_mirror, second_mirror);
        assert_eq!(resolved_first, first_revision);
        assert_eq!(resolved_second, second_revision);
        assert_ne!(resolved_first, resolved_second);
        assert_eq!(
            run_git([
                "--git-dir",
                path_text(&first_mirror).unwrap(),
                "show",
                &format!("{resolved_first}:README.md"),
            ])
            .await
            .unwrap(),
            "first repository"
        );
        assert_eq!(
            run_git([
                "--git-dir",
                path_text(&second_mirror).unwrap(),
                "show",
                &format!("{resolved_second}:README.md"),
            ])
            .await
            .unwrap(),
            "second repository"
        );
    }

    #[tokio::test]
    async fn remote_refresh_preserves_factory_branches_and_active_worktrees() {
        let root = TestRoot::new();
        tokio::fs::create_dir_all(&root.0).await.unwrap();
        let (source, first_revision) = create_source(&root.0, "source", "first revision\n").await;
        let workspaces = root.0.join("workspaces-shared-repository");
        tokio::fs::create_dir_all(workspaces.join("jobs"))
            .await
            .unwrap();
        let manager = WorkspaceManager::new(&workspaces).unwrap();
        let repository = path_text(&source).unwrap();
        let repository_id = "fixture:shared-repository";
        let (mirror, resolved_first) = manager
            .ensure_mirror(repository_id, repository, "main")
            .await
            .unwrap();
        assert_eq!(resolved_first, first_revision);

        let first_worktree = workspaces.join("jobs/first-job");
        run_git([
            "--git-dir",
            path_text(&mirror).unwrap(),
            "worktree",
            "add",
            "-B",
            "factory/first-job",
            path_text(&first_worktree).unwrap(),
            first_revision.as_str(),
        ])
        .await
        .unwrap();
        tokio::fs::write(first_worktree.join("JOB-ONE.txt"), b"uncommitted job one\n")
            .await
            .unwrap();

        tokio::fs::write(source.join("README.md"), b"second revision\n")
            .await
            .unwrap();
        run_git(["-C", repository, "add", "README.md"])
            .await
            .unwrap();
        run_git(["-C", repository, "commit", "-m", "advance source"])
            .await
            .unwrap();
        let second_revision = run_git(["-C", repository, "rev-parse", "HEAD"])
            .await
            .unwrap();

        let (same_mirror, resolved_second) = manager
            .ensure_mirror(repository_id, repository, "main")
            .await
            .unwrap();
        assert_eq!(same_mirror, mirror);
        assert_eq!(resolved_second, second_revision);
        assert_eq!(
            run_git([
                "--git-dir",
                path_text(&mirror).unwrap(),
                "rev-parse",
                "factory/first-job",
            ])
            .await
            .unwrap(),
            first_revision
        );
        assert_eq!(
            run_git([
                "-C",
                path_text(&first_worktree).unwrap(),
                "rev-parse",
                "HEAD",
            ])
            .await
            .unwrap(),
            first_revision
        );
        assert_eq!(
            tokio::fs::read(first_worktree.join("JOB-ONE.txt"))
                .await
                .unwrap(),
            b"uncommitted job one\n"
        );

        let first_fetch = run_git([
            "--git-dir",
            path_text(&mirror).unwrap(),
            "config",
            "--get-all",
            "remote.origin.fetch",
        ])
        .await
        .unwrap();
        assert!(
            first_fetch
                .lines()
                .any(|line| line == REMOTE_BRANCH_REFSPEC)
        );
        assert!(first_fetch.lines().any(|line| line == REMOTE_TAG_REFSPEC));
        assert!(!first_fetch.lines().any(|line| line == "+refs/*:refs/*"));
    }

    #[tokio::test]
    async fn legacy_mirror_is_migrated_in_place_when_it_has_an_active_worktree() {
        let root = TestRoot::new();
        tokio::fs::create_dir_all(&root.0).await.unwrap();
        let (source, first_revision) = create_source(&root.0, "source", "legacy revision\n").await;
        let workspaces = root.0.join("workspaces-legacy-migration");
        tokio::fs::create_dir_all(workspaces.join("mirrors"))
            .await
            .unwrap();
        tokio::fs::create_dir_all(workspaces.join("jobs"))
            .await
            .unwrap();
        let manager = WorkspaceManager::new(&workspaces).unwrap();
        let repository = path_text(&source).unwrap();
        let mirror = manager.mirror_path("fixture:legacy-active");
        run_git([
            "clone",
            "--mirror",
            "--",
            repository,
            path_text(&mirror).unwrap(),
        ])
        .await
        .unwrap();
        let worktree = workspaces.join("jobs/legacy-job");
        run_git([
            "--git-dir",
            path_text(&mirror).unwrap(),
            "worktree",
            "add",
            "-B",
            "factory/legacy-job",
            path_text(&worktree).unwrap(),
            first_revision.as_str(),
        ])
        .await
        .unwrap();
        tokio::fs::write(worktree.join("LEGACY-JOB.txt"), b"retain me\n")
            .await
            .unwrap();

        tokio::fs::write(source.join("README.md"), b"new remote revision\n")
            .await
            .unwrap();
        run_git(["-C", repository, "add", "README.md"])
            .await
            .unwrap();
        run_git(["-C", repository, "commit", "-m", "advance remote"])
            .await
            .unwrap();
        let moved_revision = run_git(["-C", repository, "rev-parse", "HEAD"])
            .await
            .unwrap();

        let (same_mirror, resolved) = manager
            .ensure_mirror("fixture:legacy-active", repository, "main")
            .await
            .unwrap();
        assert_eq!(same_mirror, mirror);
        assert_eq!(resolved, moved_revision);
        assert!(
            mirror_matches_repository(&mirror, repository)
                .await
                .unwrap()
        );
        assert_eq!(
            run_git(["-C", path_text(&worktree).unwrap(), "rev-parse", "HEAD"])
                .await
                .unwrap(),
            first_revision
        );
        assert_eq!(
            tokio::fs::read(worktree.join("LEGACY-JOB.txt"))
                .await
                .unwrap(),
            b"retain me\n"
        );
    }

    #[tokio::test]
    async fn invalid_partial_mirror_is_quarantined_and_rebuilt() {
        let root = TestRoot::new();
        tokio::fs::create_dir_all(&root.0).await.unwrap();
        let (source, source_revision) = create_source(&root.0, "source", "expected source\n").await;
        let workspaces = root.0.join("workspaces");
        let manager = WorkspaceManager::new(&workspaces).unwrap();
        tokio::fs::create_dir_all(workspaces.join("mirrors"))
            .await
            .unwrap();
        let mirror = manager.mirror_path(path_text(&source).unwrap());
        tokio::fs::create_dir_all(&mirror).await.unwrap();
        tokio::fs::write(mirror.join("interrupted-clone"), b"preserve me")
            .await
            .unwrap();

        let (recovered, revision) = manager
            .ensure_mirror(
                path_text(&source).unwrap(),
                path_text(&source).unwrap(),
                "main",
            )
            .await
            .unwrap();

        assert_eq!(recovered, mirror);
        assert_eq!(revision, source_revision);
        assert!(
            mirror_matches_repository(&mirror, path_text(&source).unwrap())
                .await
                .unwrap()
        );
        assert!(quarantined_marker_exists(&workspaces.join("mirrors"), "interrupted-clone").await);
    }

    #[tokio::test]
    async fn empty_bare_mirror_without_origin_is_quarantined_and_rebuilt() {
        let root = TestRoot::new();
        tokio::fs::create_dir_all(&root.0).await.unwrap();
        let (source, source_revision) = create_source(&root.0, "source", "expected source\n").await;
        let workspaces = root.0.join("workspaces");
        let mirrors = workspaces.join("mirrors");
        tokio::fs::create_dir_all(&mirrors).await.unwrap();
        let manager = WorkspaceManager::new(&workspaces).unwrap();
        let mirror = manager.mirror_path(path_text(&source).unwrap());
        run_git(["init", "--bare", path_text(&mirror).unwrap()])
            .await
            .unwrap();
        tokio::fs::write(mirror.join("empty-bare-marker"), b"preserve me")
            .await
            .unwrap();

        let (recovered, revision) = manager
            .ensure_mirror(
                path_text(&source).unwrap(),
                path_text(&source).unwrap(),
                "main",
            )
            .await
            .unwrap();

        assert_eq!(recovered, mirror);
        assert_eq!(revision, source_revision);
        assert!(quarantined_marker_exists(&mirrors, "empty-bare-marker").await);
    }

    #[tokio::test]
    async fn empty_bare_mirror_with_expected_config_is_still_quarantined_and_rebuilt() {
        let root = TestRoot::new();
        tokio::fs::create_dir_all(&root.0).await.unwrap();
        let (source, source_revision) = create_source(&root.0, "source", "expected source\n").await;
        let workspaces = root.0.join("workspaces");
        let mirrors = workspaces.join("mirrors");
        tokio::fs::create_dir_all(&mirrors).await.unwrap();
        let manager = WorkspaceManager::new(&workspaces).unwrap();
        let mirror = manager.mirror_path(path_text(&source).unwrap());
        run_git(["init", "--bare", path_text(&mirror).unwrap()])
            .await
            .unwrap();
        run_git([
            "--git-dir",
            path_text(&mirror).unwrap(),
            "remote",
            "add",
            "--mirror=fetch",
            "origin",
            path_text(&source).unwrap(),
        ])
        .await
        .unwrap();
        tokio::fs::write(mirror.join("empty-configured-marker"), b"preserve me")
            .await
            .unwrap();

        let (recovered, revision) = manager
            .ensure_mirror(
                path_text(&source).unwrap(),
                path_text(&source).unwrap(),
                "main",
            )
            .await
            .unwrap();

        assert_eq!(recovered, mirror);
        assert_eq!(revision, source_revision);
        assert!(quarantined_marker_exists(&mirrors, "empty-configured-marker").await);
    }

    #[tokio::test]
    async fn wrong_origin_is_quarantined_and_base_is_resolved_from_expected_source() {
        let root = TestRoot::new();
        tokio::fs::create_dir_all(&root.0).await.unwrap();
        let (source, source_revision) =
            create_source(&root.0, "expected", "expected source\n").await;
        let (wrong_source, wrong_revision) =
            create_source(&root.0, "wrong", "wrong source\n").await;
        assert_ne!(source_revision, wrong_revision);
        let workspaces = root.0.join("workspaces");
        let mirrors = workspaces.join("mirrors");
        tokio::fs::create_dir_all(&mirrors).await.unwrap();
        let manager = WorkspaceManager::new(&workspaces).unwrap();
        let mirror = manager.mirror_path(path_text(&source).unwrap());
        run_git([
            "clone",
            "--mirror",
            "--",
            path_text(&wrong_source).unwrap(),
            path_text(&mirror).unwrap(),
        ])
        .await
        .unwrap();
        tokio::fs::write(mirror.join("wrong-origin-marker"), b"preserve me")
            .await
            .unwrap();

        let (recovered, revision) = manager
            .ensure_mirror(
                path_text(&source).unwrap(),
                path_text(&source).unwrap(),
                "main",
            )
            .await
            .unwrap();

        assert_eq!(recovered, mirror);
        assert_eq!(revision, source_revision);
        assert!(quarantined_marker_exists(&mirrors, "wrong-origin-marker").await);
        assert!(
            mirror_matches_repository(&mirror, path_text(&source).unwrap())
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn missing_requested_base_is_rejected_before_mirror_publication() {
        let root = TestRoot::new();
        tokio::fs::create_dir_all(&root.0).await.unwrap();
        let (source, _) = create_source(&root.0, "source", "expected source\n").await;
        let workspaces = root.0.join("workspaces");
        tokio::fs::create_dir_all(workspaces.join("mirrors"))
            .await
            .unwrap();
        let manager = WorkspaceManager::new(&workspaces).unwrap();
        let mirror = manager.mirror_path(path_text(&source).unwrap());

        let error = manager
            .ensure_mirror(
                path_text(&source).unwrap(),
                path_text(&source).unwrap(),
                "missing-branch",
            )
            .await
            .unwrap_err();

        assert!(error.to_string().contains("missing-branch"));
        assert!(!mirror.exists(), "unresolved mirrors must not be published");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn restore_cleans_managed_worktree_in_place_at_recorded_revision() {
        use std::os::unix::fs::MetadataExt;

        let root = TestRoot::new();
        tokio::fs::create_dir_all(&root.0).await.unwrap();
        let (source, _) = create_source(&root.0, "source", "expected source\n").await;
        tokio::fs::write(source.join(".gitignore"), b"IGNORED.txt\n")
            .await
            .unwrap();
        run_git(["-C", path_text(&source).unwrap(), "add", ".gitignore"])
            .await
            .unwrap();
        run_git([
            "-C",
            path_text(&source).unwrap(),
            "commit",
            "-m",
            "ignore generated fixture",
        ])
        .await
        .unwrap();
        let source_revision = run_git(["-C", path_text(&source).unwrap(), "rev-parse", "HEAD"])
            .await
            .unwrap();
        let workspaces = root.0.join("workspaces");
        tokio::fs::create_dir_all(workspaces.join("mirrors"))
            .await
            .unwrap();
        tokio::fs::create_dir_all(workspaces.join("jobs"))
            .await
            .unwrap();
        let manager = WorkspaceManager::new(&workspaces).unwrap();
        let (mirror, revision) = manager
            .ensure_mirror(
                path_text(&source).unwrap(),
                path_text(&source).unwrap(),
                "main",
            )
            .await
            .unwrap();
        assert_eq!(revision, source_revision);

        let job_id = JobId::new("restore-test");
        let worktree = workspaces.join("jobs").join(job_id.as_str());
        let branch_name = format!("factory/{job_id}");
        run_git([
            "--git-dir",
            path_text(&mirror).unwrap(),
            "worktree",
            "add",
            "--force",
            "-B",
            branch_name.as_str(),
            path_text(&worktree).unwrap(),
            revision.as_str(),
        ])
        .await
        .unwrap();
        let canonical_root = tokio::fs::canonicalize(&worktree).await.unwrap();
        let now = chrono::Utc::now();
        let record = WorkspaceRecord {
            job_id,
            repository_id: path_text(&source).unwrap().to_string(),
            repository: path_text(&source).unwrap().to_string(),
            base_ref: "main".to_string(),
            base_revision: revision.clone(),
            branch_name,
            root: path_text(&canonical_root).unwrap().to_string(),
            revision,
            state: WorkspaceState::Active,
            created_at: now,
            updated_at: now,
        };
        manager.validate_pristine(&record).await.unwrap();
        let inode_before = tokio::fs::metadata(&worktree).await.unwrap().ino();

        tokio::fs::write(source.join("SOURCE-ADVANCED.txt"), b"newer source commit\n")
            .await
            .unwrap();
        run_git([
            "-C",
            path_text(&source).unwrap(),
            "add",
            "SOURCE-ADVANCED.txt",
        ])
        .await
        .unwrap();
        run_git([
            "-C",
            path_text(&source).unwrap(),
            "commit",
            "-m",
            "advance mutable source ref",
        ])
        .await
        .unwrap();

        tokio::fs::write(worktree.join("README.md"), b"committed mutation\n")
            .await
            .unwrap();
        run_git(["-C", record.root.as_str(), "add", "README.md"])
            .await
            .unwrap();
        run_git([
            "-C",
            record.root.as_str(),
            "-c",
            "user.name=Factory Restore Test",
            "-c",
            "user.email=factory-restore@example.invalid",
            "commit",
            "-m",
            "mutate disposable worktree",
        ])
        .await
        .unwrap();
        tokio::fs::write(worktree.join("UNTRACKED.txt"), b"remove me\n")
            .await
            .unwrap();
        tokio::fs::write(worktree.join("IGNORED.txt"), b"remove me too\n")
            .await
            .unwrap();

        let drift = manager.validate_pristine(&record).await.unwrap_err();
        assert!(drift.to_string().contains("drifted from revision"));
        assert!(drift.to_string().contains("IGNORED.txt"));

        manager.restore(&record).await.unwrap();
        let inode_after = tokio::fs::metadata(&worktree).await.unwrap().ino();
        assert_eq!(
            inode_after, inode_before,
            "restore replaced workspace inode"
        );
        manager.validate_pristine(&record).await.unwrap();
        assert_eq!(
            tokio::fs::read_to_string(worktree.join("README.md"))
                .await
                .unwrap(),
            "expected source\n"
        );
        assert!(!worktree.join("UNTRACKED.txt").exists());
        assert!(!worktree.join("IGNORED.txt").exists());
        assert!(!worktree.join("SOURCE-ADVANCED.txt").exists());
        assert_eq!(
            tokio::fs::read_to_string(source.join("README.md"))
                .await
                .unwrap(),
            "expected source\n"
        );
        assert!(source.join("SOURCE-ADVANCED.txt").is_file());
    }

    #[tokio::test]
    async fn missing_or_corrupt_worktree_requires_explicit_recreate() {
        let root = TestRoot::new();
        tokio::fs::create_dir_all(&root.0).await.unwrap();
        let (source, _) = create_source(&root.0, "rebind-source", "expected source\n").await;
        let (manager, record, worktree) =
            create_managed_record(&root.0, &source, "rebind-restore").await;

        tokio::fs::remove_file(worktree.join(".git")).await.unwrap();
        let corrupt = manager.restore(&record).await.unwrap_err();
        assert!(matches!(
            corrupt,
            CoordinatorError::WorkspaceRebindRequired { ref job_id, .. }
                if job_id == &record.job_id
        ));
        manager.recreate(&record).await.unwrap();
        manager.validate_pristine(&record).await.unwrap();

        tokio::fs::remove_dir_all(&worktree).await.unwrap();
        let missing = manager.restore(&record).await.unwrap_err();
        assert!(matches!(
            missing,
            CoordinatorError::WorkspaceRebindRequired { ref job_id, .. }
                if job_id == &record.job_id
        ));
        manager.recreate(&record).await.unwrap();
        manager.validate_pristine(&record).await.unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn detached_review_restores_guarded_content_and_preserves_ignored_artifacts() {
        use std::os::unix::fs::symlink;

        let root = TestRoot::new();
        tokio::fs::create_dir_all(&root.0).await.unwrap();
        let (source, _) = create_source(&root.0, "review-source", "source readme\n").await;
        tokio::fs::write(source.join("tracked.txt"), b"source tracked\n")
            .await
            .unwrap();
        tokio::fs::write(source.join("binary.bin"), [0_u8, 1, 2, 3])
            .await
            .unwrap();
        tokio::fs::write(source.join(".gitignore"), b"*.ignored\n")
            .await
            .unwrap();
        symlink("README.md", source.join("link")).unwrap();
        run_git(["-C", path_text(&source).unwrap(), "add", "-A"])
            .await
            .unwrap();
        run_git([
            "-C",
            path_text(&source).unwrap(),
            "commit",
            "-m",
            "add review fixture files",
        ])
        .await
        .unwrap();

        let (manager, record, worktree) =
            create_managed_record(&root.0, &source, "review-restore").await;
        tokio::fs::write(worktree.join("README.md"), b"baseline staged\n")
            .await
            .unwrap();
        run_git(["-C", record.root.as_str(), "add", "README.md"])
            .await
            .unwrap();
        tokio::fs::write(worktree.join("tracked.txt"), b"baseline unstaged\n")
            .await
            .unwrap();
        tokio::fs::write(worktree.join("binary.bin"), [9_u8, 8, 0, 7])
            .await
            .unwrap();
        tokio::fs::remove_file(worktree.join("link")).await.unwrap();
        symlink("tracked.txt", worktree.join("link")).unwrap();
        tokio::fs::write(worktree.join("baseline-new.txt"), b"baseline untracked\n")
            .await
            .unwrap();

        let snapshot = manager.capture_review_snapshot(&record).await.unwrap();

        tokio::fs::write(worktree.join("README.md"), b"review mutation\n")
            .await
            .unwrap();
        tokio::fs::remove_file(worktree.join("tracked.txt"))
            .await
            .unwrap();
        tokio::fs::write(worktree.join("binary.bin"), [5_u8, 4, 3, 2])
            .await
            .unwrap();
        tokio::fs::remove_file(worktree.join("link")).await.unwrap();
        symlink("binary.bin", worktree.join("link")).unwrap();
        tokio::fs::write(
            worktree.join("baseline-new.txt"),
            b"review changed untracked\n",
        )
        .await
        .unwrap();
        tokio::fs::write(worktree.join("review-new.txt"), b"remove this\n")
            .await
            .unwrap();
        tokio::fs::write(
            worktree.join("compiler.ignored"),
            b"leave ignored artifact\n",
        )
        .await
        .unwrap();

        assert!(
            manager
                .restore_review_snapshot(&record, snapshot)
                .await
                .unwrap()
        );
        assert_eq!(
            tokio::fs::read(worktree.join("README.md")).await.unwrap(),
            b"baseline staged\n"
        );
        assert_eq!(
            tokio::fs::read(worktree.join("tracked.txt")).await.unwrap(),
            b"baseline unstaged\n"
        );
        assert_eq!(
            tokio::fs::read(worktree.join("binary.bin")).await.unwrap(),
            [9_u8, 8, 0, 7]
        );
        assert_eq!(
            std::fs::read_link(worktree.join("link")).unwrap(),
            PathBuf::from("tracked.txt")
        );
        assert_eq!(
            tokio::fs::read(worktree.join("baseline-new.txt"))
                .await
                .unwrap(),
            b"baseline untracked\n"
        );
        assert!(!worktree.join("review-new.txt").exists());
        assert_eq!(
            tokio::fs::read(worktree.join("compiler.ignored"))
                .await
                .unwrap(),
            b"leave ignored artifact\n"
        );
        assert_eq!(
            run_git([
                "-C",
                record.root.as_str(),
                "diff",
                "--cached",
                "--name-only",
            ])
            .await
            .unwrap(),
            "README.md"
        );
        assert!(manager.recover_review_snapshot(&record).await.unwrap());
        manager.acknowledge_review_mutation(&record).await.unwrap();
    }

    #[tokio::test]
    async fn replacement_process_recovers_a_durable_review_snapshot() {
        let root = TestRoot::new();
        tokio::fs::create_dir_all(&root.0).await.unwrap();
        let (source, _) = create_source(&root.0, "recovery-source", "source readme\n").await;
        let (manager, record, worktree) =
            create_managed_record(&root.0, &source, "review-recovery").await;
        tokio::fs::write(worktree.join("README.md"), b"pre-review implementation\n")
            .await
            .unwrap();
        let snapshot = manager.capture_review_snapshot(&record).await.unwrap();
        tokio::fs::write(worktree.join("README.md"), b"abandoned review mutation\n")
            .await
            .unwrap();
        tokio::fs::write(worktree.join("review-only.txt"), b"remove me\n")
            .await
            .unwrap();
        drop(snapshot);
        drop(manager);

        let replacement = WorkspaceManager::new(root.0.join("workspaces-review-recovery")).unwrap();
        assert!(replacement.recover_review_snapshot(&record).await.unwrap());
        assert_eq!(
            tokio::fs::read(worktree.join("README.md")).await.unwrap(),
            b"pre-review implementation\n"
        );
        assert!(!worktree.join("review-only.txt").exists());
        assert!(replacement.recover_review_snapshot(&record).await.unwrap());
        replacement
            .acknowledge_review_mutation(&record)
            .await
            .unwrap();
        assert!(
            !root
                .0
                .join("workspaces-review-recovery/review-snapshots/review-recovery")
                .exists()
        );
    }

    #[tokio::test]
    async fn read_only_git_index_refresh_is_not_a_review_mutation() {
        let root = TestRoot::new();
        tokio::fs::create_dir_all(&root.0).await.unwrap();
        let (source, _) = create_source(&root.0, "read-only-source", "source readme\n").await;
        let (manager, record, worktree) =
            create_managed_record(&root.0, &source, "review-read-only").await;
        let snapshot = manager.capture_review_snapshot(&record).await.unwrap();

        tokio::fs::write(worktree.join("README.md"), b"source readme\n")
            .await
            .unwrap();
        run_git(["-C", record.root.as_str(), "status", "--short"])
            .await
            .unwrap();

        assert!(
            !manager
                .restore_review_snapshot(&record, snapshot)
                .await
                .unwrap()
        );
        assert!(
            !root
                .0
                .join("workspaces-review-read-only/review-snapshots/review-read-only")
                .exists()
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn result_patch_applies_text_binary_deletion_untracked_and_mode_changes() {
        use std::io::Write;
        use std::os::unix::fs::PermissionsExt;
        use std::process::Stdio;

        let root = TestRoot::new();
        tokio::fs::create_dir_all(&root.0).await.unwrap();
        let (source, _) = create_source(&root.0, "result-source", "base readme\n").await;
        tokio::fs::write(source.join("delete.txt"), b"delete me\n")
            .await
            .unwrap();
        tokio::fs::write(source.join("binary.bin"), [0_u8, 1, 2, 3])
            .await
            .unwrap();
        tokio::fs::write(source.join("mode.sh"), b"#!/bin/sh\nexit 0\n")
            .await
            .unwrap();
        run_git([
            "-C",
            path_text(&source).unwrap(),
            "add",
            "delete.txt",
            "binary.bin",
            "mode.sh",
        ])
        .await
        .unwrap();
        run_git([
            "-C",
            path_text(&source).unwrap(),
            "commit",
            "-m",
            "result base",
        ])
        .await
        .unwrap();

        let (_, record, worktree) = create_managed_record(&root.0, &source, "result-export").await;
        tokio::fs::write(worktree.join("README.md"), b"result readme\n")
            .await
            .unwrap();
        tokio::fs::remove_file(worktree.join("delete.txt"))
            .await
            .unwrap();
        tokio::fs::write(worktree.join("binary.bin"), [9_u8, 0, 8, 7])
            .await
            .unwrap();
        tokio::fs::write(worktree.join("new.bin"), [6_u8, 0, 5, 4])
            .await
            .unwrap();
        let mut permissions = tokio::fs::metadata(worktree.join("mode.sh"))
            .await
            .unwrap()
            .permissions();
        permissions.set_mode(0o755);
        tokio::fs::set_permissions(worktree.join("mode.sh"), permissions)
            .await
            .unwrap();

        let index = root.0.join("result.index");
        let patch = generate_result_patch(&worktree, &record.base_revision, &index)
            .await
            .unwrap();
        assert!(String::from_utf8_lossy(&patch).contains("GIT binary patch"));

        let target = root.0.join("result-target");
        run_git([
            "clone",
            "--",
            path_text(&source).unwrap(),
            path_text(&target).unwrap(),
        ])
        .await
        .unwrap();
        let mut child = std::process::Command::new("git")
            .arg("-C")
            .arg(&target)
            .args(["apply", "--binary", "-"])
            .stdin(Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.as_mut().unwrap().write_all(&patch).unwrap();
        assert!(child.wait().unwrap().success());

        assert_eq!(
            tokio::fs::read(target.join("README.md")).await.unwrap(),
            b"result readme\n"
        );
        assert!(!target.join("delete.txt").exists());
        assert_eq!(
            tokio::fs::read(target.join("binary.bin")).await.unwrap(),
            [9_u8, 0, 8, 7]
        );
        assert_eq!(
            tokio::fs::read(target.join("new.bin")).await.unwrap(),
            [6_u8, 0, 5, 4]
        );
        assert_ne!(
            tokio::fs::metadata(target.join("mode.sh"))
                .await
                .unwrap()
                .permissions()
                .mode()
                & 0o100,
            0
        );
        let mode_change = run_git([
            "-C",
            path_text(&target).unwrap(),
            "diff",
            "--summary",
            "--",
            "mode.sh",
        ])
        .await
        .unwrap();
        assert!(mode_change.contains("mode change 100644 => 100755 mode.sh"));
    }
}
