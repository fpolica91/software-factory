use std::collections::BTreeMap;
use std::time::Duration;

use anyhow::{Context, Result, ensure};
use factory_coordinator::ExecutionEnvironmentRecord;
use k8s_openapi::api::core::v1::*;
use k8s_openapi::apimachinery::pkg::api::resource::Quantity;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;
use k8s_openapi::apimachinery::pkg::util::intstr::IntOrString;
use uuid::Uuid;

use crate::execution_environment::ExecutionEnvironmentProvisionRequest;

pub(crate) const EXEC_SERVER_PORT: i32 = 4500;
const IDENTITY_ANNOTATION: &str = "software-factory.io/execution-environment";

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct KubernetesResourceConfig {
    pub cpu_request_millis: Option<u32>,
    pub memory_request_mib: Option<u32>,
    pub cpu_limit_millis: Option<u32>,
    pub memory_limit_mib: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KubernetesExecutionEnvironmentConfig {
    pub namespace: String,
    pub image: String,
    pub workspace_pvc: String,
    pub workspace_root: String,
    pub runtime_class_name: Option<String>,
    pub run_as_uid: Option<i64>,
    pub run_as_gid: Option<i64>,
    pub readiness_timeout: Duration,
    pub resources: KubernetesResourceConfig,
}

impl KubernetesExecutionEnvironmentConfig {
    pub(crate) fn normalized(mut self) -> Result<Self> {
        for (field, value) in [
            ("namespace", &mut self.namespace),
            ("execution image", &mut self.image),
            ("workspace PVC", &mut self.workspace_pvc),
            ("workspace root", &mut self.workspace_root),
        ] {
            *value = value.trim().to_string();
            ensure!(!value.is_empty(), "Kubernetes {field} must not be empty");
        }
        validate_kubernetes_image_reference(&self.image)?;
        validate_absolute_path("Kubernetes workspace root", &self.workspace_root)?;
        ensure!(
            self.workspace_root != "/" && !self.workspace_root.ends_with('/'),
            "Kubernetes workspace root must be a normalized non-root path"
        );
        self.runtime_class_name = self
            .runtime_class_name
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string);
        match (self.run_as_uid, self.run_as_gid) {
            (Some(uid), Some(gid)) => {
                ensure!(uid >= 0 && gid >= 0, "run-as IDs must be non-negative")
            }
            (None, None) => {}
            _ => anyhow::bail!("Kubernetes run-as UID and GID must be configured together"),
        }
        ensure!(
            !self.readiness_timeout.is_zero(),
            "readiness timeout must be positive"
        );
        self.resources.normalize()?;
        Ok(self)
    }
}

impl KubernetesResourceConfig {
    fn normalize(&mut self) -> Result<()> {
        for (field, value) in [
            ("CPU request millicores", self.cpu_request_millis),
            ("memory request MiB", self.memory_request_mib),
            ("CPU limit millicores", self.cpu_limit_millis),
            ("memory limit MiB", self.memory_limit_mib),
        ] {
            ensure!(
                value.is_none_or(|value| value > 0),
                "{field} must be positive"
            );
        }
        self.cpu_request_millis = self.cpu_request_millis.or(self.cpu_limit_millis);
        self.memory_request_mib = self.memory_request_mib.or(self.memory_limit_mib);
        ensure!(
            self.cpu_request_millis
                .zip(self.cpu_limit_millis)
                .is_none_or(|(request, limit)| request <= limit),
            "CPU request cannot exceed its limit"
        );
        ensure!(
            self.memory_request_mib
                .zip(self.memory_limit_mib)
                .is_none_or(|(request, limit)| request <= limit),
            "memory request cannot exceed its limit"
        );
        Ok(())
    }

    fn requirements(&self) -> Option<ResourceRequirements> {
        let map = |cpu: Option<u32>, memory: Option<u32>| {
            let mut values = BTreeMap::new();
            if let Some(cpu) = cpu {
                let cpu = if cpu % 1000 == 0 {
                    (cpu / 1000).to_string()
                } else {
                    format!("{cpu}m")
                };
                values.insert("cpu".to_string(), Quantity(cpu));
            }
            if let Some(memory) = memory {
                values.insert("memory".to_string(), Quantity(canonical_memory(memory)));
            }
            values
        };
        let requests = map(self.cpu_request_millis, self.memory_request_mib);
        let limits = map(self.cpu_limit_millis, self.memory_limit_mib);
        (!requests.is_empty() || !limits.is_empty()).then_some(ResourceRequirements {
            requests: (!requests.is_empty()).then_some(requests),
            limits: (!limits.is_empty()).then_some(limits),
            ..ResourceRequirements::default()
        })
    }
}

