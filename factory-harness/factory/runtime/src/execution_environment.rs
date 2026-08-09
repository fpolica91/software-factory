//! Factory-owned provisioning for the remote Codex execution environment.
//!
//! This module owns backend lifecycle only. Codex remains the execution
//! harness and connects to the resulting native `exec-server` through its
//! existing `EnvironmentManager`.

use std::collections::HashMap;
use std::env;
use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::time::Duration;

use anyhow::Context;
use anyhow::Result;
use anyhow::anyhow;
use anyhow::bail;
use bollard::Docker;
use bollard::errors::Error as DockerError;
use bollard::models::ContainerCreateBody;
use bollard::models::ContainerInspectResponse;
use bollard::models::EndpointSettings;
use bollard::models::HostConfig;
use bollard::models::Mount;
use bollard::models::MountPoint;
use bollard::models::MountType;
use bollard::models::MountVolumeOptions;
use bollard::models::NetworkingConfig;
use bollard::query_parameters::CreateContainerOptionsBuilder;
use bollard::query_parameters::RemoveContainerOptionsBuilder;
use bollard::query_parameters::StopContainerOptionsBuilder;
use factory_coordinator::ExecutionEnvironmentRecord;
use tokio::net::TcpStream;

pub const DOCKER_EXECUTION_ENVIRONMENT_BACKEND: &str = "docker";

const EXEC_SERVER_PORT: u16 = 4500;
const WORKSPACES_TARGET: &str = "/workspaces";
const FACTORY_MANAGED_LABEL: &str = "com.software-factory.managed";
const FACTORY_JOB_LABEL: &str = "com.software-factory.job-id";
const FACTORY_ENVIRONMENT_LABEL: &str = "com.software-factory.environment-id";
const FACTORY_GENERATION_LABEL: &str = "com.software-factory.environment-generation";
const READY_TIMEOUT: Duration = Duration::from_secs(30);
const READY_RETRY: Duration = Duration::from_millis(100);

pub type ProvisionFuture<'a> =
    Pin<Box<dyn Future<Output = Result<ProvisionedExecutionEnvironment>> + Send + 'a>>;
