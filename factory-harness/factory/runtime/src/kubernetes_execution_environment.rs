//! Kubernetes lifecycle for one remote Codex execution Pod per environment generation.

use std::net::IpAddr;
use std::net::SocketAddr;
use std::time::Duration;

use anyhow::Context;
use anyhow::Result;
use anyhow::bail;
use anyhow::ensure;
use factory_coordinator::ExecutionEnvironmentDesiredState;
use factory_coordinator::ExecutionEnvironmentRecord;
use factory_coordinator::ExecutionEnvironmentStatus;
use k8s_openapi::api::core::v1::Pod;
use kube::Api;
use kube::Client;
use kube::api::DeleteParams;
use kube::api::PostParams;
use kube::api::Preconditions;
use tokio::net::TcpStream;
use tokio::time::Instant;
use tokio::time::sleep_until;
use tokio::time::timeout_at;

use crate::execution_environment::ExecutionEnvironmentProvisionRequest;
use crate::execution_environment::ExecutionEnvironmentProvisioner;
use crate::execution_environment::ExecutionEnvironmentReleaseRequest;
use crate::execution_environment::ProvisionFuture;
use crate::execution_environment::ProvisionedExecutionEnvironment;
use crate::execution_environment::ReleaseFuture;
use crate::kubernetes_pod::EXEC_SERVER_PORT;
use crate::kubernetes_pod::ExistingPodDisposition;
use crate::kubernetes_pod::KubernetesExecutionEnvironmentConfig;
use crate::kubernetes_pod::OwnedPod;
use crate::kubernetes_pod::OwnedPodIdentity;
use crate::kubernetes_pod::required_uid;

pub const KUBERNETES_EXECUTION_ENVIRONMENT_BACKEND: &str = "kubernetes";
const READY_RETRY: Duration = Duration::from_millis(200);

pub struct KubernetesExecutionEnvironmentProvisioner {
    client: Client,
    config: KubernetesExecutionEnvironmentConfig,
}

impl KubernetesExecutionEnvironmentProvisioner {
    pub async fn discover(config: KubernetesExecutionEnvironmentConfig) -> Result<Self> {
        let client = Client::try_default()
            .await
            .context("load Kubernetes client configuration")?;
        Self::new(client, config)
    }

    pub fn new(client: Client, config: KubernetesExecutionEnvironmentConfig) -> Result<Self> {
        let config = config.normalized()?;
        Ok(Self { client, config })
    }

    async fn ensure_pod(
        &self,
        request: &ExecutionEnvironmentProvisionRequest,
    ) -> Result<ProvisionedExecutionEnvironment> {
        ensure_backend(&request.environment)?;
        validate_active_environment(&request.environment)?;
        let had_persisted_reference = request.environment.backend_ref.is_some();
        let persisted = resolve_pod_reference(&request.environment, &self.config.namespace)?;
        let mut persisted_config = self.config.clone();
        persisted_config.namespace = persisted.namespace.clone();
        let expected = OwnedPod::new(&persisted_config, request)?;
        persisted.validate_target(&persisted_config.namespace, &expected.name)?;
        let pods = Api::namespaced(self.client.clone(), &persisted_config.namespace);
        let mut may_adopt_replacement = had_persisted_reference;
        let mut expected_uid = persisted.uid.clone();
        let deadline = Instant::now() + self.config.readiness_timeout;

        loop {
            ensure_before_deadline(deadline, "wait for Kubernetes execution Pod")?;
            let Some(pod) = self.get_pod(&pods, &expected.name, deadline, "get").await? else {
                may_adopt_replacement = false;
                let created = self.create_or_observe(&pods, &expected, deadline).await?;
                expected.validate(&created, None)?;
                expected_uid = Some(required_uid(&created)?.to_string());
                pause_until_next_poll(deadline).await;
                continue;
            };

            let disposition =
                expected.disposition(&pod, expected_uid.as_deref(), may_adopt_replacement)?;
            may_adopt_replacement = false;
            let uid = match disposition {
                ExistingPodDisposition::Reuse(uid) => uid,
                ExistingPodDisposition::ReplaceOwned(uid) => {
                    if pod.metadata.deletion_timestamp.is_some() {
                        self.wait_for_uid_gone(&pods, &expected.name, &uid, deadline)
                            .await?;
                    } else {
                        self.delete_and_wait(&pods, &expected.name, &uid, deadline)
                            .await?;
                    }
                    expected_uid = None;
                    continue;
                }
            };
            expected_uid = Some(uid.clone());
            if pod.metadata.deletion_timestamp.is_some() {
                self.wait_for_uid_gone(&pods, &expected.name, &uid, deadline)
                    .await?;
                expected_uid = None;
                continue;
            }
            if pod_is_terminal(&pod) {
                self.delete_and_wait(&pods, &expected.name, &uid, deadline)
                    .await?;
                expected_uid = None;
                continue;
            }
            if pod_is_ready(&pod) {
                let ip = pod_ip(&pod)?;
                match timeout_at(
                    deadline,
                    TcpStream::connect(SocketAddr::new(ip, EXEC_SERVER_PORT as u16)),
                )
                .await
                {
                    Ok(Ok(stream)) => {
                        drop(stream);
                        return Ok(ProvisionedExecutionEnvironment {
                            backend_ref: KubernetesBackendReference {
                                namespace: persisted_config.namespace.clone(),
                                name: expected.name,
                                uid: Some(uid),
                            }
                            .encode(),
                            url: pod_ip_url(ip),
                        });
                    }
                    Ok(Err(_)) => {}
                    Err(_) => bail!("timed out connecting to Kubernetes exec-server"),
                }
            }
            pause_until_next_poll(deadline).await;
        }
    }

