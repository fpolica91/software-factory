# ADR 0001: Codex Kernel, Factory Extensions, and Durable Coordinator

- Status: Accepted
- Date: 2026-07-31
- Scope: Software Factory V1

## Context

The existing Software Factory contains useful behavior for durable workflows,
checkpoints, review/remediation, worktrees, and memory, but some of it is tied
to Cursor, Boss/Hydra assumptions, and Linear/GitLab. Codex already supplies a
complete execution harness. Rebuilding or narrowing that harness would lose
proven capabilities and create two competing agent runtimes.

## Decision

Software Factory V1 uses **one distribution, two lifecycles, no duplicated harness**.

### Lifecycle 1: complete Codex execution kernel

Codex remains responsible for its real planning and agent loop, tools,
existing sandbox and approval behavior, threads, persistence/resume/fork,
context compaction, goals, skills, MCP, extensions, and subagent primitives.
Factory must not rebuild, bypass, or disable those capabilities.

Factory adds long-term memory/context, decomposition, progress,
review/remediation, and Factory-specific subagent behavior as native Codex
extensions. These features participate in the Codex loop; they do not form a
second harness.

### Lifecycle 2: `factoryd`

`factoryd` is a separate long-lived coordinator responsible for durable jobs,
checkpoints, retries, crash recovery, integrations, and scheduling. Hatchet is
the durable workflow engine. PostgreSQL stores durable workflow and Factory
state. `factoryd` drives the execution lifecycle through the stable Factory
protocol and never reimplements the Codex agent loop.

The runtime and `factoryd` are shipped as one distribution but start, stop,
retry, and recover independently.

## Repository and crate boundaries

Keep the full Codex fork at `factory-harness/`. Preserve `codex-rs/` exactly as
an upstream-shaped directory tree: do not rename, underscore, relocate, or
copy its internal directories. Preserve `factory-harness/UPSTREAM.md`, the
pinned upstream SHA, and the record of modified upstream files.

All downstream Rust code belongs under these crate/directory boundaries:

| Path | Responsibility | Allowed dependencies |
| --- | --- | --- |
| `factory/runtime/` | Composes the full Codex kernel with Factory extensions and exposes the app-server/in-process execution lifecycle | `extension`, `providers`, `protocol`, public Codex app-server/client APIs |
| `factory/extension/` | Memory/context, decomposition, progress, review/remediation, and Factory subagent extensions | `protocol`, public Codex extension APIs |
| `factory/coordinator/` | The `factoryd` PostgreSQL state, recovery, and workspace service | `protocol`; communicates with runtime only through durable identifiers and the stable client boundary |
| `factory/providers/` | Generic provider configuration and translation into Codex provider APIs | `protocol`, public Codex provider APIs |
| `factory/protocol/` | Stable Factory profile over typed app-server v2 messages plus durable correlation contracts | `codex-app-server-protocol` only; no dependency on other Factory crates |

Dependency direction is always **Factory -> public Codex APIs**. Codex core
must never depend on Factory. An upstream edit is permitted only for a genuine
runtime primitive that cannot be expressed through a public API; it must be
minimal and recorded in `UPSTREAM.md`.

Use Cargo `default-members`, focused CI paths, IDE exclusions, and CODEOWNERS
to keep ordinary work focused without altering upstream paths.

`factory/Cargo.toml` is a virtual workspace containing `factory-runtime`,
`factory-extension`, `factory-coordinator` (binary `factoryd`),
`factory-providers`, and `factory-protocol`. From `factory-harness/factory/`,
`just build` builds the distribution's runtime and coordinator lifecycles;
`cargo build -p factory-runtime` and
`cargo build -p factory-coordinator --bin factoryd` are the focused target
commands.

## Stable protocol and maintainer path

Factory Protocol V1 is the stable profile of the full Factory-enabled Codex
app-server v2 lifecycle. It uses the existing app-server patterns rather than
inventing another turn API:

- The separate-process baseline is stdio with one JSON message per line. The
  messages use JSON-RPC 2.0 request/response/notification semantics while
  omitting the `jsonrpc` header, exactly as app-server does.
- A connection initializes once with `initialize` followed by `initialized`,
  then uses `thread/start`, `thread/resume`, or `thread/fork`, `turn/start`,
  streamed `thread/*`, `turn/*`, and `item/*` notifications, responses to
  server requests such as approvals, and graceful shutdown.
- In-process Rust hosts use
  `codex_app_server_client::InProcessAppServerClient`; its typed request and
  event path reaches the same app-server handlers without JSON transport.
- JSON-RPC `RequestId` correlates each request and response. App-server
  `threadId`, `turnId`, and `itemId` correlate execution events. `factoryd`
  separately persists the existing `jobId`, `operationId`, `attemptId`, and
  optional Hatchet `workflowRunId`/`taskRunExternalId` mapping to those runtime
  identifiers; no durable job identity is smuggled into Codex core.
- The Factory protocol release pins the generated TypeScript/JSON app-server
  schema to the distribution's Codex SHA. Compatible additions retain the
  Factory protocol major; breaking changes require a new major and matching
  `harness-client`. The old custom `protocolVersion: 1` one-shot
  `TurnRequest`/`ServerEvent` is migration input, not the target entrypoint.

The profile remains independent of a model vendor, issue tracker, source host,
or workflow implementation.

Ordinary Software Factory maintainers work in:

```text
apps/ | workflows/ | integrations/ | docs/
                         -> harness-client/
                         -> stable Factory protocol
```

