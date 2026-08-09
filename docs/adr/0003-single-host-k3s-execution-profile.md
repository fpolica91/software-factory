# ADR 0003: Optional Single-Host K3s Execution Profile

- Status: Accepted; runc and Kata live acceptance passed
- Date: 2026-08-08

## Context

Factory needs a scheduler-backed execution option without rebuilding node,
Pod, or runtime management and without moving its durable lifecycle into
Kubernetes. The first runnable profile targets one Linux K3s node sharing the
coordinator's local workspace directory.

## Decision

Docker remains the default. Selecting backend `kubernetes` adds a separate
Compose overlay that runs the existing `factory-worker` with host networking,
a read-only K3s kubeconfig, and no Docker socket. The entrypoint copies the
root-readable kubeconfig into the worker's Codex state before dropping to the
configured host UID/GID.

The launcher validates the Kubernetes configuration and node before recording
one immutable backend marker beside its installation configuration. An upgrade
without a marker infers Docker when existing Compose-labelled workspace or
PostgreSQL volumes exist. A mismatch is refused; Factory never deletes or
migrates either backend's data.
K3s therefore requires a fresh checkout with a separate Compose project.
That project identity deterministically derives the default namespace, PV, PVC,
and host workspace directory; explicit overrides must remain unique.
The Kubernetes execution image is required to be an immutable,
cluster-reachable `registry/repository@sha256:<64 lowercase hex>` reference.
The deliberately conservative supported subset accepts a lowercase
DNS/IPv4-style registry with an optional numeric port and lowercase repository
components separated by single `.`, `_`, or `-` characters. Bracketed IPv6 and
tag+digest references are unsupported. The launcher and Rust runtime both
enforce this invariant; an invalid value is rejected before the backend marker,
host workspace, or cluster is changed.

`factoryd` and the worker bind the same host workspace at `/workspaces`. The
launcher discovers the sole K3s node and applies only a Namespace, static local
PV with node affinity, and PVC. The Factory Kubernetes provisioner creates one
plain `restartPolicy: Never` Pod per durable environment and mounts only its
worktree and Git common directory as PVC subpaths. It connects directly to the
Pod IP; no Service, Deployment, Job, operator, CRD, or workflow engine is added.

PostgreSQL, Qdrant, `factoryd`, provider bridges, artifacts, and the Codex model
loop remain in Compose. Kubernetes owns Pod placement and runtime execution;
Factory owns jobs, retries, checkpoints, environment generations, cancellation,
and release. An empty RuntimeClass uses the cluster default. A nonempty value
only selects an operator-installed runtime such as Kata. Launcher preflight
requires that selected RuntimeClass to exist, then reports its exact name and
handler before starting the worker.

## Limits and Acceptance

This host-local PV is deliberately single-node and is not a multi-node storage
design. Factory ships a pinned Kata values profile for operators but does not
auto-install it. Installing Kata runs a privileged host-mounted DaemonSet that
modifies K3s/containerd and can temporarily restart the node runtime. Shell
syntax, both Compose models, and manifest rendering remain configuration gates.

Live runc acceptance passed on ARM64 K3s node `spark-91b3` with real DeepSeek
job `a99d38e3-82f6-4caf-8f3b-14812f5fb03b`. The execution Pod ran remote
commands and patches, used a native subagent, passed detached review,
materialized and applied the exact artifact, disappeared after success, and
left its durable environment `released/released` with the UID-bound backend
reference retained.

Definitive exact-source Kata acceptance passed from source fingerprint
`dec512b9…b8c3ce` with RuntimeClass `kata-qemu-runtime-rs` and immutable image
`docker.io/library/software-factory@sha256:2bd920060b337573e8cbd751cc64c514174d2acdbad7a32f9f3c3caa6201611d`.
DeepSeek model `deepseek-v4-pro` completed all stages in job
`7003ae36-6f72-4d1a-830b-20f78c3cbeac`. Plan attempt 1 hit a fixture-only
`ImagePullBackOff` because the offline `k3s ctr images import` lacked the exact
digest alias; adding that alias let durable attempt 2 recover. Execute, Review,
and Remediate each passed on attempt 1. The alias repair was local offline-import
setup, not a Factory retry bypass.

Pod `factory-9a32720327d94a39a51c3121aeb9f269-g1` (UID
`519c1713-84d8-4b23-b05f-8aaa28895c3b`) used the selected RuntimeClass. Guest
kernel 6.18.35 differed from host kernel 6.17.0-1014-nvidia. Environment
`9a327203-27d9-4a39-a51c-3121aeb9f269`, generation 1, ended
`released/released` and the Pod was removed. Native-subagent verification
passed; attach, result, and apply succeeded; host `result.md` was verified; and
the sole applied file was `KATA_FINAL_ACCEPTANCE.txt`, exactly 14 bytes containing
`KATA-FINAL-OK\n`.
