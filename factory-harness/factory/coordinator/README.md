# Factory Coordinator

`factory-coordinator` is the durable lifecycle behind the `factoryd` binary.
It is separate from the Codex process and stores jobs, operations, leased
attempts, immutable runtime correlations, append-only checkpoints, and
Factory-owned Codex thread state in PostgreSQL. It does not run a scheduler or
an agent loop; Hatchet owns workflow ordering and Codex owns execution.

Recovery selection treats three operation states as eligible: a new ready
operation, a scheduled retry whose time has arrived, or a running attempt
whose lease expired. A claim locks only the selected operation with
`FOR UPDATE SKIP LOCKED`, abandons an expired attempt, and creates the next
attempt with a durable link to the most recent checkpoint and its Factory
Protocol correlation.

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
| `POST` | `/v1/jobs` | `201` with the durable job and ordered operations |
| `GET` | `/v1/jobs/{jobId}` | `200` with the durable job |
| `PUT` | `/v1/jobs/{jobId}/workspace` | `200` with the materialized or reused worktree |
| `GET` | `/v1/jobs/{jobId}/workspace` | `200` with the durable workspace binding |
| `POST` | `/v1/jobs/{jobId}/workspace/revision` | `200` after refreshing the worktree HEAD |
| `DELETE` | `/v1/jobs/{jobId}/workspace` | `200` after explicit worktree removal |
| `POST` | `/v1/operations/{operationId}/claim` | `200` with a recovery lease, or `204` when ineligible |
| `POST` | `/v1/recoveries/claim` | `200` with a recovery lease, or `204` when none is eligible |
| `POST` | `/v1/correlations` | `201` with the immutable correlation record |
| `POST` | `/v1/checkpoints` | `201` with the checkpoint record |
| `POST` | `/v1/attempts/{attemptId}/renew` | `200` with the renewed attempt |
| `POST` | `/v1/attempts/{attemptId}/complete` | `204` |
| `POST` | `/v1/attempts/{attemptId}/fail` | `204` |
| `GET` | `/v1/threads/{threadId}/state` | `200` with the Factory thread-state record |
| `PUT` | `/v1/threads/{threadId}/state` | `200` with the inserted or updated record |

Errors have the stable envelope
`{"error":{"code":"...","message":"..."}}`.

### Durable workspaces

`factoryd` owns repository-neutral Git workspace materialization. Set
`FACTORY_WORKSPACE_ROOT` to the directory that holds shared mirrors and
per-job worktrees. Bind a job to a repository URL or local path once:

```json
{"repository":"https://example.invalid/org/repository.git","baseRef":"main"}
```

`PUT /v1/jobs/{jobId}/workspace` clones or updates the shared bare mirror and
creates `factory/{jobId}` as the durable worktree branch. Repeating the request
or restarting `factoryd` returns the same root. Checkpoints bind that root and
revision to attempts; Codex only receives the selected root and remains the
owner of tool execution inside it. `DELETE` is the explicit cleanup boundary.

### Jobs and ordered claims

Create a job with the operations in Hatchet task order:

```json
{
  "kind": "softwareFactory.delivery",
  "input": { "issueId": "ENG-431" },
  "workflowRunId": "hatchet-run-123",
  "operations": [
    { "kind": "plan", "input": {}, "maxAttempts": 3 },
    { "kind": "execute", "input": {}, "maxAttempts": 3 },
    { "kind": "review", "input": {}, "maxAttempts": 2 }
  ]
}
```

A non-null `workflowRunId` is the idempotency key for job creation. Repeating
the same definition returns the existing durable job; reusing that key with a
different kind, input, operation order, or attempt limit returns HTTP `409`.
Omit the field when every create call must produce a new job.

Hatchet tasks should claim the exact `operationId` returned by job creation:

```json
{"ownerInstanceId":"factory-worker-1","leaseSeconds":300}
```

