# Rust Cutover Ledger

This file is the authoritative cleanup boundary. Update it in the same change
that moves or deletes a path; do not rediscover completed work after a context
reset.

## Protected Upstream: Do Not Rewrite

- `factory-harness/codex-rs/`
- `factory-harness/codex-cli/`
- `factory-harness/sdk/`
- Other upstream Codex support paths directly under `factory-harness/`

These paths may contain TypeScript or JavaScript because they belong to the
vendored Codex source, not Factory. Do not count or port them during the Rust
cutover.

The deliberate upstream Codex seams in the working tree carry detached review
history isolation and lineage into native extension state, plus generic
per-step host-tool removal used by Factory Plan and explicit remote-only
environment construction. Their hand-edited files are:

- `factory-harness/codex-rs/app-server-protocol/src/protocol/v2/review.rs`
- `factory-harness/codex-rs/app-server/src/request_processors/turn_processor.rs`
- `factory-harness/codex-rs/ext/agent/Cargo.toml`
- `factory-harness/codex-rs/ext/agent/src/lib.rs`
- `factory-harness/codex-rs/ext/agent/tests/agent_service.rs`
- `factory-harness/codex-rs/ext/extension-api/src/contributors.rs`
- `factory-harness/codex-rs/ext/extension-api/src/contributors/thread_lifecycle.rs`
- `factory-harness/codex-rs/ext/extension-api/src/lib.rs`
- `factory-harness/codex-rs/core/src/session/turn.rs`
- `factory-harness/codex-rs/core/src/tools/router.rs`
- `factory-harness/codex-rs/core/src/tools/spec_plan.rs`
- `factory-harness/codex-rs/core/src/tools/spec_plan_tests.rs`
- `factory-harness/codex-rs/exec-server/src/environment.rs`
- Mechanical `detached_context: None` call-site updates in
  `codex-rs/app-server/tests/suite/v2/client_metadata.rs`,
  `codex-rs/app-server/tests/suite/v2/review.rs`, `codex-rs/exec/src/lib.rs`,
  and `codex-rs/tui/src/app_server_session.rs`
- `factory-harness/codex-rs/Cargo.lock`

`just write-app-server-schema` generated only the corresponding protocol
fixtures:

- `factory-harness/codex-rs/app-server-protocol/schema/json/ClientRequest.json`
- `factory-harness/codex-rs/app-server-protocol/schema/json/codex_app_server_protocol.schemas.json`
- `factory-harness/codex-rs/app-server-protocol/schema/json/codex_app_server_protocol.v2.schemas.json`
- `factory-harness/codex-rs/app-server-protocol/schema/json/v2/ReviewStartParams.json`
- `factory-harness/codex-rs/app-server-protocol/schema/typescript/v2/DetachedReviewContext.ts`
- `factory-harness/codex-rs/app-server-protocol/schema/typescript/v2/ReviewStartParams.ts`
- `factory-harness/codex-rs/app-server-protocol/schema/typescript/v2/index.ts`

Do not edit generated files by hand. Any other upstream working-tree change is
unplanned and must be investigated before continuing.

## Cleanup Complete

- Deleted `factory-harness/factory/protocol/` and its generated schemas.
- Deleted the Factory V1 codec/manifests and protocol sync scripts from
  `harness-client/`.
- Removed the Factory-to-Codex translation layer and version handshake.
- Removed obsolete HTTP recovery demo commands and scripts.
- Deleted the unused coordinator pending-approval request API, storage,
  pairing code, and test fixtures. The autonomous native Codex session answers
  its server requests directly; no runtime caller used this second workflow.
- Standardized runtime checkpoints on one `factory.stage` document.
- Deleted the superseded `_legacy/` JavaScript acceptance fixtures after native
  Rust real-model acceptance replaced their workflow, provider, memory, and
  current recovery-mechanics coverage.
- Removed Docker and package-script references to those quarantined acceptance
  files. A fresh clone no longer relies on an untracked empty `scripts/`
  directory to make the legacy image build context valid.
- Switched the runnable container distribution from the TypeScript/Hatchet
  workflow to the native Rust `factory`, `factory-worker`, and `factoryd`
  binaries. The image no longer builds or copies `harness-client/`,
  `workflows/`, or `integrations/` artifacts. The same image now also carries
  the preserved upstream `codex` binary solely to run Codex's native
  `exec-server`; it is not another Factory application or harness.
- Removed Hatchet from the default Compose stack and removed its token
  bootstrap from the host launcher. PostgreSQL, Qdrant, `factoryd`, Codex's
  `exec-server`, and the Rust durable worker are now the baseline services.
- Deleted the Hatchet-era external workflow idempotency and task-run
  correlation fields, ID wrappers, conflict branch, columns, and index. Native
  job, operation, attempt, request, thread, turn, and item IDs are the complete
  coordinator identity surface.
- Deleted `factory-harness/factory/providers/bridge/` package sources. Provider
  translation is now native Rust; the empty ignored directory, if present in a
  local checkout, has no runtime meaning.
- Deleted the complete root `harness-client/`, `workflows/`, and `integrations/`
  trees plus `docker-compose.build.yml` after verifying that they contained no
  untracked or ignored user files and had no active Rust or distribution caller.
  This removed 687 tracked legacy paths, including generated client sources,
  Node lockfiles, Hatchet workflow code, and the obsolete build overlay.
- Deleted the unshipped `factory-runtime` stdio executable and its public
  process-memory startup fallback. The `factory-runtime` crate remains the
  library used by `factory-worker`, which always supplies a fenced factoryd
  state backend.
- Removed obsolete legacy provider identifiers. Migration 13 rewrites only
  retained `factory.task` inputs pinned to exact provider `claude` as
  `anthropic`; durable claim, guard, and runtime boundaries use canonical IDs
  only. `claude` remains a friendly alias solely at configuration input.
- Removed the public recovery claim, operation claim, correlation, checkpoint,
  and attempt renew/complete/fail HTTP mutations. The in-process Rust
  `DurableRunner` is their only production caller and now uses the coordinator
  store directly. HTTP retains job/workspace lifecycle, result export, durable
  event append/replay, and fenced Factory thread state.

## Active Rust System

- `factory-harness/factory/coordinator/`: PostgreSQL jobs, attempts, leases,
  checkpoints, recovery, correlations, managed Git worktrees, and the fenced
  append-only job event stream used by CLI attach/log replay. Its store is one
  unversioned direct-implementation module tree: `src/store.rs` owns the pool
  lifecycle and shared numeric/ID helpers; `src/store/jobs.rs` owns jobs,
  aggregate reads, and workspaces; `src/store/events.rs` owns durable event
  append/replay; `src/store/attempts.rs` owns correlations, checkpoints,
  thread state, leases, and settlement; and `src/store/recovery.rs` owns
  recovery selection and claims. No repository abstraction or duplicate store
  surface was added. Migration 14 and `src/store/environments.rs` add exactly
  one stable execution-environment identity per job. Its backend and optional
  backend reference, URL, and error are lifecycle data; retries and lease
  transfers retain the identity and generation. Provisioning, ready, and
  failure writes require the live `AttemptFence`; release completion compares
  the persisted generation. Terminal success, terminal failure, and queued
  cancellation request release in the same transaction as terminal job state.
  A running cancellation keeps the job and fence in `cancelling` while its
  owner interrupts and drains Codex, restores disposable worktree state,
  requests release through that fence, removes the backend, and marks it
  released. Only then may cancellation become terminal. Continuation is
  rejected until terminal teardown is durably `released`; it then reactivates
  the same identity with one generation increment. Continuation and explicit
  workspace removal take the same per-job advisory lock, so they cannot commit
  a queued continuation against a removed worktree.
  `coordinator/README.md` documents `factory-worker` and
  `DurableRunner` as the current claim/ordering owner rather than Hatchet.
  Dedicated PostgreSQL advisory-lock connections fence each job worktree
  across worker and `factoryd` processes and fence shared cache publication.
  They use a pool separate from query/control traffic. Worker lock capacity is
  derived from the accepted 1-32 slots with two lifecycle connections of
  headroom, while eight independent query connections keep lease heartbeats,
  correlations, events, and checkpoints moving when every lock slot is held.
  Running cancellation is a request/acknowledgement lifecycle: the job remains
  `cancelling` with its live fence intact while a one-second control poll asks
  the executor to stop, drain, and restore disposable state. Only the cleanup
  owner can release the execution environment and acknowledge terminal
  `cancelled`. Queued jobs without a live attempt still cancel immediately. A slow or transiently failed job-state
  poll is advisory and does not impersonate lease loss; execution continues
  while the independent fenced heartbeat proves ownership. Only an observed
  terminal/cancelling job or confirmed heartbeat fence failure stops it.
  Graceful worker shutdown explicitly
  expires the retained attempt lease after the same drain and cleanup path, so
  restart recovery does not wait for the configured production lease.
  Lease loss cancels and fully drains the old executor; it is never task-aborted
  after an arbitrary grace period, so a replacement lease cannot overlap the
  old Codex runtime in the same worktree.
  Shared Git caches are bare repositories with remote branches isolated under
  `refs/remotes/origin/*`; remote pruning can never delete local
  `refs/heads/factory/*` worktree branches. Legacy `clone --mirror` refspecs are
  never refreshed in place as mirrors: linked repositories are converted to
  the safe remote-tracking layout before fetch, while invalid or inactive
  targets are quarantined intact and rebuilt from a unique partial path. Caches
  and job bindings use a hashed repository identity separate from the container
  clone path, so detached jobs from different host repositories remain distinct
  even though both are mounted at `/workspace/project`. Each binding retains an
  immutable base revision. Missing or removed worktrees rematerialize from that
  exact recorded commit even after the requested branch moves. A succeeded job
  exports its complete worktree as a
  digest-labelled standard binary Git patch through
  `GET /jobs/{jobId}/result`; legacy workspace rows receive non-reusable
  `legacy:` identities rather than an ambiguous path-derived backfill.
  Operational job artifact plumbing uses the single `FACTORY_ARTIFACT_ROOT`
  seam. Durable jobs, operations, attempts, checkpoints, and events are the
  only source of truth; artifact files are disposable renderings. Each job may
  have a coordinator-side rendering plus `.factory/jobs/<job-id>/` in the
  matching local checkout. Remote repositories and jobs from another mounted
  checkout have no local projection. Artifact paths are separate from managed
  worktrees and therefore never enter result patches. Job creation initializes
  Factory-owned `job.json` and `task.md`; runtime repeats that step idempotently
  for externally created jobs. Terminal observers regenerate either missing
  inventory file under the publication lock and project existing bytes without
  rewriting them, preserving any recorded base metadata. A successful stage
  settlement atomically records the exact accepted turn and its findings in
  `stage.completed`. The runtime
  reconstructs all settled stage output from events under a per-job file lock
  and atomically refreshes stage Markdown, exact accepted `findings.json`, and
  cumulative `result.md`, with `result.md` written last. Legacy completion
  events without a `findings` field mean that findings are unknown, not empty;
  reconstruction removes any stale `findings.json` projection and reports that
  file unavailable. A present empty array remains an exact, known empty result.
  Terminal observers perform the same reconstruction before reporting a
  succeeded job, so attach, status, result, and artifact reads repair missing
  or stale renderings. A late
  publisher cannot regress output because it reloads current settled events
  after settlement and publication is serialized. Artifact failures never
  change a validated stage settlement: they emit a normal durable warning plus
  worker log. Remediation retains each accepted fix and re-review in cycle order
  while failed, replaced, and unaccepted turns are excluded. Agents never author
  these fixed files.
