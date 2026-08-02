# Factory Coordinator

`factory-coordinator` is the durable lifecycle behind the `factoryd` binary.
It is separate from the Codex process and stores jobs, operations, leased
attempts, immutable runtime correlations, append-only checkpoints, and
an append-only job event stream, plus Factory-owned Codex thread state in
PostgreSQL. `factory-worker` runs `DurableRunner`, which polls and claims
eligible operations, renews leases, and settles their attempts. Codex remains
the only agent loop and owns execution inside the selected worktree.

Recovery selection treats three operation states as eligible: a new ready
operation, a scheduled retry whose time has arrived, or a running attempt
whose lease expired. A claim locks only the selected operation with
`FOR UPDATE SKIP LOCKED`. New and scheduled-retry work creates the next attempt
with a durable checkpoint link. Expired-lease recovery instead transfers the
same running attempt under a new lease epoch, preserving its attempt budget.

Cancelling a queued job with no live attempt is immediate. Cancelling a running
job records `cancelling` without invalidating its fence. The worker polls this
control state independently of its lease heartbeat, interrupts and drains
Codex, rolls back disposable Plan or detached-review state, then atomically
acknowledges `cancelled`. Graceful worker shutdown uses the same drain and
cleanup path and explicitly expires the retained attempt lease, so a restart
can reclaim it immediately even when the production lease is 900 seconds.

## JSON server

`serve` applies pending coordinator migrations, prints the bound address as a
single JSON line, and serves the API until it receives an interrupt:

```sh
target/debug/factoryd \
  --database-url "$FACTORY_DATABASE_URL" \
  serve --bind 0.0.0.0:8787
```

All request and response fields use `camelCase`.

| Method | Path | Success |
| --- | --- | --- |
| `GET` | `/healthz` | `200 {"status":"ok"}` |
| `POST` | `/jobs` | `201` with the durable job and ordered operations |
| `GET` | `/jobs/active` | `200` with active jobs |
| `GET` | `/jobs/{jobId}` | `200` with the durable job |
| `POST` | `/jobs/{jobId}/cancel` | `200` with the cancelled job or live cancellation request |
| `GET` | `/jobs/{jobId}/attempts` | `200` with the job's attempts |
| `GET` | `/jobs/{jobId}/events?after={cursor}&limit={limit}` | `200` with the next ordered event page |
| `GET` | `/jobs/{jobId}/result` | `200` with a standard binary Git patch and result metadata headers |
| `GET` | `/jobs/{jobId}/stage-checkpoints` | `200` with completed-stage checkpoints |
| `PUT` | `/jobs/{jobId}/workspace` | `200` with the materialized or reused worktree |
| `GET` | `/jobs/{jobId}/workspace` | `200` with the durable workspace binding |
| `POST` | `/jobs/{jobId}/workspace/revision` | `200` after refreshing the worktree HEAD |
| `DELETE` | `/jobs/{jobId}/workspace` | `200` after explicit worktree removal |
| `POST` | `/attempts/{attemptId}/events` | `201` with the fenced append-only event record |
| `GET` | `/threads/{threadId}/state` | `200` with the Factory thread-state record |
| `PUT` | `/threads/{threadId}/state` | `200` with the inserted or updated record |

Errors have the stable envelope
`{"error":{"code":"...","message":"..."}}`.

### Job events

Job events provide durable CLI attach and log replay. Each record contains a
globally monotonic `sequence`, `jobId`, optional `operationId` and `attemptId`,
an event `kind`, opaque JSON `payload`, and `createdAt`. Poll
`GET /jobs/{jobId}/events?after=0`; pass the returned `nextCursor` as `after`
on the next request. Results are ordered by `sequence`; the default page size
is 200 and the maximum is 1000. An empty page preserves the supplied cursor.

Coordinator lifecycle code appends job events with `append_job_event`.
Execution code must use `append_attempt_event`, which derives job and operation
identity from the attempt and rejects writes from expired or superseded lease
owners. Attempt events may include a stable `deduplicationKey`. Replaying the
same key and content returns the original event; reusing a key for different
content is rejected. Keys are scoped to the job so an activity remains
idempotent when recovery creates a new attempt.

