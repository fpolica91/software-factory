# Factory Boundary Rules

- Keep downstream implementation and runtime code under `factory/`.
- Add root-level provenance, governance, or build-integration metadata only
  when required by tooling or repository convention; never use it as a second
  implementation boundary.
- Never rename, relocate, or copy an upstream directory.
- Factory may depend inward on public Codex extension or core APIs; Codex core
  must never depend on Factory.
- Do not modify upstream source or Cargo manifests until a named requirement
  proves the public extension and core APIs insufficient and the change is
  documented and reviewed.
- Codex is vendored under the `factory-harness/` prefix, not checked out as a
  nested repository. Do not assume an `upstream` remote or integration branch,
  and do not directly cherry-pick upstream commits into the enclosing
  repository. Follow the prefix-aware review and import process in
  `../UPSTREAM.md`.
- Always preserve `../LICENSE` and `../NOTICE`, including all attribution and
  notice duties for downstream copies and distributions, regardless of whether
  upstream source or Cargo manifests are modified.
- Record an upstream baseline or selective update in `UPSTREAM.md` only after
  verifying its exact source SHA and the prefix-adjusted downstream result.
- For upstream changes, run `just fmt` and `just test -p <package>` from
  `codex-rs/`, then follow any path-specific `AGENTS.md` instructions for the
  changed paths. If `common`, `core`, or `protocol` changed, ask the user for
  approval before running the complete test suite with `just test` after the
  package-specific tests pass.

## Software Factory Scope

- Follow `../../PRODUCT.md` and
  `../../docs/adr/0001-codex-kernel-factory-extension.md` as the canonical
  product and architecture decisions.
- Preserve the complete Codex kernel and implement Factory behavior through
  native extensions. Keep `factoryd` a separate durable lifecycle. Do not
  duplicate or narrow the harness.
- Keep downstream crates under `runtime/`, `extension/`, `coordinator/`,
  `cli/`, and `providers/`. Dependency direction is Factory to public
  Codex APIs only; Codex core must never depend on Factory.
- Use the complete Factory-enabled app-server/in-process lifecycle and exact
  upstream types. Do not add Factory protocol versions, mirrored wire types,
  schemas, manifests, hashes, or compatibility negotiation. Durable Factory
  identifiers and correlations belong to the coordinator domain.
- `factory/coordinator` owns durable worktrees and drives root integrations.
  Runtime receives the selected workspace for Codex tool execution and never
  imports root integration adapters.
- Build user-facing functionality before acceptance work. Acceptance flows
  directly prove planning, real tool use, compaction, resume, provider
  behavior, durable recovery, decomposition, review/remediation, and memory.
- Do not propose, implement, or test security architecture, hardening,
  threat modeling, isolation design, mount/trust verification,
  credential-broker work, egress-control work, container-security work, or
  security-only tests. Do not create or run mechanical/security test suites.
- The only baseline in this area is normal Codex access/approval requests,
  disabling unintended external telemetry and logging, and preserving
  inherited safeguards against obviously destructive commands. Do not expand
  this baseline into additional work.
- If a software subagent proposes prohibited security work, immediately
  terminate that subagent task and discard its contribution.
- Preserve user-owned files. Do not commit or push without explicit user
  authorization.
- Treat `../../RUST_CUTOVER.md` as the authoritative compaction boundary.
  Update it whenever a path becomes active, protected, quarantined, or
  deleted; do not repeat a repository-wide inventory after a reset.
