# Software Factory V1

Software Factory V1 turns the complete Codex runtime into a durable software
delivery system without replacing or duplicating its harness.

## Product contract

The architecture is **one distribution, two lifecycles, no duplicated harness**:

1. **Codex is the execution kernel.** Keep its real planning and agent loop,
   tools, existing sandbox and approval behavior, threads, persistence,
   resume/fork, context compaction, goals, skills, MCP, extensions, and
   subagent primitives working as upstream capabilities.
2. **Factory is a native Codex extension.** It adds long-term memory and
   context, decomposition, progress, review/remediation, and Factory-specific
   subagent behavior through Codex extension APIs. It does not implement a
   second agent loop.
3. **`factoryd` is the durable lifecycle.** It owns jobs, checkpoints, retries,
   crash recovery, integrations, and scheduling. Hatchet is the durable
   workflow engine and PostgreSQL is the durable workflow/state store.

The complete Codex fork lives under `factory-harness/`; its upstream
`codex-rs/` layout and internal paths remain untouched. All downstream Rust
implementation belongs under:

```text
factory-harness/factory/
├── runtime/
├── extension/
├── coordinator/
├── providers/
└── protocol/
```

Factory may depend on public Codex APIs. Codex core must never depend on
Factory. The stable, versioned Factory protocol is the boundary between the
harness fork and ordinary work in `apps/`, `workflows/`, `integrations/`,
`harness-client/`, and `docs/`.

The runtime surface is the full Factory-enabled Codex app-server v2 lifecycle,
not the current one-shot Factory turn protocol. Across the process boundary,
`harness-client` uses app-server's supported stdio JSONL transport and JSON-RPC
2.0 message semantics (with the `jsonrpc` header omitted on the wire). In Rust,
the same lifecycle uses `codex-app-server-client` typed requests and events:
initialize, thread start/resume/fork, turn start, streamed notifications,
server-request responses, and shutdown. The distribution pins the generated
app-server schema as its Factory protocol version.

`factoryd` persists the durable mapping from Factory job/operation/attempt and
Hatchet run identifiers to app-server request/thread/turn identifiers. It also
owns clone/worktree creation, reuse, checkpoint binding, and cleanup. Runtime
receives the selected workspace and context; Codex owns tool execution there.
Neutral adapters in root `integrations/` are called by Hatchet workers inside
the durable `factoryd` lifecycle, never by Codex core or Factory runtime.

## V1 capabilities

- Generalize the useful durable workflow, checkpoint, review/remediation,
  worktree, and memory behavior in the existing Software Factory.
- Incrementally remove only Cursor, Boss/Hydra assumptions, and Linear/GitLab
  coupling. Do not remove the Factory capabilities behind those integrations.
- Retain RAG and long-term memory, with Qdrant as the current vector index.
- Provide a generic direct boundary for Responses-compatible model providers.
  Keep protocol translation in explicit optional adapters and prove GLM 5.2
  through one such adapter with a real tool-using turn.
- Disable unintended Codex external analytics, feedback, OTel, and log
  exporters while retaining functional model/tool traffic and any explicitly
  configured Factory observability profile.

Baseline infrastructure is Hatchet for durable workflows and PostgreSQL for
durable workflow/Factory state. Qdrant is the current V1 RAG/long-term-memory
index. Redis is conditional coordination, cache, and pub/sub only; it is not
Codex compaction, which stays inside Codex. Langfuse and ClickHouse are an
explicitly enabled, non-default observability profile. MinIO provides artifact
storage where used, independently of that profile. Ollama is an optional local
embedding/extraction provider.

## V1 delivery policy

Build user-facing functionality first. Acceptance flows must directly prove
planning, real tool use, compaction, resume, generic-provider behavior and GLM
5.2, durable recovery, decomposition, review/remediation, and memory.

V1 adds no security architecture or security-only test program. Direct Factory
jobs run Codex autonomously by default and do not pause for command approval or
clarification; attach remains an observation and cancellation surface. Preserve
the underlying Codex approval protocol for explicit non-autonomous clients,
disable unintended external telemetry and logging, and retain inherited safeguards against obviously destructive
commands. Hardening, threat modeling, isolation design, mount/trust
verification, credential brokers, egress controls, container-security work,
and mechanical or security-only test suites are outside V1.

If a software subagent proposes prohibited V1 security work, immediately
terminate that subagent task and discard its contribution. Preserve user-owned
files. Do not commit or push without explicit user authorization.

The accepted architectural details and dependency rules are recorded in
[`docs/adr/0001-codex-kernel-factory-extension.md`](docs/adr/0001-codex-kernel-factory-extension.md).
