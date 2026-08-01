# Factory Workflows

This package is the durable orchestration layer between Hatchet, `factoryd`,
and the native Codex harness. It uses the pinned Hatchet TypeScript SDK 1.28.0
and `@software-factory/harness-client`; it does not implement a model or tool
loop.

`factory-job` loads one durable job, orders its operations by coordinator
ordinal, and exact-claims each operation before running it. Supported operation
kinds are `codex.plan`, `codex.execute`, `codex.review`, and
`codex.remediate`. Their exact Codex V2 mappings are:

- `codex.plan`: `turn/start` with experimental `collaborationMode.mode = "plan"`.
- `codex.execute` and `codex.remediate`: `turn/start` with
  `collaborationMode.mode = "default"` so resumed plan threads re-enter execution mode.
- `codex.review`: inline native `review/start` with a custom target built from
  the operation prompt.

All four stages resume the same Codex thread. Terminal state comes from exact
`turn/completed` notifications, and timeout cleanup uses exact
`turn/interrupt`.

Turn completion alone does not complete a stage. The workflow also reads the
durable native Factory state from `factoryd` and enforces these contracts:

- plan records a non-empty, dependency-valid decomposition;
- execute marks every current work unit completed;
- review records a verdict, summary, and findings tied to current units; and
- remediation covers every current finding exactly once when review requests
  changes (an approved review requires no remediation write).

Planning creates implementation/verification units only; review and remediation
remain independent Factory stages. After remediation, the workflow launches a
fresh native review and repeats remediation/re-review until approval, bounded by
`FACTORY_MAX_REVIEW_CYCLES` (default `5`). Every nested turn records its phase,
cycle, remediation-state baseline, and review generation so crash recovery
resumes the current phase. Each review records the exact native review-child
turn that called `factory_record_review` and verifies both its parent Factory
thread and parent `review/start` turn, so stale or unrelated review state cannot
satisfy a new review.

CLI-created jobs default to Codex `approvalPolicy = "never"` and
`sandbox = "danger-full-access"`. Any approval request is accepted, clarification
is answered with an empty response, and MCP elicitation is declined without
blocking. `attach` remains useful for progress and cancellation.

The runner injects the matching native-tool instruction for each stage. A
semantic miss writes a `<stage>.semantic-gate-failed` checkpoint and remains
retryable; integrations are notified only after the state gate passes.

Each real operation writes exact request/notification correlations plus thread-bound and completed
stage checkpoints to `factoryd`. Later stages must receive the prior checkpoint
and resume the same Codex thread, otherwise the task fails rather than silently
starting an unrelated lineage. Attempt completion, scheduled retries, terminal
failures, lease renewal, and crash recovery all remain coordinator-owned.

## Build and Run

```sh
npm ci
npm run build
```

Run the disposable exact-V2 plan-to-remediation acceptance with Docker and a
canonical Codex login from `${FACTORY_SEED_CODEX_HOME:-$HOME/.codex}`:

```sh
./scripts/full-v2-acceptance.sh
```

`FACTORY_PROVIDER_BASE_URL`, `FACTORY_MODEL_CATALOG_JSON`,
`FACTORY_RUNTIME_PATH`, and `FACTORYD_PATH` override its runtime dependencies.

Start the Hatchet worker with `FACTORYD_URL`, `HATCHET_CLIENT_TOKEN`, and
`FACTORY_RUNTIME_PATH` configured:

```sh
npm run worker
```

With the same Hatchet connection environment, dispatch an existing factoryd
job through the registered task and wait for its durable result:

```sh
npm run dispatch -- job_123
```

Schedule an existing durable job once at an ISO timestamp, or use it as the
immutable template for a named five-field cron. Each accepted cron tick copies
the template job kind, input, ordered operations, and attempt limits into a new
factoryd job before execution. Ticks for the same template do not overlap;
Hatchet cancels the newer tick while the prior one is still running.

```sh
npm run schedule -- at 2026-08-01T09:30:00Z job_123
npm run schedule -- cron nightly-factory '30 2 * * *' job_123
```

Hatchet retries either outer durable task after worker failure. Configure the
retry limit with `FACTORY_HATCHET_TASK_RETRIES` (default `20`); factoryd still
owns the operation attempt counts and recovery checkpoints. Cron cloning uses
the Hatchet workflow-run ID as factoryd's idempotency key, so replay returns the
same fresh job instead of creating another one.

`FACTORY_WORKFLOW_SLOTS` controls worker concurrency (default `4`).
`FACTORY_LEASE_SECONDS` controls each claimed operation's renewable factoryd
lease (default `900` seconds); use a shorter value only for crash-recovery
testing.

For direct GLM and PostgreSQL acceptance without a Hatchet server, create a job
through `POST /v1/jobs`, then run its returned ID:

```sh
FACTORYD_URL=http://127.0.0.1:8787/v1 \
FACTORY_RUNTIME_PATH=../factory-harness/factory/target/debug/factory-runtime \
npm run run -- job_123
```

Job-level input supplies shared defaults; each operation input overrides them.
Both must be neutral JSON. A stage requires `prompt`, and may provide `cwd`,
`model`, `modelProvider`, Codex `config`, `codexHome`, sandbox settings, or a
workspace revision. Deployment defaults are read from `FACTORY_RUNTIME_PATH`,
`FACTORY_CODEX_HOME`, `FACTORY_MODEL`, `FACTORY_MODEL_PROVIDER`,
`FACTORY_PROVIDER_BASE_URL`, and `FACTORY_MODEL_CATALOG_JSON`; job and operation
values remain authoritative.

A job may instead provide a durable repository workspace:

```json
{
  "workspace": {
    "repository": "https://github.com/example/project.git",
    "baseRef": "main"
  }
}
```

The worker asks `factoryd` to ensure that worktree once at job start, then uses
its root and refreshed Git revision for checkpoints. Operation-level `cwd`
explicitly overrides the managed root. The coordinator client also exposes
`ensureWorkspace`, `loadWorkspace`, `refreshWorkspaceRevision`, and
`removeWorkspace` for other neutral adapters.