    async fn release_pod(&self, environment: &ExecutionEnvironmentRecord) -> Result<()> {
        ensure_backend(environment)?;
        let identity = OwnedPodIdentity::new(environment)?;
        let persisted = resolve_pod_reference(environment, &self.config.namespace)?;
        let namespace = persisted.namespace.as_str();
        let name = persisted.name.as_str();
        let pods = Api::namespaced(self.client.clone(), namespace);
        let deadline = Instant::now() + self.config.readiness_timeout;
        let Some(pod) = self
            .get_pod(&pods, name, deadline, "get for release")
            .await?
        else {
            return Ok(());
        };
        identity.validate(namespace, &pod)?;
        let uid = required_uid(&pod)?;
        if pod.metadata.deletion_timestamp.is_some() {
            self.wait_for_uid_gone(&pods, name, uid, deadline).await
        } else {
            self.delete_and_wait(&pods, name, uid, deadline).await
        }
    }

    async fn delete_and_wait(
        &self,
        pods: &Api<Pod>,
        name: &str,
        uid: &str,
        deadline: Instant,
    ) -> Result<()> {
        let params = DeleteParams::default().preconditions(Preconditions {
            uid: Some(uid.to_string()),
            resource_version: None,
        });
        match timeout_at(deadline, pods.delete(name, &params)).await {
            Err(_) => bail!("timed out deleting Kubernetes execution Pod {name}"),
            Ok(Ok(_)) => {}
            Ok(Err(error)) if kube_status(&error) == Some(404) => {}
            Ok(Err(error)) => return Err(error).context("delete Kubernetes execution Pod"),
        }
        self.wait_for_uid_gone(pods, name, uid, deadline).await
    }

    async fn wait_for_uid_gone(
        &self,
        pods: &Api<Pod>,
        name: &str,
        old_uid: &str,
        deadline: Instant,
    ) -> Result<()> {
        loop {
            ensure_before_deadline(deadline, "wait for Kubernetes execution Pod deletion")?;
            let pod = self
                .get_pod(pods, name, deadline, "wait for deletion")
                .await?;
            if uid_is_gone(pod.as_ref(), old_uid)? {
                return Ok(());
            }
            pause_until_next_poll(deadline).await;
        }
    }

    async fn get_pod(
        &self,
        pods: &Api<Pod>,
        name: &str,
        deadline: Instant,
        action: &str,
    ) -> Result<Option<Pod>> {
        timeout_at(deadline, pods.get_opt(name))
            .await
            .with_context(|| format!("timed out while attempting to {action} Pod {name}"))?
            .with_context(|| format!("{action} Kubernetes execution Pod {name}"))
    }

    async fn create_or_observe(
        &self,
        pods: &Api<Pod>,
        expected: &OwnedPod,
        deadline: Instant,
    ) -> Result<Pod> {
        match timeout_at(
            deadline,
            pods.create(&PostParams::default(), &expected.manifest),
        )
        .await
        {
            Err(_) => bail!(
                "timed out creating Kubernetes execution Pod {}",
                expected.name
            ),
            Ok(Ok(created)) => Ok(created),
            Ok(Err(error)) if kube_status(&error) == Some(409) => self
                .get_pod(pods, &expected.name, deadline, "get after create race")
                .await?
                .ok_or_else(|| anyhow::anyhow!("Pod disappeared after create conflict")),
            Ok(Err(error)) => Err(error).context("create Kubernetes execution Pod"),
        }
    }
}