- `factory-harness/factory/extension/`: native Factory state, tools, context,
  review provenance, subagent activity, and optional Qdrant memory. Sparse
  lexical recall requires a score of at least `2.0`, preventing a single generic
  term from injecting unrelated memory while preserving exact nonce recall.
  Full subagent lifecycle snapshots are archived first in fenced append-only
  coordinator job events with deterministic job-scoped identities. Legacy
  unbounded documents are backfilled once, retries resolve to the same event,
  and only after every archive is acknowledged does thread state keep a
  24-entry latest-per-child/recent projection plus the durable event cursor;
  a backend without event archival retains full detail.
  `factory_record_review` rejects approve with nonempty findings at the tool
  boundary, leaving durable state unchanged; passing evidence belongs in the
  review summary. Non-approved verdicts continue to require actionable
  findings.
  Detached review persists a minimal rollback baseline for review,
  remediation, history, and revision fields. The marker survives compaction
  and process death but is excluded from model context.
  The production extension installer requires a host-provided backend; the
  removed standalone process-memory backend is not a product mode.
  `src/stage.rs` defines the runtime-provided stage used both to filter the
  model-visible tool catalog before sampling and to authorize every mutating
  call: Plan exposes only decomposition, Execute only progress, detached review
  only review, and Remediate only dispositions. Memory exposes recall in every
  stage and durable writes only during Execute and Remediate. Ordinary native
  subagents receive no Factory mutation tools or parent-state prompt; they may
  read repository-scoped memory and return work to the parent for reconciliation.