Inside the fork, `factory/ -> protocol -> tests` is the prescribed shorthand:
start in the relevant downstream Factory crate, cross boundaries through
`factory/protocol`, and prove behavior in that crate's functional acceptance
coverage. It is a navigation rule, not a literal directory chain. Maintainers
enter `codex-rs/` only to change a genuine runtime primitive.

## Migration

Treat the existing TypeScript worker and workflows as behavioral reference
material. Incrementally generalize and migrate their durable workflow,
checkpoint, review/remediation, worktree, and memory patterns behind the
stable protocol. Remove only:

- Cursor-backed execution;
- Boss/Hydra-specific assumptions; and
- Linear/GitLab product coupling.

The corresponding Factory capabilities remain. Integrations become
repository- and tracker-neutral adapters rather than being deleted.

`factory/coordinator` owns the durable workspace lifecycle: clone/worktree
creation, selection, reuse, job/checkpoint binding, and cleanup. It sends the
selected workspace root plus job context through the protocol. Runtime does
not provision durable worktrees; it hands that workspace to Codex, which owns
tool execution and file changes within the turn.

Root `integrations/` contains neutral external-system adapters. Call direction
is `factoryd lifecycle/Hatchet worker -> integrations -> external systems` and
`factoryd lifecycle/Hatchet worker -> harness-client -> Factory runtime`.
Codex core and Factory runtime never import root integrations, and integrations
never call Codex core directly. The worker publishes lifecycle events inside
the same factoryd attempt/checkpoint retry boundary as the corresponding stage.

## Infrastructure decisions

| Component | V1 role |
| --- | --- |
| Hatchet | Baseline durable workflow engine for `factoryd` |
| PostgreSQL | Baseline durable workflow and Factory state |
| Qdrant | Current vector index for RAG/long-term memory |
| Redis | Conditional coordination, cache, and pub/sub only where needed; never Codex compaction |
| Langfuse | Explicit non-default Factory observability profile, paired with ClickHouse |
| ClickHouse | Only the explicitly enabled Langfuse/observability profile; not a default V1 dependency |
| MinIO | Artifact and object storage where used, independent of the observability profile |
| Ollama | Optional local embedding and extraction provider |

Codex owns context compaction inside its thread/history machinery.

## Providers and observability

Define a generic model-provider boundary rather than a vendor-specific harness.
The default direct path accepts any Responses-compatible endpoint, provider ID,
model, and optional API key through Codex's native provider configuration.
Providers using another wire protocol require an explicitly selected
translation adapter. Factory does not invoke the Claude Code or Cursor
SDK/harness. GLM 5.2 must be functionally proven through the complete Codex
kernel, including a real turn that invokes a tool and consumes its result.
Disable unintended Codex external analytics, feedback uploads, OTel export, and
log exporters. This does not disable an operator's explicitly configured
Factory observability profile: `factoryd` may send intentional Factory
lifecycle and trace data to Langfuse/ClickHouse through that profile. Model API
calls, explicit tool traffic, and explicitly configured Factory observability
are distinct from unintended Codex export.

The optional translation adapters use the MIT-licensed `@bitkyc08/opencodex`
2.8.0 public
`startServer()` API, supervised as an isolated Bun sidecar and pinned through
the package lock. Factory never invokes its CLI or lets it rewrite a user's
Codex configuration. The explicit `zai` preset targets Z.AI's Standard API at
`https://api.z.ai/api/paas/v4`; Coding Developer Plan is an explicit override at
`https://api.z.ai/api/coding/paas/v4`. Both select `glm-5.2` behind the same
generic Codex Responses provider boundary. The separate `deepseek` preset uses
DeepSeek's official OpenAI-compatible Chat base at `https://api.deepseek.com`
and selects `deepseek-v4-pro`. It relies on Codex fallback model metadata rather
than inventing a downstream catalog; OpenCodex preserves translated tool calls
and the provider's model-specific reasoning history. Neither optional provider
is a product default, and their sidecars use separate state volumes.

Acceptance on 2026-07-31 proved a hidden-value round trip through five
successful Codex shell-tool executions across two GLM turns, explicit
model-backed Codex compaction, stdio runtime shutdown/restart, persisted thread
resume, and exact recall from compacted context. This is the minimum evidence
bar for subsequent provider adapters.

## V1 implementation and acceptance constraints

Implementation is functionality-first. Functional acceptance flows must
directly demonstrate:

- Codex planning and a real tool-using turn;
- context compaction and session resume;
- generic-provider behavior and GLM 5.2;
- durable checkpoint/retry/crash recovery;
- decomposition and Factory subagent behavior;
- autonomous review, remediation, and approving re-review; and
- long-term memory retrieval through Qdrant.

V1 does not include security architecture, hardening, threat modeling,
isolation design, mount/trust verification, credential-broker work,
egress-control work, container-security work, or security-only tests. Do not
create or run mechanical/security suites. The only baseline in scope is
autonomous direct-job execution with the Codex approval protocol retained for
explicit supervised clients, disabling unintended external telemetry and
logging, and preserving inherited safeguards against obviously destructive
commands.

If a software subagent proposes prohibited V1 security work, immediately
terminate that subagent task and discard its contribution. Preserve user-owned
files. Do not commit or push without explicit user authorization.

## Consequences

- Codex updates remain reviewable because upstream structure and provenance are
  preserved.
- Factory functionality can evolve without forking a second agent loop.
- Durable recovery does not become part of Codex's in-process thread lifecycle.
- Removing legacy product coupling does not accidentally remove memory,
  review, remediation, worktree, or workflow capabilities.
- Any V1 implementation or document that narrows the Codex kernel or substitutes
  a security-focused canary/harness is superseded by this decision.
