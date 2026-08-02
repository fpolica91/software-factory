# Factory Worker Runtime

The `factory-runtime` crate is the library behind the shipped `factory-worker`
binary. The worker embeds the full Codex app-server lifecycle and installs
native Factory contributors; it does not implement a second model or agent
loop.

Every operation uses a fenced `FactorydStateBackend`. `FACTORYD_URL` identifies
that durable state service; there is no standalone process-memory product
mode.

Long-term memory is enabled by setting:

```sh
FACTORY_QDRANT_URL=http://127.0.0.1:6333
FACTORY_QDRANT_COLLECTION=factory_memories
FACTORY_MEMORY_NAMESPACE=default
```

`FACTORY_QDRANT_API_KEY` is optional. If `FACTORY_QDRANT_URL` is absent, the
runtime reports that memory is disabled, omits `factory_remember` and
`factory_recall`, and otherwise runs the complete Codex harness unchanged.
Qdrant sparse lexical retrieval is the baseline; Ollama is not required. The
worker passes the durable job repository identity into each Codex session, so
automatic and explicit recall remain repository-scoped even when the configured
namespace is shared by every worker.

Planning, execution, and remediation run autonomously inside the isolated job
container. Plan uses Codex's read-only Landlock path, removes `apply_patch` and
`request_permissions` before sampling, and still allows native shell and
subagent inspection. Its stage validator additionally requires a pure
decomposition. Native Rust functional acceptance is recorded in
`RUST_CUTOVER.md`; the superseded JavaScript fixtures have been deleted.

A recovered Plan first promotes a valid completed checkpoint instead of
discarding durable work. Only after a replacement turn is proven necessary does
the runtime restore the recorded Factory state and managed-worktree baseline,
immediately before starting that turn. The current attempt's thread correlation
is stored before any fenced Plan or review recovery write.

Cancellation, provider failure, validation failure, and worker shutdown close
and drain the native Codex session before rollback. Detached review snapshots
preserve tracked, staged, and nonignored untracked content while allowing
ignored build artifacts. A review mutation is restored and rejected, with a
durable marker retaining that fact across process death until Factory review
state has also rolled back. A running job remains `cancelling` until this cleanup
finishes; only then does the worker acknowledge its terminal cancellation.
Compose gives `factory-worker` a 75-second stop grace period so Docker does not
force-kill it during the app-server drain, rollback, and immediate lease
relinquishment path.