- `factory-harness/factory/runtime/`: native Codex bootstrap, autonomous
  session, stage contracts, checkpoints, fenced live event output, and the
  production durable worker binary (`src/bin/factory_worker.rs`). Production
  requires a per-job execution-environment provisioner; bootstrap carries only
  an inert remote-only manager, which is replaced by the ensured job URL before
  every Codex session. There is no local or static execution fallback. The deliberate public
  `EnvironmentManager::remote_only` seam creates a fresh manager for one exact
  caller-named remote through Codex's existing snapshot path, with the supplied
  HTTP policy and optional connection timeout, no local fallback, and no state
  shared between constructor calls. It is the protected-upstream primitive for
  later Factory-owned per-job environment selection. Factory passes Codex's exact
  `TurnEnvironmentParams` with the managed worktree path to new threads and
  every text turn. Each job's `codex exec-server` container owns only tool and
  filesystem execution over two exact writable mounts: that job's worktree and
  its repository's Git common directory. The worker retains the model loop and
  Factory retains durable lifecycle authority. Detached review uses the same
  remote-only manager default, and native thread-spawn subagents inherit their
  parent's selection. The session watches the selected remote environment's
  authoritative connection state. A disconnect gets Codex's full 30-second
  recovery window exactly once; if it remains disconnected, the active turn is
  interrupted and returned to Factory's durable retry loop. Reconnect and
  disconnect events remain visible in compact CLI output. Real-model Compose
  acceptance covers remote commands and patches, native subagents, skills,
  durable retry, disconnect/reconnect recovery, cancellation, detached review,
  remediation, and independent re-review.
  `src/execution_environment.rs` is the Factory-owned provisioning seam. When
  the worker enables its Docker backend, every claimed operation uses its live
  `AttemptFence` to ensure the job's stable coordinator environment record,
  idempotently create or reuse one environment-and-generation-named sibling
  container, and publish the exact container ID and WebSocket URL through the
  fenced ready transition. The provisioner uses Bollard's Docker Engine API,
  never a Docker subprocess. It discovers the worker's exact image, attached
  network, and `/workspaces` volume or bind mount by inspecting the running
  worker, so no Compose project name is encoded in Rust. The container runs
  only the preserved Codex `exec-server`; it receives no Docker socket and
  shares only the managed worktree and required Git common directory. For a
  named volume, Docker subpaths narrow the worker's backing mount; bind-backed
  deployments use the two corresponding narrowed host paths. Reuse validates the exact
  invariant-bearing image environment and entrypoint, command, working
  directory, user, single network, and exactly two writable scoped mounts; a
  stale or raced container is rejected rather than silently adopted. Each
  operation clones the reusable
  Codex startup arguments and replaces their manager with a fresh
  `EnvironmentManager::remote_only` whose default ID is the exact durable
  environment ID. Detached review and native subagents therefore retain the
  same per-job selection, while retries, later stages, lease recovery, and
  graceful worker shutdown reuse the same container generation. Missing active
  containers are recreated and stopped active containers restarted. Terminal
  success/failure and cancellation drive idempotent release. Release inspects by
  persisted container ID when available, otherwise by deterministic name, and
  validates exact job, environment, generation, and backend-reference identity
  before Bollard stop/remove calls. Absence is already released; mismatches are
  retained and reported. Startup and normal polling retry persisted `releasing`
  rows without preventing other releases or job claims.
  Phase 6 acceptance is complete on final image
  `sha256:9a3418d3e470071abdebed5224f9b25153509efeedd1cafc58826141b8f6a656`.
  Claude Sonnet job `7773ded7-2b75-4268-b3f3-8680fda57b53` passed the full
  Plan, remote command and patch Execute, detached Review, Remediate, and
  independent re-review lifecycle with a repository skill, native Codex
  subagent, `VERIFY_FINAL_OK`, terminal environment release, container removal,
  and all eight host artifacts. Concurrent jobs
  `6afe719a-7dfd-4b47-a917-e31ad5a80068` and
  `0cab3836-9a57-4298-8d22-2023a518f017` each saw exactly its own worktree and
  Git-common-directory subpath mounts. Removing the first job's active
  container produced the visible 30-second Codex disconnect, retry, and
  recreation of the same environment and generation before its successful
  full-contract completion. The second job reached terminal success and
  release but its model incorrectly approved the fixture's deliberate
  `STATUS=needs-remediation` defect, so it proves only concurrency, scoped
  isolation, and release. Both environments finished `released/released` with
  no remaining container and complete host artifacts.
  The optional Kubernetes distribution profile is wired through
  `docker-compose.kubernetes.yml` and the existing `factory-worker`; its default
  `local` workspace mode additionally uses
  `deploy/k3s/single-node-workspaces.yaml`. Docker remains the exact default.
  Kubernetes mode keeps shared state services in Compose, gives the worker
  host-local service URLs and no Docker socket, and bind-mounts the same host
  workspace into `factoryd` and the worker. In `local` mode only, Factory exposes
  that directory to execution Pods through one static local PV/PVC pinned to the
  discovered node, and the launcher validates and applies that template before
  starting the worker. A configured RuntimeClass must exist
  during launcher preflight; its exact name and handler are reported before the
  worker starts, then the class is only passed through. The overlay overrides the existing
  `factory-worker`; it does not define a second worker service or retain a
  backend-selection reconciliation layer. Pod reuse checks Factory-owned fields
  directly on native Kubernetes types. No Factory Pod schema version, shadow
  struct, desired-spec hash, or quantity parser remains.
  `FACTORY_KUBERNETES_WORKSPACE_MODE=local` preserves that exact default.
  The alternative `existing-pvc` launcher mode is the smallest truthful
  multi-node workspace seam: it requires an explicit writable host mount,
  namespace, and existing PVC; accepts one or more Ready schedulable nodes; and
  read-only preflights a Bound Filesystem `ReadWriteMany` claim. It skips host
  directory creation/chown, local storage size handling, and namespace/PV/PVC
  manifest application. Kubernetes remains the scheduler. The operator owns the
  contract that the host mount shared by `factoryd` and the worker has the same
  backing filesystem as the PVC mounted by execution Pods; the launcher reports
  both endpoints but cannot mechanically prove that storage identity. The
  launcher derives `FACTORY_WORKSPACE_OWNERSHIP_MODE=preserve` for both host-side
  services, so their shared entrypoint does not recursively chown the
  operator-managed workspace mount. The worker validates that same mode and
  renders execution Pods without `fsGroup` in `preserve` mode, while retaining
  the configured `runAsUser` and `runAsGroup`. The operator-managed filesystem
  must already permit those UID/GID values to access the required subpaths.
  Kubernetes resource configuration remains native Pod data. A worker may
  optionally pin all of its execution Pods to one preflighted Ready schedulable
  node with `FACTORY_KUBERNETES_NODE_NAME`; an empty value leaves scheduling to
  Kubernetes. `FACTORY_KUBERNETES_GPU_COUNT=0` preserves the CPU-only manifest.
  A positive count preflights the configurable fully qualified extended
  resource (default `nvidia.com/gpu`) against eligible node allocatable data and
  renders equal whole-number request and limit entries. No per-job scheduling
  protocol or Factory Pod-spec mirror was added.
  Docker and `local` mode retain the default `manage` behavior and unchanged
  Pod `fsGroup`.
  Final K3s/runc acceptance passed on ARM64 node `spark-91b3` with acceptance
  image `sha256:df6e4338afc7428dc11786085f8ca5ad8cf6f27628b4509de9e216b444592d5e`
  and immutable execution manifest
  `sha256:6b72e173796f9e4c719fb0a7d336ff5bc715b4f9d46a791b9748e7a2eca875a4`.
  Removing one unused serialization derive did not change behavior; the exact
  final source rebuilt successfully as distribution image
  `sha256:9a1fcdcf450fcedc236e57d8d0f91607805b19fe4649ba99aa9a96e96ee66357`.
  Real DeepSeek job `a99d38e3-82f6-4caf-8f3b-14812f5fb03b` completed native
  planning, Pod-hostname and cwd commands, `apply_patch`, exact-byte
  verification, one native subagent, detached review, and the remediation gate.
  The applied host artifact was exactly `K3S-CLEANUP-OK\n`. Environment
  `62f8dbc5-9852-41ab-8304-eb26d211560b`, generation 1, retained Pod UID
  `6940ad52-3095-4e58-88d0-feeef178f58d`, finished `released/released`, and
  left no execution Pod. The final Compose project contained only one
  `factory-worker`; the obsolete service was removed as an orphan.
  Initial live startup exposed a deterministic Rustls-provider requirement
  after `kube` enabled both crypto backends. `factory-worker` now calls Codex's
  existing `ensure_rustls_crypto_provider` helper before any async client or
  arg0 dispatch; the targeted worker check and first-start container gate pass.
  The pinned `deploy/k3s/kata-qemu-runtime-rs.values.yaml` operator profile
  enables only ARM64 `kata-qemu-runtime-rs` and disables optional snapshotters
  and every other shim. Definitive exact-source Kata acceptance passed from
  source fingerprint `dec512b9…b8c3ce` with immutable image
  `docker.io/library/software-factory@sha256:2bd920060b337573e8cbd751cc64c514174d2acdbad7a32f9f3c3caa6201611d`.
  DeepSeek model `deepseek-v4-pro` completed all stages in job
  `7003ae36-6f72-4d1a-830b-20f78c3cbeac`. Plan attempt 1 hit a fixture-only
  `ImagePullBackOff` because the offline `k3s ctr images import` lacked the
  exact digest alias; adding that alias let durable attempt 2 recover. Execute,
  Review, and Remediate each passed on attempt 1. The alias repair was local
  offline-import setup, not a Factory retry bypass. Pod
  `factory-9a32720327d94a39a51c3121aeb9f269-g1` (UID
  `519c1713-84d8-4b23-b05f-8aaa28895c3b`) used RuntimeClass
  `kata-qemu-runtime-rs`; guest kernel 6.18.35 differed from host kernel
  6.17.0-1014-nvidia. Environment
  `9a327203-27d9-4a39-a51c-3121aeb9f269`, generation 1, ended
  `released/released` and the Pod was removed. Native-subagent verification
  passed; attach, result, and apply succeeded; host `result.md` was verified;
  and the sole applied file was `KATA_FINAL_ACCEPTANCE.txt`, exactly 14 bytes
  containing `KATA-FINAL-OK\n`.
  Before first persisting the immutable per-installation backend marker, the
  launcher runs a read-only Kubernetes preflight over configuration,
  kubeconfig, immutable registry image digest, and resource values. Default
  `local` workspace mode additionally requires exactly one Kubernetes node;
  `existing-pvc` mode accepts one or more Ready schedulable nodes and read-only
  validates the operator-managed Bound Filesystem `ReadWriteMany` PVC and its
  existing writable host mount. The launcher and Rust runtime both require
  `registry/repository@sha256:<64 lowercase hex>` from a deliberately
  conservative subset: a lowercase DNS/IPv4-style registry with an optional
  numeric port and lowercase repository components separated by single `.`,
  `_`, or `-` characters. Bracketed IPv6, tag+digest references, tags, empty
  values, and malformed digests fail before the backend marker, host workspace,
  or cluster is changed. A missing
  kubeconfig regression exits with no marker; a live-node preflight records
  `kubernetes` only after success. Markerless upgrades infer Docker from existing
  Compose-labelled workspace/PostgreSQL volumes; mismatches refuse rather than
  deleting or migrating data. Selecting Kubernetes remains
  fresh-install/separate-project only. Its kubeconfig must be readable by the
  invoking host user. In `local` mode, workspace paths use a conservative
  YAML-safe character set before Factory creates the host directory and renders
  or applies the single-node local-PV manifest. In `existing-pvc` mode, Factory
  creates, changes, and applies no storage resources; the operator must mount
  the same shared backing filesystem at the validated host path and through the
  PVC used by execution Pods.
  In `local` mode, the default K3s namespace, PV, PVC, and host workspace
  identities derive
  deterministically from the validated Compose project name, preventing two
  separate Factory projects from sharing those resources accidentally.
  In `existing-pvc` mode, the namespace, PVC, and host mount are
  explicit operator-owned inputs, and Factory does not manage a PV.
  The four-stage executor has one unversioned module tree: `src/executor/mod.rs`
  owns its public lifecycle, `src/executor/task.rs` owns task/config validation,
  `src/executor/stage_loop.rs` owns stage and remediation turns, and
  `src/executor/resume.rs` owns checkpoints, correlations, and recovery. The
  former flat `src/executor.rs` is deleted. Runtime event work is confined to
  `src/events.rs`, its `src/lib.rs` module export, notification forwarding in
  `src/session.rs`, and stage lifecycle calls in `src/executor/stage_loop.rs`.
  The sink stores only exact active-turn events, coalesces text streams to
  roughly 1 KiB without trimming chunk join boundaries, and never stores
  completed command output a second time. A streamed agent message ends with a
  metadata-only completion event carrying its phase; its complete text remains
  in the preceding chunks instead of being duplicated.
  `src/bootstrap.rs` and `src/bin/factory_worker.rs` now install the canonical
  Rust provider profile directly into Codex's existing config override layer;
  they no longer accept an unregistered provider ID without its provider table.
  Planning, execution, and remediation use autonomous container access. Session
  startup sends Codex's exact upstream `CollaborationMode`: Plan, execute, and
  remediation use `ModeKind::Default`. Factory's Plan prompt and validation
  remain the single planning contract, including exactly one
  `factory_decompose` call and a pure pending decomposition. Plan turns use a
  read-only workspace and carry the exact Factory stage into extension tool
  calls, so providers cannot execute work or persist memory during Plan and
  rely on post-turn rollback to catch it. Plan also removes Codex's
  `apply_patch` and `request_permissions` tools from both the model catalog and
  dispatcher before sampling. The worker forces Codex's native legacy Landlock
  path for read-only commands because default Docker containers cannot create
  bubblewrap's nested namespaces; the image needs neither bubblewrap nor
  elevated container capabilities.
  Plan decomposes only implementation and verification work owned by Execute;
  it cannot schedule duplicate Review, Remediate, or re-review units. Execute
  likewise leaves explicitly later-stage work to Factory's durable lifecycle.
  Execute records
  each incomplete unit once as completed with evidence after implementation
  and verification; the durable progress tool accepts only a first completion,
  rejects status or evidence rewrites, and atomically promotes persisted
  pending, in-progress, or blocked recovery state to completed. A recovered
  Plan first promotes a valid persisted completed checkpoint and state. Only
  when a replacement turn is proven necessary does the runtime restore
  coordinator-owned disposable state and the managed worktree, immediately
  before starting that replacement. Current-attempt thread correlation is
  persisted before any fenced Plan or review recovery write. The Plan gate
  requires a pristine worktree,
  pending work units with no progress summaries, and no review, remediation, or
  history state. A rejected Plan is reset only after its Codex session shuts
  down. Ordinary rollback checks out, resets, and cleans the managed worktree
  in place so an active scoped mount retains the same directory inode. A
  missing or corrupt linked worktree requires the old backend to be removed
  before explicit recreation and reprovisioning of the same durable environment
  generation. The same rollback path covers cancellation, provider/runtime
  failure, and shutdown failure, while replacement Plan preflight repeats
  cleanup after a process crash. Detached review starts in a fresh Codex thread without copying
  the parent conversation, while retaining Codex review source metadata and the
  typed parent thread, parent turn, and durable-state attachment used by
  Factory lineage. It captures a durable semantic Git snapshot of tracked,
  staged, and nonignored untracked content. The top-level `.factory/` path is
  reserved for untracked Factory artifacts. Each managed mirror preserves its
  existing `info/exclude` contents while adding `/.factory/`, keeping artifacts
  out of ordinary agent `git status` and `git add -A`. Repositories whose
  current index, HEAD, or immutable workspace base tracks that path are
  rejected during workspace materialization, review capture, and result
  export. Any review mutation is restored and rejected. A mutation-detected
  marker is written before restore
  and removed only after the Factory review-state rollback is durably saved,
  closing the process-death window between those operations. Ignored build and
  test artifacts are intentionally outside this read-only content gate. The
  execute gate still requires completed work units and an advanced Factory
  revision. `stage.completed` is emitted only after the native turn, Factory
  state, worktree, and final checkpoint have all passed validation; semantic
  failures emit `stage.error` instead. This guard does not add a second sandbox
  or prevent native subagents from reading the worktree.