The native extension archives each complete `factory.subagent.activity`
snapshot through `/attempts/{attemptId}/events` before bounding its current
thread-state projection. The event payload retains call, turn, sender, receiver,
prompt, status, and child-result fields. Thread state records the latest event
sequence, so omitted projection entries remain traceable in this stream. The
extension derives a deterministic key from each timestamp-free snapshot and
performs a one-time archive of legacy unbounded state before pruning it.

### Durable workspaces

`factoryd` owns repository-neutral Git workspace materialization. Repository
identity is distinct from the clone transport, so multiple host repositories
mounted at `/workspace/project` cannot share a cache or job binding. Set
`FACTORY_WORKSPACE_ROOT` to the directory that holds shared bare caches and
per-job worktrees. Bind a job to a repository URL or local path once:

```json
{
  "repositoryId":"remote:0123456789abcdef...",
  "repository":"https://example.invalid/org/repository.git",
  "baseRef":"main"
}
```

`PUT /jobs/{jobId}/workspace` initializes or updates one shared bare cache and
creates `factory/{jobId}` as the durable worktree branch. Remote branches live
only under `refs/remotes/origin/*`; pruning them cannot delete Factory's local
job branches. Legacy mirror refspecs are migrated before refresh. The resolved
commit is retained as immutable `baseRevision` for safe result application. If
an existing worktree must be recreated after its branch moves, Factory uses
that recorded commit rather than resolving `baseRef` again. Checkpoints bind
the root and revision to attempts; Codex only receives the selected root and
remains the owner of tool execution inside it.

PostgreSQL advisory locks serialize each job worktree across worker and
`factoryd` processes and serialize shared-cache publication. They use a pool
separate from durable query/control traffic. `factory-worker` accepts 1-32
slots and reserves one lock connection per slot plus two for lifecycle work;
heartbeats, events, correlations, and checkpoints retain eight independent
query connections. `DELETE` is the explicit cleanup boundary.

After the job succeeds, `GET /jobs/{jobId}/result` compares the complete
worktree to `baseRevision` through a temporary Git index and returns
`application/vnd.git.patch`. It includes additions, modifications, deletions,
binary data, symlinks, and mode changes without changing the managed index.
Response headers carry `x-factory-repository-id`,
`x-factory-base-revision`, and `x-factory-patch-sha256`. The CLI verifies all
three before applying to a clean matching host checkout.

### Jobs and ordered claims

The native `factory` CLI creates `factory.task` jobs in the four-stage order
owned by the Rust worker:

```json
{
  "kind": "factory.task",
  "input": {
    "task": "implement authentication",
    "executionProfile": { "provider": "deepseek", "model": "deepseek-v4-pro" },
    "repositoryId": "local:0123456789abcdef..."
  },
  "operations": [
    { "kind": "codex.plan", "input": {}, "maxAttempts": 3 },
    { "kind": "codex.execute", "input": {}, "maxAttempts": 3 },
    { "kind": "codex.review", "input": {}, "maxAttempts": 3 },
    { "kind": "codex.remediate", "input": {}, "maxAttempts": 3 }
  ]
}
```

Each create request produces one new durable job. The native CLI submits each
user request once and then addresses the job by its returned `jobId`; there is
no external workflow identity or unused compatibility idempotency field.

`factory-worker` claims eligible work directly through the in-process
`DurableRunner`. The database rejects a claim until every lower-ordinal
operation in that job has succeeded. A `factory.task` job is also ineligible
until its durable workspace record exists with status `active`, so creating a
job before materializing its worktree cannot race a polling worker. This
preserves `plan -> execute -> review` even while an earlier stage has a live
lease. Factory tasks additionally require an exact provider/model match with
the worker claim capability. Jobs created before profile pinning and jobs for a
different profile remain ready and consume no attempts until a matching worker
is started. Changing the launcher's active provider or model therefore affects
new jobs without silently changing an existing job's recovery behavior.

Internally, a successful claim returns a `selection`, an `attempt`, and its exact `fence`.
`selection.cause` is `newOperation`, `retryScheduled`, or `leaseExpired`.
Fresh and scheduled-retry claims create the next attempt. An expired-lease
claim transfers the existing running attempt to the new owner and increments
its `leaseEpoch`; it does not create another attempt or consume the attempt
budget. Every later mutation must copy both `ownerInstanceId` and `leaseEpoch`
from the returned `fence`. The owner name alone is not a valid lease handle.
`selection.resume` is either `{"kind":"fresh"}` or:

