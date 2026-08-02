# ADR 0001: Codex Kernel with a Durable Factory Lifecycle

- Status: Accepted
- Date: 2026-07-31
- Updated: 2026-08-02

## Context

Software Factory needs durable jobs, checkpoints, retries, recovery, managed
worktrees, long-term memory, and repeatable review/remediation. Codex already
provides a complete model and tool harness. Rebuilding that loop in a workflow
engine, generated client, or provider-specific CLI would create two competing
harnesses and make both behavior and maintenance harder to reason about.

## Decision

Use **one distribution, two lifecycles, no duplicated harness**.

Codex is the execution kernel. It retains its native planning and agent loop,
tools, threads, persistence, resume/fork, context compaction, approvals, goals,
skills, MCP, extensions, and subagent primitives. Factory behavior participates
through native extension APIs rather than wrapping Codex in another agent loop.

`factoryd` is the separate durable lifecycle. It stores jobs, operations,
attempts, leases, checkpoints, retries, runtime correlations, event history,
and managed-worktree state in PostgreSQL. `factory-worker` claims work directly
from that coordinator and drives the native Codex session through plan,
execute, detached review, remediation, and approving re-review. The two
lifecycles ship together but recover independently.

## Repository Boundaries

Preserve `factory-harness/codex-rs/` as an upstream-shaped Codex tree. All
Factory-owned application code is Rust under `factory-harness/factory/`:

| Path | Responsibility |
| --- | --- |
| `cli/` | Interactive and non-interactive run, attach, status, and stop commands |
| `coordinator/` | `factoryd`, PostgreSQL state, recovery, events, and worktrees |
| `extension/` | Memory/context, decomposition, progress, and review behavior |
| `providers/` | Canonical provider profiles and optional transport adapter |
| `runtime/` | Codex configuration, sessions, stages, checkpoints, and worker |

Dependencies point from Factory into public Codex APIs; Codex core does not
depend on Factory. A genuine missing runtime primitive may justify a minimal
upstream seam, but it must be recorded in `UPSTREAM.md` and `RUST_CUTOVER.md`.
The current recorded seam carries detached-review lineage into native
extension state.

## Execution and Identity

Rust hosts use Codex's in-process app-server lifecycle and exact upstream
request, response, notification, and server-request types. Factory does not
version, mirror, generate, hash, or negotiate a second wire protocol.

Codex `threadId`, `turnId`, and `itemId` identify agent execution. Factory
`jobId`, `operationId`, `attemptId`, checkpoint, lease, and workspace records
remain coordinator-owned domain state. Persisted correlations join these
lifecycles without moving durable scheduling into Codex core.

Each job receives a coordinator-managed Git worktree. Runtime gives that path
to Codex for tool execution; it does not provision a second workspace. Durable
events allow the CLI to detach and replay progress without controlling the
agent loop.

## Infrastructure

The baseline deployment is PostgreSQL, Qdrant, `factoryd`, and
`factory-worker`. PostgreSQL stores lifecycle state; Qdrant is the current
long-term-memory/RAG index. Hatchet and the former TypeScript client/workflows
are not part of the application.

Provider bridges are selected profiles. Redis coordination, MinIO artifacts,
Ollama local models, and Langfuse/ClickHouse observability are optional Compose
profiles, not baseline requirements. Codex continues to own context compaction;
Redis does not.

## Providers

OpenAI Responses uses Codex's direct provider boundary. Anthropic Messages and
DeepSeek/Z.AI Chat Completions use the Rust `factory-provider-bridge` for
wire-format translation only. The bridge does not plan, call tools, manage
threads, or become another harness. Custom Responses-compatible providers may
use the direct path.

Unintended analytics, feedback, OTel, and log exporters are disabled by
default. Model traffic and explicitly enabled operator observability are not
treated as telemetry export.

## Consequences

- Codex remains independently updateable because its structure and provenance
  are preserved.
- Factory durability can evolve without forking the agent loop.
- Provider translation remains replaceable transport plumbing.
- Maintainers have one Factory implementation language and no generated client
  or versioned compatibility surface.
- Compilation alone does not complete the migration. Real-model acceptance has
  passed plan-to-re-review, native subagent delegation, detach/attach replay,
  cross-job Qdrant memory, and automatic token-triggered compaction that
  continued the same native Codex turn through later tool calls and approval.
  It also passed the combined recovery-and-final-correctness gate: an execute
  turn was killed after a durable in-flight checkpoint and tool activity, the
  expired lease was reclaimed on the same attempt, execution continued on the
  same parent thread, exact output verification passed, and a distinct detached
  review approved it.
