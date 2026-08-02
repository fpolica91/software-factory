# Factory Applications

User-facing Factory applications live here. The root launcher runs provider
onboarding through the image's native Rust CLI while bind-mounting the product
checkout's ignored `.env`, then starts the container stack. Job commands invoke
that same CLI in the running `factoryd` container. The CLI creates jobs and
managed worktrees through the coordinator; `factory-worker` executes their
durable plan, execute, review, and remediate stages through the native Codex
runtime.

`apps/cli/` contains the container entrypoint used by the root `factory`
launcher. Install that launcher once with `./factory install`, then run
`factory run "<task>"` from any Git repository. It presents planning,
durable progress, review, detach/attach, and stop behavior without raw HTTP
commands. Applications may use the coordinator's Factory job API, but they must
not mirror Codex wire types, define a versioned Factory protocol, or import the
preserved `factory-harness/codex-rs` kernel directly.
