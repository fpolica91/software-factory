# Upstream Boundary

This repository is a downstream Codex harness. Its upstream is
<https://github.com/openai/codex.git>, recorded locally as the `upstream`
remote.

- Pinned base: `406dc9239492aff6d295cca5eebe2a548548d42f`
  - This is the newest commit in the contiguous upstream baseline incorporated
    by the fork. Selectively cherry-picked later commits do not move it.
- `factory/main` is the local downstream integration branch.
- Upstream branches remain upstream-only; do not put Factory changes on them.
- Never rename, relocate, or copy upstream directories. Keep downstream work
  under `factory/`. Root-level provenance, governance, and build-integration
  metadata is allowed only when required by tooling or repository convention.
- Factory may depend inward on public Codex extension or core APIs. Codex core
  must never depend on Factory.

The root `LICENSE` and `NOTICE` must always be preserved. Retain all required
attribution and notices in downstream copies and distributions, and update
`NOTICE` whenever downstream modifications or bundled dependencies create an
additional notice obligation. These duties apply whether or not upstream source
or Cargo metadata is modified.

## Updating

Fetch without rewriting the downstream branch:

```sh
git fetch upstream
git switch factory/main
git cherry-pick -x <reviewed-upstream-sha>
```

Cherry-pick only reviewed upstream commits and record each one below. Resolve
conflicts explicitly. A selective update does not change the pinned base.
Change the pinned base only after every upstream commit through the new base has
been incorporated as a contiguous, reviewed baseline.

From `codex-rs/`, run at least:

```sh
just fmt
just test -p <package>
```

Replace `<package>` with every affected package. Also read and follow any
path-specific `AGENTS.md` files for the changed paths and run their additional
checks. If `common`, `core`, or `protocol` changed, ask the user for approval
before running the complete test suite with `just test` after the
package-specific tests pass.

## Selective Upstream Updates

| Upstream commit | Downstream commit | Purpose |
| --- | --- | --- |
| _None_ | — | — |

## Downstream Patches

### Current V1 runtime and extension seam

The current product path is `factory/runtime`: the complete Codex app-server
lifecycle with Factory contributors composed through the generic native
extension seam below. It preserves Codex planning and agent behavior, tools,
threads, compaction, goals, skills, MCP, plugins, memories, approvals, and
subagents. This is the implementation described by
[`PRODUCT.md`](../PRODUCT.md),
[`ADR 0001`](../docs/adr/0001-codex-kernel-factory-extension.md), and
[`factory/README.md`](factory/README.md).

- `codex-rs/app-server/src/lib.rs`, `in_process.rs`, `message_processor.rs`,
  `extensions.rs`, `mcp_refresh.rs`, and `message_processor_tracing_tests.rs`,
  together with `codex-rs/app-server-client/src/lib.rs`, expose one generic
  extension-installer callback through stdio and in-process startup. The final
  two app-server files only keep existing internal builders compiling with an
  empty installer list. Stock startup supplies an empty installer list.
  Downstream Factory code appends native contributors through the public Codex
  registry builder; no upstream crate depends on a Factory crate or type.
  The stdio seam also accepts Codex's existing runtime startup options, and the
  in-process seam accepts its existing plugin-startup policy. Factory uses
  those options to skip remote plugin warmup, start remote control disabled,
  and keep analytics off while leaving local plugin, skill, MCP, and turn
  behavior available on demand.

### Contributed host-created turn-item lifecycle

- `codex-rs/core/src/session/mod.rs` adds generic contributed lifecycle emitters
  for host-created turn items. They run the existing `TurnItemContributor`
  chain against the active turn store before delegating to Codex's unchanged
  item-started/item-completed emitters. Parsed model items retain their existing
  stream-finalization contribution path, avoiding duplicate contribution.
- Native collaboration handlers under
  `codex-rs/core/src/tools/handlers/multi_agents/` and
  `multi_agents_v2/wait.rs` use those generic emitters. The existing
  `multi_agents_v2::emit_sub_agent_activity` helper does the same for V2 spawn,
  message/follow-up, and interrupt activity. This exposes Codex's existing
  `CollabAgentToolCall` and `SubAgentActivity` lifecycle to downstream
  extensions without adding any Factory dependency, type, tool, or agent loop
  to Codex core.

### Native extension tool turn ancestry

- `codex-rs/core/src/turn_metadata.rs` exposes extension-tool metadata that
  retains the active parent turn identifier while leaving Codex's MCP metadata
  unchanged.
- `codex-rs/core/src/tools/handlers/extension_tools.rs` passes that scoped
  metadata to native extension tools. Factory review records use it to prove
  that an independent review child belongs to the exact durable review turn.
