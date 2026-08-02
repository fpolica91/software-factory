# Upstream Boundary

`factory-harness/` contains a vendored, prefix-shifted Codex source tree whose
source project is <https://github.com/openai/codex.git>, alongside the
downstream `factory/` workspace. It is not a submodule or a nested Git
checkout.

- Verified contiguous baseline:
  `406dc9239492aff6d295cca5eebe2a548548d42f` ("Expose MCP read-only hints in
  tool call items", 2026-07-30). Selective later imports do not move this pin.
- The baseline was verified against the official repository at Software
  Factory import commit `80876746c53bb95f916157a2c26ad87c8ec57194`.
  Of 5,831 shared files, 5,816 had identical Git blob IDs and the remaining 15
  are exactly the initial native app-server/subagent extension seam documented
  below. The prefix import omitted upstream editor files under `.vscode/` and
  five `codex-rs/secrets/` files; it was therefore a traceable source
  snapshot, not a byte-for-byte mirror of the complete upstream tree.

- Upstream paths such as `codex-rs/core/` live here as
  `factory-harness/codex-rs/core/`.
- Never rename, relocate, or copy upstream directories. Keep downstream work
  under `factory-harness/factory/`. Root-level provenance, governance, and
  build-integration metadata is allowed only when required by tooling or
  repository convention.
- Factory may depend inward on public Codex extension or core APIs. Codex core
  must never depend on Factory.
- Do not infer a new baseline from a Cargo lockfile, file timestamp, or an
  unverified historical note. Move the pin only after comparing the vendored
  tree to the exact upstream revision.

The vendored `factory-harness/LICENSE` and `factory-harness/NOTICE` must always
be preserved. Retain all required attribution and notices in downstream copies
and distributions, and update the notice whenever downstream modifications or
bundled dependencies create an additional notice obligation. These duties
apply whether or not upstream source or Cargo metadata is modified.

## Updating the vendored prefix

Review candidate changes in a temporary checkout of the official repository.
Use the full reviewed commit SHA, inspect its parent diff, and apply only those
hunks under the `factory-harness/` prefix. A direct `git cherry-pick` in this
repository is incorrect because upstream paths are rooted one directory higher
and the Factory tree contains downstream seams.

One suitable patch workflow is:

```sh
git -C /tmp/codex-upstream show --stat --oneline <reviewed-upstream-sha>
git -C /tmp/codex-upstream diff <reviewed-upstream-sha>^ <reviewed-upstream-sha> \
  | git apply --check --directory=factory-harness -
git -C /tmp/codex-upstream diff <reviewed-upstream-sha>^ <reviewed-upstream-sha> \
  | git apply --directory=factory-harness -
```

Use a newly created temporary checkout, not `/tmp/codex-upstream` blindly; the
literal path above only makes the required prefix transformation visible.
Resolve conflicts against the downstream seam explicitly, inspect the complete
result, and record the verified upstream SHA and resulting Software Factory
commit in the table below. Do not move a baseline claim until a contiguous
source baseline has actually been established.

From `factory-harness/codex-rs/`, run at least:

```sh
just fmt
just test -p <package>
```

Replace `<package>` with every affected package. Also read and follow any
path-specific `AGENTS.md` files for the changed paths and run their additional
checks. If `common`, `core`, or `protocol` changed, ask the user for approval
before running the complete test suite with `just test` after the
package-specific tests pass.

## Recorded prefix-aware imports

| Upstream commit | Downstream commit | Purpose |
| --- | --- | --- |
| `406dc9239492aff6d295cca5eebe2a548548d42f` | `80876746c53bb95f916157a2c26ad87c8ec57194` | Initial prefix import and native extension seam |

## Downstream Patches

### Current runtime and extension seam

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
  in-process seam accepts its existing plugin-startup policy. The shipped
  Factory worker consumes only the in-process seam: it skips remote plugin
  warmup and sets analytics off in the Rust configuration while leaving local
  plugin, skill, MCP, and turn behavior available on demand.

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

### Per-step host tool availability

- `codex-rs/ext/extension-api/src/contributors.rs` lets a native tool
  contributor disable named host tools for one sampling step.
- `codex-rs/core/src/tools/router.rs`, `tools/spec_plan.rs`, and
  `session/turn.rs` aggregate that decision before tool planning. Disabled
  runtimes are removed before model specs and the dispatch registry are built;
  `tools/spec_plan_tests.rs` proves they are neither advertised nor callable.
- Factory uses this generic seam only during Plan to remove `apply_patch` and
  `request_permissions`. Read-only shell inspection remains native Codex and is
  enforced by the turn sandbox.

### Detached review host context

- `codex-rs/app-server-protocol/src/protocol/v2/review.rs` adds optional
  `detachedContext` to detached `review/start` requests. Inline reviews reject
  the detached-only field.
- `codex-rs/app-server/src/request_processors/turn_processor.rs` validates the
  supplied parent thread and carries parent turn plus durable state key into a
  typed extension attachment. Detached reviews start with fresh model history
  and are identified through Codex's existing
  `SessionSource::SubAgent(Review)` and `ThreadSource::Subagent` values.
- `codex-rs/ext/agent/src/lib.rs` adds default-preserving host start options
  with an explicit history policy. Ordinary agents continue to fork parent
  history; detached reviews select an empty initial history. The existing
  review target, skill prompt, and app-server API remain unchanged.
  `codex-rs/ext/agent/tests/agent_service.rs` proves both history policies, and
  `codex-rs/ext/extension-api/src/contributors/thread_lifecycle.rs` defines
  the generic typed attachment. Neither layer contains a Factory dependency.
- Factory runtime supplies this context only for durable review stages. The
  Factory extension then shares the parent state and records exact detached
  review thread, turn, and parent lineage without a parallel review harness.