impl ExecutionEnvironmentProvisioner for KubernetesExecutionEnvironmentProvisioner {
    fn backend(&self) -> &'static str {
        KUBERNETES_EXECUTION_ENVIRONMENT_BACKEND
    }

    fn durable_locator(&self, environment: &ExecutionEnvironmentRecord) -> Result<Option<String>> {
        ensure_backend(environment)?;
        Ok(Some(
            resolve_pod_reference(environment, &self.config.namespace)?
                .locator()
                .encode(),
        ))
    }

    fn ensure(&self, request: ExecutionEnvironmentProvisionRequest) -> ProvisionFuture<'_> {
        Box::pin(async move { self.ensure_pod(&request).await })
    }

    fn release(&self, request: ExecutionEnvironmentReleaseRequest) -> ReleaseFuture<'_> {
        Box::pin(async move { self.release_pod(&request.environment).await })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct KubernetesBackendReference {
    namespace: String,
    name: String,
    uid: Option<String>,
}

impl KubernetesBackendReference {
    fn parse(value: &str) -> Result<Self> {
        let mut parts = value.split('/');
        let namespace = parts.next().unwrap_or_default();
        let name = parts.next().unwrap_or_default();
        let uid = parts.next();
        ensure!(
            !namespace.is_empty()
                && !name.is_empty()
                && uid.is_none_or(|uid| !uid.is_empty())
                && parts.next().is_none(),
            "Kubernetes backend reference must be namespace/name or namespace/name/UID"
        );
        Ok(Self {
            namespace: namespace.to_string(),
            name: name.to_string(),
            uid: uid.map(str::to_string),
        })
    }

    fn encode(&self) -> String {
        match &self.uid {
            Some(uid) => format!("{}/{}/{}", self.namespace, self.name, uid),
            None => format!("{}/{}", self.namespace, self.name),
        }
    }

    fn locator(&self) -> Self {
        Self {
            namespace: self.namespace.clone(),
            name: self.name.clone(),
            uid: None,
        }
    }

    fn validate_target(&self, namespace: &str, name: &str) -> Result<()> {
        ensure!(
            self.namespace == namespace && self.name == name,
            "Kubernetes backend reference does not match the durable environment target"
        );
        Ok(())
    }

    fn validate_name(&self, name: &str) -> Result<()> {
        ensure!(
            self.name == name,
            "Kubernetes backend reference does not match the durable environment name"
        );
        Ok(())
    }
}

fn resolve_pod_reference(
    environment: &ExecutionEnvironmentRecord,
    configured_namespace: &str,
) -> Result<KubernetesBackendReference> {
    let identity = OwnedPodIdentity::new(environment)?;
    let reference = environment
        .backend_ref
        .as_deref()
        .map(KubernetesBackendReference::parse)
        .transpose()?
        .unwrap_or_else(|| KubernetesBackendReference {
            namespace: configured_namespace.to_string(),
            name: identity.name.clone(),
            uid: None,
        });
    reference.validate_name(&identity.name)?;
    Ok(reference)
}

fn ensure_backend(environment: &ExecutionEnvironmentRecord) -> Result<()> {
    ensure!(
        environment.backend == KUBERNETES_EXECUTION_ENVIRONMENT_BACKEND,
        "execution environment belongs to backend {}, not Kubernetes",
        environment.backend
    );
    Ok(())
}

fn validate_active_environment(environment: &ExecutionEnvironmentRecord) -> Result<()> {
    ensure!(
        environment.desired_state == ExecutionEnvironmentDesiredState::Active,
        "Kubernetes execution environment is not active"
    );
    ensure!(
        matches!(
            environment.status,
            ExecutionEnvironmentStatus::Provisioning
                | ExecutionEnvironmentStatus::Ready
                | ExecutionEnvironmentStatus::Failed
        ),
        "Kubernetes execution environment cannot be ensured from its current status"
    );
    Ok(())
}

fn pod_is_terminal(pod: &Pod) -> bool {
    matches!(
        pod.status
            .as_ref()
            .and_then(|status| status.phase.as_deref()),
        Some("Failed" | "Succeeded")
    )
}

fn pod_is_ready(pod: &Pod) -> bool {
    pod.status
        .as_ref()
        .and_then(|status| status.conditions.as_ref())
        .is_some_and(|conditions| {
            conditions
                .iter()
                .any(|condition| condition.type_ == "Ready" && condition.status == "True")
        })
        && pod
            .status
            .as_ref()
            .and_then(|status| status.pod_ip.as_deref())
            .is_some()
}

fn pod_ip(pod: &Pod) -> Result<IpAddr> {
    let value = pod
        .status
        .as_ref()
        .and_then(|status| status.pod_ip.as_deref())
        .ok_or_else(|| anyhow::anyhow!("ready Kubernetes execution Pod has no Pod IP"))?;
    value
        .parse()
        .with_context(|| format!("parse Kubernetes Pod IP {value:?}"))
}

fn pod_ip_url(ip: IpAddr) -> String {
    match ip {
        IpAddr::V4(ip) => format!("ws://{ip}:{EXEC_SERVER_PORT}"),
        IpAddr::V6(ip) => format!("ws://[{ip}]:{EXEC_SERVER_PORT}"),
    }
}