- `factory-harness/factory/cli/`: native `factory` job-lifecycle CLI. It creates
  the exact four-stage job directly through `factoryd`, materializes the managed
  worktree, streams durable events, and implements run/attach/status/stop
  without Hatchet or TypeScript. `src/config.rs` owns interactive and
  noninteractive provider onboarding, hidden API-key input, endpoint choice,
  model selection, and provider/model switching while preserving unrelated
  deployment configuration; provider metadata comes from the canonical Rust
  profiles crate. `src/profile_guard.rs` checks queued, running, and cancelling
  Factory tasks before a profile change. It refuses mismatched pinned and
  legacy unpinned jobs with exact serve/status/stop commands; `--force` warns
  and changes configuration without pretending to migrate or cancel them.
  First-time native CLI setup still works without a coordinator. Exported
  provider keys satisfy launcher preflight, custom direct Responses bases are
  persisted as the actual Codex endpoint, and
  workspace clone/fetch uses a client without the 30-second general request
  deadline. An ambiguous workspace transport failure leaves the unclaimed job
  intact for explicit inspection or cancellation instead of issuing a false
  cancellation while server-side Git may still be running. Every new task
  stores its canonical provider and exact model in the direct
  `FactoryTaskInput`; workers claim only an exact profile and legacy unpinned
  tasks remain ready without consuming attempts. Attached local runs apply a
  successful result back to the clean originating checkout by default.
  `--no-apply`, `factory apply`, and `factory export` provide explicit control.
  User-facing local runs and apply always target the launcher invocation
  checkout; `--repository` accepts remote Git URLs only, while internal
  checkout overrides stay hidden. Apply verifies the patch digest, the local
  path identity or normalized remote origin identity, immutable base revision,
  clean host state, and Git's binary-patch preflight before changing any host
  file. Completed-result apply/export and safe status/stop commands start only
  PostgreSQL plus `factoryd`, so result retrieval neither validates a provider
  key nor starts Qdrant, a provider bridge, or the model worker. Attach starts
  the full runtime and performs a final event-cursor drain after observing a
  terminal job, so the atomically committed final `stage.completed` event is
  printed before the complete untruncated result. `factory result` reconstructs
  current settled output from durable events, refreshes disposable coordinator
  and matching workspace renderings, and then prints that reconstruction.
  `factory artifacts` uses the same event reducer and repair path, reports the
  fixed artifact set, and prints a `.factory/jobs/<job-id>/` path only when its
  regular files byte-match the refreshed coordinator rendering. Pre-artifact
  jobs therefore self-heal through exact durable operation/attempt/accepted-turn
  selectors rather than trusting old files. Result and artifact commands use
  only PostgreSQL and `factoryd`, and JSON terminal output carries the full
  result plus artifact metadata. Provider/model configuration also starts only
  PostgreSQL plus `factoryd` for its active-job check. `src/transcript.rs`
  reduces paired and streamed records into lossless display cells without
  discarding durable records, and `src/live.rs` renders a bounded Ratatui view
  with keyboard and mouse expansion. Non-TTY output is compact, `--verbose`
  retains the complete human replay, and `--json` preserves every event and
  payload. Final output replaces the former 180-character stage previews with
  the complete durable result; `--verbose` remains lossless without repeating
  the same result body at exit. The private Codex TUI modules were not exposed
  or copied: their cells depend on the full chat application and would add its
  entire dependency closure to the standalone Factory CLI.
- `factory-harness/factory/providers/`: native Rust transport adapter and
  canonical provider profiles. OpenAI Responses is direct; Anthropic Messages
  and DeepSeek/Z.AI Chat Completions are translated into the Responses surface
  without adding another agent harness. `profiles.rs` and
  `CodexProviderSelection` compile without the adapter feature for lightweight
  CLI/runtime use; `responses.rs`, `tools.rs`, and `response_stream.rs` keep
  request parsing, tool mapping, and streaming output as separate concerns.
  Claude selection includes low-cost Haiku 4.5 alongside Sonnet 5, Opus 5, and
  Fable 5. Anthropic request generation emits the current nested
  `output_config.effort` contract only for models that support effort; Haiku
  uses its 200k context metadata and 64k output ceiling without unsupported
  thinking controls. Adaptive models translate requested reasoning summaries
  through `thinking.display`. Generated model metadata advertises `xhigh` only
  where Anthropic supports it, and always-adaptive models cannot be translated
  to a disabled-thinking request.
  The centralized Anthropic capability resolver recognizes dated 4.5 aliases,
  preserves Opus 4.5 effort without adaptive thinking, and rejects explicit
  effort for unknown custom models instead of silently discarding it.
  Adapter-backed selections default an absent or legacy-blank catalog setting
  to the bridge-generated shared catalog, while an explicit override wins;
  direct OpenAI keeps no catalog override unless one is explicitly supplied.
  Unsupported hosted `web_search` declarations are omitted from chat-provider
  requests instead of failing the entire Codex turn. Anthropic endpoint
  construction trims trailing slashes, removes at most one terminal `/v1`,
  and appends `/v1/messages`, preserving tenant paths and repeated segments.

## Root Documentation and Configuration Cleanup

Audited and updated on 2026-08-02:

- `README.md`, `PRODUCT.md`, and
  `docs/adr/0001-codex-kernel-factory-extension.md` now describe only the
  implemented Rust application boundary, current real-model evidence, and
  remaining acceptance gates.
- `AGENTS.md` now treats the deleted TypeScript/Hatchet trees as prohibited
  history rather than temporary application code.
- `.env.example` no longer exposes Hatchet tokens, endpoints, ports, retries,
  the deleted integration-plugin loader, duplicate Codex provider IDs, or the
  removed provider-bridge token. The worker slot variable is now
  `FACTORY_WORKER_SLOTS` throughout, and every optional provider port in
  Compose has a matching example override. `FACTORY_QDRANT_API_KEY` is wired
  from the standard environment file into the worker without being printed.
- Deleted the obsolete `postgres-init/` script and Compose mount; PostgreSQL
  creates the baseline `factory` database directly through `POSTGRES_DB`.
- Removed nonexistent paths from `.dockerignore` and dead Node/Codex analytics
  environment knobs from the image and Compose wiring. Codex analytics remain
  disabled by the Rust runtime configuration itself.
- `.github/workflows/publish-container.yml` no longer watches deleted
  `harness-client/`, `workflows/`, or `integrations/` paths.

The 2026-08-02 post-deletion inventory contains exactly zero Factory-owned
`.ts`, `.js`, `.mjs`, `.tsx`, or `.jsx` files outside protected upstream.
Deleted legacy paths stay deleted; do not recreate a Node runtime, generated
client, Hatchet workflow, or integration loader.

## Root Distribution and Documentation Surface

The runnable distribution switched to Rust on 2026-08-02:

- `Dockerfile` builds and installs `factory`, `factory-worker`, `factoryd`, the
  Rust `factory-provider-bridge`, and the preserved upstream `codex` binary
  used solely for Codex's native `exec-server`. It has no Node build stage, and
  no legacy client, integration, workflow, or provider script enters the
  image. `factoryd` and the provider bridge drain Axum gracefully on both
  SIGINT and the SIGTERM used by container shutdown.
- `deploy/gpu/` is the optional multi-architecture GPU execution-image seam.
  It copies the Factory/Codex binaries and worker entrypoint from an immutable
  Factory image into the immutable NVIDIA PyTorch 25.08 CUDA 13.0 base already
  exercised on GB10. Its NVIDIA index contains both AMD64 and ARM64 images for
  the A100 and GB10 hosts. A pinned multi-architecture `uv` binary supports
  workspace-local alternate Python versions without changing the profile.
  Pod-local `HOME`, XDG, Codex, uv cache, and uv managed-Python paths live under
  `/tmp`, so arbitrary non-root execution UIDs can initialize them without a
  user-specific image layer. It is selected only through the Kubernetes
  execution image setting and never enlarges or replaces the default
  control-plane image.
  Repo dependencies, credentials, datasets, and benchmark output remain
  runtime workspace state and are not baked into this profile.
- `benchmarks/cleanrl-gpu/` is the active Factory-owned two-node GPU/GLM
  evidence harness. Its standard-library collectors, strict schemas, and
  deterministic renderer cover the GB10 and A100 runs; normalized metrics and
  rendered charts are generated only after real jobs execute, never as sample
  or placeholder evidence. On 2026-08-11, both GLM-5.2 issue jobs and both
  independent CUDA C51 train-checkpoint-evaluate runs completed. The four
  normalized rows, timestamped GPU samples, exported issue patches, PNG/SVG
  chart, and scope limitations are published under that directory; see
  `benchmarks/cleanrl-gpu/REPORT.md` for the measured evidence.