fn canonical_memory(memory_mib: u32) -> String {
    let (mut value, mut unit) = (u64::from(memory_mib), 0);
    const UNITS: [&str; 4] = ["Mi", "Gi", "Ti", "Pi"];
    while unit + 1 < UNITS.len() && value % 1024 == 0 {
        value /= 1024;
        unit += 1;
    }
    format!("{value}{}", UNITS[unit])
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct OwnedPodIdentity {
    pub name: String,
    annotation: String,
}

impl OwnedPodIdentity {
    pub(crate) fn new(environment: &ExecutionEnvironmentRecord) -> Result<Self> {
        let environment_id = Uuid::parse_str(environment.environment_id.as_str())
            .context("execution environment ID must be a UUID for Kubernetes")?;
        let name = format!(
            "factory-{}-g{}",
            environment_id.simple(),
            environment.generation
        );
        ensure!(
            name.len() <= 63,
            "deterministic Kubernetes Pod name is too long"
        );
        Ok(Self {
            name,
            annotation: format!(
                "{}/{}/{}",
                environment.job_id, environment.environment_id, environment.generation
            ),
        })
    }

    pub(crate) fn validate(&self, namespace: &str, pod: &Pod) -> Result<()> {
        ensure!(
            pod.metadata.name.as_deref() == Some(&self.name),
            "Pod name drifted"
        );
        ensure!(
            pod.metadata.namespace.as_deref() == Some(namespace),
            "Pod namespace drifted"
        );
        ensure!(
            annotation(pod, IDENTITY_ANNOTATION) == Some(&self.annotation),
            "Pod generation identity drifted"
        );
        Ok(())
    }
}

#[derive(Clone, Debug)]
pub(crate) struct OwnedPod {
    pub name: String,
    pub manifest: Pod,
    pub identity: OwnedPodIdentity,
}

impl OwnedPod {
    pub(crate) fn new(
        config: &KubernetesExecutionEnvironmentConfig,
        request: &ExecutionEnvironmentProvisionRequest,
    ) -> Result<Self> {
        validate_kubernetes_image_reference(&config.image)?;
        ensure!(
            request.workspace_root != request.repository_metadata_root,
            "worktree and Git common directory must be distinct"
        );
        let identity = OwnedPodIdentity::new(&request.environment)?;
        let mount = |path: &str| -> Result<VolumeMount> {
            Ok(VolumeMount {
                name: "workspace".to_string(),
                mount_path: path.to_string(),
                read_only: Some(false),
                sub_path: Some(workspace_subpath(&config.workspace_root, path)?),
                ..VolumeMount::default()
            })
        };
        let manifest = Pod {
            metadata: ObjectMeta {
                name: Some(identity.name.clone()),
                namespace: Some(config.namespace.clone()),
                labels: Some(BTreeMap::from([
                    (
                        "app.kubernetes.io/managed-by".to_string(),
                        "software-factory".to_string(),
                    ),
                    (
                        "app.kubernetes.io/component".to_string(),
                        "execution".to_string(),
                    ),
                    (
                        "software-factory.io/job-id".to_string(),
                        request.environment.job_id.to_string(),
                    ),
                    (
                        "software-factory.io/environment-id".to_string(),
                        request.environment.environment_id.to_string(),
                    ),
                    (
                        "software-factory.io/environment-generation".to_string(),
                        request.environment.generation.to_string(),
                    ),
                ])),
                annotations: Some(BTreeMap::from([(
                    IDENTITY_ANNOTATION.to_string(),
                    identity.annotation.clone(),
                )])),
                ..ObjectMeta::default()
            },
            spec: Some(PodSpec {
                automount_service_account_token: Some(false),
                enable_service_links: Some(false),
                restart_policy: Some("Never".to_string()),
                runtime_class_name: config.runtime_class_name.clone(),
                security_context: config.run_as_uid.map(|uid| PodSecurityContext {
                    run_as_user: Some(uid),
                    run_as_group: config.run_as_gid,
                    fs_group: config.run_as_gid,
                    ..PodSecurityContext::default()
                }),
                containers: vec![Container {
                    name: "codex-exec-server".to_string(),
                    image: Some(config.image.clone()),
                    image_pull_policy: Some("IfNotPresent".to_string()),
                    command: Some(vec!["codex".to_string()]),
                    args: Some(vec![
                        "exec-server".to_string(),
                        "--listen".to_string(),
                        "ws://0.0.0.0:4500".to_string(),
                    ]),
                    ports: Some(vec![ContainerPort {
                        container_port: EXEC_SERVER_PORT,
                        name: Some("exec-server".to_string()),
                        protocol: Some("TCP".to_string()),
                        ..ContainerPort::default()
                    }]),
                    readiness_probe: Some(Probe {
                        tcp_socket: Some(TCPSocketAction {
                            host: None,
                            port: IntOrString::Int(EXEC_SERVER_PORT),
                        }),
                        initial_delay_seconds: Some(1),
                        period_seconds: Some(1),
                        timeout_seconds: Some(1),
                        failure_threshold: Some(3),
                        success_threshold: Some(1),
                        ..Probe::default()
                    }),
                    resources: config.resources.requirements(),
                    volume_mounts: Some(vec![
                        mount(&request.workspace_root)?,
                        mount(&request.repository_metadata_root)?,
                    ]),
                    working_dir: Some(request.workspace_root.clone()),
                    ..Container::default()
                }],
                volumes: Some(vec![Volume {
                    name: "workspace".to_string(),
                    persistent_volume_claim: Some(PersistentVolumeClaimVolumeSource {
                        claim_name: config.workspace_pvc.clone(),
                        read_only: Some(false),
                    }),
                    ..Volume::default()
                }]),
                ..PodSpec::default()
            }),
            status: None,
        };
        Ok(Self {
            name: identity.name.clone(),
            manifest,
            identity,
        })
    }

    pub(crate) fn validate(&self, observed: &Pod, expected_uid: Option<&str>) -> Result<()> {
        let namespace = self
            .manifest
            .metadata
            .namespace
            .as_deref()
            .expect("Pod namespace");
        self.identity.validate(namespace, observed)?;
        if let Some(expected_uid) = expected_uid {
            ensure!(required_uid(observed)? == expected_uid, "Pod UID drifted");
        }
        self.validate_required_invariants(observed)?;
        Ok(())
    }

    pub(crate) fn disposition(
        &self,
        observed: &Pod,
        expected_uid: Option<&str>,
        may_adopt_replacement: bool,
    ) -> Result<ExistingPodDisposition> {
        let namespace = self
            .manifest
            .metadata
            .namespace
            .as_deref()
            .expect("Pod namespace");
        self.identity.validate(namespace, observed)?;
        let uid = required_uid(observed)?;
        if self.validate_required_invariants(observed).is_err() {
            return Ok(ExistingPodDisposition::ReplaceOwned(uid.to_string()));
        }
        ensure!(
            may_adopt_replacement || expected_uid.is_none_or(|expected| expected == uid),
            "Pod UID does not match the active backend reference"
        );
        Ok(ExistingPodDisposition::Reuse(uid.to_string()))
    }

    fn validate_required_invariants(&self, observed: &Pod) -> Result<()> {
        let expected = self.manifest.spec.as_ref().expect("owned Pod spec");
        let actual = observed.spec.as_ref().context("Pod has no spec")?;

        ensure!(
            actual.runtime_class_name == expected.runtime_class_name,
            "Pod runtime class drifted"
        );
        ensure!(
            actual.restart_policy.as_deref() == Some("Never"),
            "Pod restart policy drifted"
        );
        ensure!(
            actual.automount_service_account_token == expected.automount_service_account_token,
            "Pod service-account token setting drifted"
        );
        ensure!(
            actual.enable_service_links == expected.enable_service_links,
            "Pod service-links setting drifted"
        );

        let security_ids = |context: Option<&PodSecurityContext>| {
            context.map_or((None, None, None), |context| {
                (context.run_as_user, context.run_as_group, context.fs_group)
            })
        };
        ensure!(
            security_ids(actual.security_context.as_ref())
                == security_ids(expected.security_context.as_ref()),
            "Pod run-as identity drifted"
        );

        ensure!(
            actual.containers.len() == 1,
            "Pod must have exactly one container"
        );
        let container = &actual.containers[0];
        let expected_container = &expected.containers[0];
        ensure!(
            container.name == expected_container.name,
            "container name drifted"
        );
        ensure!(
            container.image == expected_container.image,
            "container image drifted"
        );
        ensure!(
            container.image_pull_policy == expected_container.image_pull_policy,
            "container image pull policy drifted"
        );
        ensure!(
            container.command == expected_container.command,
            "container command drifted"
        );
        ensure!(
            container.args == expected_container.args,
            "container arguments drifted"
        );
        ensure!(
            container.working_dir == expected_container.working_dir,
            "container working directory drifted"
        );

        let ports = container.ports.as_deref().unwrap_or_default();
        let expected_ports = expected_container.ports.as_deref().expect("owned port");
        ensure!(ports.len() == 1, "container must have exactly one port");
        let port = &ports[0];
        let expected_port = &expected_ports[0];
        ensure!(
            port.container_port == expected_port.container_port
                && port.name == expected_port.name
                && port.host_ip == expected_port.host_ip
                && port.host_port == expected_port.host_port
                && port.protocol.as_deref().unwrap_or("TCP")
                    == expected_port.protocol.as_deref().unwrap_or("TCP"),
            "container exec-server port drifted"
        );

        let probe = container
            .readiness_probe
            .as_ref()
            .context("container readiness probe is missing")?;
        ensure!(
            probe.exec.is_none()
                && probe.grpc.is_none()
                && probe.http_get.is_none()
                && probe.tcp_socket.is_some(),
            "readiness probe must use only TCP"
        );
        ensure!(
            Some(probe) == expected_container.readiness_probe.as_ref(),
            "container readiness probe drifted"
        );
        ensure!(
            container.resources == expected_container.resources,
            "container resource requirements drifted"
        );

        let mounts = container.volume_mounts.as_deref().unwrap_or_default();
        let expected_mounts = expected_container
            .volume_mounts
            .as_deref()
            .expect("owned mounts");
        ensure!(
            mounts.len() == 2,
            "container must have exactly two volume mounts"
        );
        for expected_mount in expected_mounts {
            let mount = mounts
                .iter()
                .find(|mount| mount.mount_path == expected_mount.mount_path)
                .with_context(|| {
                    format!(
                        "workspace mount at {} is missing",
                        expected_mount.mount_path
                    )
                })?;
            ensure!(
                mount.name == expected_mount.name
                    && mount.mount_path == expected_mount.mount_path
                    && mount.sub_path == expected_mount.sub_path,
                "workspace mount path contract drifted"
            );
            ensure!(
                mount.read_only.unwrap_or(false) == expected_mount.read_only.unwrap_or(false),
                "workspace mount read-only setting drifted"
            );
            ensure!(
                mount.sub_path_expr.is_none(),
                "workspace mount must not use subPathExpr"
            );
            ensure!(
                matches!(mount.mount_propagation.as_deref(), None | Some("None")),
                "workspace mount propagation drifted"
            );
            ensure!(
                matches!(
                    mount.recursive_read_only.as_deref(),
                    None | Some("Disabled")
                ),
                "workspace recursive read-only setting drifted"
            );
        }

        let volumes = actual.volumes.as_deref().unwrap_or_default();
        let expected_volumes = expected.volumes.as_deref().expect("owned volume");
        ensure!(volumes.len() == 1, "Pod must have exactly one volume");
        ensure!(
            normalized_pvc_volume(&volumes[0])? == normalized_pvc_volume(&expected_volumes[0])?,
            "workspace PVC volume drifted"
        );
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ExistingPodDisposition {
    Reuse(String),
    ReplaceOwned(String),
}

fn validate_kubernetes_image_reference(image: &str) -> Result<()> {
    const EXAMPLE: &str = "registry.example/org/image@sha256:<64 lowercase hex characters>";
    let (name, digest) = image
        .split_once("@sha256:")
        .with_context(|| format!("Kubernetes execution image must use {EXAMPLE}"))?;
    ensure!(
        !name.is_empty() && name.len() <= 255 && !name.contains('@'),
        "Kubernetes execution image must use {EXAMPLE}"
    );
    ensure!(
        digest.len() == 64
            && digest
                .bytes()
                .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f')),
        "Kubernetes execution image must use {EXAMPLE}"
    );
    let (registry, repository) = name
        .split_once('/')
        .with_context(|| format!("Kubernetes execution image must use {EXAMPLE}"))?;
    ensure!(
        valid_registry(registry),
        "Kubernetes execution image must use {EXAMPLE}"
    );
    ensure!(
        !repository.is_empty() && repository.split('/').all(valid_repository_component),
        "Kubernetes execution image must use {EXAMPLE}"
    );
    Ok(())
}

fn valid_registry(registry: &str) -> bool {
    let (host, port) = match registry.rsplit_once(':') {
        Some((host, port)) => (host, Some(port)),
        None => (registry, None),
    };
    if host.is_empty() || host.len() > 253 || host.contains(':') {
        return false;
    }
    if port.is_some_and(|port| {
        port.is_empty()
            || port.len() > 5
            || !port.bytes().all(|byte| byte.is_ascii_digit())
            || !port.parse::<u16>().is_ok_and(|port| port > 0)
    }) {
        return false;
    }
    host.split('.').all(valid_dns_label)
}

fn valid_dns_label(label: &str) -> bool {
    let bytes = label.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= 63
        && bytes.first().is_some_and(u8::is_ascii_alphanumeric)
        && bytes.last().is_some_and(u8::is_ascii_alphanumeric)
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'-')
}

fn valid_repository_component(component: &str) -> bool {
    let bytes = component.as_bytes();
    if bytes.is_empty()
        || !bytes.first().is_some_and(u8::is_ascii_alphanumeric)
        || !bytes.last().is_some_and(u8::is_ascii_alphanumeric)
    {
        return false;
    }
    let mut previous_separator = false;
    for byte in bytes {
        if byte.is_ascii_lowercase() || byte.is_ascii_digit() {
            previous_separator = false;
        } else if matches!(byte, b'.' | b'_' | b'-') && !previous_separator {
            previous_separator = true;
        } else {
            return false;
        }
    }
    true
}

fn normalized_pvc_volume(volume: &Volume) -> Result<Volume> {
    let mut volume = volume.clone();
    let claim = volume
        .persistent_volume_claim
        .as_mut()
        .context("workspace PVC source is missing")?;
    claim.read_only = Some(claim.read_only.unwrap_or(false));
    Ok(volume)
}

fn annotation<'a>(pod: &'a Pod, name: &str) -> Option<&'a String> {
    pod.metadata.annotations.as_ref()?.get(name)
}

