# ADR 0002: Codex Remote Execution Environments

- Status: Accepted
- Date: 2026-08-07

## Context

Factory needs to run repository tools in disposable isolated environments
without moving the model loop, durable lifecycle, or provider transport into a
second harness. Every production job now receives a durable execution-
environment identity and a provisioned remote-only Codex environment selected
explicitly on new threads and turns.

## Decision

Codex remains the only agent harness. The model loop, threads, tools, planning,
compaction, skills, and subagents stay in Codex. Factory remains the durable
authority for jobs, operations, attempts, leases, checkpoints, retries,
recovery, worktrees, review, and remediation.

Tool and filesystem execution crosses Codex's existing remote-execution seam.
The Factory provisioner ensures one backend per durable job generation, then
constructs a fresh `EnvironmentManager::remote_only` for its exact URL.
Factory selects environments through Codex's exact
`ThreadStartParams.environments` and
`TurnStartParams.environments` types. The job worktree must be available at the
same absolute path inside that environment. Factory will not add a second
protocol or tool dispatcher.

Model/provider selection is a separate plane: it chooses model transport and
does not determine where tools execute. Brokkr may supply and operate compute
capacity, but it does not own Codex threads or Factory lifecycle state.
Disposable containers, Kubernetes `RuntimeClass`, and Kata microVMs are
replaceable execution backends, not Factory domain concepts.

## Current Source Anchors

- `OperationExecutor` and `DurableRunner` in
  `factory-harness/factory/coordinator/src/runner.rs` retain claim, lease,
  retry, cancellation, execution, and settlement ownership.
- `build_worker_start_args` in
  `factory-harness/factory/runtime/src/bootstrap.rs` supplies an inert
  remote-only manager; the operation executor must replace it with the ensured
  per-job environment before Codex starts.
- `ExecutionEnvironmentProvisioner` in
  `factory-harness/factory/runtime/src/execution_environment.rs` is the
  replaceable ensure/release seam. Docker is the accepted default. Kubernetes
  is optional; both its K3s/runc and operator-installed Kata lifecycles are
  accepted.
- `EnvironmentManager::remote_only` in
  `factory-harness/codex-rs/exec-server/src/environment.rs` is the explicit
  no-local-fallback construction seam.
- `ThreadStartParams::environments` and `TurnStartParams::environments` in
  `factory-harness/codex-rs/app-server-protocol/src/protocol/v2/` are the exact
  upstream environment-selection fields.
- `AutonomousSession::start`, `AutonomousSession::text_turn_params`, and
  `default_turn_environments` in
  `factory-harness/factory/runtime/src/session.rs` build one exact selection
  and carry it into new threads and text turns.

## Current Implementation and Acceptance

The image carries the preserved upstream `codex` binary. The Docker provisioner
derives the worker image, network, and `/workspaces` backing mount through
Bollard, then gives each sibling container only two writable roots at their
original absolute paths: the exact job worktree and that repository's Git
common directory. Named volumes use Docker subpaths; bind deployments use the
corresponding narrowed host paths. Each generation runs
`codex exec-server --listen ws://0.0.0.0:4500`. Compose has no static
exec-server service or global URL. Factory uses Codex's exact
`TurnEnvironmentParams`; no Factory protocol was added.

Retries, later stages, lease recovery, and graceful worker shutdown retain the
same environment generation. Normal Plan rollback checks out, resets, and
cleans the worktree in place so the scoped mount keeps the same directory inode.
A missing or corrupt linked worktree triggers backend removal before explicit
recreation and reprovisioning of the same durable generation. Missing active
containers are recreated and stopped ones restarted. Terminal success/failure
and queued cancellation record a durable release intent. Running cancellation
drains Codex and restores the worktree, requests and completes environment
release, then acknowledges terminal `cancelled`. Continuation waits until
release is complete before incrementing the generation; continuation and
workspace removal serialize on the same job lock. Startup and normal worker
polling retry persisted release rows.

Docker teardown looks up the persisted container ID when present and otherwise
the deterministic generation name. It validates exact Factory job,
environment, generation, and backend-reference identity before stop and remove;
absence is idempotent success and mismatch is never removed.

Historical Phase 4 evidence: real DeepSeek job
`5e865e18-c5f6-43c3-895d-4e4e325ff3d0` passed the Compose
gate on image
`sha256:086337e6ed373c402198ab4f3f109cb98866032c6327d23f1499aa823ac10f20`.
Plan created only Execute-owned implementation and verification units. Remote
tools ran the probe and verifier, a native subagent returned its token, a
repository skill supplied its token, and `apply_patch` created the expected
file. Two malformed provider tool calls became durable retryable attempts; the
third Execute attempt succeeded. Detached Review then requested the deliberate
`controlled-status` change, Remediate patched it, and an independent re-review
approved `VERIFY_FINAL_OK`. Terminal attach replayed the complete result and the
fixed `.factory/jobs/<job-id>/` artifacts were visible in the host checkout.

