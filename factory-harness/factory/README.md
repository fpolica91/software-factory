# Factory Runtime

The default Factory product path is the complete Codex app-server lifecycle.
It retains Codex planning and the agent loop, tools, approvals, threads,
persistence and resume/fork, compaction, goals, skills, MCP, memories, plugins,
extensions, and subagents.

## Crates

- `runtime/` exposes the upstream typed in-process client with Factory's native
  extension installed; its durable worker runs the product stage loop.
- `extension/` owns Factory's native Codex lifecycle contributors and
  Factory-scoped thread state.
- `providers/` owns the native Rust model-provider transport adapters and the
  canonical provider/model profiles used by the runtime and CLI.
- `coordinator/` owns the separate `factoryd` durable lifecycle. PostgreSQL
  stores jobs, operations, leased attempts, immutable runtime correlations,
  and append-only checkpoints. Scheduled retries create a new attempt linked
  to the selected checkpoint; expired-lease recovery transfers the same
  attempt under a higher lease epoch without consuming its retry budget.
- `cli/` owns interactive onboarding plus `run`, `attach`, `status`, and `stop`.

The workspace root is virtual; only these five product crates are members.

Factory depends on public Codex APIs. Codex core does not depend on Factory.

## Build and run

```sh
just build
target/debug/factory --help
```

`just build` produces the four distribution binaries: `factory`,
`factory-worker`, `factoryd`, and `factory-provider-bridge`. The root Dockerfile
copies exactly those binaries into the runtime image.

`coordinator/README.md` documents the fixture-runner recovery gate. Against
disposable PostgreSQL and `factoryd`, it restarts the acceptance runner to
prove stored retries, checkpointed completion, lease heartbeats and fencing,
process-kill recovery, cooperative shutdown, and worker-slot isolation.

Factory does not define or negotiate a second wire protocol. Separate-process
clients speak the exact upstream app-server surface; Rust hosts use the exact
upstream typed client. Durable job, operation, attempt, checkpoint, and runtime
correlation records belong to `factory-coordinator` and never become Codex wire
types.

The worker sets Codex analytics off in the Rust startup configuration and skips
remote plugin warmup. Local plugins, skills, MCP, model traffic, tools, and all
thread behavior remain available.

## In-process worker

The worker calls `factory_runtime::in_process::start_with_backend` with an
`InProcessClientStartArgs` and a fenced `FactorydStateBackend`. The namespace
also exposes
`InProcessAppServerClient`, `InProcessServerEvent`, and the upstream default
channel capacity. These retain the upstream typed app-server lifecycle,
including its initialize handshake, requests, events, server-request
resolution, and graceful shutdown, while installing Factory's native
contributors through the generic Codex composition seam. There is no shipped
standalone or process-memory runtime path.

The initial Factory contributor establishes Factory-owned state through the
native Codex thread lifecycle. Memory/context, decomposition, progress,
review/remediation, and Factory subagent contributors extend that boundary;
they do not create a second agent loop.