```json
{
  "kind": "fromCheckpoint",
  "checkpoint": {
    "checkpointId": "...",
    "attemptId": "...",
    "sequence": 1,
    "kind": "factory.stage",
    "payload": {
      "operation": "execute",
      "parentExecutionThreadId": "...",
      "activeThreadId": "...",
      "turnId": "...",
      "phase": "completed",
      "turnRole": "stage",
      "reviewCycle": 0,
      "stateRevisionBaseline": 1
    },
    "workspaceRoot": null,
    "workspaceRevision": "git-revision",
    "correlationId": "...",
    "createdAt": "..."
  }
}
```

Checkpoint selection prefers the current operation's newest checkpoint. If it
has none, it uses the newest checkpoint from the closest earlier operation in
the same job. The latter is the durable plan-to-execute handoff. When that
checkpoint has a correlation, `selection.checkpointCorrelation` carries its
request, Codex thread, turn, and item identifiers.

### Factory thread state

The native extension can rehydrate its contributors independently of Codex
compaction or process lifetime. `PUT /threads/{threadId}/state` is fenced by
the live attempt and accepts the single extension-owned state document:

```json
{
  "attemptId": "...",
  "ownerInstanceId": "factory-worker-1",
  "leaseEpoch": 1,
  "state": {
    "decomposition": { "revision": 1, "work_units": [] },
    "progress": { "work_units": [] },
    "remediation": { "records": [] },
    "reviewHistory": { "cycles": [] },
    "subagents": {
      "activities": [],
      "history": {
        "source": "coordinator_job_events",
        "event_kind": "factory.subagent.activity",
        "latest_sequence": 42
      }
    }
  }
}
```

The coordinator treats contributor fields as opaque JSON. The returned record
wraps the document in `state` and includes `threadId`, monotonically increasing
`revision`, `createdAt`, and `updatedAt`.

## Functional acceptance

Workspace crash/reuse behavior has a separate functional acceptance:

```sh
bash coordinator/acceptance/workspace-recovery.sh
```

It creates a local Git source, materializes a job worktree, restarts all of
`factoryd`, proves the same binding and revision are reused, then exercises the
explicit cleanup route.

The durable runner has a feature-gated fixture acceptance:

```sh
bash coordinator/acceptance/runner-recovery.sh
```

It proves checkpointed success, atomic completion events, Plan-validation and
review-task-panic business retries, cancellation request/acknowledgement,
graceful lease relinquishment at a 900-second lease, lease heartbeats and
fencing, process-kill recovery at a one-attempt budget, and worker-slot
isolation against disposable PostgreSQL. Its two-worker fault case expires the
old lease without killing that process and proves both the replacement runtime
and a separate workspace lifecycle request wait until the old runtime drains.

Focused functional gates cover the product boundary directly:

```sh
cargo test -p factory-coordinator repository_identity_keeps_fixed_locator_mirrors_separate
cargo test -p factory-coordinator remote_refresh_preserves_factory_branches_and_active_worktrees
cargo test -p factory-coordinator result_patch_applies_text_binary_deletion_untracked_and_mode_changes
cargo test -p factory-cli result_apply_refuses_every_conflict_before_mutating_host
FACTORY_COORDINATOR_TEST_DATABASE_URL=postgres://... \
  cargo test -p factory-coordinator --test workspace_claim_gate -- --ignored
FACTORY_COORDINATOR_TEST_DATABASE_URL=postgres://... \
  cargo test -p factory-coordinator --test workspace_integrity -- --ignored
FACTORY_COORDINATOR_TEST_DATABASE_URL=postgres://... \
  cargo test -p factory-coordinator --test pool_capacity -- --ignored
```

Together they exercise fixed-locator repository switching, concurrent
same-repository worktrees, immutable-base rematerialization, binary result
delivery, refusal without host mutation, exact provider/model claims, and
maximum worker lock capacity against PostgreSQL. They are behavior checks, not
a substitute for the real-model workflow and crash-recovery acceptance recorded
in the root cutover ledger.