fn workspace_subpath(workspace_root: &str, path: &str) -> Result<String> {
    validate_absolute_path("execution workspace path", path)?;
    let relative = path
        .strip_prefix(&format!("{workspace_root}/"))
        .ok_or_else(|| {
            anyhow::anyhow!("execution workspace path {path:?} is not below {workspace_root:?}")
        })?;
    ensure!(!relative.is_empty(), "execution workspace subpath is empty");
    Ok(relative.to_string())
}

fn validate_absolute_path(field: &str, value: &str) -> Result<()> {
    ensure!(value.starts_with('/'), "{field} must be absolute");
    ensure!(
        !value
            .split('/')
            .skip(1)
            .any(|part| part.is_empty() || matches!(part, "." | "..")),
        "{field} must be normalized"
    );
    Ok(())
}

pub(crate) fn required_uid(pod: &Pod) -> Result<&str> {
    pod.metadata
        .uid
        .as_deref()
        .filter(|uid| !uid.trim().is_empty())
        .ok_or_else(|| anyhow::anyhow!("Kubernetes execution Pod has no UID"))
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use factory_coordinator::{
        ExecutionEnvironmentDesiredState, ExecutionEnvironmentId, ExecutionEnvironmentStatus, JobId,
    };

    use super::*;

    const TEST_IMAGE: &str = "registry.example/factory/runtime@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    #[test]
    fn typed_resources_are_positive_and_emit_canonical_quantities() {
        let mut resources = KubernetesResourceConfig {
            cpu_request_millis: Some(500),
            memory_request_mib: Some(1536),
            cpu_limit_millis: Some(2000),
            memory_limit_mib: Some(4096),
        };
        resources.normalize().expect("resources");
        let requirements = resources.requirements().expect("requirements");
        assert_eq!(
            requirements.requests.as_ref().expect("requests")["cpu"].0,
            "500m"
        );
        assert_eq!(
            requirements.requests.as_ref().expect("requests")["memory"].0,
            "1536Mi"
        );
        assert_eq!(requirements.limits.as_ref().expect("limits")["cpu"].0, "2");
        assert_eq!(
            requirements.limits.as_ref().expect("limits")["memory"].0,
            "4Gi"
        );
        resources.cpu_request_millis = Some(0);
        assert!(resources.normalize().is_err());
    }

    #[test]
    fn immutable_image_reference_is_required_at_config_and_pod_boundaries() {
        for valid in [
            TEST_IMAGE,
            "localhost:5000/org/image_name.v2@sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
        ] {
            assert!(
                validate_kubernetes_image_reference(valid).is_ok(),
                "valid image rejected: {valid}"
            );
        }
        for invalid in [
            "registry.example/factory/runtime:edge",
            "factory@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "registry.example/factory/runtime@sha256:",
            "registry.example/factory/runtime@sha256:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
            "registry.example/factory/runtime@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "registry.example/factory/runtime@@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "-registry.example/factory/runtime@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "registry-.example/factory/runtime@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "registry.example//factory/runtime@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "registry.example/factory//runtime@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "registry.example/factory/runtime/@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "registry.example/factory/runtime--bad@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "registry.example/factory/Runtime@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "[::1]:5000/factory/runtime@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "registry.example/factory/runtime:edge@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        ] {
            assert!(
                validate_kubernetes_image_reference(invalid).is_err(),
                "invalid image accepted: {invalid}"
            );
            let mut invalid_config = config(TEST_IMAGE);
            invalid_config.image = invalid.to_string();
            assert!(
                invalid_config.clone().normalized().is_err(),
                "normalized config accepted invalid image: {invalid}"
            );
            assert!(
                OwnedPod::new(&invalid_config, &request()).is_err(),
                "Pod construction accepted invalid image: {invalid}"
            );
        }

        let pod = OwnedPod::new(&config(TEST_IMAGE), &request()).expect("digest Pod");
        assert_eq!(
            pod.manifest.spec.expect("spec").containers[0]
                .image_pull_policy
                .as_deref(),
            Some("IfNotPresent")
        );
    }

    #[test]
    fn native_semantic_defaults_and_mount_order_are_reusable() {
        let expected = OwnedPod::new(&config(TEST_IMAGE), &request()).expect("Pod");
        let mut observed = observed(&expected);
        let spec = observed.spec.as_mut().expect("spec");
        let container = &mut spec.containers[0];
        container.ports.as_mut().expect("ports")[0].protocol = None;
        container.volume_mounts.as_mut().expect("mounts").reverse();
        for mount in container.volume_mounts.as_mut().expect("mounts") {
            mount.read_only = None;
            mount.mount_propagation = Some("None".to_string());
            mount.recursive_read_only = Some("Disabled".to_string());
        }
        spec.volumes.as_mut().expect("volumes")[0]
            .persistent_volume_claim
            .as_mut()
            .expect("PVC")
            .read_only = None;
        assert_eq!(
            expected
                .disposition(&observed, None, false)
                .expect("reuse normalized Pod"),
            ExistingPodDisposition::Reuse("replacement".to_string())
        );
    }

    #[test]
    fn missing_explicit_image_pull_policy_is_stale() {
        let expected = OwnedPod::new(&config(TEST_IMAGE), &request()).expect("Pod");
        assert_stale(&expected, |pod| {
            pod.spec.as_mut().expect("spec").containers[0].image_pull_policy = None;
        });
    }

    #[test]
    fn required_invariant_drift_is_replaceable_before_uid_gate() {
        let expected = OwnedPod::new(&config(TEST_IMAGE), &request()).expect("Pod");
        assert_stale(&expected, |pod| pod.spec = None);
        assert_stale(&expected, |pod| {
            pod.spec.as_mut().expect("spec").runtime_class_name = Some("runc".to_string());
        });
        assert_stale(&expected, |pod| {
            pod.spec.as_mut().expect("spec").restart_policy = Some("Always".to_string());
        });
        assert_stale(&expected, |pod| {
            pod.spec
                .as_mut()
                .expect("spec")
                .automount_service_account_token = None;
        });
        assert_stale(&expected, |pod| {
            pod.spec.as_mut().expect("spec").enable_service_links = None;
        });
        for field in 0..3 {
            assert_stale(&expected, |pod| {
                let context = pod
                    .spec
                    .as_mut()
                    .expect("spec")
                    .security_context
                    .as_mut()
                    .expect("security context");
                match field {
                    0 => context.run_as_user = Some(2000),
                    1 => context.run_as_group = Some(2000),
                    _ => context.fs_group = Some(2000),
                }
            });
        }
        assert_stale(&expected, |pod| {
            let spec = pod.spec.as_mut().expect("spec");
            spec.containers.push(spec.containers[0].clone());
        });
        assert_stale(&expected, |pod| {
            pod.spec.as_mut().expect("spec").containers[0].name = "other".to_string();
        });
        assert_stale(&expected, |pod| {
            pod.spec.as_mut().expect("spec").containers[0].image =
                Some("factory:other".to_string());
        });
        assert_stale(&expected, |pod| {
            pod.spec.as_mut().expect("spec").containers[0].command = None;
        });
        assert_stale(&expected, |pod| {
            pod.spec.as_mut().expect("spec").containers[0].args = None;
        });
        assert_stale(&expected, |pod| {
            pod.spec.as_mut().expect("spec").containers[0].working_dir =
                Some("/workspaces/other".to_string());
        });
        assert_stale(&expected, |pod| {
            pod.spec.as_mut().expect("spec").containers[0]
                .ports
                .as_mut()
                .expect("ports")[0]
                .container_port = 4501;
        });
        assert_stale(&expected, |pod| {
            pod.spec.as_mut().expect("spec").containers[0]
                .readiness_probe
                .as_mut()
                .expect("probe")
                .period_seconds = Some(2);
        });
        assert_stale(&expected, |pod| {
            pod.spec.as_mut().expect("spec").containers[0]
                .resources
                .as_mut()
                .expect("resources")
                .requests
                .as_mut()
                .expect("requests")
                .insert("cpu".to_string(), Quantity("0.5".to_string()));
        });
        assert_stale(&expected, |pod| {
            pod.spec.as_mut().expect("spec").containers[0]
                .volume_mounts
                .as_mut()
                .expect("mounts")[0]
                .sub_path_expr = Some("$(WORKSPACE)".to_string());
        });
        assert_stale(&expected, |pod| {
            pod.spec.as_mut().expect("spec").containers[0]
                .volume_mounts
                .as_mut()
                .expect("mounts")[0]
                .mount_propagation = Some("HostToContainer".to_string());
        });
        assert_stale(&expected, |pod| {
            pod.spec
                .as_mut()
                .expect("spec")
                .volumes
                .as_mut()
                .expect("volumes")[0]
                .persistent_volume_claim
                .as_mut()
                .expect("PVC")
                .read_only = Some(true);
        });
        assert_stale(&expected, |pod| {
            pod.spec
                .as_mut()
                .expect("spec")
                .volumes
                .as_mut()
                .expect("volumes")[0]
                .empty_dir = Some(EmptyDirVolumeSource::default());
        });
    }

    #[test]
    fn manifest_has_identity_but_no_spec_hash_annotation() {
        let expected = OwnedPod::new(&config(TEST_IMAGE), &request()).expect("Pod");
        let annotations = expected
            .manifest
            .metadata
            .annotations
            .as_ref()
            .expect("annotations");
        assert_eq!(annotations.len(), 1);
        assert!(annotations.contains_key(IDENTITY_ANNOTATION));
        assert!(!annotations.contains_key("software-factory.io/desired-spec-sha256"));
    }

    #[test]
    fn identity_mismatches_are_errors() {
        let expected = OwnedPod::new(&config(TEST_IMAGE), &request()).expect("Pod");
        assert_identity_error(&expected, |pod| {
            pod.metadata.name = Some("other".to_string());
        });
        assert_identity_error(&expected, |pod| {
            pod.metadata.namespace = Some("other".to_string());
        });
        assert_identity_error(&expected, |pod| {
            pod.metadata
                .annotations
                .as_mut()
                .expect("annotations")
                .insert(
                    IDENTITY_ANNOTATION.to_string(),
                    "other/generation/1".to_string(),
                );
        });
    }

    #[test]
    fn admitted_owned_field_mutation_fails_validation() {
        let expected = OwnedPod::new(&config(TEST_IMAGE), &request()).expect("Pod");
        let mut admitted = observed(&expected);
        admitted.spec.as_mut().expect("spec").containers[0]
            .resources
            .as_mut()
            .expect("resources")
            .requests
            .as_mut()
            .expect("requests")
            .insert("cpu".to_string(), Quantity("0.5".to_string()));
        assert!(expected.validate(&admitted, Some("replacement")).is_err());
    }

    #[test]
    fn unowned_api_metadata_status_and_scheduler_fields_are_reusable() {
        let expected = OwnedPod::new(&config(TEST_IMAGE), &request()).expect("Pod");
        let mut admitted = observed(&expected);
        admitted.metadata.resource_version = Some("7".to_string());
        admitted
            .metadata
            .labels
            .as_mut()
            .expect("labels")
            .insert("admission.example/label".to_string(), "value".to_string());
        admitted
            .metadata
            .annotations
            .as_mut()
            .expect("annotations")
            .insert(
                "admission.example/annotation".to_string(),
                "value".to_string(),
            );
        admitted.status = Some(PodStatus::default());
        let spec = admitted.spec.as_mut().expect("spec");
        spec.scheduler_name = Some("default-scheduler".to_string());
        spec.node_name = Some("node-a".to_string());
        spec.dns_policy = Some("ClusterFirst".to_string());
        spec.termination_grace_period_seconds = Some(30);
        assert_eq!(
            expected
                .disposition(&admitted, Some("replacement"), false)
                .expect("reuse admitted Pod"),
            ExistingPodDisposition::Reuse("replacement".to_string())
        );
    }

    #[test]
    fn pod_manifest_keeps_runtime_mount_and_exec_server_contract() {
        let expected = OwnedPod::new(&config(TEST_IMAGE), &request()).expect("Pod");
        let spec = expected.manifest.spec.as_ref().expect("spec");
        assert_eq!(spec.runtime_class_name.as_deref(), Some("kata"));
        assert_eq!(spec.restart_policy.as_deref(), Some("Never"));
        assert_eq!(spec.containers.len(), 1);
        let container = &spec.containers[0];
        assert_eq!(container.image_pull_policy.as_deref(), Some("IfNotPresent"));
        assert_eq!(
            container.args.as_deref().expect("args"),
            ["exec-server", "--listen", "ws://0.0.0.0:4500"]
        );
        let mounts = container.volume_mounts.as_deref().expect("mounts");
        assert_eq!(mounts[0].sub_path.as_deref(), Some("jobs/job-1"));
        assert_eq!(
            mounts[1].sub_path.as_deref(),
            Some("repositories/repo-1.git")
        );
    }

    fn observed(expected: &OwnedPod) -> Pod {
        let mut pod = expected.manifest.clone();
        pod.metadata.uid = Some("replacement".to_string());
        pod
    }

    fn assert_stale(expected: &OwnedPod, mutate: impl FnOnce(&mut Pod)) {
        let mut pod = observed(expected);
        mutate(&mut pod);
        assert_eq!(
            expected
                .disposition(&pod, Some("persisted-other-uid"), false)
                .expect("replace stale owned Pod"),
            ExistingPodDisposition::ReplaceOwned("replacement".to_string())
        );
    }

    fn assert_identity_error(expected: &OwnedPod, mutate: impl FnOnce(&mut Pod)) {
        let mut pod = observed(expected);
        mutate(&mut pod);
        assert!(expected.disposition(&pod, None, false).is_err());
    }

    fn config(image: &str) -> KubernetesExecutionEnvironmentConfig {
        KubernetesExecutionEnvironmentConfig {
            namespace: "factory".to_string(),
            image: image.to_string(),
            workspace_pvc: "workspaces".to_string(),
            workspace_root: "/workspaces".to_string(),
            runtime_class_name: Some("kata".to_string()),
            run_as_uid: Some(1000),
            run_as_gid: Some(1000),
            readiness_timeout: Duration::from_secs(30),
            resources: KubernetesResourceConfig {
                cpu_request_millis: Some(500),
                memory_request_mib: Some(1024),
                cpu_limit_millis: Some(2000),
                memory_limit_mib: Some(4096),
            },
        }
        .normalized()
        .expect("config")
    }

    fn request() -> ExecutionEnvironmentProvisionRequest {
        ExecutionEnvironmentProvisionRequest {
            environment: ExecutionEnvironmentRecord {
                job_id: JobId::new("bbbbbbbb-bbbb-4bbb-abbb-bbbbbbbbbbbb"),
                environment_id: ExecutionEnvironmentId::new("aaaaaaaa-aaaa-4aaa-aaaa-aaaaaaaaaaaa"),
                backend: "kubernetes".to_string(),
                generation: 2,
                desired_state: ExecutionEnvironmentDesiredState::Active,
                status: ExecutionEnvironmentStatus::Provisioning,
                backend_ref: None,
                url: None,
                error: None,
                created_at: Utc::now(),
                updated_at: Utc::now(),
            },
            workspace_root: "/workspaces/jobs/job-1".to_string(),
            repository_metadata_root: "/workspaces/repositories/repo-1.git".to_string(),
        }
    }
}