pub type ReleaseFuture<'a> = Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExecutionEnvironmentProvisionRequest {
    pub environment: ExecutionEnvironmentRecord,
    pub workspace_root: String,
    pub repository_metadata_root: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExecutionEnvironmentReleaseRequest {
    pub environment: ExecutionEnvironmentRecord,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProvisionedExecutionEnvironment {
    pub backend_ref: String,
    pub url: String,
}

/// Backend seam for one durable job execution environment.
///
/// Calls are deliberately idempotent: every operation and retry re-ensures
/// the same generation-fenced backend object recorded by the coordinator.
pub trait ExecutionEnvironmentProvisioner: Send + Sync {
    fn backend(&self) -> &'static str;

    /// Returns the backend object's stable address before external creation.
    ///
    /// Backends whose object address is assigned only after creation leave this
    /// unset. The runtime persists a returned locator before calling `ensure`,
    /// so retries continue against the original target even if worker
    /// configuration changes.
    fn durable_locator(&self, _environment: &ExecutionEnvironmentRecord) -> Result<Option<String>> {
        Ok(None)
    }

    fn ensure(&self, request: ExecutionEnvironmentProvisionRequest) -> ProvisionFuture<'_>;

    fn release(&self, request: ExecutionEnvironmentReleaseRequest) -> ReleaseFuture<'_>;
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct WorkspaceBackingSpec {
    typ: MountType,
    source: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ScopedWorkspaceMountSpec {
    typ: MountType,
    source: String,
    target: String,
    volume_subpath: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct DockerWorkerTemplate {
    image: String,
    network: String,
    workspace_backing: WorkspaceBackingSpec,
    run_as: Option<(String, String)>,
    image_entrypoint: Option<Vec<String>>,
    image_environment: Option<Vec<String>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct DockerContainerIdentity {
    name: String,
    labels: HashMap<String, String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct DockerContainerSpec {
    identity: DockerContainerIdentity,
    image: String,
    network: String,
    workspace_root: String,
    workspace_mounts: Vec<ScopedWorkspaceMountSpec>,
    run_as: Option<(String, String)>,
    image_entrypoint: Option<Vec<String>>,
    image_environment: Option<Vec<String>>,
    url: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ExistingContainerDisposition {
    Reuse,
    ReplaceOwned,
}

impl DockerContainerIdentity {
    fn new(environment: &ExecutionEnvironmentRecord) -> Self {
        let name = deterministic_container_name(
            environment.environment_id.as_str(),
            environment.generation,
        );
        let labels = HashMap::from([
            (FACTORY_MANAGED_LABEL.to_string(), "true".to_string()),
            (
                FACTORY_JOB_LABEL.to_string(),
                environment.job_id.as_str().to_string(),
            ),
            (
                FACTORY_ENVIRONMENT_LABEL.to_string(),
                environment.environment_id.as_str().to_string(),
            ),
            (
                FACTORY_GENERATION_LABEL.to_string(),
                environment.generation.to_string(),
            ),
        ]);
        Self { name, labels }
    }
}

impl ScopedWorkspaceMountSpec {
    fn docker_mount(&self) -> Mount {
        Mount {
            target: Some(self.target.clone()),
            source: Some(self.source.clone()),
            typ: Some(self.typ),
            read_only: Some(false),
            volume_options: self
                .volume_subpath
                .as_ref()
                .map(|subpath| MountVolumeOptions {
                    subpath: Some(subpath.clone()),
                    ..MountVolumeOptions::default()
                }),
            ..Mount::default()
        }
    }
}

fn workspace_relative_subpath(root: &str) -> Result<String> {
    let prefix = format!("{WORKSPACES_TARGET}/");
    let relative = root.strip_prefix(&prefix).ok_or_else(|| {
        anyhow!(
            "execution workspace path {root:?} is not an absolute descendant of {WORKSPACES_TARGET}"
        )
    })?;
    if relative.is_empty()
        || relative
            .split('/')
            .any(|component| component.is_empty() || matches!(component, "." | ".."))
    {
        bail!("execution workspace path {root:?} is not normalized");
    }
    Ok(relative.to_string())
}

fn scoped_workspace_mount(
    backing: &WorkspaceBackingSpec,
    target: &str,
    relative_subpath: &str,
) -> Result<ScopedWorkspaceMountSpec> {
    let (source, volume_subpath) = match backing.typ {
        MountType::VOLUME => (backing.source.clone(), Some(relative_subpath.to_string())),
        MountType::BIND => {
            let source = Path::new(&backing.source).join(relative_subpath);
            let source = source.to_str().ok_or_else(|| {
                anyhow!(
                    "scoped workspace bind path is not UTF-8: {}",
                    source.display()
                )
            })?;
            (source.to_string(), None)
        }
        _ => bail!("unsupported {WORKSPACES_TARGET} backing mount type"),
    };
    Ok(ScopedWorkspaceMountSpec {
        typ: backing.typ,
        source,
        target: target.to_string(),
        volume_subpath,
    })
}

impl DockerContainerSpec {
    fn new(
        template: &DockerWorkerTemplate,
        environment: &ExecutionEnvironmentRecord,
        workspace_root: &str,
        repository_metadata_root: &str,
    ) -> Result<Self> {
        let identity = DockerContainerIdentity::new(environment);
        let workspace_subpath = workspace_relative_subpath(workspace_root)?;
        let repository_metadata_subpath = workspace_relative_subpath(repository_metadata_root)?;
        if workspace_subpath == repository_metadata_subpath {
            bail!("workspace root and repository metadata root must be distinct");
        }
        let workspace_mounts = vec![
            scoped_workspace_mount(
                &template.workspace_backing,
                workspace_root,
                &workspace_subpath,
            )?,
            scoped_workspace_mount(
                &template.workspace_backing,
                repository_metadata_root,
                &repository_metadata_subpath,
            )?,
        ];
        Ok(Self {
            identity: identity.clone(),
            image: template.image.clone(),
            network: template.network.clone(),
            workspace_root: workspace_root.to_string(),
            workspace_mounts,
            run_as: template.run_as.clone(),
            image_entrypoint: template.image_entrypoint.clone(),
            image_environment: template.image_environment.clone(),
            url: format!("ws://{}:{EXEC_SERVER_PORT}", identity.name),
        })
    }

    fn create_body(&self) -> ContainerCreateBody {
        ContainerCreateBody {
            image: Some(self.image.clone()),
            user: self
                .run_as
                .as_ref()
                .map(|(uid, gid)| format!("{uid}:{gid}")),
            cmd: Some(vec![
                "codex".to_string(),
                "exec-server".to_string(),
                "--listen".to_string(),
                "ws://0.0.0.0:4500".to_string(),
            ]),
            working_dir: Some(self.workspace_root.clone()),
            labels: Some(self.identity.labels.clone()),
            exposed_ports: Some(vec![format!("{EXEC_SERVER_PORT}/tcp")]),
            host_config: Some(HostConfig {
                network_mode: Some(self.network.clone()),
                mounts: Some(
                    self.workspace_mounts
                        .iter()
                        .map(ScopedWorkspaceMountSpec::docker_mount)
                        .collect(),
                ),
                ..HostConfig::default()
            }),
            networking_config: Some(NetworkingConfig {
                endpoints_config: Some(HashMap::from([(
                    self.network.clone(),
                    EndpointSettings {
                        aliases: Some(vec![self.identity.name.clone()]),
                        ..EndpointSettings::default()
                    },
                )])),
            }),
            ..ContainerCreateBody::default()
        }
    }
}

/// Docker Engine implementation that derives its image, network, and shared
/// workspace mount from the running Factory worker rather than a Compose
/// project name.
pub struct DockerExecutionEnvironmentProvisioner {
    docker: Docker,
    template: DockerWorkerTemplate,
}

impl DockerExecutionEnvironmentProvisioner {
    pub async fn discover() -> Result<Self> {
        let docker = Docker::connect_with_local_defaults()
            .context("connect to the local Docker Engine")?
            .negotiate_version()
            .await
            .context("negotiate the Docker Engine API version")?;
        let worker = nonempty_env("FACTORY_DOCKER_WORKER_CONTAINER")
            .or_else(|| nonempty_env("HOSTNAME"))
            .ok_or_else(|| {
                anyhow!(
                    "cannot discover the Factory worker container; set FACTORY_DOCKER_WORKER_CONTAINER"
                )
            })?;
        let inspected = docker
            .inspect_container(&worker, None)
            .await
            .with_context(|| format!("inspect Factory worker container {worker}"))?;
        let mut template = DockerWorkerTemplate::from_worker_inspect(&inspected)?;
        let image = docker
            .inspect_image(&template.image)
            .await
            .with_context(|| format!("inspect Factory worker image {}", template.image))?;
        let image_config = image
            .config
            .as_ref()
            .ok_or_else(|| anyhow!("Factory worker image has no configuration"))?;
        template.image_entrypoint = image_config.entrypoint.clone();
        template.image_environment = image_config.env.clone();
        docker
            .inspect_network(&template.network, None)
            .await
            .with_context(|| format!("inspect Factory worker network {}", template.network))?;
        Ok(Self { docker, template })
    }

    async fn ensure_container(
        &self,
        environment: &ExecutionEnvironmentRecord,
        workspace_root: &str,
        repository_metadata_root: &str,
    ) -> Result<ProvisionedExecutionEnvironment> {
        let spec = DockerContainerSpec::new(
            &self.template,
            environment,
            workspace_root,
            repository_metadata_root,
        )?;
        let mut inspected = self.inspect_or_create_container(&spec).await?;
        if existing_container_disposition(&spec, environment, &inspected)?
            == ExistingContainerDisposition::ReplaceOwned
        {
            self.remove_owned_container(&spec.identity, environment, &inspected)
                .await
                .with_context(|| {
                    format!(
                        "replace owned stale execution container {}",
                        spec.identity.name
                    )
                })?;
            inspected = self.inspect_or_create_container(&spec).await?;
            validate_existing_container(&spec, &inspected)?;
        }
        if !container_is_running(&inspected) {
            match self.docker.start_container(&spec.identity.name, None).await {
                Ok(()) => {}
                Err(error) if docker_status(&error) == Some(304) => {}
                Err(error) => {
                    return Err(error).with_context(|| {
                        format!("start execution container {}", spec.identity.name)
                    });
                }
            }
        }
        wait_until_ready(&self.docker, &spec.identity.name, &spec.network).await?;
        let current = self
            .docker
            .inspect_container(&spec.identity.name, None)
            .await
            .with_context(|| {
                format!("inspect running execution container {}", spec.identity.name)
            })?;
        validate_existing_container(&spec, &current)?;
        if !container_is_running(&current) {
            bail!(
                "execution container {} stopped before becoming ready",
                spec.identity.name
            );
        }
        let backend_ref = required_text("execution container ID", current.id.as_deref())?;
        Ok(ProvisionedExecutionEnvironment {
            backend_ref: backend_ref.to_string(),
            url: spec.url,
        })
    }

    async fn inspect_or_create_container(
        &self,
        spec: &DockerContainerSpec,
    ) -> Result<ContainerInspectResponse> {
        let inspected = match self
            .docker
            .inspect_container(&spec.identity.name, None)
            .await
        {
            Ok(inspected) => inspected,
            Err(error) if docker_status(&error) == Some(404) => {
                let options = CreateContainerOptionsBuilder::default()
                    .name(&spec.identity.name)
                    .build();
                match self
                    .docker
                    .create_container(Some(options), spec.create_body())
                    .await
                {
                    Ok(_) => self
                        .docker
                        .inspect_container(&spec.identity.name, None)
                        .await
                        .with_context(|| {
                            format!(
                                "inspect newly created execution container {}",
                                spec.identity.name
                            )
                        })?,
                    Err(error) if docker_status(&error) == Some(409) => self
                        .docker
                        .inspect_container(&spec.identity.name, None)
                        .await
                        .with_context(|| {
                            format!(
                                "inspect concurrently created execution container {}",
                                spec.identity.name
                            )
                        })?,
                    Err(error) => {
                        return Err(error).with_context(|| {
                            format!("create execution container {}", spec.identity.name)
                        });
                    }
                }
            }
            Err(error) => {
                return Err(error).with_context(|| {
                    format!("inspect execution container {}", spec.identity.name)
                });
            }
        };
        Ok(inspected)
    }

    async fn release_container(&self, environment: &ExecutionEnvironmentRecord) -> Result<()> {
        let identity = DockerContainerIdentity::new(environment);
        let (inspected, release_environment) = match environment.backend_ref.as_deref() {
            Some(backend_ref) => match self.docker.inspect_container(backend_ref, None).await {
                Ok(inspected) => (inspected, environment.clone()),
                Err(error) if docker_status(&error) == Some(404) => {
                    let inspected = match self.docker.inspect_container(&identity.name, None).await
                    {
                        Ok(inspected) => inspected,
                        Err(error) if docker_status(&error) == Some(404) => return Ok(()),
                        Err(error) => {
                            return Err(error).with_context(|| {
                                format!(
                                    "inspect deterministic execution container {} after persisted backend reference disappeared",
                                    identity.name
                                )
                            });
                        }
                    };
                    let reconciled =
                        reconcile_release_fallback(&identity, environment, &inspected)?;
                    (inspected, reconciled)
                }
                Err(error) => {
                    return Err(error)
                        .with_context(|| format!("inspect execution container {backend_ref}"));
                }
            },
            None => match self.docker.inspect_container(&identity.name, None).await {
                Ok(inspected) => (inspected, environment.clone()),
                Err(error) if docker_status(&error) == Some(404) => return Ok(()),
                Err(error) => {
                    return Err(error)
                        .with_context(|| format!("inspect execution container {}", identity.name));
                }
            },
        };
        self.remove_owned_container(&identity, &release_environment, &inspected)
            .await
    }

    async fn remove_owned_container(
        &self,
        identity: &DockerContainerIdentity,
        environment: &ExecutionEnvironmentRecord,
        inspected: &ContainerInspectResponse,
    ) -> Result<()> {
        validate_release_identity(identity, environment, inspected)?;
        let container_id =
            required_text("execution container ID", inspected.id.as_deref())?.to_string();

        if container_is_running(inspected) {
            let options = StopContainerOptionsBuilder::default().t(10).build();
            match self
                .docker
                .stop_container(&container_id, Some(options))
                .await
            {
                Ok(()) => {}
                Err(error) if matches!(docker_status(&error), Some(304 | 404)) => {}
                Err(error) => {
                    return Err(error)
                        .with_context(|| format!("stop execution container {container_id}"));
                }
            }
        }

        let current = match self.docker.inspect_container(&container_id, None).await {
            Ok(inspected) => inspected,
            Err(error) if docker_status(&error) == Some(404) => return Ok(()),
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("reinspect execution container {container_id}"));
            }
        };
        validate_release_identity(identity, environment, &current)?;
        let options = RemoveContainerOptionsBuilder::default()
            .force(false)
            .v(false)
            .build();
        match self
            .docker
            .remove_container(&container_id, Some(options))
            .await
        {
            Ok(()) => Ok(()),
            Err(error) if docker_status(&error) == Some(404) => Ok(()),
            Err(error) => {
                Err(error).with_context(|| format!("remove execution container {container_id}"))
            }
        }
    }
}

impl ExecutionEnvironmentProvisioner for DockerExecutionEnvironmentProvisioner {
    fn backend(&self) -> &'static str {
        DOCKER_EXECUTION_ENVIRONMENT_BACKEND
    }

    fn ensure(&self, request: ExecutionEnvironmentProvisionRequest) -> ProvisionFuture<'_> {
        Box::pin(async move {
            self.ensure_container(
                &request.environment,
                &request.workspace_root,
                &request.repository_metadata_root,
            )
            .await
        })
    }

    fn release(&self, request: ExecutionEnvironmentReleaseRequest) -> ReleaseFuture<'_> {
        Box::pin(async move { self.release_container(&request.environment).await })
    }
}

impl DockerWorkerTemplate {
    fn from_worker_inspect(inspected: &ContainerInspectResponse) -> Result<Self> {
        let image = required_text("Factory worker image ID", inspected.image.as_deref())?;
        let network = select_network(inspected)?;
        let workspace_mount = inspected
            .mounts
            .as_ref()
            .and_then(|mounts| {
                mounts
                    .iter()
                    .find(|mount| mount.destination.as_deref() == Some(WORKSPACES_TARGET))
            })
            .ok_or_else(|| anyhow!("Factory worker has no {WORKSPACES_TARGET} mount"))?;
        let workspace_backing = workspace_backing_spec(workspace_mount)?;
        let uid = nonempty_env("FACTORY_RUN_AS_UID");
        let gid = nonempty_env("FACTORY_RUN_AS_GID");
        let run_as = match (uid, gid) {
            (Some(uid), Some(gid)) => Some((uid, gid)),
            (None, None) => None,
            _ => bail!("FACTORY_RUN_AS_UID and FACTORY_RUN_AS_GID must be set together"),
        };
        Ok(Self {
            image: image.to_string(),
            network,
            workspace_backing,
            run_as,
            image_entrypoint: None,
            image_environment: None,
        })
    }
}

fn select_network(inspected: &ContainerInspectResponse) -> Result<String> {
    let networks = inspected
        .network_settings
        .as_ref()
        .and_then(|settings| settings.networks.as_ref())
        .ok_or_else(|| anyhow!("Factory worker has no Docker network"))?;
    if let Some(requested) = nonempty_env("FACTORY_DOCKER_NETWORK") {
        if networks.contains_key(&requested) {
            return Ok(requested);
        }
        bail!("Factory worker is not attached to requested Docker network {requested}");
    }
    if networks.len() == 1 {
        return Ok(networks.keys().next().expect("one network").clone());
    }
    if let Some(mode) = inspected
        .host_config
        .as_ref()
        .and_then(|host| host.network_mode.as_deref())
        && networks.contains_key(mode)
    {
        return Ok(mode.to_string());
    }
    bail!("Factory worker is attached to multiple Docker networks; set FACTORY_DOCKER_NETWORK")
}

fn workspace_backing_spec(mount: &MountPoint) -> Result<WorkspaceBackingSpec> {
    let typ = match mount.typ.as_deref() {
        Some("volume") => MountType::VOLUME,
        Some("bind") => MountType::BIND,
        Some(other) => bail!("unsupported {WORKSPACES_TARGET} mount type {other}"),
        None => bail!("Factory worker {WORKSPACES_TARGET} mount has no type"),
    };
    let source = if typ == MountType::VOLUME {
        required_text(
            "Factory worker workspace volume name",
            mount.name.as_deref(),
        )?
    } else {
        required_text(
            "Factory worker workspace bind source",
            mount.source.as_deref(),
        )?
    };
    Ok(WorkspaceBackingSpec {
        typ,
        source: source.to_string(),
    })
}

fn validate_existing_container(
    spec: &DockerContainerSpec,
    inspected: &ContainerInspectResponse,
) -> Result<()> {
    let name = &spec.identity.name;
    if inspected.image.as_deref() != Some(spec.image.as_str()) {
        bail!("execution container {name} does not use the worker image");
    }
    let config = inspected
        .config
        .as_ref()
        .ok_or_else(|| anyhow!("execution container {name} has no configuration"))?;
    let labels = config
        .labels
        .as_ref()
        .ok_or_else(|| anyhow!("execution container {name} has no Factory labels"))?;
    for (key, expected) in &spec.identity.labels {
        if labels.get(key) != Some(expected) {
            bail!("execution container {name} has stale Factory labels");
        }
    }
    let expected_command = vec![
        "codex".to_string(),
        "exec-server".to_string(),
        "--listen".to_string(),
        "ws://0.0.0.0:4500".to_string(),
    ];
    if config.cmd.as_ref() != Some(&expected_command) {
        bail!("execution container {name} has an unexpected command");
    }
    if config.entrypoint != spec.image_entrypoint {
        bail!("execution container {name} overrides the worker image entrypoint");
    }
    if config.env != spec.image_environment {
        bail!("execution container {name} overrides the worker image environment");
    }
    if config.working_dir.as_deref() != Some(spec.workspace_root.as_str()) {
        bail!("execution container {name} has an unexpected working directory");
    }
    let expected_user = spec
        .run_as
        .as_ref()
        .map(|(uid, gid)| format!("{uid}:{gid}"));
    let actual_user = config.user.as_deref().filter(|user| !user.is_empty());
    if actual_user != expected_user.as_deref() {
        bail!("execution container {name} does not use the worker UID/GID");
    }
    let networks = inspected
        .network_settings
        .as_ref()
        .and_then(|settings| settings.networks.as_ref())
        .ok_or_else(|| anyhow!("execution container {name} has no network state"))?;
    if networks.len() != 1 || !networks.contains_key(&spec.network) {
        bail!(
            "execution container {name} is not attached only to worker network {}",
            spec.network,
        );
    }

    let configured_mounts = inspected
        .host_config
        .as_ref()
        .and_then(|host| host.mounts.as_ref())
        .ok_or_else(|| anyhow!("execution container {name} has no configured mount state"))?;
    if configured_mounts.len() != spec.workspace_mounts.len()
        || !spec.workspace_mounts.iter().all(|expected| {
            configured_mounts
                .iter()
                .any(|actual| configured_mount_matches(expected, actual))
        })
    {
        bail!(
            "execution container {name} does not have only the configured scoped workspace mounts"
        );
    }

    let observed_mounts = inspected
        .mounts
        .as_ref()
        .ok_or_else(|| anyhow!("execution container {name} has no observed mount state"))?;
    if observed_mounts.len() != spec.workspace_mounts.len()
        || !spec.workspace_mounts.iter().all(|expected| {
            observed_mounts
                .iter()
                .any(|actual| observed_mount_matches(expected, actual))
        })
    {
        bail!(
            "execution container {name} does not have only the observed writable scoped workspace mounts"
        );
    }
    Ok(())
}

fn existing_container_disposition(
    spec: &DockerContainerSpec,
    environment: &ExecutionEnvironmentRecord,
    inspected: &ContainerInspectResponse,
) -> Result<ExistingContainerDisposition> {
    match validate_existing_container(spec, inspected) {
        // An exact deterministic container may have been recreated immediately
        // before a crash that prevented mark_ready from updating backend_ref.
        Ok(()) => Ok(ExistingContainerDisposition::Reuse),
        Err(spec_error) => {
            match validate_release_identity(&spec.identity, environment, inspected) {
                Ok(()) => Ok(ExistingContainerDisposition::ReplaceOwned),
                Err(identity_error) => Err(anyhow!(
                    "execution container {} has stale configuration ({spec_error:#}) but is not the persisted Factory container ({identity_error:#}); it was retained",
                    spec.identity.name
                )),
            }
        }
    }
}

fn configured_mount_matches(expected: &ScopedWorkspaceMountSpec, actual: &Mount) -> bool {
    actual.typ == Some(expected.typ)
        && actual.source.as_deref() == Some(expected.source.as_str())
        && actual.target.as_deref() == Some(expected.target.as_str())
        && actual.read_only != Some(true)
        && actual.consistency.is_none()
        && match expected.typ {
            MountType::VOLUME => {
                actual.bind_options.is_none()
                    && actual.image_options.is_none()
                    && actual.tmpfs_options.is_none()
                    && actual.volume_options
                        == expected
                            .volume_subpath
                            .as_ref()
                            .map(|subpath| MountVolumeOptions {
                                subpath: Some(subpath.clone()),
                                ..MountVolumeOptions::default()
                            })
            }
            MountType::BIND => {
                actual.volume_options.is_none()
                    && actual.image_options.is_none()
                    && actual.tmpfs_options.is_none()
                    && actual
                        .bind_options
                        .as_ref()
                        .is_none_or(|options| options == &Default::default())
            }
            _ => false,
        }
}

fn observed_mount_matches(expected: &ScopedWorkspaceMountSpec, actual: &MountPoint) -> bool {
    actual.destination.as_deref() == Some(expected.target.as_str())
        && actual.rw == Some(true)
        && match expected.typ {
            MountType::VOLUME => {
                actual.typ.as_deref() == Some("volume")
                    && actual.name.as_deref() == Some(expected.source.as_str())
            }
            MountType::BIND => {
                actual.typ.as_deref() == Some("bind")
                    && actual.source.as_deref() == Some(expected.source.as_str())
            }
            _ => false,
        }
}

fn validate_release_identity(
    identity: &DockerContainerIdentity,
    environment: &ExecutionEnvironmentRecord,
    inspected: &ContainerInspectResponse,
) -> Result<()> {
    validate_deterministic_factory_identity(identity, inspected)?;
    if let Some(expected_id) = environment.backend_ref.as_deref()
        && inspected.id.as_deref() != Some(expected_id)
    {
        bail!(
            "execution container {} does not match persisted backend reference",
            identity.name
        );
    }
    Ok(())
}

fn validate_deterministic_factory_identity(
    identity: &DockerContainerIdentity,
    inspected: &ContainerInspectResponse,
) -> Result<()> {
    let actual_name = required_text("execution container name", inspected.name.as_deref())?;
    if actual_name != identity.name && actual_name != format!("/{}", identity.name) {
        bail!(
            "execution container {} does not match its deterministic Factory name",
            identity.name
        );
    }
    let config = inspected
        .config
        .as_ref()
        .ok_or_else(|| anyhow!("execution container {} has no configuration", identity.name))?;
    let labels = config.labels.as_ref().ok_or_else(|| {
        anyhow!(
            "execution container {} has no Factory labels",
            identity.name
        )
    })?;
    for key in [
        FACTORY_MANAGED_LABEL,
        FACTORY_JOB_LABEL,
        FACTORY_ENVIRONMENT_LABEL,
        FACTORY_GENERATION_LABEL,
    ] {
        let expected = identity.labels.get(key).expect("release label is present");
        if labels.get(key) != Some(expected) {
            bail!(
                "execution container {} does not match Factory job/environment/generation labels",
                identity.name
            );
        }
    }
    Ok(())
}

fn reconcile_release_fallback(
    identity: &DockerContainerIdentity,
    environment: &ExecutionEnvironmentRecord,
    inspected: &ContainerInspectResponse,
) -> Result<ExecutionEnvironmentRecord> {
    validate_deterministic_factory_identity(identity, inspected)?;
    let current_id = required_text("execution container ID", inspected.id.as_deref())?;
    let mut reconciled = environment.clone();
    reconciled.backend_ref = Some(current_id.to_string());
    Ok(reconciled)
}

fn container_is_running(inspected: &ContainerInspectResponse) -> bool {
    inspected
        .state
        .as_ref()
        .and_then(|state| state.running)
        .unwrap_or(false)
}

async fn wait_until_ready(docker: &Docker, container_name: &str, network: &str) -> Result<()> {
    tokio::time::timeout(READY_TIMEOUT, async {
        loop {
            if let Ok(inspected) = docker.inspect_container(container_name, None).await {
                let address = inspected
                    .network_settings
                    .as_ref()
                    .and_then(|settings| settings.networks.as_ref())
                    .and_then(|networks| networks.get(network))
                    .and_then(|endpoint| endpoint.ip_address.as_deref())
                    .filter(|address| !address.is_empty())
                    .map(|address| format!("{address}:{EXEC_SERVER_PORT}"))
                    .unwrap_or_else(|| format!("{container_name}:{EXEC_SERVER_PORT}"));
                if TcpStream::connect(address).await.is_ok() {
                    return;
                }
            }
            tokio::time::sleep(READY_RETRY).await;
        }
    })
    .await
    .with_context(|| format!("execution container {container_name} did not become ready"))
}

fn deterministic_container_name(environment_id: &str, generation: u64) -> String {
    let environment_id = environment_id
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.') {
                character
            } else {
                '-'
            }
        })
        .collect::<String>();
    format!("factory-exec-{environment_id}-g{generation}")
}

