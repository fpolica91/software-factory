# Contributing to Software Factory

Thank you for improving Software Factory. This repository combines a preserved
Codex kernel with Factory-owned Rust. Change the correct boundary and verify it.

## Before You Start

Read the following documents before exploring or editing the repository:

- [`PRODUCT.md`](PRODUCT.md) defines the product and its intended behavior.
- [`AGENTS.md`](AGENTS.md) contains repository-wide working rules.
- [`RUST_CUTOVER.md`](RUST_CUTOVER.md) inventories active and protected paths.
- [`factory-harness/UPSTREAM.md`](factory-harness/UPSTREAM.md) records the Codex baseline and update process.
- [`factory-harness/AGENTS.md`](factory-harness/AGENTS.md) applies inside vendored Codex.
- [`factory-harness/factory/AGENTS.md`](factory-harness/factory/AGENTS.md) applies to Factory Rust.

Also read any nearer `AGENTS.md` before changing a nested path. Update
`RUST_CUTOVER.md` whenever a path becomes active, protected, quarantined, or deleted.

## Source Boundaries

The following paths are protected upstream Codex source:

- `factory-harness/codex-rs/`
- `factory-harness/codex-cli/`
- `factory-harness/sdk/`
- Other upstream support paths directly under `factory-harness/`

Do not rename, relocate, copy, or broadly rewrite these directories. Factory may depend on public Codex APIs, but Codex core must not depend on Factory.

Factory-owned Rust is the workspace under `factory-harness/factory/`:

- `cli/` provides the native `factory` command and interactive configuration.
- `coordinator/` owns jobs, attempts, leases, checkpoints, events, recovery, and worktrees.
- `extension/` owns Factory behavior, memory, review, and subagent integration.
- `providers/` owns provider profiles and transport adapters.
- `runtime/` composes Codex and runs durable stages and workers.

Repository-root paths have supporting roles:

- `factory`, `Dockerfile`, and Compose files package and launch the product.
- `apps/cli/` contains distribution entrypoint wiring.
- `deploy/` contains deployment manifests and runtime profiles.
- `docs/` contains architecture decisions and provider documentation.

Codex is the only agent harness. Do not add a parallel harness, second Factory
wire protocol, Node runtime, Hatchet, Cursor, or TypeScript workflow clients.

## Prerequisites

Install the following tools:

- Git.
- Docker with the Compose v2 plugin.
- `just` for repository recipes.
- The Rust toolchain pinned by `factory-harness/factory/rust-toolchain.toml`,
  currently Rust 1.95.0 with `rustfmt` and Clippy.
- `cargo-nextest` for repository-wide test execution.

Docker Compose supplies PostgreSQL, Qdrant, and selected runtime services.

## Build and Static Checks

Run Factory Rust commands from `factory-harness/factory/`:

```sh
cd factory-harness/factory
just fmt-check
just build
just clippy
cargo test --locked --workspace --no-run
cargo check --locked --workspace --all-targets
```

Use `just fmt` to apply formatting. `just build` creates the four distribution
binaries with locked dependencies. `just clippy` denies warnings on all targets.

The `default-members` are only `coordinator` and `runtime`. The current
`just test` and `just test-compile` recipes inherit that selection. Pass
`--workspace` explicitly for repository-wide verification of all five crates.

Validate root distribution changes from the repository root:

```sh
bash -n factory
docker compose config
```

For Kubernetes execution changes, also validate the combined configuration:

```sh
docker compose \
  -f docker-compose.yml \
  -f docker-compose.kubernetes.yml \
  config
```

## Coding Style and Naming

Rust uses edition 2024 and standard `rustfmt` output. Keep modules focused and
prefer descriptive names over abbreviations.

Follow Rust naming conventions:

- Modules, files, functions, and test functions use `snake_case`.
- Types, traits, and enum variants use `UpperCamelCase`.
- Constants and environment variables use `SCREAMING_SNAKE_CASE`.
- Crate package names use kebab case, such as `factory-coordinator`; library
  imports use snake case, such as `factory_coordinator`.

Keep public interfaces small. Reuse exact upstream Codex types instead of
mirroring them in Factory. Keep coordinator lifecycle records out of the Codex
wire surface.

The root launcher must remain compatible with its supported Bash environments.
Run `bash -n factory` after every launcher edit.

## Testing

Start with the smallest focused test that covers the changed behavior. Unit
tests live beside their modules. Integration tests use descriptive `snake_case`
filenames under each crate's `tests/` directory, including examples such as
`compact_attach.rs`, `lease_fencing.rs`, and `detached_review.rs`.

Run the complete Factory test suite with:

```sh
cd factory-harness/factory
cargo nextest run --locked --workspace
```

Some coordinator integration tests are ignored unless disposable PostgreSQL is
available. Provide its URL explicitly and select the relevant test:

```sh
FACTORY_COORDINATOR_TEST_DATABASE_URL=postgres://... \
  cargo test --locked -p factory-coordinator \
  --test workspace_integrity -- --ignored
```

Recovery behavior has dedicated functional scripts:

```sh
cd factory-harness/factory
bash coordinator/acceptance/workspace-recovery.sh
bash coordinator/acceptance/runner-recovery.sh
```

The project defines no numeric coverage threshold. Prefer evidence that the
changed behavior works over test-count totals.

Real-model functional acceptance is required when changing runtime, provider,
durability, recovery, stage orchestration, or execution-environment boundaries.
Exercise the affected plan, tool, review, remediation, attach, resume, or
recovery behavior and record the observed result. Routine documentation or
isolated formatting changes do not require a paid model run.

## Updating the Codex Prefix

Modify protected upstream code only when a named requirement proves that the
public extension or core APIs are insufficient. Review the exact upstream
commit in a fresh temporary checkout and use the prefix-aware process in
`factory-harness/UPSTREAM.md`; do not directly cherry-pick it into this
repository.

Record the reviewed upstream SHA, downstream result, and every deliberate seam
in `factory-harness/UPSTREAM.md` and `RUST_CUTOVER.md`. Preserve the vendored
`LICENSE` and `NOTICE`. Follow all path-specific instructions and run focused
upstream formatting and package tests for every affected crate.

## Documentation and Example Data

Keep examples reproducible and repository-neutral. Never include API keys,
tokens, hostnames, real job IDs, container IDs, fixture IDs, or local
absolute paths in documentation, commit messages, snapshots, or test output.
Use obvious placeholders such as `example.invalid`, `<job-id>`, or
`postgres://...`.

Update product documentation and architecture records when behavior or a source
boundary changes. Avoid unrelated cleanup in a focused change.

## Commits

Use a short, imperative subject. Recent maintainable history follows forms such
as:

```text
fix(kubernetes): validate existing PVC access modes
feat(kubernetes): support shared existing workspaces
factory: persist readable job artifacts
```

Prefer `type(scope): imperative summary` when a scope is clear. Keep refactors,
behavior changes, and unrelated formatting in separate commits.

## Pull Requests

A pull request should make review possible without reconstructing your work.
Include:

- The problem and the intended scope.
- The affected architecture boundary and why the change belongs there.
- Exact commands run and their results.
- Functional evidence for user-visible or lifecycle behavior.
- Documentation, `RUST_CUTOVER.md`, or `UPSTREAM.md` updates when applicable.
- A terminal capture or screenshot only when it materially clarifies a CLI
  presentation change.

State any verification that was not run and why. Keep the diff limited to the
described change and preserve user-owned files. Do not rely on container
publication as a substitute for local formatting, compilation, and focused
behavior checks.
