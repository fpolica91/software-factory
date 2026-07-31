use crate::CoordinatorError;
use crate::CoordinatorStore;
use crate::EnsureWorkspaceRequest;
use crate::Result;
use crate::WorkspaceRecord;
use crate::WorkspaceState;
use factory_protocol::ids::JobId;
use sha2::Digest;
use sha2::Sha256;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::process::Command;
use tokio::sync::Mutex;

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
        let _guard = self.mutation_gate.lock().await;
        store.load_job(job_id).await?;

        if let Some(existing) = store.load_workspace(job_id).await? {
            if existing.repository != request.repository || existing.base_ref != request.base_ref {
                return Err(CoordinatorError::InvalidInput(format!(
                    "job {job_id} is already bound to repository {} at {}",
                    existing.repository, existing.base_ref
                )));
            }
            if existing.state == WorkspaceState::Active && Path::new(&existing.root).is_dir() {
                return Ok(existing);
            }
        }

        let mirror = self.mirror_path(&request.repository);
        let worktree = self.root.join("jobs").join(job_id.as_str());
        tokio::fs::create_dir_all(self.root.join("mirrors"))
            .await
            .map_err(workspace_io)?;
        tokio::fs::create_dir_all(self.root.join("jobs"))
            .await
            .map_err(workspace_io)?;

        if mirror.is_dir() {
            run_git([
                "--git-dir",
                path_text(&mirror)?,
                "remote",
                "update",
                "--prune",
            ])
            .await?;
        } else {
            run_git([
                "clone",
                "--mirror",
                "--",
                request.repository.as_str(),
                path_text(&mirror)?,
            ])
            .await?;
        }

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

        let revision = run_git([
            "--git-dir",
            path_text(&mirror)?,
            "rev-parse",
            "--verify",
            &format!("{}^{{commit}}", request.base_ref),
        ])
        .await?;
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
            .put_workspace(
                job_id,
                &request.repository,
                &request.base_ref,
                &branch_name,
                path_text(&canonical_root)?,
                &revision,
            )
            .await
    }

    pub async fn load(&self, store: &CoordinatorStore, job_id: &JobId) -> Result<WorkspaceRecord> {
        store
            .load_workspace(job_id)
            .await?
            .ok_or_else(|| CoordinatorError::WorkspaceNotFound(job_id.clone()))
    }

    pub async fn refresh_revision(
        &self,
        store: &CoordinatorStore,
        job_id: &JobId,
    ) -> Result<WorkspaceRecord> {
        let _guard = self.mutation_gate.lock().await;
        let current = self.load(store, job_id).await?;
        if current.state != WorkspaceState::Active {
            return Err(CoordinatorError::Workspace(format!(
                "workspace for job {job_id} is not active"
            )));
        }
        let revision = run_git(["-C", current.root.as_str(), "rev-parse", "HEAD"]).await?;
        store
            .put_workspace(
                job_id,
                &current.repository,
                &current.base_ref,
                &current.branch_name,
                &current.root,
                &revision,
            )
            .await
    }

    pub async fn remove(
        &self,
        store: &CoordinatorStore,
        job_id: &JobId,
    ) -> Result<WorkspaceRecord> {
        let _guard = self.mutation_gate.lock().await;
        let current = self.load(store, job_id).await?;
        if current.state == WorkspaceState::Removed {
            return Ok(current);
        }
        let worktree = PathBuf::from(&current.root);
        if worktree.exists() {
            let mirror = self.mirror_path(&current.repository);
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

    fn mirror_path(&self, repository: &str) -> PathBuf {
        let digest = Sha256::digest(repository.as_bytes());
        self.root.join("mirrors").join(format!("{digest:x}.git"))
    }
}

fn validate_request(request: &EnsureWorkspaceRequest) -> Result<()> {
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
