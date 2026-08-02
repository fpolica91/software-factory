# Software Factory

Software Factory turns the complete Codex runtime into a durable software
delivery system without replacing or duplicating its harness.

## Product Contract

The architecture is **one distribution, two lifecycles, no duplicated
harness**:

1. **Codex is the execution kernel.** It owns planning and the agent loop,
   tools, threads, persistence, resume/fork, context compaction, goals, skills,
   MCP, approvals, extensions, and subagent primitives.
2. **Factory is a native Codex extension.** It contributes long-term memory and
   context, decomposition, progress, review/remediation, and Factory-specific
   subagent behavior through native Rust extension APIs. It never introduces a
   second model or tool loop.
3. **`factoryd` is the durable lifecycle.** PostgreSQL-backed jobs, operations,
   attempts, leases, checkpoints, retries, crash recovery, correlations,
   events, scheduling, and managed worktrees live outside the Codex thread
   lifecycle. The Rust `factory-worker` claims this work and runs it directly.

The distribution contains the Rust `factory`, `factory-worker`, `factoryd`, and
`factory-provider-bridge` binaries. Factory does not depend on Hatchet,
TypeScript, Cursor, Boss/Hydra, Linear, or GitLab.

## Boundary

The preserved Codex source lives under `factory-harness/codex-rs/`. Factory
implementation lives under `factory-harness/factory/`:

```text
factory/
├── cli/          # user-facing job lifecycle
├── coordinator/  # durable state, recovery, events, worktrees
├── extension/    # memory, context, decomposition, review behavior
├── providers/    # provider profiles and transport translation
└── runtime/      # Codex bootstrap, sessions, stages, durable worker
```

Dependency direction is Factory to public Codex APIs. Codex core never depends
on Factory. A minimal upstream seam is allowed only when a required native
primitive is unavailable publicly; every such change is recorded in
`factory-harness/UPSTREAM.md` and `RUST_CUTOVER.md`.

Factory uses Codex app-server types and its in-process lifecycle directly. It
must not create a Factory protocol version, mirrored wire model, generated
schema, manifest, hash negotiation, or compatibility layer. Factory job and
attempt identifiers are coordinator domain data, not Codex protocol types.

## Durable Lifecycle

Each job runs in a coordinator-managed Git worktree. The durable runner moves
through plan, execute, independent review, remediation when requested, and a
fresh independent re-review. Checkpoints and runtime correlations allow the
current stage to resume after worker loss without turning `factoryd` into an
agent harness. The CLI can detach, replay durable events, reattach, inspect,
and cancel while the job continues independently. Repository identity is
separate from the fixed container mount path, so changing the host repository
does not alias or lose detached jobs.

Each job pins the canonical provider and exact model it was created with. A
worker can claim the job only when it serves that profile; switching the active
configuration cannot silently recover an older attempt with another model. The
CLI refuses a provider/model switch while any nonterminal pinned job needs a
different profile, and treats legacy unpinned jobs as unknown rather than
guessing. An explicit `--force` changes configuration with a warning but does
not stop or migrate those jobs.
Completed work stays in the managed worktree until delivery. Attached local
runs apply it by default, while `--no-apply`, `factory apply`, and
`factory export` allow explicit delivery. Apply refuses before mutation unless
the patch digest, repository identity, immutable base revision, clean host
checkout, and Git patch preflight all match.

PostgreSQL and Qdrant are baseline services: PostgreSQL owns durable lifecycle
state and Qdrant owns long-term memory/RAG. Redis, MinIO, Ollama, Langfuse, and
ClickHouse are opt-in profiles rather than core dependencies.

## Providers and Telemetry

The harness is model-vendor neutral. OpenAI Responses uses the direct Codex
provider path. Anthropic Messages and DeepSeek/Z.AI Chat Completions use an
explicit Rust translation adapter; it translates transport only and does not
replace the Codex harness. Custom Responses-compatible providers can use the
same direct boundary.

Unintended Codex analytics, feedback, OTel, and log export remain disabled by
default. Functional model/tool traffic and operator-enabled observability are
separate, explicit behavior.

## Delivery and Acceptance

Work is functionality-first. Compilation and unit checks are development gates,
not product acceptance. Completion requires a real model to prove planning,
tool use, managed-worktree execution, detached review, remediation, approving
re-review, detach/attach, crash recovery, context compaction/resume, and Qdrant
memory retrieval. The 2026-08-02 cutover run proved every Factory-owned part of
that flow, including crash resume and an actual changes-requested remediation
cycle. Automatic context compaction remains the preserved native Codex kernel's
responsibility rather than a second Factory implementation.

Do not add unrelated security architecture or security-only test programs.
Preserve user-owned files, and do not commit or push without explicit user
authorization.
