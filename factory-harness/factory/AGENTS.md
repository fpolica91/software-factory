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
- Keep `factory/main` as the downstream integration branch. Fetch from
  `upstream`; apply selective updates with
  `git cherry-pick -x <reviewed-upstream-sha>`.
- Always preserve the root `LICENSE` and `NOTICE`, including all attribution and
  notice duties for downstream copies and distributions, regardless of whether
  upstream source or Cargo manifests are modified.
- Treat the pinned base in `UPSTREAM.md` as the contiguous upstream baseline.
  Record every selective upstream update there without moving the pinned base.
- For upstream changes, run `just fmt` and `just test -p <package>` from
  `codex-rs/`, then follow any path-specific `AGENTS.md` instructions for the
  changed paths. If `common`, `core`, or `protocol` changed, ask the user for
  approval before running the complete test suite with `just test` after the
  package-specific tests pass.

## Software Factory V1 Scope

- Follow `../../PRODUCT.md` and
  `../../docs/adr/0001-codex-kernel-factory-extension.md` as the canonical V1
  product and architecture decisions.
- Preserve the complete Codex kernel and implement Factory behavior through
  native extensions. Keep `factoryd` a separate durable lifecycle. Do not
  duplicate or narrow the harness.
- Keep downstream crates under `runtime/`, `extension/`, `coordinator/`,
  `providers/`, and `protocol/`. Dependency direction is Factory to public
  Codex APIs only; Codex core must never depend on Factory.
- Use the full Factory-enabled app-server v2/in-process lifecycle and its typed
  protocol; do not extend the current one-shot turn protocol as the target
  runtime surface. Treat planned Factory package names as target contracts
  until their manifests exist.
- `factory/coordinator` owns durable worktrees and drives root integrations.
  Runtime receives the selected workspace for Codex tool execution and never
  imports root integration adapters.
- Build user-facing functionality before acceptance work. Acceptance flows
  directly prove planning, real tool use, compaction, resume, provider/GLM 5.2
  behavior, durable recovery, decomposition, review/remediation, and memory.
- Do not propose, implement, or test V1 security architecture, hardening,
  threat modeling, isolation design, mount/trust verification,
  credential-broker work, egress-control work, container-security work, or
  security-only tests. Do not create or run mechanical/security test suites.
- The only V1 baseline in this area is normal Codex access/approval requests,
  disabling unintended external telemetry and logging, and preserving
  inherited safeguards against obviously destructive commands. Do not expand
  this baseline into additional work.
- If a software subagent proposes prohibited V1 security work, immediately
  terminate that subagent task and discard its contribution.
- Preserve user-owned files. Do not commit or push without explicit user
  authorization.