Recovery job `eaa6db83-1a5e-4fe8-b41a-b128c1ad5af6` lost its execution server
mid-command. Factory exposed the disconnect, allowed Codex's 30-second native
recovery window, failed the active turn retryably, and resumed the same durable
job after the server reconnected. Cancellation job
`d2f8c849-3d80-4709-a3e7-6b58cd047304` was stopped while its remote probe was
blocked; it reached terminal `cancelled`, left no remote probe process, and
restored a clean worktree.

Phase 4's live Docker gate proved stopped-container restart, missing-container
recreation, terminal removal, repeated absent release, and mismatch retention.
Coordinator/runtime PostgreSQL gates proved terminal release intents,
cleanup-release-ack ordering, graceful-shutdown retention, continuation fencing,
and restart reconciliation that completes other releases while retaining and
later retrying one deliberate failure.

Historical Phase 5 evidence: a per-job real-model gate used image
`sha256:88ebd2543a7272b725a1b9e9682a7c25530296f0a105d6554c427a086a560781`
and isolated Compose project `sfperjobp5`. Rendered Compose had no static
`codex-exec-server`. Concurrent jobs
`c71292b1-cba6-447d-b711-0e03e2b709b0` and
`a2b87743-406c-42c3-b18b-a74975291dfe` received distinct durable environment
IDs and generation-1 containers. The first completed Plan, Execute, detached
Review, Remediate, and independent re-review with `VERIFY_FINAL_OK`; its
commands, skill, native subagent, patch, and verifier activity stayed on its
per-job manager, terminal release removed its container, and terminal status
reconstructed the complete host `.factory/jobs/<job-id>/` artifact set.

Recovery job `fb3e65de-1957-48d7-a17e-4d28db8902ad` ran a blocked probe in
container `a4941b88657c...`. Removing that container produced the visible
disconnect and exact 30-second timeout, a retryable Execute failure, and a
`retryScheduled` second attempt. Factory recreated the same durable environment
`351e3b9d-74d4-44a9-97aa-6198033a6c21` at generation 1 as container
`133f9d692eae...`; the probe hostname proved execution moved to the recreated
container. The job then succeeded and terminal release removed the container.
An earlier candidate exhausted its three Execute attempts because DeepSeek
repeated malformed `apply_patch` syntax; it is provider-failure evidence only,
not part of the disconnect-causality gate.

Current Phase 6 acceptance used final image
`sha256:9a3418d3e470071abdebed5224f9b25153509efeedd1cafc58826141b8f6a656`.
Claude Sonnet job `7773ded7-2b75-4268-b3f3-8680fda57b53` completed Plan,
tool-using Execute, detached Review, Remediate, and independent re-review. It
ran repository commands and a repository skill, spawned a native Codex
subagent, created the fixture through native `apply_patch`, and finished with
`VERIFY_FINAL_OK`. Environment
`d5516e32-0bb1-4121-8e8a-bff807040f92` reached durable
`released/released`, its container was removed, and all eight expected host
artifacts were present.

Concurrent final-image jobs `6afe719a-7dfd-4b47-a917-e31ad5a80068` and
`0cab3836-9a57-4298-8d22-2023a518f017` received distinct generation-1
environments. Docker inspection showed exactly two subpath mounts per
container, and each container could enumerate only its own job worktree.
During the first job's Execute turn, container `7b3d0d060d90...` was forcibly
removed. Factory emitted the visible disconnect, allowed Codex's exact
30-second recovery window, recorded a retryable attempt, and recreated the
same environment
`71d970be-4345-4865-bbc3-36c0292f4b80` at generation 1 as container
`f9323b5ba54d...`. The retry and all later stages passed the contract. The
second job reached terminal success and release, but its model incorrectly
approved the deliberate `STATUS=needs-remediation` defect; it is retained only
as concurrency, mount-isolation, and release evidence, not functional-contract
acceptance. Both environments ended `released/released`, both containers were
absent, and both host artifact sets were complete.

Each current Docker container receives only the exact worktree and repository
Git common directory plus one worker network. Codex selects that worktree as
its cwd and workspace root. The worker's broader `/workspaces` mount is backing
storage only and is not exposed to execution containers.

Codex's model loop, context compaction, skills, and memory never crossed this
new execution seam. Their existing native real-model gates remain applicable;
Factory adds no second compactor, memory path, orchestration protocol, or tool
dispatcher. Docker remains the default. The optional Kubernetes backend uses
the same lifecycle contract and requires an immutable
`registry/repository@sha256:<64 lowercase hex>` reference. Its conservative
supported subset accepts a lowercase DNS/IPv4-style registry with an optional
numeric port and lowercase repository components separated by single `.`, `_`,
or `-` characters; bracketed IPv6 and tag+digest references are unsupported.
On Pod-producing startup, the launcher enforces the invariant before changing
the backend marker, workspace, or cluster. The Rust runtime independently
validates the reference during configuration normalization and again
immediately before Pod construction. Kata may be selected through RuntimeClass
after the operator installs it; its live lifecycle gate has passed.
