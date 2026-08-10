# ADR 0003: Optional Kubernetes Execution Profile

- Status: Accepted; single-node runc/Kata and multi-node runc live acceptance passed
- Date: 2026-08-08

## Context

Factory needs a scheduler-backed execution option without rebuilding node,
Pod, or runtime management and without moving its durable lifecycle into
Kubernetes. The default local-storage profile targets one Linux K3s node
sharing the coordinator's local workspace directory. Multi-node execution uses
operator-managed shared storage; high availability of the Compose control
plane is outside this decision.

## Decision

Docker remains the default. Selecting backend `kubernetes` adds a separate
Compose overlay that runs the existing `factory-worker` with host networking,
a read-only K3s kubeconfig, and no Docker socket. The entrypoint copies the
root-readable kubeconfig into the worker's Codex state before dropping to the
configured host UID/GID.

The launcher validates the Kubernetes configuration and, in `local` workspace
mode, the sole K3s node before recording one immutable backend marker beside its
installation configuration. An upgrade without a marker infers Docker when
existing Compose-labelled workspace or PostgreSQL volumes exist. A mismatch is
refused; Factory never deletes or migrates either backend's data.
The Kubernetes profile therefore requires a fresh checkout with a separate
Compose project in either workspace mode. In `local` mode, that project identity
deterministically derives the default namespace, PV, PVC, and host workspace
directory; explicit overrides must remain unique. In `existing-pvc` mode, the
operator must explicitly provide the namespace, PVC, and host mount; Factory
validates them and does not create or manage a PV.
The Kubernetes execution image is required to be an immutable,
cluster-reachable `registry/repository@sha256:<64 lowercase hex>` reference.
The deliberately conservative supported subset accepts a lowercase
DNS/IPv4-style registry with an optional numeric port and lowercase repository
components separated by single `.`, `_`, or `-` characters. Bracketed IPv6 and
tag+digest references are unsupported. The launcher and Rust runtime both
enforce this invariant; an invalid value is rejected before the backend marker,
host workspace, or cluster is changed.

`factoryd` and the worker bind the same host workspace at `/workspaces`.
In the default `local` mode, the launcher discovers the sole K3s node and
applies only a Namespace, static local PV with node affinity, and PVC. In
`existing-pvc` mode, it instead validates an operator-owned Bound Filesystem
`ReadWriteMany` claim and writable host mount without creating or changing
storage; Kubernetes may then schedule execution Pods across multiple Ready
nodes. The operator must provide the same backing filesystem at both endpoints
and must grant the configured `FACTORY_RUN_AS_UID` and
`FACTORY_RUN_AS_GID` access in advance. Factory preserves that ownership and
omits Pod `fsGroup` while retaining `runAsUser` and `runAsGroup`.
The Factory Kubernetes provisioner creates one plain `restartPolicy: Never`
Pod per durable environment and mounts only its worktree and Git common
directory as PVC subpaths. It connects directly to the Pod IP; no Service,
Deployment, Job, operator, CRD, or workflow engine is added.

PostgreSQL, Qdrant, `factoryd`, provider bridges, artifacts, and the Codex model
loop remain in Compose. Kubernetes owns Pod placement and runtime execution;
Factory owns jobs, retries, checkpoints, environment generations, cancellation,
and release. An empty RuntimeClass uses the cluster default. A nonempty value
only selects an operator-installed runtime such as Kata. Launcher preflight
requires that selected RuntimeClass to exist, then reports its exact name and
handler before starting the worker.

## Limits and Acceptance

The `local` mode's host-local PV is deliberately single-node. The
`existing-pvc` mode is the multi-node execution seam, but its shared storage
and permissions are an operator contract and it does not provide a highly
available Factory control plane. Factory ships a pinned Kata values profile for
operators but does not auto-install it. Installing Kata runs a privileged
host-mounted DaemonSet that
modifies K3s/containerd and can temporarily restart the node runtime. Shell
syntax, both Compose models, and manifest rendering remain configuration gates.

Acceptance covers single-node runc and Kata plus multi-node runc on a mixed
ARM64/AMD64 cluster backed by an explicit `ReadWriteMany` PV/PVC. A
multi-architecture execution image lets runc Pods use either architecture;
Kata remains available only on nodes where its handler is installed, so
selecting its RuntimeClass narrows the eligible nodes. Real-model runc jobs
scheduled and completed on each architecture, verifying shared-workspace
access and the plan, execute, detached-review, and release lifecycle. A
separate x86_64 (AMD64) run verified worker-loss lease recovery and a
substantive request-changes, remediation, and independent re-review cycle.