fn docker_status(error: &DockerError) -> Option<u16> {
    match error {
        DockerError::DockerResponseServerError { status_code, .. } => Some(*status_code),
        _ => None,
    }
}

fn nonempty_env(name: &str) -> Option<String> {
    env::var(name).ok().filter(|value| !value.trim().is_empty())
}

fn required_text<'a>(field: &str, value: Option<&'a str>) -> Result<&'a str> {
    value
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| anyhow!("{field} is missing"))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::Mutex;

    use bollard::exec::CreateExecOptions;
    use bollard::exec::StartExecOptions;
    use bollard::exec::StartExecResults;
    use chrono::Utc;
    use factory_coordinator::ExecutionEnvironmentDesiredState;
    use factory_coordinator::ExecutionEnvironmentId;
    use factory_coordinator::ExecutionEnvironmentStatus;
    use factory_coordinator::JobId;
    use factory_coordinator::WorkspaceManager;
    use factory_coordinator::WorkspaceRecord;
    use factory_coordinator::WorkspaceState;

    use super::*;

    struct RecordingProvisioner {
        requests: Arc<Mutex<Vec<ExecutionEnvironmentProvisionRequest>>>,
    }

    impl ExecutionEnvironmentProvisioner for RecordingProvisioner {
        fn backend(&self) -> &'static str {
            "fake"
        }

        fn ensure(&self, request: ExecutionEnvironmentProvisionRequest) -> ProvisionFuture<'_> {
            self.requests.lock().unwrap().push(request.clone());
            Box::pin(async move {
                Ok(ProvisionedExecutionEnvironment {
                    backend_ref: format!("fake:{}", request.environment.environment_id),
                    url: "ws://fake:4500".to_string(),
                })
            })
        }

        fn release(&self, _request: ExecutionEnvironmentReleaseRequest) -> ReleaseFuture<'_> {
            Box::pin(async { Ok(()) })
        }
    }

    #[tokio::test]
    async fn provisioner_seam_preserves_the_exact_durable_identity() {
        let requests = Arc::new(Mutex::new(Vec::new()));
        let provisioner = RecordingProvisioner {
            requests: Arc::clone(&requests),
        };
        let environment = environment_record("environment-42", 3);
        let request = provision_request(environment.clone());

        let provisioned = provisioner.ensure(request.clone()).await.unwrap();

        assert_eq!(provisioner.backend(), "fake");
        assert_eq!(requests.lock().unwrap().as_slice(), &[request]);
        assert_eq!(provisioned.backend_ref, "fake:environment-42");
        assert_eq!(provisioned.url, "ws://fake:4500");
    }

    #[test]
    fn docker_spec_is_deterministic_and_generation_fenced() {
        let template = docker_template();
        let first = docker_spec(&template, &environment_record("env/42", 7));
        let repeated = docker_spec(&template, &environment_record("env/42", 7));
        let continuation = docker_spec(&template, &environment_record("env/42", 8));

        assert_eq!(first, repeated);
        assert_eq!(first.identity.name, "factory-exec-env-42-g7");
        assert_eq!(first.url, "ws://factory-exec-env-42-g7:4500");
        assert_ne!(first.identity.name, continuation.identity.name);
        assert_eq!(
            first.identity.labels.get(FACTORY_GENERATION_LABEL),
            Some(&"7".to_string())
        );
        let body = first.create_body();
        assert_eq!(body.image.as_deref(), Some("sha256:worker"));
        assert_eq!(body.user.as_deref(), Some("501:20"));
        assert!(body.entrypoint.is_none());
        assert!(body.env.is_none());
        assert_eq!(
            body.cmd,
            Some(vec![
                "codex".to_string(),
                "exec-server".to_string(),
                "--listen".to_string(),
                "ws://0.0.0.0:4500".to_string(),
            ])
        );
        assert_eq!(body.working_dir.as_deref(), Some(workspace_root()));
        let host = body.host_config.unwrap();
        assert_eq!(host.network_mode.as_deref(), Some("example_default"));
        let mounts = host.mounts.unwrap();
        assert_eq!(mounts.len(), 2);
        assert_eq!(mounts[0].typ, Some(MountType::VOLUME));
        assert_eq!(mounts[0].source.as_deref(), Some("example_workspaces"));
        assert_eq!(mounts[0].target.as_deref(), Some(workspace_root()));
        assert_eq!(
            mounts[0]
                .volume_options
                .as_ref()
                .and_then(|options| options.subpath.as_deref()),
            Some("jobs/job-9")
        );
        assert_eq!(
            mounts[1].target.as_deref(),
            Some(repository_metadata_root())
        );
        assert_eq!(
            mounts[1]
                .volume_options
                .as_ref()
                .and_then(|options| options.subpath.as_deref()),
            Some("mirrors/repository.git")
        );
    }

    #[test]
    fn scoped_workspace_paths_reject_root_relative_and_traversal_forms() {
        for invalid in [
            "/workspaces",
            "/workspaces/",
            "workspaces/jobs/job-9",
            "/workspaces/../job-9",
            "/workspaces/jobs/../job-9",
            "/workspaces/jobs/./job-9",
            "/workspaces/jobs//job-9",
            "/workspaces/jobs/job-9/",
            "/other/jobs/job-9",
        ] {
            assert!(
                workspace_relative_subpath(invalid).is_err(),
                "{invalid:?} must be rejected"
            );
        }
        assert_eq!(
            workspace_relative_subpath(workspace_root()).unwrap(),
            "jobs/job-9"
        );
    }

    #[test]
    fn bind_backing_maps_only_the_two_corresponding_host_subdirectories() {
        let mut template = docker_template();
        template.workspace_backing = WorkspaceBackingSpec {
            typ: MountType::BIND,
            source: "/host/factory-workspaces".to_string(),
        };
        let spec = docker_spec(&template, &environment_record("bind-env", 1));
        let mounts = spec.create_body().host_config.unwrap().mounts.unwrap();

        assert_eq!(mounts.len(), 2);
        assert_eq!(mounts[0].typ, Some(MountType::BIND));
        assert_eq!(
            mounts[0].source.as_deref(),
            Some("/host/factory-workspaces/jobs/job-9")
        );
        assert_eq!(mounts[0].target.as_deref(), Some(workspace_root()));
        assert!(mounts[0].volume_options.is_none());
        assert_eq!(
            mounts[1].source.as_deref(),
            Some("/host/factory-workspaces/mirrors/repository.git")
        );
        assert_eq!(
            mounts[1].target.as_deref(),
            Some(repository_metadata_root())
        );
        assert!(mounts[1].volume_options.is_none());
    }

    #[test]
    fn stale_container_reuse_rejects_any_execution_invariant_drift() {
        let spec = docker_spec(&docker_template(), &environment_record("env-9", 2));
        let inspected = inspected_container(&spec);
        validate_existing_container(&spec, &inspected).unwrap();

        let mut normalized_writable = inspected.clone();
        for mount in normalized_writable
            .host_config
            .as_mut()
            .unwrap()
            .mounts
            .as_mut()
            .unwrap()
        {
            mount.read_only = None;
        }
        validate_existing_container(&spec, &normalized_writable).unwrap();

        let mut extra_mount = inspected.clone();
        extra_mount.mounts.as_mut().unwrap().push(MountPoint {
            typ: Some("bind".to_string()),
            source: Some("/var/run/docker.sock".to_string()),
            destination: Some("/var/run/docker.sock".to_string()),
            rw: Some(true),
            ..MountPoint::default()
        });
        assert!(validate_existing_container(&spec, &extra_mount).is_err());

        let mut wrong_subpath = inspected.clone();
        wrong_subpath
            .host_config
            .as_mut()
            .unwrap()
            .mounts
            .as_mut()
            .unwrap()[0]
            .volume_options
            .as_mut()
            .unwrap()
            .subpath = Some("jobs/other-job".to_string());
        assert!(validate_existing_container(&spec, &wrong_subpath).is_err());

        let mut unexpected_volume_option = inspected.clone();
        unexpected_volume_option
            .host_config
            .as_mut()
            .unwrap()
            .mounts
            .as_mut()
            .unwrap()[0]
            .volume_options
            .as_mut()
            .unwrap()
            .no_copy = Some(true);
        assert!(validate_existing_container(&spec, &unexpected_volume_option).is_err());

        let mut consistency_drift = inspected.clone();
        consistency_drift
            .host_config
            .as_mut()
            .unwrap()
            .mounts
            .as_mut()
            .unwrap()[0]
            .consistency = Some("cached".to_string());
        assert!(validate_existing_container(&spec, &consistency_drift).is_err());

        let mut whole_volume = inspected.clone();
        whole_volume
            .host_config
            .as_mut()
            .unwrap()
            .mounts
            .as_mut()
            .unwrap()[0]
            .target = Some(WORKSPACES_TARGET.to_string());
        assert!(validate_existing_container(&spec, &whole_volume).is_err());

        let mut wrong_source = inspected.clone();
        wrong_source
            .host_config
            .as_mut()
            .unwrap()
            .mounts
            .as_mut()
            .unwrap()[1]
            .source = Some("other_workspaces".to_string());
        assert!(validate_existing_container(&spec, &wrong_source).is_err());

        let mut extra_network = inspected.clone();
        extra_network
            .network_settings
            .as_mut()
            .unwrap()
            .networks
            .as_mut()
            .unwrap()
            .insert("unexpected".to_string(), EndpointSettings::default());
        assert!(validate_existing_container(&spec, &extra_network).is_err());

        let mut wrong_process = inspected.clone();
        let config = wrong_process.config.as_mut().unwrap();
        config.entrypoint = Some(vec!["wrapper".to_string()]);
        assert!(validate_existing_container(&spec, &wrong_process).is_err());

        let mut wrong_directory = inspected.clone();
        wrong_directory.config.as_mut().unwrap().working_dir = Some("/tmp".to_string());
        assert!(validate_existing_container(&spec, &wrong_directory).is_err());

        let mut read_only = inspected.clone();
        read_only.mounts.as_mut().unwrap()[0].rw = Some(false);
        assert!(validate_existing_container(&spec, &read_only).is_err());

        let mut extra_configured_mount = inspected;
        extra_configured_mount
            .host_config
            .as_mut()
            .unwrap()
            .mounts
            .as_mut()
            .unwrap()
            .push(Mount {
                target: Some("/var/run/docker.sock".to_string()),
                source: Some("/var/run/docker.sock".to_string()),
                typ: Some(MountType::BIND),
                read_only: Some(false),
                ..Mount::default()
            });
        assert!(validate_existing_container(&spec, &extra_configured_mount).is_err());
    }

    #[test]
    fn bind_reuse_rejects_nondefault_bind_options() {
        let mut template = docker_template();
        template.workspace_backing = WorkspaceBackingSpec {
            typ: MountType::BIND,
            source: "/host/factory-workspaces".to_string(),
        };
        let spec = docker_spec(&template, &environment_record("bind-stale", 1));
        let inspected = inspected_container(&spec);
        validate_existing_container(&spec, &inspected).unwrap();

        let mut stale = inspected;
        stale.host_config.as_mut().unwrap().mounts.as_mut().unwrap()[0].bind_options =
            Some(bollard::models::MountBindOptions {
                propagation: Some(bollard::models::MountBindOptionsPropagationEnum::RSHARED),
                ..bollard::models::MountBindOptions::default()
            });
        assert!(validate_existing_container(&spec, &stale).is_err());
    }

    #[test]
    fn stale_cutover_replaces_only_the_exact_persisted_container() {
        let template = docker_template();
        let mut environment = environment_record("owned-stale", 4);
        environment.backend_ref = Some("container-id".to_string());
        let spec = docker_spec(&template, &environment);
        let current = inspected_container(&spec);
        assert_eq!(
            existing_container_disposition(&spec, &environment, &current).unwrap(),
            ExistingContainerDisposition::Reuse
        );

        let mut wrong_backend_ref_current = environment.clone();
        wrong_backend_ref_current.backend_ref = Some("other-container-id".to_string());
        assert_eq!(
            existing_container_disposition(&spec, &wrong_backend_ref_current, &current).unwrap(),
            ExistingContainerDisposition::Reuse
        );

        let mut stale_owned = current.clone();
        stale_owned.config.as_mut().unwrap().working_dir = Some("/workspaces".to_string());
        assert_eq!(
            existing_container_disposition(&spec, &environment, &stale_owned).unwrap(),
            ExistingContainerDisposition::ReplaceOwned
        );

        let mut unknown_labels = stale_owned.clone();
        unknown_labels
            .config
            .as_mut()
            .unwrap()
            .labels
            .as_mut()
            .unwrap()
            .insert(FACTORY_JOB_LABEL.to_string(), "other-job".to_string());
        assert!(
            existing_container_disposition(&spec, &environment, &unknown_labels)
                .unwrap_err()
                .to_string()
                .contains("it was retained")
        );

        let mut wrong_backend_ref = environment;
        wrong_backend_ref.backend_ref = Some("other-container-id".to_string());
        assert!(
            existing_container_disposition(&spec, &wrong_backend_ref, &stale_owned)
                .unwrap_err()
                .to_string()
                .contains("it was retained")
        );
    }

    #[test]
    fn release_fallback_reconciles_stale_reference_but_rejects_wrong_identity() {
        let template = docker_template();
        let mut environment = environment_record("release-fallback", 5);
        environment.backend_ref = Some("removed-container-id".to_string());
        let spec = docker_spec(&template, &environment);
        let current = inspected_container(&spec);

        let reconciled =
            reconcile_release_fallback(&spec.identity, &environment, &current).unwrap();
        assert_eq!(reconciled.backend_ref.as_deref(), Some("container-id"));

        let mut wrong_labels = current.clone();
        wrong_labels
            .config
            .as_mut()
            .unwrap()
            .labels
            .as_mut()
            .unwrap()
            .insert(FACTORY_JOB_LABEL.to_string(), "other-job".to_string());
        assert!(reconcile_release_fallback(&spec.identity, &environment, &wrong_labels).is_err());

        let mut wrong_name = current;
        wrong_name.name = Some("/factory-exec-someone-else-g5".to_string());
        assert!(reconcile_release_fallback(&spec.identity, &environment, &wrong_name).is_err());
    }

    #[cfg(unix)]
    #[tokio::test]
    #[ignore = "requires a Docker worker container and its network"]
    async fn docker_backend_live_reuses_one_generation_container() {
        use std::os::unix::fs::MetadataExt;

        let provisioner = DockerExecutionEnvironmentProvisioner::discover()
            .await
            .expect("discover worker template");
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let job_id = JobId::new(format!("factory-exec-live-{nonce}"));
        let repository_id = format!("factory-exec-live-repository-{nonce}");
        let workspaces = WorkspaceManager::new(WORKSPACES_TARGET).unwrap();
        let repository_metadata_root = workspaces.repository_metadata_root(&repository_id).unwrap();
        let fixture = LiveLinkedWorktreeFixture::new(nonce, repository_metadata_root);
        fixture.prepare(&provisioner, nonce).await;
        let revision = live_git_output(&fixture.workspace_root, ["rev-parse", "HEAD"]);
        let now = Utc::now();
        let workspace = WorkspaceRecord {
            job_id: job_id.clone(),
            repository_id,
            repository: fixture.seed_root.clone(),
            base_ref: "main".to_string(),
            base_revision: revision.clone(),
            branch_name: format!("factory/live-{nonce}"),
            root: fixture.workspace_root.clone(),
            revision,
            state: WorkspaceState::Active,
            created_at: now,
            updated_at: now,
        };
        let mut environment =
            environment_record_for_job(&format!("phase3-docker-live-{nonce}"), 1, job_id);

        let first = provisioner
            .ensure(provision_request_for_roots(
                environment.clone(),
                &fixture.workspace_root,
                &fixture.repository_metadata_root,
            ))
            .await
            .expect("create execution container");
        let repeated = provisioner
            .ensure(provision_request_for_roots(
                environment.clone(),
                &fixture.workspace_root,
                &fixture.repository_metadata_root,
            ))
            .await
            .expect("reuse execution container");

        assert_eq!(first, repeated);
        assert!(!first.backend_ref.trim().is_empty());
        assert_eq!(
            first.url,
            format!("ws://factory-exec-phase3-docker-live-{nonce}-g1:4500")
        );
        let run_as = provisioner
            .template
            .run_as
            .as_ref()
            .map(|(uid, gid)| format!("{uid}:{gid}"));
        for command in [
            vec![
                "git".to_string(),
                "-C".to_string(),
                fixture.workspace_root.clone(),
                "status".to_string(),
                "--porcelain=v1".to_string(),
            ],
            vec![
                "git".to_string(),
                "-C".to_string(),
                fixture.workspace_root.clone(),
                "rev-parse".to_string(),
                "--git-common-dir".to_string(),
            ],
            vec![
                "touch".to_string(),
                format!("{}/SCOPED-WRITE", fixture.workspace_root),
            ],
            vec![
                "test".to_string(),
                "-f".to_string(),
                format!("{}/SCOPED-WRITE", fixture.workspace_root),
            ],
            vec![
                "test".to_string(),
                "!".to_string(),
                "-e".to_string(),
                fixture.sibling_root.clone(),
            ],
            vec![
                "test".to_string(),
                "!".to_string(),
                "-e".to_string(),
                fixture.unrelated_mirror_root.clone(),
            ],
            vec![
                "test".to_string(),
                "!".to_string(),
                "-e".to_string(),
                fixture.parent_sentinel.clone(),
            ],
        ] {
            run_container_command(
                &provisioner.docker,
                &first.backend_ref,
                run_as.clone(),
                command,
            )
            .await;
        }

        let inode_before = std::fs::metadata(&fixture.workspace_root).unwrap().ino();
        for command in [
            vec![
                "sh".to_string(),
                "-c".to_string(),
                "printf 'MUTATED\\n' > README.md; printf 'UNTRACKED\\n' > UNTRACKED.txt; printf 'IGNORED\\n' > generated.ignored"
                    .to_string(),
            ],
            vec![
                "git".to_string(),
                "add".to_string(),
                "README.md".to_string(),
            ],
        ] {
            run_container_command(
                &provisioner.docker,
                &first.backend_ref,
                run_as.clone(),
                command,
            )
            .await;
        }
        workspaces
            .restore(&workspace)
            .await
            .expect("restore mounted linked worktree in place");
        let inode_after = std::fs::metadata(&fixture.workspace_root).unwrap().ino();
        assert_eq!(inode_after, inode_before, "restore replaced mounted inode");

        for command in [
            vec![
                "sh".to_string(),
                "-c".to_string(),
                "test -z \"$(git status --porcelain=v1 --ignored=matching)\"".to_string(),
            ],
            vec![
                "grep".to_string(),
                "-qx".to_string(),
                "LINKED-WORKTREE-BASE".to_string(),
                "README.md".to_string(),
            ],
            vec![
                "test".to_string(),
                "!".to_string(),
                "-e".to_string(),
                "UNTRACKED.txt".to_string(),
            ],
            vec![
                "test".to_string(),
                "!".to_string(),
                "-e".to_string(),
                "generated.ignored".to_string(),
            ],
            vec![
                "test".to_string(),
                "!".to_string(),
                "-e".to_string(),
                "SCOPED-WRITE".to_string(),
            ],
            vec!["touch".to_string(), "POST-RESTORE-WRITE".to_string()],
            vec![
                "test".to_string(),
                "-f".to_string(),
                "POST-RESTORE-WRITE".to_string(),
            ],
            vec![
                "test".to_string(),
                "!".to_string(),
                "-e".to_string(),
                fixture.sibling_root.clone(),
            ],
        ] {
            run_container_command(
                &provisioner.docker,
                &first.backend_ref,
                run_as.clone(),
                command,
            )
            .await;
        }
        environment.backend_ref = Some(first.backend_ref);
        environment.desired_state = ExecutionEnvironmentDesiredState::Released;
        environment.status = ExecutionEnvironmentStatus::Releasing;
        provisioner
            .release(ExecutionEnvironmentReleaseRequest { environment })
            .await
            .expect("release execution container");
        fixture.cleanup(&provisioner).await;
    }

    #[tokio::test]
    #[ignore = "requires FACTORY_DOCKER_WORKER_CONTAINER naming a running worker"]
    async fn docker_backend_live_covers_restart_recreate_release_and_identity_mismatch() {
        let provisioner = DockerExecutionEnvironmentProvisioner::discover()
            .await
            .expect("discover worker template");
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let (workspace_root, repository_metadata_root) = live_workspace_roots(nonce);
        prepare_live_workspace_roots(&provisioner, &workspace_root, &repository_metadata_root)
            .await;
        let mut environment = environment_record(&format!("phase4-live-{nonce}"), 1);
        let first = provisioner
            .ensure(provision_request_for_roots(
                environment.clone(),
                &workspace_root,
                &repository_metadata_root,
            ))
            .await
            .expect("create active execution container");

        provisioner
            .docker
            .stop_container(
                &first.backend_ref,
                Some(StopContainerOptionsBuilder::default().t(10).build()),
            )
            .await
            .expect("stop active execution container");
        let restarted = provisioner
            .ensure(provision_request_for_roots(
                environment.clone(),
                &workspace_root,
                &repository_metadata_root,
            ))
            .await
            .expect("restart stopped active execution container");
        assert_eq!(restarted.backend_ref, first.backend_ref);
        let restarted_inspect = provisioner
            .docker
            .inspect_container(&restarted.backend_ref, None)
            .await
            .expect("inspect restarted execution container");
        assert!(container_is_running(&restarted_inspect));

        provisioner
            .docker
            .stop_container(
                &restarted.backend_ref,
                Some(StopContainerOptionsBuilder::default().t(10).build()),
            )
            .await
            .expect("stop active container before removal");
        provisioner
            .docker
            .remove_container(
                &restarted.backend_ref,
                Some(
                    RemoveContainerOptionsBuilder::default()
                        .force(false)
                        .v(false)
                        .build(),
                ),
            )
            .await
            .expect("remove active execution container");
        let recreated = provisioner
            .ensure(provision_request_for_roots(
                environment.clone(),
                &workspace_root,
                &repository_metadata_root,
            ))
            .await
            .expect("recreate missing active execution container");
        assert_ne!(recreated.backend_ref, restarted.backend_ref);

        // Preserve the removed container ID to exercise the crash window where
        // recreation succeeded but mark_ready never persisted the new ID.
        environment.backend_ref = Some(restarted.backend_ref.clone());
        environment.url = Some(recreated.url.clone());
        environment.desired_state = ExecutionEnvironmentDesiredState::Released;
        environment.status = ExecutionEnvironmentStatus::Releasing;
        let release = ExecutionEnvironmentReleaseRequest {
            environment: environment.clone(),
        };
        provisioner
            .release(release.clone())
            .await
            .expect("release execution container");
        assert!(matches!(
            provisioner
                .docker
                .inspect_container(&recreated.backend_ref, None)
                .await
                .unwrap_err(),
            error if docker_status(&error) == Some(404)
        ));
        provisioner
            .release(release)
            .await
            .expect("repeated release treats absence as released");

        let mut mismatched = environment_record(&format!("phase4-mismatch-{nonce}"), 1);
        let mismatch_spec = docker_spec_for_roots(
            &provisioner.template,
            &mismatched,
            &workspace_root,
            &repository_metadata_root,
        );
        let mut mismatch_body = mismatch_spec.create_body();
        mismatch_body.labels.as_mut().unwrap().insert(
            FACTORY_JOB_LABEL.to_string(),
            "not-the-durable-job".to_string(),
        );
        let created = provisioner
            .docker
            .create_container(
                Some(
                    CreateContainerOptionsBuilder::default()
                        .name(&mismatch_spec.identity.name)
                        .build(),
                ),
                mismatch_body,
            )
            .await
            .expect("create mismatched-label fixture");
        provisioner
            .docker
            .start_container(&created.id, None)
            .await
            .expect("start mismatched-label fixture");
        mismatched.backend_ref = Some(created.id.clone());
        mismatched.desired_state = ExecutionEnvironmentDesiredState::Released;
        mismatched.status = ExecutionEnvironmentStatus::Releasing;
        let error = provisioner
            .release(ExecutionEnvironmentReleaseRequest {
                environment: mismatched,
            })
            .await
            .expect_err("mismatched Factory identity must not be released");
        assert!(error.to_string().contains("does not match Factory"));
        let retained = provisioner
            .docker
            .inspect_container(&created.id, None)
            .await
            .expect("mismatched fixture remains present");
        assert!(container_is_running(&retained));
        provisioner
            .docker
            .remove_container(
                &created.id,
                Some(
                    RemoveContainerOptionsBuilder::default()
                        .force(true)
                        .v(false)
                        .build(),
                ),
            )
            .await
            .expect("remove mismatched-label fixture after assertion");
        cleanup_live_workspace_roots(&provisioner, &workspace_root, &repository_metadata_root)
            .await;
    }

    fn docker_template() -> DockerWorkerTemplate {
        DockerWorkerTemplate {
            image: "sha256:worker".to_string(),
            network: "example_default".to_string(),
            workspace_backing: WorkspaceBackingSpec {
                typ: MountType::VOLUME,
                source: "example_workspaces".to_string(),
            },
            run_as: Some(("501".to_string(), "20".to_string())),
            image_entrypoint: None,
            image_environment: Some(vec!["PATH=/usr/local/bin".to_string()]),
        }
    }

    fn workspace_root() -> &'static str {
        "/workspaces/jobs/job-9"
    }

    fn repository_metadata_root() -> &'static str {
        "/workspaces/mirrors/repository.git"
    }

    fn provision_request(
        environment: ExecutionEnvironmentRecord,
    ) -> ExecutionEnvironmentProvisionRequest {
        provision_request_for_roots(environment, workspace_root(), repository_metadata_root())
    }

    fn provision_request_for_roots(
        environment: ExecutionEnvironmentRecord,
        workspace_root: &str,
        repository_metadata_root: &str,
    ) -> ExecutionEnvironmentProvisionRequest {
        ExecutionEnvironmentProvisionRequest {
            environment,
            workspace_root: workspace_root.to_string(),
            repository_metadata_root: repository_metadata_root.to_string(),
        }
    }

    fn docker_spec(
        template: &DockerWorkerTemplate,
        environment: &ExecutionEnvironmentRecord,
    ) -> DockerContainerSpec {
        docker_spec_for_roots(
            template,
            environment,
            workspace_root(),
            repository_metadata_root(),
        )
    }

    fn docker_spec_for_roots(
        template: &DockerWorkerTemplate,
        environment: &ExecutionEnvironmentRecord,
        workspace_root: &str,
        repository_metadata_root: &str,
    ) -> DockerContainerSpec {
        DockerContainerSpec::new(
            template,
            environment,
            workspace_root,
            repository_metadata_root,
        )
        .unwrap()
    }

    fn live_workspace_roots(nonce: u128) -> (String, String) {
        (
            format!("/workspaces/jobs/factory-exec-live-{nonce}"),
            format!("/workspaces/mirrors/factory-exec-live-{nonce}.git"),
        )
    }

    struct LiveLinkedWorktreeFixture {
        workspace_root: String,
        repository_metadata_root: String,
        seed_root: String,
        sibling_root: String,
        unrelated_mirror_root: String,
        parent_sentinel: String,
    }

    impl LiveLinkedWorktreeFixture {
        fn new(nonce: u128, repository_metadata_root: String) -> Self {
            let (workspace_root, _) = live_workspace_roots(nonce);
            Self {
                workspace_root,
                repository_metadata_root,
                seed_root: format!("/workspaces/jobs/factory-exec-seed-{nonce}"),
                sibling_root: format!("/workspaces/jobs/factory-exec-sibling-{nonce}"),
                unrelated_mirror_root: format!(
                    "/workspaces/mirrors/factory-exec-unrelated-{nonce}.git"
                ),
                parent_sentinel: format!("/workspaces/factory-exec-parent-{nonce}.sentinel"),
            }
        }

        async fn prepare(&self, provisioner: &DockerExecutionEnvironmentProvisioner, nonce: u128) {
            let worker = nonempty_env("FACTORY_DOCKER_WORKER_CONTAINER")
                .or_else(|| nonempty_env("HOSTNAME"))
                .expect("name the running Factory worker container");
            let run_as = provisioner
                .template
                .run_as
                .as_ref()
                .map(|(uid, gid)| format!("{uid}:{gid}"));
            let run = |command| {
                run_container_command(&provisioner.docker, &worker, run_as.clone(), command)
            };

            run(vec![
                "git".to_string(),
                "init".to_string(),
                "-b".to_string(),
                "main".to_string(),
                self.seed_root.clone(),
            ])
            .await;
            run(vec![
                "sh".to_string(),
                "-c".to_string(),
                "printf 'LINKED-WORKTREE-BASE\\n' > \"$1\"; printf '*.ignored\\n' > \"$2\""
                    .to_string(),
                "fixture".to_string(),
                format!("{}/README.md", self.seed_root),
                format!("{}/.gitignore", self.seed_root),
            ])
            .await;
            run(vec![
                "git".to_string(),
                "-C".to_string(),
                self.seed_root.clone(),
                "add".to_string(),
                ".".to_string(),
            ])
            .await;
            run(vec![
                "git".to_string(),
                "-C".to_string(),
                self.seed_root.clone(),
                "-c".to_string(),
                "user.name=Factory Live Test".to_string(),
                "-c".to_string(),
                "user.email=factory-live@example.invalid".to_string(),
                "commit".to_string(),
                "-m".to_string(),
                "linked worktree fixture".to_string(),
            ])
            .await;
            run(vec![
                "git".to_string(),
                "init".to_string(),
                "--bare".to_string(),
                self.repository_metadata_root.clone(),
            ])
            .await;
            run(vec![
                "git".to_string(),
                format!("--git-dir={}", self.repository_metadata_root),
                "remote".to_string(),
                "add".to_string(),
                "origin".to_string(),
                self.seed_root.clone(),
            ])
            .await;
            for refspec in [
                "+refs/heads/*:refs/remotes/origin/*",
                "+refs/tags/*:refs/tags/*",
            ] {
                run(vec![
                    "git".to_string(),
                    format!("--git-dir={}", self.repository_metadata_root),
                    "config".to_string(),
                    "--add".to_string(),
                    "remote.origin.fetch".to_string(),
                    refspec.to_string(),
                ])
                .await;
            }
            run(vec![
                "git".to_string(),
                format!("--git-dir={}", self.repository_metadata_root),
                "remote".to_string(),
                "update".to_string(),
                "--prune".to_string(),
            ])
            .await;
            run(vec![
                "git".to_string(),
                format!("--git-dir={}", self.repository_metadata_root),
                "worktree".to_string(),
                "add".to_string(),
                "-b".to_string(),
                format!("factory/live-{nonce}"),
                self.workspace_root.clone(),
                "refs/remotes/origin/main".to_string(),
            ])
            .await;
            run(vec![
                "mkdir".to_string(),
                "-p".to_string(),
                self.sibling_root.clone(),
                self.unrelated_mirror_root.clone(),
            ])
            .await;
            run(vec![
                "touch".to_string(),
                format!("{}/SENTINEL", self.sibling_root),
                format!("{}/SENTINEL", self.unrelated_mirror_root),
                self.parent_sentinel.clone(),
            ])
            .await;
        }

        async fn cleanup(&self, provisioner: &DockerExecutionEnvironmentProvisioner) {
            run_worker_filesystem_command(
                provisioner,
                vec![
                    "rm".to_string(),
                    "-rf".to_string(),
                    "--".to_string(),
                    self.workspace_root.clone(),
                    self.repository_metadata_root.clone(),
                    self.seed_root.clone(),
                    self.sibling_root.clone(),
                    self.unrelated_mirror_root.clone(),
                    self.parent_sentinel.clone(),
                ],
            )
            .await;
        }
    }

    async fn run_worker_filesystem_command(
        provisioner: &DockerExecutionEnvironmentProvisioner,
        command: Vec<String>,
    ) {
        let worker = nonempty_env("FACTORY_DOCKER_WORKER_CONTAINER")
            .or_else(|| nonempty_env("HOSTNAME"))
            .expect("name the running Factory worker container");
        run_container_command(&provisioner.docker, &worker, Some("0".to_string()), command).await;
    }

    async fn run_container_command(
        docker: &Docker,
        container: &str,
        user: Option<String>,
        command: Vec<String>,
    ) {
        let created = docker
            .create_exec(
                container,
                CreateExecOptions {
                    cmd: Some(command),
                    user,
                    ..CreateExecOptions::default()
                },
            )
            .await
            .expect("create Docker fixture command");
        let started = docker
            .start_exec(
                &created.id,
                Some(StartExecOptions {
                    detach: true,
                    ..StartExecOptions::default()
                }),
            )
            .await
            .expect("start Docker fixture command");
        assert!(matches!(started, StartExecResults::Detached));
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let inspected = docker
                    .inspect_exec(&created.id)
                    .await
                    .expect("inspect Docker fixture command");
                if inspected.running == Some(false) {
                    assert_eq!(inspected.exit_code, Some(0));
                    return;
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        })
        .await
        .expect("Docker fixture command timed out");
    }

    async fn prepare_live_workspace_roots(
        provisioner: &DockerExecutionEnvironmentProvisioner,
        workspace_root: &str,
        repository_metadata_root: &str,
    ) {
        run_worker_filesystem_command(
            provisioner,
            vec![
                "mkdir".to_string(),
                "-p".to_string(),
                workspace_root.to_string(),
                repository_metadata_root.to_string(),
            ],
        )
        .await;
    }

    async fn cleanup_live_workspace_roots(
        provisioner: &DockerExecutionEnvironmentProvisioner,
        workspace_root: &str,
        repository_metadata_root: &str,
    ) {
        run_worker_filesystem_command(
            provisioner,
            vec![
                "rm".to_string(),
                "-rf".to_string(),
                "--".to_string(),
                workspace_root.to_string(),
                repository_metadata_root.to_string(),
            ],
        )
        .await;
    }

    fn inspected_container(spec: &DockerContainerSpec) -> ContainerInspectResponse {
        let configured_mounts = spec
            .workspace_mounts
            .iter()
            .map(ScopedWorkspaceMountSpec::docker_mount)
            .collect::<Vec<_>>();
        let observed_mounts = spec
            .workspace_mounts
            .iter()
            .map(|mount| MountPoint {
                typ: Some(mount.typ.to_string()),
                name: (mount.typ == MountType::VOLUME).then(|| mount.source.clone()),
                source: (mount.typ == MountType::BIND).then(|| mount.source.clone()),
                destination: Some(mount.target.clone()),
                rw: Some(true),
                ..MountPoint::default()
            })
            .collect();
        ContainerInspectResponse {
            id: Some("container-id".to_string()),
            name: Some(format!("/{}", spec.identity.name)),
            image: Some(spec.image.clone()),
            config: Some(bollard::models::ContainerConfig {
                user: spec
                    .run_as
                    .as_ref()
                    .map(|(uid, gid)| format!("{uid}:{gid}")),
                env: spec.image_environment.clone(),
                cmd: Some(vec![
                    "codex".to_string(),
                    "exec-server".to_string(),
                    "--listen".to_string(),
                    "ws://0.0.0.0:4500".to_string(),
                ]),
                working_dir: Some(spec.workspace_root.clone()),
                entrypoint: spec.image_entrypoint.clone(),
                labels: Some(spec.identity.labels.clone()),
                ..bollard::models::ContainerConfig::default()
            }),
            host_config: Some(HostConfig {
                network_mode: Some(spec.network.clone()),
                mounts: Some(configured_mounts),
                ..HostConfig::default()
            }),
            mounts: Some(observed_mounts),
            network_settings: Some(bollard::models::NetworkSettings {
                networks: Some(HashMap::from([(
                    spec.network.clone(),
                    EndpointSettings::default(),
                )])),
                ..bollard::models::NetworkSettings::default()
            }),
            ..ContainerInspectResponse::default()
        }
    }

    fn environment_record(environment_id: &str, generation: u64) -> ExecutionEnvironmentRecord {
        environment_record_for_job(environment_id, generation, JobId::new("job-9"))
    }

    fn environment_record_for_job(
        environment_id: &str,
        generation: u64,
        job_id: JobId,
    ) -> ExecutionEnvironmentRecord {
        ExecutionEnvironmentRecord {
            job_id,
            environment_id: ExecutionEnvironmentId::new(environment_id),
            backend: DOCKER_EXECUTION_ENVIRONMENT_BACKEND.to_string(),
            generation,
            desired_state: ExecutionEnvironmentDesiredState::Active,
            status: ExecutionEnvironmentStatus::Provisioning,
            backend_ref: None,
            url: None,
            error: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    fn live_git_output<const N: usize>(root: &str, args: [&str; N]) -> String {
        let output = std::process::Command::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .output()
            .expect("run live Git command");
        assert!(
            output.status.success(),
            "live Git command failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap().trim().to_string()
    }
}