- `docker-compose.yml` runs PostgreSQL, Qdrant, `factoryd`, and
  `factory-worker` by default. `factoryd` sees the
  selected host repository at `/workspace/project` so the native CLI can apply
  a verified completed result. Only the worker sees coordinator-owned
  `/workspaces` as a backing tree; each per-job Codex execution container sees
  its exact worktree and repository Git common directory. Hatchet
  and runtime Node commands are absent. Optional profiles remain optional; all
  provider adapters share the one selected-provider catalog volume, and
  provider health checks use `curl` instead of Node.
  The worker mounts the Docker Engine socket and enables the Docker execution
  provisioner. Its generic entrypoint retains the socket's numeric group while
  dropping to the configured workspace UID/GID. Provisioned exec-server
  containers do not receive the socket. The former static `codex-exec-server`
  service, global URL, launcher startup/log entries, and worker dependency are
  removed; every production session uses its durable per-job environment.
- The root `factory` launcher owns only Docker/bootstrap and host-file
  lifecycle. It delegates onboarding, hidden key input,
  provider/model switching, run, attach, status, stop, result/artifact reads,
  result apply, and result export to the Rust CLI. It derives a stable local
  repository identity from the canonical host Git root before crossing the
  fixed container mount. It
  reads the same Docker Compose-expanded provider, model, and key values used
  by the worker, and streams verified export bytes from container stdout into
  an atomic no-overwrite host file rather than passing host paths into `factoryd`.
  Controlled stack startup removes orphaned pre-cutover services, so deleted
  workflow-worker or Hatchet containers cannot survive an in-place upgrade.
  `logs` follows the active Rust services and `build` invokes the root Dockerfile
  directly.
- `apps/README.md` now documents the Rust ownership boundary.
- `apps/cli/factory-worker-entrypoint.sh` is Factory-owned distribution wiring
  and now validates the workspace ownership policy used by the Rust services.
  Its default `manage` mode retains the local recursive ownership setup, while
  `preserve` leaves an operator-owned `existing-pvc` workspace mount untouched.
  This deliberate entrypoint seam does not modify the protected Codex kernel.

These root paths are distribution wiring, not separate application
implementations:

- Maintainer and product documentation aligned with the Rust distribution:
  `AGENTS.md`, `PRODUCT.md`, `README.md`, and
  `docs/adr/0001-codex-kernel-factory-extension.md`. Provider-specific documents
  remain owned by the provider slice.
- `factory-harness/UPSTREAM.md` records the blob-verified Codex baseline
  `406dc9239492aff6d295cca5eebe2a548548d42f`, the exact native seam, the
  initial prefix omissions, and the prefix-aware update workflow.
- `RUST_CUTOVER.md` is this compaction-safe ledger and must be included with
  the cutover rather than left as an untracked local note.

## Compaction-Safe Audit State

- Deleted paths stay deleted; do not recreate protocol versions or generated
  Factory compatibility surfaces.
- `_legacy/` was deleted after native Rust functional acceptance replaced it.
- Ignored `node_modules/` trees formerly under the now-deleted provider bridge,
  `harness-client/`, `workflows/`, and `integrations/` were deleted after
  verification; they were 331 MB of reproducible generated dependencies, not
  source.
- Ignored generated `dist/` trees under `harness-client/`, `workflows/`, and
  `integrations/` were also deleted after verification: 2,613 reproducible
  files totaling about 11.5 MB.
- The protected upstream paths above are excluded from Factory language and
  dead-code inventories except for the recorded review seam.
- The protected upstream, active Rust, and deleted-path lists above are the
  current production boundary. Update this ledger in the same patch that
  moves, deletes, activates, or protects another path.
- On restart, read this file and inspect `git diff --name-status`; do not begin
  with a repository-wide rediscovery pass.

## Verification Evidence

Recorded on 2026-08-02:

- The Rust workspace check and Rust-only language inventory passed.
- Profile-switch acceptance passed five fake-API binary paths: default refusal
  listed mismatched pinned and cancelling legacy-unpinned jobs, `--force`
  warned and persisted the requested profile, a requested profile matching all
  active jobs succeeded, first-time configuration succeeded with no
  coordinator, and first-time configuration respected a reachable retained
  legacy job. Neither old nor newly supplied API-key bytes appeared in any
  output. The same refusal, force, and matching-profile sequence passed
  through a real `factoryd` against a disposable PostgreSQL container. The root
  launcher fixture proved configuration starts only PostgreSQL and `factoryd`,
  joins the coordinator network, and starts no worker, Qdrant, or provider
  bridge.
- `cargo test --workspace --all-targets` passed after the CLI cutover fixes.
  The attach race fixture forces terminal state to appear between event pages
  and proves the final `stage.completed` event precedes the terminal result.
  The pre-workspace fixture now supplies a hermetic provider/model profile,
  uses current repository/workspace fields, and proves a model containing
  dollar expressions, `#`, spaces, quotes, and backslashes is pinned unchanged.
- Host export of succeeded job `95843a17-0eb9-46da-a21c-9ef4ab3ac824`
  wrote a nonempty binary patch to an absolute host path, matched raw `-o -`
  output byte-for-byte, and refused a second export without changing the first.
  A Compose profile fixture proved the same special-character model and key
  bytes reach both worker and selected provider service. Live host binaries for
  `factoryd` and `factory-provider-bridge` both drained and exited on SIGTERM.
- The Codex pin `406dc9239492aff6d295cca5eebe2a548548d42f` was verified
  against the official repository and the initial Software Factory import:
  5,816 of 5,831 shared blobs were identical, while the 15 differences are the
  documented original native extension seam. Prefix omissions are recorded in
  `factory-harness/UPSTREAM.md` rather than hidden by a fabricated mirror claim.
- Focused workspace acceptance covers staged and unstaged text, binary files,
  symlinks, nonignored untracked files, ignored artifacts, semantic read-only
  Git index refresh, and replacement-process recovery of a retained review
  mutation marker.
- The disposable-PostgreSQL runner acceptance completed as overlap job
  `9232af26-7cc1-45d6-8cc4-33d01f41f3a0`: lease epoch two was acquired while
  epoch one remained alive, but neither the replacement runtime nor a separate
  `factoryd` workspace request entered until epoch one observed cancellation,
  drained, and released its advisory lock. Successful fixture operations each
  stored exactly one transactional `stage.completed` event; direct stale-owner
  renewal, checkpoint, settlement, correlation, and thread-state writes were
  fenced by the live PostgreSQL store test.
- The focused runner control-poll regression held the job-state read beyond its
  one-second budget while the independent heartbeat advanced the live lease.
  The executor remained running, then an explicit cancellation drained its
  runtime, completed cleanup within the bounded wait, and marked the attempt
  abandoned instead of leaving a live lease parked until expiry.
- The updated disposable-PostgreSQL runner acceptance passed Plan-validation
  retry job `71dd05dd-8c24-4033-8579-09a52aec34d2` and review-task-panic retry
  job `21dedaa7-a071-42cb-94ad-5a63d67a98da`; each completed on its second
  durable business attempt, with the first review failure recorded as
  `executorPanicked`. Running cancellation job
  `1d906737-1fc5-4fee-bf07-fee516b96543` returned `cancelling` and reached
  terminal acknowledgement in 1,035 ms. At a 900-second lease, graceful
  restart job `57974d7b-b0f9-44eb-ba5e-abe24bed5e62` relinquished immediately
  and the same attempt resumed at lease epoch two in 226 ms.
- Real container image
  `sha256:362190f69341d5507b4e50543ad92da74317bdec052ae15faa832f52a64195f5`
  passed SIGTERM recovery with a production 900-second lease. Mid-Plan job
  `986305f2-cd22-43cb-8a31-f4d5f185ddcc` is durable database/rollout recovery
  evidence only: it drained in 304 ms and reclaimed the same attempt at epoch
  two in 1,364 ms. Its fixture was later externally dirtied, so it is not
  current worktree-correctness evidence. Mid-review job
  `e8bb775d-664e-4c10-9634-bfe0076a2759` restored the same injected mutation
  while preserving the intended execute result, drained in 281 ms, resumed the
  same review attempt at epoch two 132 ms after the replacement worker process
  started, and completed with exact `BASELINE\n` bytes. The full Compose restart
  took 9.5 seconds because dependency health checks ran before worker startup;
  it did not wait for the lease.
- Real running-cancellation job
  `ab2dac37-3de8-4950-893c-85046e7eaa0c` was held inside a live detached-review
  turn, then received an injected tracked-file mutation. Its API response was
  `cancelling`; the worker interrupted Codex, restored exact `BASELINE\n` source
  and execute-result bytes, removed review recovery state, and acknowledged
  terminal `cancelled` in 1,093 ms. The review attempt was durably abandoned
  with cause `jobCancelled`; a subsequent valid Factory job was claimed in
  under 0.5 seconds, proving the worker slot remained usable.
- Fixed-locator repository switching passed
  `repository_identity_keeps_fixed_locator_mirrors_separate`: two repositories
  presented at the same container-style path produced separate identity-keyed
  mirrors, and both original revisions and contents remained readable.
- Shared-cache concurrency passed both the in-process Git gates and disposable
  PostgreSQL test `same_repository_jobs_and_rematerialization_keep_immutable_bases`.
  Creating job two while job one's uncommitted changes and worktree branch were
  live preserved both HEADs and snapshots; their binary exports remained
  isolated. After the source `main` branch advanced, deleting and rematerializing
  a third worktree restored its recorded original `baseRevision`, HEAD, and
  bytes. The legacy-mirror regression also migrated an in-use old refspec
  without invalidating its active worktree.
- Advisory-lock isolation passed disposable PostgreSQL test
  `maximum_worker_slots_leave_durable_control_capacity`: a worker configured at
  the accepted 32-slot maximum filled all 34 lock-pool connections, while a
  lease heartbeat, correlation append, and durable attempt event completed
  through the independent query pool.
