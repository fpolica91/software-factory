# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What This Is

Software Factory runs the native Codex agent as a durable, autonomous software delivery job. The shipped application is Rust-only: one Docker image containing four binaries — `factory` (CLI), `factory-worker` (durable runner), `factoryd` (coordinator), and `factory-provider-bridge` (optional wire-format translation for Anthropic/DeepSeek/Z.AI).

Read `RUST_CUTOVER.md` before any repository-wide exploration — it is the authoritative, compaction-safe inventory of protected upstream code, active Rust, deleted paths, and verification gates. Update it in the same patch that activates, quarantines, or deletes a path; do not re-inventory classified trees.

## Commands

All Rust development happens in `factory-harness/factory/`:

```sh
cd factory-harness/factory
just build          # build the four distribution binaries (cargo build --locked)
just test           # cargo nextest run --locked
just clippy         # cargo clippy --locked --all-targets -- -D warnings
just fmt            # cargo fmt --all
cargo check --workspace --all-targets   # fast type-check
cargo nextest run -p factory-coordinator <filter>   # single crate / single test
```

Distribution and deployment (repo root):

```sh
docker compose config    # validate deployment configuration
./factory install        # install the launcher symlink into ~/.local/bin
factory run "<task>"     # submit a durable job from any Git repository
factory up / logs / down # manage the Compose stack
factory build            # maintainer-only local image build
```

For changes to upstream Codex code (rare — see boundary rules below), run `just fmt` and `just test -p <package>` from `factory-harness/codex-rs/`. If `common`, `core`, or `protocol` changed, ask the user before running the full suite.

## Architecture

**One distribution, two lifecycles, no duplicated harness** (canonical contract: `PRODUCT.md`, `docs/adr/0001-codex-kernel-factory-extension.md`):

1. **Codex is the only agent harness.** `factory-harness/codex-rs/` is the preserved upstream Codex kernel — it owns the model loop, tools, threads, approvals, persistence, resume/fork, and context compaction. Never rename, relocate, copy, or modify it unless a named requirement proves its public API insufficient; record any deliberate seam in `factory-harness/UPSTREAM.md` and `RUST_CUTOVER.md`.
2. **Factory is a native Codex extension.** Factory-owned Rust lives in the `factory-harness/factory/` workspace (five crates):
   - `runtime/` — composes Codex in-process (`factory_runtime::in_process`) and runs the durable stage loop
   - `extension/` — Factory agent behavior: memory (Qdrant), context, decomposition, review/remediation
   - `coordinator/` — `factoryd`: PostgreSQL-backed jobs, attempts, leases, checkpoints, retries, crash recovery, event replay, managed worktrees
   - `cli/` — native job CLI (`run`, `attach`, `status`, `stop`, `result`, `apply`, `export`, provider configuration)
   - `providers/` — provider/model profiles and the optional Rust transport bridge
3. **`factoryd` is the durable lifecycle.** Job, operation, attempt, checkpoint, lease, event, and runtime-correlation records belong to the coordinator domain, outside the Codex thread lifecycle. `factory-worker` claims jobs and runs the plan → execute → review → remediate → re-review stages through the native Codex runtime.

Root `factory` (shell launcher), `Dockerfile`, `docker-compose.yml`, and `apps/cli/` are distribution wiring, not another implementation. The Compose baseline is PostgreSQL + Qdrant + `factoryd` + `factory-worker`; provider bridges and everything else (Redis, MinIO, Ollama, Langfuse) are optional profiles.

## Hard Rules

- Dependency direction is Factory → public Codex APIs only; Codex core must never depend on Factory. Applications must not import `codex-rs` directly.
- Use exact upstream Codex app-server types and the in-process lifecycle. Never add a Factory protocol version, mirrored wire type, generated schema, manifest, hash negotiation, or compatibility layer.
- Do not reintroduce Hatchet, a Node runtime, Cursor, or the deleted TypeScript clients/workflows/integrations.
- Compilation counts are not product acceptance: verify user-facing behavior with the smallest relevant functional flow (real-model plan/tool/review/remediation, detach/attach, recovery, resume, memory) before declaring work complete.
- Do not add security architecture, hardening, or security-only test suites — the baseline is normal Codex approvals, telemetry off, and inherited destructive-command safeguards.
- Preserve user-owned files. Do not commit or push without explicit authorization.