fn uid_is_gone(pod: Option<&Pod>, old_uid: &str) -> Result<bool> {
    match pod {
        None => Ok(true),
        Some(pod) => Ok(required_uid(pod)? != old_uid),
    }
}

fn ensure_before_deadline(deadline: Instant, action: &str) -> Result<()> {
    if Instant::now() >= deadline {
        bail!("timed out while attempting to {action}");
    }
    Ok(())
}

async fn pause_until_next_poll(deadline: Instant) {
    sleep_until(next_poll_at(Instant::now(), deadline)).await;
}

fn next_poll_at(now: Instant, deadline: Instant) -> Instant {
    std::cmp::min(deadline, now + READY_RETRY)
}

fn kube_status(error: &kube::Error) -> Option<u16> {
    match error {
        kube::Error::Api(response) => Some(response.code),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use factory_coordinator::ExecutionEnvironmentId;
    use factory_coordinator::JobId;
    use k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;

    use super::*;

    #[test]
    fn uid_wait_finishes_only_for_absence_or_a_replacement_uid() {
        let pod = |uid: Option<&str>| Pod {
            metadata: ObjectMeta {
                uid: uid.map(str::to_string),
                ..ObjectMeta::default()
            },
            ..Pod::default()
        };
        assert!(uid_is_gone(None, "old").expect("absent"));
        assert!(uid_is_gone(Some(&pod(Some("new"))), "old").expect("replacement"));
        assert!(!uid_is_gone(Some(&pod(Some("old"))), "old").expect("same Pod"));
        assert!(uid_is_gone(Some(&pod(None)), "old").is_err());
    }

    #[test]
    fn endpoints_and_backend_references_are_generation_specific() {
        assert_eq!(
            pod_ip_url("10.2.3.4".parse().expect("IPv4")),
            "ws://10.2.3.4:4500"
        );
        assert_eq!(
            pod_ip_url("fd00::1".parse().expect("IPv6")),
            "ws://[fd00::1]:4500"
        );
        let reference = KubernetesBackendReference {
            namespace: "factory".to_string(),
            name: "factory-env-g2".to_string(),
            uid: Some("uid-2".to_string()),
        };
        assert_eq!(
            KubernetesBackendReference::parse(&reference.encode()).expect("reference"),
            reference
        );
        assert_eq!(
            KubernetesBackendReference::parse("factory/name")
                .expect("locator-only reference")
                .uid,
            None
        );
        assert!(KubernetesBackendReference::parse("factory").is_err());
        assert!(KubernetesBackendReference::parse("factory/name/").is_err());
        assert!(KubernetesBackendReference::parse("factory/name/uid/extra").is_err());
    }

    #[test]
    fn retry_and_release_keep_the_persisted_namespace_after_config_change() {
        let mut environment = kubernetes_environment();
        let name = OwnedPodIdentity::new(&environment).expect("identity").name;
        environment.backend_ref = Some(format!("original/{name}"));

        let retry = resolve_pod_reference(&environment, "reconfigured").expect("retry target");
        assert_eq!(retry.namespace, "original");
        assert_eq!(retry.name, name);
        assert_eq!(retry.uid, None);

        environment.backend_ref = Some(format!("original/{name}/pod-uid"));
        let bound = resolve_pod_reference(&environment, "reconfigured").expect("bound target");
        assert_eq!(bound.namespace, "original");
        assert_eq!(bound.name, name);
        assert_eq!(bound.uid.as_deref(), Some("pod-uid"));

        environment.backend_ref = Some("original/not-the-durable-name".to_string());
        assert!(resolve_pod_reference(&environment, "reconfigured").is_err());
    }

    #[test]
    fn polling_never_sleeps_past_the_operation_deadline() {
        let now = Instant::now();
        assert_eq!(
            next_poll_at(now, now + Duration::from_millis(10)),
            now + Duration::from_millis(10)
        );
        assert_eq!(
            next_poll_at(now, now + Duration::from_secs(1)),
            now + READY_RETRY
        );
    }

    fn kubernetes_environment() -> ExecutionEnvironmentRecord {
        ExecutionEnvironmentRecord {
            job_id: JobId::new("job-kubernetes-locator"),
            environment_id: ExecutionEnvironmentId::new("d5516e32-0bb1-4121-8e8a-bff807040f92"),
            backend: KUBERNETES_EXECUTION_ENVIRONMENT_BACKEND.to_string(),
            generation: 3,
            desired_state: ExecutionEnvironmentDesiredState::Active,
            status: ExecutionEnvironmentStatus::Provisioning,
            backend_ref: None,
            url: None,
            error: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }
}