- Result delivery passed a binary Git round trip covering text edits, binary
  additions and modifications, deletion, untracked files, and executable-mode
  change. The CLI refusal gate proved wrong repository identity, wrong base
  revision, and a dirty host checkout all leave host files unchanged before a
  matching clean checkout applies successfully.
- Exact execution-profile claiming passed against disposable PostgreSQL: a
  mismatched provider/model worker and an unpinned legacy Factory task each
  created zero attempts, while the exact pinned profile claimed the ready
  operation. `cargo check --workspace --all-targets` passed after these changes.
- A retained-job migration regression re-applied migration 13 to an exact
  `claude` JSONB pin and observed canonical `anthropic` in the stored input.
  The profile guard accepted the matching Anthropic configuration without a
  warning; an alias claim matched nothing, while the canonical Anthropic/model
  claim acquired the operation. A runtime regression independently rejects
  noncanonical persisted IDs before starting Codex.
- Focused startup acceptance proved an exported provider key satisfies launcher
  preflight without appearing in output, a custom direct Responses base is the
  persisted runtime endpoint, workspace setup outlives the general HTTP timeout,
  and an interrupted mirror is preserved and replaced by a valid atomic clone.
- Provider regressions cover Anthropic bases without `/v1`, with `/v1/`, and
  with `/tenant/v1/v1`; each resolves to exactly one appended `/v1/messages`
  path. Compose rendering confirms the optional Qdrant API key reaches the
  standard worker environment without appearing in command output.
- Job `7195380e-0641-4346-9949-0b0a93eea2c8` completed read-only plan,
  tool-using execute, detached review, and approval in a managed Git worktree.
- Job `27013fd5-fcd1-45ea-a639-b70422854dd4` was observed continuing the same
  in-flight durable job after a worker restart. Treat it as recovery-mechanics
  evidence only, not as final output-correctness or review/remediation evidence.
- Do not use job `3c108cb4-50f4-431d-9921-ebff7aef71d3` as correctness
  evidence. Its stage structure completed before the sparse-recall threshold
  fix, but unrelated one-token memory recall contaminated the task result.
- Job `f0a47111-b774-4a2c-87e1-c3dacd5d96d3` is the valid review-cycle gate:
  detached review requested changes for an injected post-execution defect,
  remediation resolved both findings, an independent detached re-review
  approved the corrected bytes, and `reviewHistory` preserved the rejected
  review with its matching remediation dispositions.
- Current-image DeepSeek job `95843a17-0eb9-46da-a21c-9ef4ab3ac824`
  completed plan, execute, detached review, and the approved remediation no-op.
  Its durable parent rollout contains exactly one successful `get_goal` call;
  native Codex subagent `019fc0c9-55de-7e23-aab5-c2198513b13a` returned
  `# Greeting Fixture` and has persisted spawn, wait, and close activity. The
  review ran on distinct thread `019fc0c9-e99b-75a0-a361-b7d4098f6920` and
  independently verified the only worktree change: 12-byte `ACCEPTANCE.txt`
  containing `SUBAGENT-OK\n`. A post-terminal `factory attach --json` replay
  returned all 134 durable events plus the terminal result. Provider fixtures
  separately prove that an unambiguous original name such as `get_goal` maps
  back to its namespaced tool and that forked history replays calls to tools
  intentionally hidden from the current review turn.
- Job `071ca34f-f61e-4782-95bd-5fc2b42afe53` stored an explicit memory in
  Qdrant; a separate job recalled its value without receiving that value in its
  prompt. Job `679c2d2c-b145-4be8-9200-a0f6cb2ee26d` then proved an unrelated
  one-token collision no longer overrides its task.
- Exact job status, workspace branch/diff, review lineage, recovery links, and
  final worktree bytes were independently inspected in PostgreSQL, Codex
  rollouts, the managed worktree, and CLI replay.
- Job `85fab50b-0530-4f60-9e27-f2d6f507df56` was a discovery run, not
  acceptance evidence. It exposed a mutating Plan turn, rejected execute state,
  and premature `stage.completed` emission; the Plan guard, rollback, and event
  ordering above are the resulting fixes.
- Job `d402b429-2ee0-4b9e-8951-569c9f60a5b4` was a cancelled diagnostic run.
  Its artificial 14,000-token threshold proved that native compaction fired,
  but repeatedly compacted the model's near-baseline context and never reached
  a terminal workflow result. It is not correctness evidence.
- Job `dbb91eb1-a63d-4040-b134-47e972ad2ab6` is discovery evidence only. Its
  recovered execute turn succeeded, but detached review repeatedly submitted
  approve with informational findings. The resulting fix makes that invalid
  combination fail inside `factory_record_review` before state mutation and
  states the verdict contract in the prompt, tool description, and schema. The
  focused regression proves invalid approve and `request_changes` calls preserve
  state while approve with an empty findings array persists successfully.
- Current corrected image digest
  `sha256:f835ea1fed605178f5fcd1c9bcce71449376edd599f3e1e8ba33a78a1140ff54`
  passed the combined crash-recovery and final-correctness gate in DeepSeek job
  `fdbe0b11-2a87-4cbf-bf36-e6dde4e76a38`. The isolated worker had no model
  catalog override; the generated DeepSeek profile retained its normal
  128,000-token context window, 95-percent effective window, and no explicit
  auto-compaction limit. Execute operation
  `305e9f10-31ef-4616-a498-f63d61d9eca6`, attempt
  `9726f73b-b826-46d6-81a3-55ae4dcd0bc1` number 1, epoch 1 held awaiting-turn
  checkpoint `e82d378e-b221-4684-9a80-b19fa3e0f8ba` for thread
  `019fc15f-b162-7662-8093-cdbd3b756c3c` and turn
  `019fc160-00ec-76c0-a62e-c52d0fe0e80a`. Event 102 recorded completed model
  tool activity while the exact turn-completed count was zero. SIGKILL then
  stopped the worker with exit 137 before that turn completed.
- After the lease expired, the new worker reclaimed the same attempt ID and
  attempt number at epoch 2 with recovery cause `lease_expired`, a self resume
  link, and the exact checkpoint above. The original turn remained incomplete;
  the same parent thread continued with replacement turn
  `019fc160-bf55-7c20-b39e-f387057c5876`. Its events 109-120 independently
  compared the recovered file, checked its length, and ran `verify.sh` before
  turn and stage completion at events 129-130. Detached review used distinct
  thread `019fc160-fae7-7323-9081-0a26f97edde3` and turn
  `019fc160-fb11-7890-a72a-e09639de89f0`, reran byte and verifier checks, and
  recorded approve with an empty findings array and exact parent-turn lineage.
  All four operations succeeded with no stage or turn errors. The only
  worktree change is the 22-byte `RECOVERY-PROOF.txt`; it matches `SOURCE.txt`
  at SHA-256
  `2323ad3a8d838354d026e0ef5139b4b724e77ca130cad91d589336b57020fe5e`.
  Post-terminal attach returned exit 0 and replayed all 101 durable events plus
  one terminal result.
- Image digest
  `sha256:6cdcebbbee3d0b559591d25c78d408069c2075f9044f24f922ad390a74cddd96`
  passed the terminal compaction gate in DeepSeek job
  `0ff583b2-a1d9-4ef0-ba48-eb2e9ceeb812`. Plan, execute, detached review, and
  the approved remediation no-op each succeeded on attempt 1 with no retry or
  error. Plan turn `019fc149-dcf9-7ce1-a48b-8f449cab3da9` recorded native
  `collaboration_mode_kind=plan`, used only `ls`/`cat` plus exactly one
  `factory_decompose`, and left the worktree pristine. Execute turn
  `019fc14a-1a5e-7890-ac35-c6c0ac03c3af` recorded native default mode, advanced
  Factory state from revision 1 to 5, created only `COMPACTION-PROOF.txt`, and
  passed `verify.sh`. The output exactly matched `SOURCE.txt`, with SHA-256
  `44696a073037c6f849dbad32d88c008418a932ab6c8e828282d38a435353bf01`.
  Detached review turn `019fc14a-61fe-7a11-b01a-66e55d30494e` emitted one
  native compaction pair at durable events 599 and 600, then continued in the
  same turn with additional repository tools from event 605 onward and recorded
  approval. Event order was `turn.completed` then validated `stage.completed`
  for Plan (555/556), execute (589/590), and review (645/646), with no
  `stage.error`. The job was created detached; `factory attach` replayed its
  durable stream through the terminal result. The 17,000-token test threshold
  existed only in the isolated acceptance volume, not in product source.
- Real-model acceptance image
  `sha256:2142a3ef481e99b4dc3e90319d558b288522e2dc8ec72eb2a682305d53922eba`
  passed DeepSeek job `181412b0-7234-498f-bcb1-642aa455b64a` from the real root
  launcher. The image contains the four Rust product binaries and entrypoint,
  with no Node, npm, npx, Bun, or Deno runtime. The pinned job profile was
  `deepseek/deepseek-v4-pro`, repository identity
  `local:cd38b4a9e0972ee58f47fef8685471055508f60a8270a11de93e45d188927ffa`,
  and immutable base `72d699f4c38131277958503f3bf3b628c290d444`.
- Native Plan thread `019fc21c-b809-7450-bfec-290c76bf3b55`, turn
  `019fc21c-b861-7bf2-a7ce-d574671d2670`, recorded Plan collaboration mode,
  used only read commands plus exactly one `factory_decompose`, and left the
  result file absent until execute turn
  `019fc21d-3494-7390-b2a4-e026f339bdf0`. Independent review used distinct
  thread `019fc21d-b7f1-7110-bdc5-abc70ae6e387` and turn
  `019fc21d-b81b-75b2-9696-3fe78843f96f`, reran byte and verifier checks, and
  approved with an empty findings array. Each of the four operations completed
  on attempt one with exactly one `stage.completed` and zero `stage.error`.
  Host auto-apply produced only 16-byte `FINAL.txt` containing
  `CURRENT-TREE-OK\n`, SHA-256
  `f2385a9effd8ce839536bdf0f302c9dd797b2ba0d5eb854cf8076611c4887367`, and
  `verify.sh` passed.
