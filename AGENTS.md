# Repository Guidelines

## Project Structure

`factory-harness/codex-rs/` is the preserved upstream Codex kernel; do not
rename, relocate, or copy its internals. Factory-owned Rust lives under
`factory-harness/factory/`: `runtime/` composes Codex and runs stages,
`extension/` owns Factory agent behavior and memory, `coordinator/` owns
durable jobs and worktrees, `cli/` is the native user interface, and
`providers/` owns provider profiles and transport adapters. Root `factory`,
`Dockerfile`, and `docker-compose.yml` are distribution wiring, not another
application implementation.

## Architecture Rules

Codex is the only agent harness. Use its exact app-server types and in-process
lifecycle; never add a Factory protocol version, mirrored wire type, generated
schema, manifest, hash negotiation, or compatibility layer. Factory job,
attempt, checkpoint, lease, event, and runtime-correlation records belong to
the coordinator domain. The Rust durable runner owns retries and recovery; do
not reintroduce Hatchet, a Node runtime, Cursor, or deleted TypeScript clients,
workflows, and integrations.

Read `RUST_CUTOVER.md` before repository-wide exploration. It is the
compaction-safe inventory of protected upstream, active Rust, deleted paths,
and verification gates. Update it in the same patch that activates,
quarantines, or deletes a path; do not rediscover classified trees.

## Build and Development Commands

- `cd factory-harness/factory && just build` builds the Rust distribution.
- `cd factory-harness/factory && cargo check --workspace --all-targets` checks
  Factory-owned Rust targets.
- `docker compose config` validates deployment configuration.
- `./factory install` installs the launcher; `factory run "<task>"` starts a
  durable job from a Git repository.

Format Rust with `cargo fmt` and keep modules focused with descriptive names.
Do not modify upstream Codex unless a named requirement proves its public API
insufficient; record every deliberate seam in `UPSTREAM.md` and
`RUST_CUTOVER.md`.

## Verification and Change Policy

Compilation counts are not product acceptance. Verify user-facing behavior
with the smallest relevant functional flow, then complete the real-model
plan/tool/review/remediation/re-review, detach/attach, recovery, resume, and
memory gate before declaring the cutover complete. Do not add unrelated
security architecture or security-only suites. Preserve user-owned files, and
do not commit or push without explicit authorization.
