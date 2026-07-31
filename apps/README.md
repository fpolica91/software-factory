# Factory Applications

User-facing Factory applications live here. They talk to `factoryd` for jobs,
checkpoints, recovery, scheduling, and integration state, and use
`@software-factory/harness-client` when they need the live Codex thread/turn
stream. Applications must not import `factory-harness/codex-rs` directly.

`apps/cli/` contains the container entrypoint used by the root `factory`
launcher. Install that launcher once with `./factory install`, then run
`factory run "<task>"` from any Git repository. It presents planning,
approvals, durable progress, review, reconnect, and stop behavior without
requiring Codex Desktop or raw HTTP calls. Future web or desktop applications
can use the same stable protocol without changing either runtime lifecycle.