- The same run proved normal catalog selection for legacy blank configuration:
  the generated DeepSeek entry was 128,000 tokens at 95 percent effective
  context with no explicit auto-compaction limit, and every rollout token event
  reported 121,600. The focused regression also proves an explicit catalog
  override wins and direct OpenAI remains override-free by default.
- After a full stack shutdown and with an intentionally invalid provider plus
  blank model and keys, result export started only PostgreSQL and `factoryd`.
  Its 206-byte absolute-host patch had SHA-256
  `40614dcb29feb4b8c3af40f9c99d8ab96c859aa4cd76169de9b360541db87241`;
  a second export refused with exit 1 and preserved the digest. A second full
  shutdown followed by provider-free `factory apply` restored the exact host
  bytes while still starting only those two services. Post-terminal attach
  replay printed review and remediate completion events 2352 and 2353 before
  the single terminal result.
- Final gates passed after these fixes: Factory workspace format, all-target
  check and tests, `just build`, root launcher syntax, Compose rendering, and
  `git diff --check`. The launcher fixture separately proves apply/export do
  not validate provider configuration or start Qdrant, a model worker, or a
  provider bridge; URL/help tests reject host paths for `--repository`, hide
  internal checkout overrides, and retain normalized-origin remote apply.
- A fresh post-closing-fix release build from the exact current Rust source
  produced Linux/arm64 image
  `sha256:173cb6f044fa5400fae62b5bb578a9038e9235fc4beae69be80340c984220e1e`
  at 494,554,054 bytes. Direct inspection found exactly the four named Rust
  product binaries plus `factory-worker-entrypoint`; Node, npm, npx, Bun,
  Deno, and bubblewrap are absent. The source format, all-target check, 79
  nonignored tests, exact-bin `just build`, launcher syntax, every Compose
  profile, staged and unstaged diff checks, Rust-only language inventory, and
  deleted-path gates all passed against this source. The model-run evidence
  above remains tied to its recorded acceptance image rather than being
  silently reassigned to this later digest.
- Final Plan-sandbox and host-tool acceptance produced Linux/arm64 image
  `sha256:59d2866c24fabec0e7a583bf60fdda678d7e8f457ff8b4b42f224223ae03cf10`
  at 494,554,054 bytes. The exact Codex tool-plan regression proves Plan removes
  `apply_patch` and `request_permissions` from both the model catalog and
  dispatcher while retaining `exec_command` and `write_stdin`; Factory's stage
  policy and runtime suites pass. The image contains the four Rust product
  binaries and entrypoint, with no Node-family runtime or bubblewrap.
- DeepSeek job `5fb89a11-c908-4816-b3e9-d75de4cb04fc` passed Plan, Execute,
  detached Review, and the approved no-op remediation on attempt 1. Plan used
  native read-only shell successfully, decomposed once, and had no file-mutation
  tool. Execute alone patched `state.txt`; fresh review independently approved
  the exact one-line diff. Detached human-readable attach exposed stage,
  summarized reasoning, agent, plan, tool, and file activity through the
  terminal result. Its 150,870 logical input tokens were 22.11 percent below
  the prior complete Factory run on the same fixture; uncached input was
  19,798, or 3.168694 times the preserved direct control.
- DeepSeek job `ee688c44-debc-4cce-ba24-23072eacfa6b` passed the real
  remediation cycle. Initial detached review thread
  `019fc419-05f6-7e90-825b-735bb6f4b84b` requested changes for deliberate
  finding `f1`; remediation changed the only file to `VALUE=remediated` and
  recorded `f1` resolved; distinct re-review thread
  `019fc419-a235-76a3-84b6-7678d41c07a3` approved with no findings. Durable
  `reviewHistory` retains generation 1 plus its disposition while generation 2
  is the current approval.
- DeepSeek recovery job `1ae3eca3-6a44-4046-8015-ea17afab1f04` was SIGKILLed
  after Execute started `sleep 30` and wrote its proof file, but before the turn
  completed. Replacement worker instance reclaimed the same attempt
  `34b7c476-3604-4057-8eb5-bbfc236b8608`, still attempt number 1, at lease
  epoch 2 with `leaseExpired` and a self-resume checkpoint. Replacement turn
  `019fc41e-6d23-77a2-af77-3d2baed67bf6` verified the existing bytes without
  rewriting and completed; all stages succeeded with one final Execute
  completion and zero stage errors. `RECOVERY-PROOF.txt` is 20 bytes at SHA-256
  `7b3b8231ac63b442ce13430cf36020359b974371e3db540665fb5d926b55d11a`.
- The persisted A/A/B real-model memory gate remained exact after all app
  containers moved to the final image: shared-namespace Qdrant counts are Repo
  A 1 and Repo B 0, Repo A automatic recall retains its recorded private-output
  digest, and Repo B remains exact `UNKNOWN\n`. Full machine-readable evidence
  for these final jobs is under
  `/tmp/software-factory-final-acceptance-59d286/` on the acceptance host. The
  worker was restored after the crash drill to four slots, a 900-second lease,
  and a 30-second shutdown grace.
- Final cleanup removed the disposable `sfcompact`, `factoryrecov802`,
  `factoryrecovfix802`, and `factorylifecycle802` Compose projects and their
  labeled containers, volumes, and networks. The temporary
  `software-factory:lifecycle` and `software-factory:acceptance` images and the
  isolated standard-plan acceptance provider volume were removed. Normal
  `software-factory` PostgreSQL, Qdrant, provider, Codex-state, and workspace
  volumes and the final local image were preserved.
  Superseded untagged acceptance image objects `2142a3ef481e`,
  `6cdcebbbee3d`, and `f835ea1fed60` were also removed after their evidence was
  recorded above; unrelated unlabeled Docker build cache was not pruned.

Automatic token-triggered context compaction is therefore accepted through the
unchanged native Codex path. Factory observes and durably replays its lifecycle;
it does not implement a second compactor.

Recorded on 2026-08-03 (clarification gate and continuation rounds):

- The interactive clarification gate is CLI-only and fail-open: real DeepSeek
  calls produced five numbered questions for an ambiguous task, an `expect`
  driven terminal session paired marker answers to their exact questions, the
  composed prompt was persisted to `.factory/prompts/prompt_<sha12>.md`, and an
  unreachable coordinator failed job creation only after the prompt file was
  saved. Piped, `--json`, and `--no-clarify` runs skip the gate.
- Root-launcher regression job `182c36c3-785d-41dc-967f-38f46ec10a99`
  proved the configured DeepSeek key and upstream base reach the interactive
  CLI process by environment-variable name rather than command-line value.
  The live gate asked five questions, saved the exact paired answers to
  `prompt_66f16bd5202e.md`, pinned the same composed task in the durable job,
  and emitted no key bytes. The disposable job was then cancelled terminally.
- `POST /jobs/{id}/continue` reopens a succeeded factory.task: remote job
  `e8059bc3-68e2-4dd9-847f-d1509c29478d` ran the base four stages, then two
  live continuation rounds (ten durable operations) against real
  `deepseek/deepseek-v4-pro`. Each round appended its feedback to the durable
  task, appended `codex.iterate`, `codex.review`, `codex.remediate`, and
  requeued; the recovery claim SQL was unchanged. The iterate turns resumed
  the original parent Codex thread (three turns on one thread across rounds
  and a container image swap), each round's detached review approved against
  the amended task, and the final result exported one cumulative patch against
  the immutable base containing every round's changes.
- Iterate turns are exempt from the execute-stage state-revision-advance
  requirement because `factory_update_progress` rejects rewriting a completed
  unit: failed job `61adaadd-ca47-44d1-873b-1ad38bd5fdbd` is the recorded
  counterexample, where three correct iterate attempts were rejected by that
  invariant before the exemption. Its round's detached review remains the
  functional gate for iterate output. Settled `iterate` stages project to the
  fixed `iterate.md` artifact; repeated stages keep one file per stage kind
  holding the latest settled output.

Recorded on 2026-08-05 (verification-residue cleanup):

- Execute and Iterate now require full verification followed by removal of
  only untracked residue created solely by verification. Review checks the
  complete diff, including untracked files, and Remediate cleans any finding.
  Every stage explicitly preserves tracked files and task-required outputs,
  including requested generated files. Result export remains complete and
  unfiltered so it cannot silently underproduce.
- Real `deepseek/deepseek-v4-pro` job
  `6c0407e2-be31-43af-8c3f-4c3293b1a56b` completed Plan, Execute, detached
  Review, and Remediate on the Python normalization fixture. Execute ran five
  standard-library tests and reported cleanup; Review independently approved
  the requested implementation, test file, and README example with no
  transient residue. Applying the exported result produced exactly modified
  `normalize.py`, modified `README.md`, and untracked `test_normalize.py`;
  `python3 -B -m unittest -v` passed all five tests and a host scan found no
  `__pycache__`, `.pytest_cache`, `*.pyc`, or `*.pyo` path.
- Two fresh `zai/glm-5.2` jobs did not reach the cleanup gate. Jobs
  `98c2e4ad-79eb-48e9-bf93-1f8beaa954ed` and
  `88183e16-7b52-46f4-9501-8d804345cc77` each exhausted three Execute
  attempts because the provider repeatedly supplied malformed `apply_patch`
  arguments, either a bare `@@` hunk or a missing `*** End Patch`. These runs
  are recorded provider/tool-format failures, not cleanup acceptance evidence.