The database rejects an exact or generic claim until every lower-ordinal
operation in that job has succeeded. This preserves `plan -> execute -> review`
even if a generic recovery worker polls the job while an earlier stage has a
live lease. The generic endpoint accepts the same fields plus an optional
`jobId`; omit `jobId` only for a recovery worker that may claim across jobs.

A successful claim returns a `selection` and a newly running `attempt`.
`selection.cause` is `newOperation`, `retryScheduled`, or `leaseExpired`.
`selection.resume` is either `{"kind":"fresh"}` or:

```json
{
  "kind": "fromCheckpoint",
  "checkpoint": {
    "checkpointId": "...",
    "attemptId": "...",
    "sequence": 1,
    "kind": "stage-complete",
    "payload": {},
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

### Correlations, checkpoints, and attempts

Append the complete runtime correlation after Codex creates the request or
thread identifiers:

```json
{
  "jobId": "...",
  "operationId": "...",
  "attemptId": "...",
  "workflowRunId": "...",
  "taskRunExternalId": "...",
  "requestId": "...",
  "threadId": "...",
  "turnId": "...",
  "itemId": "..."
}
```

Append progress to the running attempt with `POST /v1/checkpoints`:

```json
{
  "attemptId": "...",
  "kind": "turn-progress",
  "payload": { "stage": "execute" },
  "workspaceRoot": "/workspace/repository",
  "workspaceRevision": "git-revision",
  "correlationId": "..."
}
```

Renew a lease with the same `ownerInstanceId` that claimed it:

```json
{"ownerInstanceId":"factory-worker-1","leaseSeconds":300}
```

Complete takes no body. Failure is either retryable at an explicit time or
terminal:

```json
{"disposition":"retryAt","retryAt":"2026-08-01T10:00:00Z","detail":{}}
```

```json
{"disposition":"terminal","detail":{}}
```

### Factory thread state

The native extension can rehydrate its contributors independently of Codex
compaction or process lifetime. `PUT /v1/threads/{threadId}/state` accepts:

```json
{
  "decomposition": {},
  "progress": {},
  "review": {},
  "remediation": {},
  "subagents": {}
}
```

Every field is optional opaque JSON. The returned record wraps the document in
`state` and includes `threadId`, monotonically increasing `revision`,
`createdAt`, and `updatedAt`.

## Functional HTTP recovery acceptance

Run the repository acceptance directly:

```sh
bash coordinator/acceptance/http-recovery.sh
```

The script starts disposable PostgreSQL and two fresh `factoryd serve`
processes. It proves predecessor ordering for both claim routes, lease renewal,
correlation and checkpoint persistence, process-restart lease recovery,
cross-stage checkpoint handoff, thread-state rehydration and revision,
completion, terminal failure, and scheduled retry. It emits one compact JSON
receipt when all checks pass.

Workspace crash/reuse behavior has a separate functional acceptance:

```sh
bash coordinator/acceptance/workspace-recovery.sh
```

It creates a local Git source, materializes a job worktree, restarts all of
`factoryd`, proves the same binding and revision are reused, then exercises the
explicit cleanup route.

## Store-level recovery acceptance

Use a disposable PostgreSQL database. The two commands intentionally run as
separate processes; the second process has no in-memory state from the first.

```sh
cargo build --locked -p factory-coordinator --bin factoryd

WRITE_RECEIPT="$(target/debug/factoryd \
  --database-url "$FACTORY_DATABASE_URL" acceptance-write)"
JOB_ID="$(printf '%s' "$WRITE_RECEIPT" | jq -r .jobId)"

target/debug/factoryd \
  --database-url "$FACTORY_DATABASE_URL" \
  acceptance-recover --job-id "$JOB_ID"
```

`acceptance-write` persists an immediately expired lease, a complete
job/operation/attempt to request/thread/turn/item correlation, and a bound
checkpoint, then closes its pool and exits. `acceptance-recover` reconnects,
loads the job and checkpoint, proves resume eligibility, atomically creates a
checkpoint-linked attempt, marks the expired attempt abandoned, and completes
the job.
