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
per-step host-tool removal used by Factory Plan. Their hand-edited files are:

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
  `workflows/`, or `integrations/` artifacts.
- Removed Hatchet from the default Compose stack and removed its token
  bootstrap from the host launcher. PostgreSQL, Qdrant, `factoryd`, and the
  Rust durable worker are now the baseline services.
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
  surface was added. `coordinator/README.md` documents `factory-worker` and
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
  owner can acknowledge terminal `cancelled`. Queued jobs without a live
  attempt still cancel immediately. A slow or transiently failed job-state
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
  production durable worker binary (`src/bin/factory_worker.rs`). The
  four-stage executor has one unversioned module tree: `src/executor/mod.rs`
  owns its public lifecycle, `src/executor/task.rs` owns task/config validation,
  `src/executor/stage_loop.rs` owns stage and remediation turns, and
  `src/executor/resume.rs` owns checkpoints, correlations, and recovery. The
  former flat `src/executor.rs` is deleted. Runtime event work is confined to
  `src/events.rs`, its `src/lib.rs` module export, notification forwarding in
  `src/session.rs`, and stage lifecycle calls in `src/executor/stage_loop.rs`.
  The sink stores only exact active-turn events, coalesces text streams to
  roughly 1 KiB, and never stores completed command output a second time.
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
  down, then the managed worktree is recreated at the recorded revision. The
  same rollback path covers cancellation, provider/runtime failure, and
  shutdown failure, while replacement Plan preflight repeats cleanup after a
  process crash. Detached review starts in a fresh Codex thread without copying
  the parent conversation, while retaining Codex review source metadata and the
  typed parent thread, parent turn, and durable-state attachment used by
  Factory lineage. It captures a durable semantic Git snapshot of
  tracked, staged, and nonignored untracked content. Any review mutation is
  restored and rejected. A mutation-detected marker is written before restore
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
  printed before the result. Provider/model configuration also starts only
  PostgreSQL plus `factoryd` for its active-job check.
- `factory-harness/factory/providers/`: native Rust transport adapter and
  canonical provider profiles. OpenAI Responses is direct; Anthropic Messages
  and DeepSeek/Z.AI Chat Completions are translated into the Responses surface
  without adding another agent harness. `profiles.rs` and
  `CodexProviderSelection` compile without the adapter feature for lightweight
  CLI/runtime use; `responses.rs`, `tools.rs`, and `response_stream.rs` keep
  request parsing, tool mapping, and streaming output as separate concerns.
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

- `Dockerfile` builds and installs `factory`, `factory-worker`, `factoryd`, and
  the Rust `factory-provider-bridge`. It has no Node build stage, and no legacy
  client, integration, workflow, or provider script enters the image.
  `factoryd` and the provider bridge drain Axum gracefully on both SIGINT and
  the SIGTERM used by container shutdown.
- `docker-compose.yml` runs PostgreSQL, Qdrant, `factoryd`, and
  `factory-worker` by default. `factoryd` sees the selected host repository at
  `/workspace/project` so the native CLI can apply a verified completed result;
  the model worker still sees only coordinator-owned `/workspaces`. Hatchet and
  runtime Node commands are absent. Optional profiles remain optional; all
  provider adapters share the one selected-provider catalog volume, and
  provider health checks use `curl` instead of Node.
- The root `factory` launcher owns only Docker/bootstrap and host-file
  lifecycle. It delegates onboarding, hidden key input,
  provider/model switching, run, attach, status, stop, result apply, and result
  export to the Rust CLI. It derives a stable local repository identity from
  the canonical host Git root before crossing the fixed container mount. It
  reads the same Docker Compose-expanded provider, model, and key values used
  by the worker, and streams verified export bytes from container stdout into
  an atomic no-overwrite host file rather than passing host paths into `factoryd`.
  Controlled stack startup removes orphaned pre-cutover services, so deleted
  workflow-worker or Hatchet containers cannot survive an in-place upgrade.
  `logs` follows the active Rust services and `build` invokes the root Dockerfile
  directly.
- `apps/README.md` now documents the Rust ownership boundary.
- `apps/cli/factory-worker-entrypoint.sh` was audited during the switch and
  intentionally left unchanged; it is a generic UID/GID entrypoint used by
  the Rust services, not a workflow runtime.

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