Recorded on 2026-08-07 (Codex remote execution):

- Linux image
  `sha256:086337e6ed373c402198ab4f3f109cb98866032c6327d23f1499aa823ac10f20`
  contains the preserved upstream `codex` binary and Factory's Rust binaries.
  Compose runs `codex exec-server` separately with the same managed-worktree
  volume; the worker has no local execution fallback.
- DeepSeek job `5e865e18-c5f6-43c3-895d-4e4e325ff3d0` passed the clean
  lifecycle gate. Plan created four Execute-only units. Remote commands, a
  repository skill, a native Codex subagent, `apply_patch`, and both verifiers
  produced the exact final `STATUS=accepted`,
  `REMOTE_EXECUTION=confirmed`, `SKILL_TOKEN=REMOTE-SKILL-CONTENT-7`, and
  `SUBAGENT_TOKEN=NATIVE-SUBAGENT-TOKEN-9` file. Detached Review requested
  deliberate finding `controlled-status`; Remediate resolved it; a distinct
  re-review approved `VERIFY_FINAL_OK`. Two malformed DeepSeek patch calls
  failed as durable retryable Execute attempts before attempt three succeeded.
  Terminal attach replay and the fixed host-visible artifact set both passed.
- Recovery job `eaa6db83-1a5e-4fe8-b41a-b128c1ad5af6` lost
  `codex-exec-server` during its blocked remote command. The authoritative
  disconnect watch expired after 30 seconds, emitted the visible turn and stage
  errors, scheduled the next durable attempt after five seconds, observed the
  reconnected environment, and completed the same job. This job is recovery
  evidence only; the later job above is the stage-separation gate.
- Cancellation job `d2f8c849-3d80-4709-a3e7-6b58cd047304` received
  `factory stop` while the remote probe was blocked. Its running Execute
  attempt became abandoned with `jobCancelled`, the remaining operations were
  cancelled, no probe process remained in the execution container, and the
  managed worktree was clean at terminal `cancelled`.
- These remote jobs emitted no `context.compacted` event and are not presented
  as a new compaction or Qdrant-memory gate. Those mechanisms remain in the
  unchanged worker-side Codex and Factory extension paths and retain their
  separately recorded native real-model evidence above.

Recorded on 2026-08-07 (per-job Docker environment binding):

- The runtime provisioner and deterministic Docker-spec regressions pass, as
  do the complete runtime suite, warning-denied runtime clippy, all-target
  runtime check, Compose rendering, entrypoint syntax, and diff whitespace
  validation. The fake provisioner received the exact durable environment
  identity rather than a runtime-generated alias.
- A live Bollard 0.21 test ran from the existing isolated worker network and
  ensured the same environment generation twice. Both calls returned one
  unchanged container ID and URL. Direct Docker inspection confirmed exact
  worker image
  `sha256:086337e6ed373c402198ab4f3f109cb98866032c6327d23f1499aa823ac10f20`,
  command `codex exec-server --listen ws://0.0.0.0:4500`, numeric worker user,
  one discovered `/workspaces` volume, and one discovered worker network. The
  container had neither the Docker socket nor provider credentials and was
  removed after the test. The same live ensure/reuse test passed again after
  exact stale-container validation was added. A separate entrypoint probe
  dropped to UID/GID 1000
  while retaining only the Docker socket's live group and successfully reached
  the Engine API.
- Phase 4's live Bollard lifecycle gate additionally stopped and restarted an
  active container with the same ID, removed and recreated it with a new ID,
  released it, and repeated release successfully after Docker returned 404. A
  deterministic-name container with a mismatched Factory job label produced a
  fatal release error and remained running until test cleanup. PostgreSQL gates
  prove success/failure/queued cancellation release intents, cleanup-before-
  release-before-ack running cancellation, graceful shutdown retention,
  continuation refusal while releasing, and restart reconciliation that keeps a
  failed release durable while completing other rows and succeeds on retry.

Recorded on 2026-08-07 (per-job real-model lifecycle):

- Fresh image
  `sha256:88ebd2543a7272b725a1b9e9682a7c25530296f0a105d6554c427a086a560781`
  ran in isolated fixture `/tmp/factory-perjob-acceptance.p5`, Compose project
  `sfperjobp5`. `docker compose config --services` contained PostgreSQL,
  Qdrant, `factoryd`, and `factory-worker`; no static `codex-exec-server`
  service was rendered.
- Concurrent jobs `c71292b1-cba6-447d-b711-0e03e2b709b0` and
  `a2b87743-406c-42c3-b18b-a74975291dfe` ran with distinct environment IDs
  `4ebdc095-a51d-4102-aac8-3bccd7f82072` and
  `bc3534be-8f1e-4a29-9d2c-9589709e991d`, distinct generation-1 containers,
  and exact job worktree cwd selections. Direct inspection found only the
  `codex exec-server --listen ws://0.0.0.0:4500` command, numeric user, one
  writable `/workspaces` mount, and one worker network.
- Job `c71292b1-cba6-447d-b711-0e03e2b709b0` succeeded through Plan, Execute,
  detached Review, `controlled-status` remediation, and independent re-review.
  Durable events contained native subagent activity, repository skill use,
  command and `apply_patch` activity, four accepted stage completions, and
  final `VERIFY_FINAL_OK`. Probe hostnames for Execute, Review, and Remediate
  matched container `31821a92ab3b...`. Terminal release persisted
  `released/released`, removed the container, and terminal status rebuilt all
  eight host-visible `.factory/jobs/<job-id>/` files.
- Recovery job `fb3e65de-1957-48d7-a17e-4d28db8902ad` used environment
  `351e3b9d-74d4-44a9-97aa-6198033a6c21`, generation 1. Its first Execute
  attempt ran the blocked probe in container `a4941b88657c...`. After exact
  kill and removal, events recorded `environment.disconnected`, the 30-second
  `turn.error`, and `stage.error`; attempt
  `810f9dda-f795-46af-93ac-e591cef32662` failed with
  `stageExecutionRetry`. Attempt `35e1cbdc-fd16-4a48-a605-719cd758e01b`
  carried `retryScheduled`, recreated the same environment and generation as
  container `133f9d692eae...`, and succeeded. The matching probe hostname,
  terminal job success, `released/released` row, absent container, attach
  result, and host artifacts all passed.
- Candidate recovery job `a2b87743-406c-42c3-b18b-a74975291dfe` is not used
  as disconnect-causality evidence: it emitted the disconnect and recreated
  the same environment generation, but DeepSeek exhausted all three Execute
  attempts by repeating malformed `apply_patch` syntax. The bounded failure was
  retained rather than hidden or retried indefinitely.
- At this Phase 5 point, Codex selected an exact job cwd and workspace root,
  but each per-job container still mounted the shared `/workspaces` backing
  volume. Mount-level per-job isolation remained a later improvement. These runs emitted
  no `context.compacted` event and are not a new compaction or Qdrant-memory
  gate.

Recorded on 2026-08-10 (multi-node Kubernetes execution):

- Revision `99eb4da06609d5a76dc77b5ae72ee5f2eff491de` ran the immutable
  multi-architecture image
  `ghcr.io/fpolica91/software-factory@sha256:73183da88ee6c82c4a08931423b3734534a9f7a8ed1bf610b5577112b226aca7`.
  Real runc jobs completed on both ARM64 and AMD64 nodes through the same
  explicit `ReadWriteMany` workspace, proving cross-node planning, tool use,
  patches, detached review, and terminal environment release.
- x86_64 (AMD64) Job C, `d20d837b-f69e-47b7-8364-e2be8ae7e3ab`, supplied the
  separate recovery and remediation evidence. Killing the worker during its
  initial Review left the job running. Review attempt
  `90076f05-a602-4826-aa6e-c10c92f73b18` started at lease epoch 1, was
  reclaimed with `recovery_cause=lease_expired`, and completed at epoch 3 after
  resuming itself from `factory.stage` checkpoint
  `bc5a9e4a-56b6-489c-8a34-93c65af61e47` (sequence 3).
- The remediation test was separate from that worker crash. A defect was
  deliberately injected while no operation was active and before round-3
  Iterate established its baseline. Iterate preserved and audited the defect;
  Review attempt `3d97e00f-bcca-4454-98f8-2e7ec4cef0b0` returned two real
  `request_changes` findings. Remediation attempt
  `f9da0884-4d9e-4fdf-93aa-272662623c05` fixed the defect, passed all 20 fixture
  tests, resolved both findings, and completed a fresh independent re-review.
  This proves detection and remediation of a pre-baseline defect, not handling
  of corruption introduced during a model turn.
- The isolated control-plane fixture was Compose project `sfmultinodeaccept`.
  Its execution resources were namespace `software-factory-execution`, PV
  `software-factory-workspaces-rwx`, PVC `software-factory-workspaces`, host
  mount `/srv/software-factory-rwx`, and NFS export
  `/data0/software-factory-rwx`. These acceptance resources remain live, so
  cleanup is pending rather than completed. The planned safe order is to
  release any fixture jobs and Pods, stop `sfmultinodeaccept`, delete the PVC
  and namespace, delete the retained PV, unmount and remove the client mount,
  and remove the NFS export. Delete the server backing directory only when its
  retained data is explicitly no longer wanted. Preserve the K3s cluster and
  optional Kata installation.
- K3s v1.36 required the PVC access-mode JSONPath iterator to change from
  `{.}` to `{@}`; the former returned an empty access-mode field and falsely
  rejected a valid RWX claim. The launcher now uses `{@}`, and a focused
  regression proves the corrected preflight accepts the live response while
  the former expression fails.
