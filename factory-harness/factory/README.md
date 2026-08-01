# Factory Runtime

The default Factory product path is the complete Codex app-server lifecycle.
It retains Codex planning and the agent loop, tools, approvals, threads,
persistence and resume/fork, compaction, goals, skills, MCP, memories, plugins,
extensions, and subagents.

## Crates

- `runtime/` launches the full stdio app server and exposes the upstream typed
  in-process client with Factory's native extension installed.
- `extension/` owns Factory's native Codex lifecycle contributors and
  Factory-scoped thread state.
- `protocol/` owns the stable, versioned Factory Protocol V1 contract and its
  revision-pinned adapter to Codex app-server v2. Checked-in JSON Schema and
  TypeScript declarations expose only Factory-owned public types.
- `providers/` owns model-provider protocol bridges. The optional Z.AI and
  DeepSeek profiles supervise one pinned maintained Responses-to-Chat adapter
  and return per-thread Codex provider configuration without changing Codex
  core or a user's global Codex configuration.
- `coordinator/` owns the separate `factoryd` durable lifecycle. PostgreSQL
  stores jobs, operations, leased attempts, immutable runtime correlations,
  and append-only checkpoints; recovery claims link a new attempt to the
  checkpoint selected after a retry or expired lease.

The workspace root is virtual; only the product crates above are Cargo members.

Factory depends on public Codex APIs. Codex core does not depend on Factory.

## Build and run

```sh
just build
target/debug/factory-runtime
target/debug/factory-runtime protocol-manifest
target/debug/factory-runtime legacy-protocol-manifest
cargo build --locked -p factory-coordinator --bin factoryd
```

`coordinator/README.md` documents the two-process PostgreSQL recovery
acceptance path. It proves that a fresh `factoryd` connection can load a
checkpoint and its complete Factory Protocol correlation, claim the eligible
operation, and finish the recovered job.

The stdio process uses the app-server JSONL/JSON-RPC lifecycle: `initialize`,
`initialized`, thread start/resume/fork, turn start, streamed notifications,
server-request responses, and shutdown. Configuration continues to use normal
Codex configuration and `-c key=value` overrides. Pass `--strict-config` to
reject unknown configuration fields.

`factory-runtime protocol-manifest` prints exactly one distribution manifest
and exits without starting app-server. In-process hosts can read the same value
through `factory_runtime::protocol_manifest()`. The manifest identifies the
Factory Protocol version and schema SHA-256, pinned Codex revision, and active
Codex app-server V2 version and schema SHA-256. The runtime computes the V2
digest from the pinned upstream schema bundle at build time.

`legacy-protocol-manifest` and
`factory_runtime::legacy_protocol_manifest()` expose the old Factory
Protocol-only manifest for compatibility consumers. The harness client never
uses that legacy surface for active negotiation.

The launcher sets app-server's `default_analytics_enabled` argument to `false`,
starts remote control disabled, and skips remote plugin warmup. Local plugins,
skills, MCP, model traffic, tools, and all thread behavior remain available;
the startup policy only prevents unintended external Codex traffic.

## In-process hosts

Rust hosts call `factory_runtime::in_process::start` with an
`InProcessClientStartArgs`. The namespace also exposes
`InProcessAppServerClient`, `InProcessServerEvent`, and the upstream default
channel capacity. These retain the upstream typed app-server lifecycle,
including its initialize handshake, requests, events, server-request
resolution, and graceful shutdown, while installing Factory's native
contributors through the generic Codex composition seam.

The generated Factory Protocol V1 client contract lives under
`protocol/schema/`. The exporter verifies the JSONL envelope, request/response
pairing for every supported server request, forward-compatible unknown
payloads, and byte-stable checked-in artifacts. It also recomputes the JSON
Schema digest and rejects any mismatch with the compiled manifest before it
writes or verifies those artifacts.

The initial Factory contributor establishes Factory-owned state through the
native Codex thread lifecycle. Memory/context, decomposition, progress,
review/remediation, and Factory subagent contributors extend that boundary;
they do not create a second agent loop.
